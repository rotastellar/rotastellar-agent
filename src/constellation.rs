// subhadipmitra, 2026-03-10: WS4 — ConstellationExecutor module. Adds multi-satellite
// DAG awareness to the agent. Tracks assigned steps, coordinates ISL transfers with
// actual link quality from the sim service, handles failover when steps fail, and
// reports constellation-level progress events.
//
// Architecture: This module sits between the event replay loop and the event reporter.
// It intercepts constellation-specific events, enriches them with actual satellite
// state, and manages local DAG execution state.

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use tracing::{info, warn};

use crate::sim_client::SatelliteState;
use crate::types::{AgentEvent, Position};

/// Tracks the state of a single DAG step assigned to this satellite.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssignedStep {
    pub step_id: String,
    pub step_name: String,
    pub assigned_at: String,
    pub status: StepStatus,
    pub duration_s: f64,
    pub dependencies: Vec<String>,
    pub is_replica: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Pending,
    Executing,
    Completed,
    Failed,
    FailedOver,
}

/// Tracks an ISL transfer in progress.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ISLTransfer {
    pub transfer_id: String,
    pub from_satellite: String,
    pub to_satellite: String,
    pub data_mb: f64,
    pub hops: Vec<ISLHop>,
    pub started_at: String,
    pub status: TransferStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ISLHop {
    pub from: String,
    pub to: String,
    pub distance_km: f64,
    pub latency_ms: f64,
    pub bw_mbps: f64,
    pub quality: f64,
    pub completed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferStatus {
    InProgress,
    Completed,
    Failed,
}

/// ConstellationState tracks the local view of a constellation DAG execution.
#[derive(Debug, Clone)]
pub struct ConstellationState {
    /// Steps assigned to this satellite
    pub assigned_steps: HashMap<String, AssignedStep>,
    /// Active ISL transfers involving this satellite
    pub active_transfers: HashMap<String, ISLTransfer>,
    /// Steps completed on this satellite
    pub completed_steps: Vec<String>,
    /// Steps that failed and were handed off
    pub failed_over_steps: Vec<String>,
    /// This satellite's ID
    pub satellite_id: String,
    /// Accumulated ISL data transferred (MB)
    pub total_isl_data_mb: f64,
}

impl ConstellationState {
    pub fn new(satellite_id: String) -> Self {
        Self {
            assigned_steps: HashMap::new(),
            active_transfers: HashMap::new(),
            completed_steps: Vec::new(),
            failed_over_steps: Vec::new(),
            satellite_id,
            total_isl_data_mb: 0.0,
        }
    }

    /// Number of steps assigned to this satellite.
    pub fn steps_assigned(&self) -> usize {
        self.assigned_steps.len()
    }

    /// Number of steps completed on this satellite.
    pub fn steps_completed(&self) -> usize {
        self.completed_steps.len()
    }

    /// Whether all assigned steps are done (completed or failed over).
    pub fn all_done(&self) -> bool {
        self.assigned_steps
            .values()
            .all(|s| s.status == StepStatus::Completed || s.status == StepStatus::FailedOver)
    }
}

/// Check if an event is constellation-related.
pub fn is_constellation_event(event: &AgentEvent) -> bool {
    event.event_type.starts_with("constellation.")
        || event.event_type.starts_with("isl_transfer.")
}

/// Process a constellation event and update local state.
/// Returns optional enriched/additional events to emit.
pub fn handle_event(
    event: &AgentEvent,
    state: &mut ConstellationState,
    sat_state: &SatelliteState,
    position: Option<&Position>,
) -> Vec<AgentEvent> {
    let mut extra_events = Vec::new();

    match event.event_type.as_str() {
        "constellation.step_assigned" => {
            handle_step_assigned(event, state);
        }

        "constellation.step_started" => {
            if let Some(step_id) = &event.step_id {
                if let Some(step) = state.assigned_steps.get_mut(step_id) {
                    step.status = StepStatus::Executing;
                    info!(step_id = %step_id, step_name = %step.step_name, "Step execution started");
                }
            }
        }

        "constellation.step_completed" => {
            if let Some(step_id) = &event.step_id {
                if let Some(step) = state.assigned_steps.get_mut(step_id) {
                    step.status = StepStatus::Completed;
                    state.completed_steps.push(step_id.clone());
                    info!(
                        step_id = %step_id,
                        completed = state.completed_steps.len(),
                        total = state.assigned_steps.len(),
                        "Step completed"
                    );

                    // If all steps done, emit constellation progress event
                    if state.all_done() {
                        extra_events.push(build_progress_event(event, state, sat_state));
                    }
                }
            }
        }

        "constellation.failover" => {
            handle_failover(event, state, &mut extra_events);
        }

        "isl_transfer.started" => {
            handle_isl_started(event, state);
        }

        "isl_transfer.hop_completed" => {
            handle_isl_hop(event, state, position, &mut extra_events);
        }

        "isl_transfer.completed" => {
            handle_isl_completed(event, state, sat_state, &mut extra_events);
        }

        _ => {}
    }

    extra_events
}

/// Enrich a constellation event with actual satellite state.
pub fn enrich_event(
    event: &mut AgentEvent,
    sat_state: &SatelliteState,
    position: Option<&Position>,
) {
    if !is_constellation_event(event) {
        return;
    }

    // Add satellite state to all constellation events
    let payload = event.payload.as_object_mut();
    if let Some(p) = payload {
        p.insert(
            "actual_battery_percent".to_string(),
            json!(sat_state.battery_percent()),
        );
        p.insert(
            "actual_temperature_c".to_string(),
            json!(sat_state.temperature_c),
        );

        if let Some(pos) = position {
            p.insert("actual_lat".to_string(), json!(pos.lat));
            p.insert("actual_lon".to_string(), json!(pos.lon));
            p.insert("actual_altitude_km".to_string(), json!(pos.altitude_km));
            p.insert("actual_in_eclipse".to_string(), json!(pos.in_eclipse));
        }
    }
}

/// Check if the satellite can execute a constellation step.
/// Returns Some(reason) if the step should be failed over.
pub fn check_failover_condition(
    sat_state: &SatelliteState,
    _position: Option<&Position>,
) -> Option<String> {
    // Critical battery — can't sustain compute
    if sat_state.battery_percent() < 10.0 {
        return Some(format!(
            "Battery critically low ({:.1}%), cannot execute step",
            sat_state.battery_percent()
        ));
    }

    // Thermal shutdown
    if sat_state.temperature_c > 75.0 {
        return Some(format!(
            "Temperature too high ({:.1}°C), thermal protection active",
            sat_state.temperature_c
        ));
    }

    // Compute not available
    if !sat_state.compute_available() {
        return Some("Compute not available (battery below 15%)".to_string());
    }

    None
}

// ── Internal handlers ────────────────────────────────────────────────────

fn handle_step_assigned(event: &AgentEvent, state: &mut ConstellationState) {
    let step_id = event
        .step_id
        .clone()
        .unwrap_or_else(|| "unknown".to_string());
    let step_name = event
        .payload
        .get("step_name")
        .and_then(|v| v.as_str())
        .unwrap_or("unnamed")
        .to_string();
    let duration_s = event
        .payload
        .get("duration_s")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let is_replica = event
        .payload
        .get("is_replica")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let dependencies: Vec<String> = event
        .payload
        .get("dependencies")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let step = AssignedStep {
        step_id: step_id.clone(),
        step_name: step_name.clone(),
        assigned_at: event.timestamp.clone(),
        status: StepStatus::Pending,
        duration_s,
        dependencies,
        is_replica,
    };

    info!(
        step_id = %step_id,
        step_name = %step_name,
        is_replica = is_replica,
        "Constellation step assigned to this satellite"
    );

    state.assigned_steps.insert(step_id, step);
}

fn handle_failover(
    event: &AgentEvent,
    state: &mut ConstellationState,
    extra_events: &mut Vec<AgentEvent>,
) {
    if let Some(step_id) = &event.step_id {
        if let Some(step) = state.assigned_steps.get_mut(step_id) {
            step.status = StepStatus::FailedOver;
            state.failed_over_steps.push(step_id.clone());

            let reason = event
                .payload
                .get("reason")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");

            warn!(
                step_id = %step_id,
                reason = %reason,
                "Step failed over to another satellite"
            );

            // Emit acknowledgment event
            extra_events.push(AgentEvent {
                event_type: "constellation.failover_acknowledged".to_string(),
                timestamp: Utc::now().to_rfc3339(),
                job_id: event.job_id.clone(),
                step_id: Some(step_id.clone()),
                payload: json!({
                    "satellite_id": state.satellite_id,
                    "reason": reason,
                    "steps_remaining": state.assigned_steps.values()
                        .filter(|s| s.status == StepStatus::Pending || s.status == StepStatus::Executing)
                        .count(),
                }),
            });
        }
    }
}

fn handle_isl_started(event: &AgentEvent, state: &mut ConstellationState) {
    let transfer_id = event
        .payload
        .get("transfer_id")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let from = event
        .payload
        .get("from_satellite")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let to = event
        .payload
        .get("to_satellite")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let data_mb = event
        .payload
        .get("data_mb")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);

    info!(
        transfer_id = %transfer_id,
        from = %from,
        to = %to,
        data_mb = data_mb,
        "ISL transfer started"
    );

    state.active_transfers.insert(
        transfer_id.clone(),
        ISLTransfer {
            transfer_id,
            from_satellite: from,
            to_satellite: to,
            data_mb,
            hops: Vec::new(),
            started_at: event.timestamp.clone(),
            status: TransferStatus::InProgress,
        },
    );
}

