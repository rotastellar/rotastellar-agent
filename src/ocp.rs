// subhadipmitra, 2026-03-09: Orbital Compute Primitives (I-1) — runtime
// intelligence layer for OCP step types. The agent remains event-replay
// based, but this module intercepts OCP events and enriches them with
// actual satellite state: battery levels for eclipse-steps, tier selection
// for window-steps, comms verification for pass-steps.

use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing::warn;

use crate::sim_client::SatelliteState;
use crate::types::AgentEvent;

// subhadipmitra, 2026-03-09: Constants matching the CAE planner. The agent
// uses these to make local decisions when CAE pre-computed values don't
// match actual conditions (e.g., eclipse shorter than planned).
const MIN_MARGIN_S: f64 = 5.0;
const ECLIPSE_DOD_MAX: f64 = 0.25;

/// Step types recognized by the OCP runtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StepType {
    Standard,
    EclipseStep,
    WindowStep,
    PassStep,
}

impl StepType {
    pub fn from_event_type(event_type: &str) -> Option<Self> {
        if event_type.starts_with("eclipse_step.") {
            Some(StepType::EclipseStep)
        } else if event_type.starts_with("window_step.") {
            Some(StepType::WindowStep)
        } else if event_type.starts_with("pass_step.") {
            Some(StepType::PassStep)
        } else {
            None
        }
    }
}

/// Quality tier definition for window-step adaptive execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QualityTier {
    pub id: String,
    pub duration_s: f64,
    pub output_data_mb: f64,
    pub output_quality: f64,
    pub switch_cost_s: f64,
}

/// Throughput estimator with exponential smoothing.
/// Used by the window-adaptive executor to predict whether the current
/// tier will complete within the remaining window.
#[derive(Debug)]
pub struct ThroughputEstimator {
    // subhadipmitra, 2026-03-09: Alpha controls smoothing. Higher = more
    // weight on recent observations. 0.3 is conservative — two bad readings
    // won't trigger immediate degradation.
    alpha: f64,
    estimated_rate: Option<f64>,
}

impl ThroughputEstimator {
    pub fn new(alpha: f64) -> Self {
        Self {
            alpha,
            estimated_rate: None,
        }
    }

    /// Update the estimate with a new observation.
    /// `fraction` is the work completed (0.0-1.0), `elapsed_s` is time taken.
    pub fn update(&mut self, fraction: f64, elapsed_s: f64) {
        if elapsed_s <= 0.0 || fraction <= 0.0 {
            return;
        }
        let observed_rate = fraction / elapsed_s;
        self.estimated_rate = Some(match self.estimated_rate {
            Some(prev) => self.alpha * observed_rate + (1.0 - self.alpha) * prev,
            None => observed_rate,
        });
    }

    /// Estimate time to complete remaining work.
    /// Returns None if no observations yet.
    pub fn estimate_remaining_s(&self, fraction_done: f64) -> Option<f64> {
        self.estimated_rate.map(|rate| {
            if rate <= 0.0 {
                f64::INFINITY
            } else {
                (1.0 - fraction_done) / rate
            }
        })
    }
}

/// Eclipse energy tracker — monitors battery usage during eclipse-step
/// execution and emits warnings when approaching limits.
#[derive(Debug)]
pub struct EclipseEnergyTracker {
    pub budget_j: f64,
    pub consumed_j: f64,
    pub power_w: f64,
    pub started_battery_wh: f64,
    pub half_check_emitted: bool,
}

impl EclipseEnergyTracker {
    pub fn new(budget_j: f64, power_w: f64, current_battery_wh: f64) -> Self {
        Self {
            budget_j,
            consumed_j: 0.0,
            power_w,
            started_battery_wh: current_battery_wh,
            half_check_emitted: false,
        }
    }

    /// Update energy consumed. Returns true if over budget.
    pub fn tick(&mut self, elapsed_s: f64) -> bool {
        self.consumed_j += self.power_w * elapsed_s;
        self.consumed_j > self.budget_j
    }

    /// Check if we've passed the 50% mark (for energy_check event).
    pub fn should_emit_half_check(&mut self) -> bool {
        if !self.half_check_emitted && self.consumed_j >= self.budget_j * 0.5 {
            self.half_check_emitted = true;
            return true;
        }
        false
    }

    pub fn remaining_j(&self) -> f64 {
        (self.budget_j - self.consumed_j).max(0.0)
    }
}