fn handle_isl_hop(
    event: &AgentEvent,
    state: &mut ConstellationState,
    position: Option<&Position>,
    _extra_events: &mut Vec<AgentEvent>,
) {
    let transfer_id = event
        .payload
        .get("transfer_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if let Some(transfer) = state.active_transfers.get_mut(transfer_id) {
        let from = event
            .payload
            .get("from")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let to = event
            .payload
            .get("to")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        // Use actual position to estimate ISL link quality if available
        let (distance_km, quality) = if let Some(pos) = position {
            let dist = event
                .payload
                .get("distance_km")
                .and_then(|v| v.as_f64())
                .unwrap_or(1000.0);
            // Quality degrades with distance, worse in eclipse
            let base_quality = 1.0 - (dist / 5000.0) * 0.6;
            let eclipse_penalty = if pos.in_eclipse { 0.9 } else { 1.0 };
            (dist, (base_quality * eclipse_penalty).max(0.0))
        } else {
            let dist = event
                .payload
                .get("distance_km")
                .and_then(|v| v.as_f64())
                .unwrap_or(1000.0);
            (dist, 0.85)
        };

        let bw_mbps = 100.0 * quality;
        let latency_ms = (distance_km / 299792.458) * 1000.0 + 2.0;

        let hop = ISLHop {
            from,
            to,
            distance_km,
            latency_ms,
            bw_mbps,
            quality,
            completed: true,
        };

        info!(
            transfer_id = %transfer_id,
            hop = transfer.hops.len() + 1,
            quality = quality,
            bw_mbps = bw_mbps,
            "ISL hop completed"
        );

        transfer.hops.push(hop);
    }
}

fn handle_isl_completed(
    event: &AgentEvent,
    state: &mut ConstellationState,
    sat_state: &SatelliteState,
    extra_events: &mut Vec<AgentEvent>,
) {
    let transfer_id = event
        .payload
        .get("transfer_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if let Some(transfer) = state.active_transfers.get_mut(transfer_id) {
        transfer.status = TransferStatus::Completed;
        state.total_isl_data_mb += transfer.data_mb;

        let avg_quality = if transfer.hops.is_empty() {
            0.0
        } else {
            transfer.hops.iter().map(|h| h.quality).sum::<f64>() / transfer.hops.len() as f64
        };

        let total_latency_ms: f64 = transfer.hops.iter().map(|h| h.latency_ms).sum();

        info!(
            transfer_id = %transfer_id,
            hops = transfer.hops.len(),
            data_mb = transfer.data_mb,
            avg_quality = avg_quality,
            total_latency_ms = total_latency_ms,
            "ISL transfer completed"
        );

        // Emit enriched completion event
        extra_events.push(AgentEvent {
            event_type: "isl_transfer.quality_report".to_string(),
            timestamp: Utc::now().to_rfc3339(),
            job_id: event.job_id.clone(),
            step_id: event.step_id.clone(),
            payload: json!({
                "transfer_id": transfer_id,
                "hops": transfer.hops.len(),
                "data_mb": transfer.data_mb,
                "avg_link_quality": (avg_quality * 1000.0).round() / 1000.0,
                "total_latency_ms": (total_latency_ms * 10.0).round() / 10.0,
                "total_isl_data_mb": state.total_isl_data_mb,
                "battery_after_transfer": sat_state.battery_percent(),
            }),
        });
    }
}