/// Select the best quality tier that fits within the available window.
///
/// Returns the tier index and tier reference. Tiers must be sorted
/// highest-quality-first (as defined in presets).
pub fn select_best_tier(tiers: &[QualityTier], available_s: f64) -> Option<usize> {
    // subhadipmitra, 2026-03-09: Try tiers in order (best first).
    // Each tier needs its duration_s + switch_cost_s + MIN_MARGIN_S.
    for (i, tier) in tiers.iter().enumerate() {
        let needed = tier.duration_s + tier.switch_cost_s + MIN_MARGIN_S;
        if available_s >= needed {
            return Some(i);
        }
    }
    None
}

/// Enrich an OCP event with runtime satellite state.
///
/// Called during event replay for OCP event types. Adds actual battery,
/// temperature, and computed fields to the event payload.
pub fn enrich_event(event: &mut AgentEvent, state: &SatelliteState) {
    let payload = &mut event.payload;
    if !payload.is_object() {
        return;
    }

    match StepType::from_event_type(&event.event_type) {
        Some(StepType::EclipseStep) => {
            // subhadipmitra, 2026-03-09: Inject actual battery state into
            // eclipse-step events so the console can compare planned vs actual.
            payload["actual_battery_wh"] = json!(round2(state.battery_wh));
            payload["actual_battery_percent"] = json!(round2(state.battery_percent()));
            payload["actual_temperature_c"] = json!(round2(state.temperature_c));

            if event.event_type == "eclipse_step.started" {
                let max_available_wh = state.battery_wh * ECLIPSE_DOD_MAX;
                let max_available_j = max_available_wh * 3600.0;
                payload["max_available_energy_j"] = json!(round2(max_available_j));
            }
        }
        Some(StepType::WindowStep) => {
            payload["actual_battery_percent"] = json!(round2(state.battery_percent()));
            payload["compute_capacity"] = json!(state.compute_capacity());
        }
        Some(StepType::PassStep) => {
            payload["actual_battery_percent"] = json!(round2(state.battery_percent()));
        }
        Some(StepType::Standard) | None => {}
    }
}