fn build_progress_event(
    event: &AgentEvent,
    state: &ConstellationState,
    sat_state: &SatelliteState,
) -> AgentEvent {
    AgentEvent {
        event_type: "constellation.satellite_complete".to_string(),
        timestamp: Utc::now().to_rfc3339(),
        job_id: event.job_id.clone(),
        step_id: None,
        payload: json!({
            "satellite_id": state.satellite_id,
            "steps_completed": state.completed_steps.len(),
            "steps_failed_over": state.failed_over_steps.len(),
            "total_isl_data_mb": state.total_isl_data_mb,
            "battery_percent": sat_state.battery_percent(),
            "temperature_c": sat_state.temperature_c,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_event(event_type: &str, step_id: Option<&str>, payload: serde_json::Value) -> AgentEvent {
        AgentEvent {
            event_type: event_type.to_string(),
            timestamp: "2026-03-10T12:00:00Z".to_string(),
            job_id: "job-1".to_string(),
            step_id: step_id.map(String::from),
            payload,
        }
    }

    fn make_sat_state() -> SatelliteState {
        SatelliteState {
            battery_wh: 80.0,
            temperature_c: 25.0,
            last_update: Utc::now(),
        }
    }

    #[test]
    fn test_is_constellation_event() {
        assert!(is_constellation_event(&make_event("constellation.step_assigned", None, json!({}))));
        assert!(is_constellation_event(&make_event("isl_transfer.started", None, json!({}))));
        assert!(!is_constellation_event(&make_event("step.completed", None, json!({}))));
        assert!(!is_constellation_event(&make_event("job.accepted", None, json!({}))));
    }

    #[test]
    fn test_step_assignment_and_completion() {
        let mut state = ConstellationState::new("sat-99901".to_string());
        let sat_state = make_sat_state();

        // Assign a step
        let assign_event = make_event(
            "constellation.step_assigned",
            Some("step-1"),
            json!({ "step_name": "feature_extraction", "duration_s": 180.0, "is_replica": false }),
        );
        handle_event(&assign_event, &mut state, &sat_state, None);
        assert_eq!(state.steps_assigned(), 1);
        assert_eq!(state.assigned_steps["step-1"].status, StepStatus::Pending);

        // Start it
        let start_event = make_event("constellation.step_started", Some("step-1"), json!({}));
        handle_event(&start_event, &mut state, &sat_state, None);
        assert_eq!(state.assigned_steps["step-1"].status, StepStatus::Executing);

        // Complete it
        let complete_event = make_event("constellation.step_completed", Some("step-1"), json!({}));
        let extras = handle_event(&complete_event, &mut state, &sat_state, None);
        assert_eq!(state.assigned_steps["step-1"].status, StepStatus::Completed);
        assert_eq!(state.steps_completed(), 1);
        assert!(state.all_done());
        // Should emit satellite_complete event
        assert_eq!(extras.len(), 1);
        assert_eq!(extras[0].event_type, "constellation.satellite_complete");
    }

    #[test]
    fn test_failover() {
        let mut state = ConstellationState::new("sat-99901".to_string());
        let sat_state = make_sat_state();

        let assign = make_event(
            "constellation.step_assigned",
            Some("step-1"),
            json!({ "step_name": "inference", "duration_s": 60.0 }),
        );
        handle_event(&assign, &mut state, &sat_state, None);

        let failover = make_event(
            "constellation.failover",
            Some("step-1"),
            json!({ "reason": "battery_low", "target_satellite": "sat-99902" }),
        );
        let extras = handle_event(&failover, &mut state, &sat_state, None);
        assert_eq!(state.assigned_steps["step-1"].status, StepStatus::FailedOver);
        assert_eq!(state.failed_over_steps.len(), 1);
        assert_eq!(extras.len(), 1);
        assert_eq!(extras[0].event_type, "constellation.failover_acknowledged");
    }

    #[test]
    fn test_isl_transfer_lifecycle() {
        let mut state = ConstellationState::new("sat-99901".to_string());
        let sat_state = make_sat_state();
        let pos = Position { lat: 45.0, lon: 10.0, altitude_km: 550.0, in_eclipse: false };

        // Start transfer
        let start = make_event(
            "isl_transfer.started",
            None,
            json!({ "transfer_id": "tx-1", "from_satellite": "sat-99901", "to_satellite": "sat-99902", "data_mb": 50.0 }),
        );
        handle_event(&start, &mut state, &sat_state, Some(&pos));
        assert_eq!(state.active_transfers.len(), 1);

        // Hop completed
        let hop = make_event(
            "isl_transfer.hop_completed",
            None,
            json!({ "transfer_id": "tx-1", "from": "sat-99901", "to": "sat-99902", "distance_km": 1200.0 }),
        );
        handle_event(&hop, &mut state, &sat_state, Some(&pos));
        assert_eq!(state.active_transfers["tx-1"].hops.len(), 1);
        assert!(state.active_transfers["tx-1"].hops[0].quality > 0.0);

        // Transfer completed
        let done = make_event(
            "isl_transfer.completed",
            None,
            json!({ "transfer_id": "tx-1" }),
        );
        let extras = handle_event(&done, &mut state, &sat_state, Some(&pos));
        assert_eq!(state.active_transfers["tx-1"].status, TransferStatus::Completed);
        assert_eq!(state.total_isl_data_mb, 50.0);
        assert_eq!(extras.len(), 1);
        assert_eq!(extras[0].event_type, "isl_transfer.quality_report");
    }

    #[test]
    fn test_check_failover_condition() {
        // Healthy state
        let healthy = SatelliteState {
            battery_wh: 80.0,
            temperature_c: 25.0,
            last_update: Utc::now(),
        };
        assert!(check_failover_condition(&healthy, None).is_none());

        // Low battery
        let low_battery = SatelliteState {
            battery_wh: 5.0,
            temperature_c: 25.0,
            last_update: Utc::now(),
        };
        assert!(check_failover_condition(&low_battery, None).is_some());

        // High temperature
        let hot = SatelliteState {
            battery_wh: 80.0,
            temperature_c: 80.0,
            last_update: Utc::now(),
        };
        assert!(check_failover_condition(&hot, None).is_some());
    }

    #[test]
    fn test_enrich_event() {
        let sat_state = make_sat_state();
        let pos = Position { lat: 42.0, lon: -71.0, altitude_km: 550.0, in_eclipse: true };

        let mut event = make_event("constellation.step_completed", Some("s1"), json!({}));
        enrich_event(&mut event, &sat_state, Some(&pos));

        let p = event.payload.as_object().unwrap();
        assert!(p.contains_key("actual_battery_percent"));
        assert!(p.contains_key("actual_temperature_c"));
        assert!(p.contains_key("actual_in_eclipse"));
        assert_eq!(p["actual_in_eclipse"], json!(true));
    }

    #[test]
    fn test_non_constellation_event_not_enriched() {
        let sat_state = make_sat_state();
        let mut event = make_event("step.completed", Some("s1"), json!({"data": 1}));
        enrich_event(&mut event, &sat_state, None);
        // Should not be modified
        assert!(!event.payload.as_object().unwrap().contains_key("actual_battery_percent"));
    }

    #[test]
    fn test_multiple_steps_not_all_done() {
        let mut state = ConstellationState::new("sat-99901".to_string());
        let sat_state = make_sat_state();

        // Assign two steps
        let a1 = make_event("constellation.step_assigned", Some("s1"), json!({"step_name": "a"}));
        let a2 = make_event("constellation.step_assigned", Some("s2"), json!({"step_name": "b"}));
        handle_event(&a1, &mut state, &sat_state, None);
        handle_event(&a2, &mut state, &sat_state, None);
        assert!(!state.all_done());

        // Complete one
        let c1 = make_event("constellation.step_completed", Some("s1"), json!({}));
        let extras = handle_event(&c1, &mut state, &sat_state, None);
        assert!(!state.all_done());
        assert!(extras.is_empty()); // No satellite_complete yet

        // Complete second
        let c2 = make_event("constellation.step_completed", Some("s2"), json!({}));
        let extras = handle_event(&c2, &mut state, &sat_state, None);
        assert!(state.all_done());
        assert_eq!(extras.len(), 1); // Now emits satellite_complete
    }
}