/// Check whether the satellite is in a valid state for the given OCP event.
/// Returns an error description if execution should be skipped/deferred.
pub fn validate_ocp_precondition(
    event_type: &str,
    state: &SatelliteState,
    in_eclipse: Option<bool>,
) -> Option<String> {
    match StepType::from_event_type(event_type) {
        Some(StepType::EclipseStep) if event_type.ends_with(".started") => {
            // subhadipmitra, 2026-03-09: Eclipse-steps should only start
            // during eclipse. If the satellite has left eclipse by the time
            // the event fires, warn (but don't abort — the CAE planned it).
            if let Some(false) = in_eclipse {
                warn!("eclipse_step.started but satellite is NOT in eclipse");
                return Some("satellite not in eclipse".into());
            }
            if state.battery_percent() < 20.0 {
                warn!(
                    battery = state.battery_percent(),
                    "eclipse_step.started with low battery"
                );
            }
            None
        }
        Some(StepType::PassStep) if event_type.ends_with(".started") => {
            // subhadipmitra, 2026-03-09: Pass-steps need comms. We log a
            // warning if battery is critically low but still proceed.
            if state.battery_percent() < 10.0 {
                return Some("battery critically low for pass-step".into());
            }
            None
        }
        _ => None,
    }
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use serde_json::json;

    #[test]
    fn test_step_type_from_event() {
        assert_eq!(
            StepType::from_event_type("eclipse_step.started"),
            Some(StepType::EclipseStep)
        );
        assert_eq!(
            StepType::from_event_type("window_step.completed"),
            Some(StepType::WindowStep)
        );
        assert_eq!(
            StepType::from_event_type("pass_step.started"),
            Some(StepType::PassStep)
        );
        assert_eq!(StepType::from_event_type("step.started"), None);
        assert_eq!(StepType::from_event_type("job.completed"), None);
    }

    #[test]
    fn test_throughput_estimator() {
        let mut est = ThroughputEstimator::new(0.3);
        assert!(est.estimate_remaining_s(0.0).is_none());

        // 25% done in 45s → rate = 0.00556/s
        est.update(0.25, 45.0);
        let remaining = est.estimate_remaining_s(0.25).unwrap();
        // Should estimate ~135s for remaining 75%
        assert!((remaining - 135.0).abs() < 1.0);

        // Faster observation: 50% done in 80s total → rate for this chunk = 0.25/35 = 0.00714
        est.update(0.25, 35.0);
        // Rate should be smoothed between old and new
        let remaining2 = est.estimate_remaining_s(0.50).unwrap();
        assert!(remaining2 < 135.0); // should be faster now
    }

    #[test]
    fn test_eclipse_energy_tracker() {
        let mut tracker = EclipseEnergyTracker::new(4800.0, 8.0, 80.0);
        assert!(!tracker.tick(100.0)); // 800J < 4800J
        assert!(!tracker.should_emit_half_check()); // 800 < 2400

        tracker.tick(200.0); // now 2400J = 50%
        assert!(tracker.should_emit_half_check());
        assert!(!tracker.should_emit_half_check()); // only once

        assert!(!tracker.tick(200.0)); // 4000J < 4800J
        assert!(tracker.tick(200.0)); // 5600J > 4800J — over budget
        assert_eq!(tracker.remaining_j(), 0.0);
    }

    #[test]
    fn test_select_best_tier() {
        let tiers = vec![
            QualityTier {
                id: "full".into(),
                duration_s: 180.0,
                output_data_mb: 50.0,
                output_quality: 1.0,
                switch_cost_s: 0.0,
            },
            QualityTier {
                id: "reduced".into(),
                duration_s: 90.0,
                output_data_mb: 25.0,
                output_quality: 0.7,
                switch_cost_s: 2.0,
            },
            QualityTier {
                id: "minimal".into(),
                duration_s: 30.0,
                output_data_mb: 5.0,
                output_quality: 0.3,
                switch_cost_s: 2.0,
            },
        ];

        // Plenty of time → full tier
        assert_eq!(select_best_tier(&tiers, 200.0), Some(0));
        // Not enough for full, but enough for reduced
        assert_eq!(select_best_tier(&tiers, 100.0), Some(1));
        // Only minimal fits
        assert_eq!(select_best_tier(&tiers, 40.0), Some(2));
        // Nothing fits
        assert_eq!(select_best_tier(&tiers, 30.0), None);
    }

    #[test]
    fn test_enrich_eclipse_event() {
        let mut state = SatelliteState::new();
        state.battery_wh = 75.0;
        state.temperature_c = -15.0;

        let mut event = AgentEvent {
            event_type: "eclipse_step.started".into(),
            timestamp: Utc::now().to_rfc3339(),
            job_id: "j1".into(),
            step_id: Some("eclipse_preprocess".into()),
            payload: json!({"energy_budget_j": 4800}),
        };

        enrich_event(&mut event, &state);

        assert_eq!(event.payload["actual_battery_wh"], 75.0);
        assert_eq!(event.payload["actual_battery_percent"], 75.0);
        assert_eq!(event.payload["actual_temperature_c"], -15.0);
        // max_available = 75 * 0.25 * 3600 = 67500J
        assert!(event.payload["max_available_energy_j"].as_f64().unwrap() > 60000.0);
    }

    #[test]
    fn test_enrich_window_event() {
        let mut state = SatelliteState::new();
        state.battery_wh = 50.0;

        let mut event = AgentEvent {
            event_type: "window_step.started".into(),
            timestamp: Utc::now().to_rfc3339(),
            job_id: "j1".into(),
            step_id: Some("adaptive_analysis".into()),
            payload: json!({"planned_tier": "full"}),
        };

        enrich_event(&mut event, &state);

        assert_eq!(event.payload["actual_battery_percent"], 50.0);
        assert_eq!(event.payload["compute_capacity"], 1.0);
    }

    #[test]
    fn test_validate_eclipse_not_in_eclipse() {
        let state = SatelliteState::new();
        let result = validate_ocp_precondition("eclipse_step.started", &state, Some(false));
        assert!(result.is_some());
        assert!(result.unwrap().contains("not in eclipse"));
    }

    #[test]
    fn test_validate_eclipse_in_eclipse() {
        let state = SatelliteState::new();
        let result = validate_ocp_precondition("eclipse_step.started", &state, Some(true));
        assert!(result.is_none());
    }

    #[test]
    fn test_validate_pass_low_battery() {
        let mut state = SatelliteState::new();
        state.battery_wh = 5.0;
        let result = validate_ocp_precondition("pass_step.started", &state, None);
        assert!(result.is_some());
    }

    #[test]
    fn test_standard_events_pass_validation() {
        let state = SatelliteState::new();
        assert!(validate_ocp_precondition("step.started", &state, None).is_none());
        assert!(validate_ocp_precondition("job.completed", &state, None).is_none());
    }
}
