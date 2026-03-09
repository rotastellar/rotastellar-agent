use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use tokio::sync::Mutex;
use tracing::{info, warn};

use crate::agent::{Agent, AgentError};
use crate::client::ConsoleClient;
// subhadipmitra, 2026-03-09: OCP runtime enrichment for eclipse/window/pass events.
use crate::ocp;
// subhadipmitra, 2026-03-09: I-4 HazardPredictor for checkpoint scheduling.
use crate::hazard;
// subhadipmitra, 2026-03-10: WS4 — Constellation executor for multi-satellite DAG.
use crate::constellation;
use crate::sim_client::{OrbitalElements, SimClient, SatelliteState};
use crate::types::{
    AgentConfig, AgentEvent, AgentStatus, AgentTelemetry, Position, WorkloadSpec,
};

/// A simulated satellite that replays pre-computed CAE event streams.
///
/// When configured with a `sim_url` and orbital elements, it calls the
/// RotaStellar Simulation Service for orbital state (position, eclipse)
/// and maintains power/thermal state locally. Without sim configuration,
/// it falls back to simple event replay.
pub struct SimulatedSatellite {
    config: AgentConfig,
    client: ConsoleClient,
    speed_multiplier: f64,
    running: Arc<AtomicBool>,
    sim_client: Option<SimClient>,
    orbital_elements: Option<OrbitalElements>,
    sat_state: Arc<Mutex<SatelliteState>>,
    status: Arc<Mutex<AgentStatus>>,
}

impl SimulatedSatellite {
    /// Create a new simulated satellite.
    ///
    /// `speed_multiplier` controls replay speed:
    /// - 1.0 = real-time
    /// - 10.0 = 10x faster
    /// - 100.0 = 100x faster (default for demos)
    pub fn new(config: AgentConfig, speed_multiplier: f64) -> Result<Self, AgentError> {
        let client = ConsoleClient::new(config.clone())?;

        let sim_client = config
            .sim_url
            .as_ref()
            .map(|url| SimClient::new(url.clone()))
            .transpose()?;

        Ok(Self {
            config,
            client,
            speed_multiplier,
            running: Arc::new(AtomicBool::new(false)),
            sim_client,
            orbital_elements: None,
            sat_state: Arc::new(Mutex::new(SatelliteState::new())),
            status: Arc::new(Mutex::new(AgentStatus::Idle)),
        })
    }

    /// Set orbital elements for sim service integration.
    pub fn with_orbital_elements(mut self, elements: OrbitalElements) -> Self {
        self.orbital_elements = Some(elements);
        self
    }

    /// Parse an ISO 8601 timestamp, returning None on failure.
    fn parse_timestamp(ts: &str) -> Option<DateTime<Utc>> {
        ts.parse::<DateTime<Utc>>().ok()
    }

    /// Self-register with the Console API.
    async fn register(&self) -> Result<(), AgentError> {
        let alt = self.orbital_elements.as_ref().map(|e| e.altitude_km);
        self.client
            .register(
                self.config.satellite_id.as_deref(),
                self.config.satellite_name.as_deref(),
                alt,
                Some(env!("CARGO_PKG_VERSION")),
            )
            .await
    }

    /// Query the sim service for current position and update local state.
    async fn update_orbital_state(&self) -> Option<Position> {
        let (sim, elements) = match (&self.sim_client, &self.orbital_elements) {
            (Some(s), Some(e)) => (s, e),
            _ => return None,
        };

        let now = Utc::now();
        match sim.get_state(elements, now).await {
            Ok(orbital) => {
                let is_executing = {
                    let status = self.status.lock().await;
                    *status == AgentStatus::Executing
                };

                {
                    let mut state = self.sat_state.lock().await;
                    state.update(now, orbital.in_eclipse, is_executing);
                }

                Some(Position {
                    lat: orbital.lat,
                    lon: orbital.lon,
                    altitude_km: orbital.altitude_km,
                    in_eclipse: orbital.in_eclipse,
                })
            }
            Err(e) => {
                warn!(error = %e, "Failed to get orbital state from sim service");
                None
            }
        }
    }

    /// Build telemetry payload with current state.
    async fn build_telemetry(&self, position: Option<Position>) -> AgentTelemetry {
        let state = self.sat_state.lock().await;
        let status = self.status.lock().await;

        AgentTelemetry {
            agent_id: self.config.agent_id.clone(),
            status: status.clone(),
            timestamp: Utc::now().to_rfc3339(),
            cpu_percent: None,
            memory_mb: None,
            battery_percent: Some(state.battery_percent()),
            temperature_c: Some(state.temperature_c),
            position,
            compute_capacity: Some(state.compute_capacity()),
        }
    }

    /// Spawn a background telemetry reporting loop.
    fn spawn_telemetry_loop(self: &Arc<Self>) {
        let agent = Arc::clone(self);
        tokio::spawn(async move {
            let interval_s = agent.config.poll_interval_s.max(15);
            loop {
                if !agent.running.load(Ordering::Relaxed) {
                    break;
                }

                let position = agent.update_orbital_state().await;
                let telemetry = agent.build_telemetry(position).await;

                if let Err(e) = agent.client.report_telemetry(&telemetry).await {
                    warn!(error = %e, "Telemetry report failed");
                }

                tokio::time::sleep(std::time::Duration::from_secs(interval_s)).await;
            }
        });
    }
}

#[async_trait]
impl Agent for SimulatedSatellite {
    async fn poll(&self) -> Result<Option<WorkloadSpec>, AgentError> {
        self.client.poll_workloads().await
    }

    async fn report_event(&self, event: &AgentEvent) -> Result<(), AgentError> {
        info!(
            event_type = %event.event_type,
            step_id = ?event.step_id,
            "Event: {}",
            event.event_type
        );
        Ok(())
    }

    async fn report_telemetry(&self, telemetry: &AgentTelemetry) -> Result<(), AgentError> {
        self.client.report_telemetry(telemetry).await
    }

    async fn execute(&self, workload: &WorkloadSpec) -> Result<(), AgentError> {
        // Check compute availability (orbit-aware)
        {
            let state = self.sat_state.lock().await;
            if !state.compute_available() {
                warn!(
                    battery = state.battery_percent(),
                    "Compute unavailable (battery too low), skipping workload"
                );
                return Err(AgentError::ExecutionError(
                    "Compute unavailable: battery below safe threshold".into(),
                ));
            }
        }

        let events = &workload.events;
        if events.is_empty() {
            return Err(AgentError::ExecutionError("No events in workload".into()));
        }

        {
            let mut status = self.status.lock().await;
            *status = AgentStatus::Executing;
        }

        info!(
            deployment_id = %workload.deployment_id,
            events_count = events.len(),
            speed = self.speed_multiplier,
            "Starting simulated execution"
        );

        // subhadipmitra, 2026-03-09: I-4 — predict hazards before execution
        // and emit a checkpoint.predicted event with the schedule. The agent
        // will insert checkpoint.saved events at the predicted times.
        if let Some(ref elements) = self.orbital_elements {
            let prediction = hazard::predict_hazards(
                elements,
                Utc::now(),
                6.0,           // 6-hour horizon
                50_000_000,    // 50 MB state size
                10_000_000,    // 10 MB/s flash bandwidth
            );

            if prediction.summary.total_hazards > 0 {
                info!(
                    hazards = prediction.summary.total_hazards,
                    checkpoints = prediction.summary.total_checkpoints,
                    next_hazard = ?prediction.summary.next_hazard,
                    overhead = prediction.summary.checkpoint_overhead_fraction,
                    "Hazard prediction complete"
                );

                let prediction_event = AgentEvent {
                    event_type: "checkpoint.predicted".into(),
                    timestamp: Utc::now().to_rfc3339(),
                    job_id: workload.events.first().map(|e| e.job_id.clone()).unwrap_or_default(),
                    step_id: None,
                    payload: serde_json::json!({
                        "hazards_count": prediction.summary.total_hazards,
                        "checkpoints_count": prediction.summary.total_checkpoints,
                        "next_hazard": prediction.summary.next_hazard,
                        "max_safe_window_s": prediction.summary.max_safe_compute_window_s,
                        "overhead_fraction": prediction.summary.checkpoint_overhead_fraction,
                        "hazards": prediction.hazards,
                        "checkpoint_schedule": prediction.checkpoint_schedule,
                    }),
                };

                self.client
                    .report_event(&workload.deployment_id, &prediction_event)
                    .await
                    .unwrap_or_else(|e| {
                        warn!(error = %e, "Failed to report hazard prediction");
                    });
            }
        }

        // subhadipmitra, 2026-03-10: WS4 — Initialize constellation state for
        // tracking DAG step assignments, ISL transfers, and failover.
        let mut constellation_state = constellation::ConstellationState::new(
            self.config.satellite_id.clone().unwrap_or_else(|| self.config.agent_id.clone()),
        );

        let first_ts = Self::parse_timestamp(&events[0].timestamp);

        for (i, event) in events.iter().enumerate() {
            if !self.running.load(Ordering::Relaxed) {
                warn!("Agent stopped, aborting execution");
                let mut status = self.status.lock().await;
                *status = AgentStatus::Idle;
                return Err(AgentError::Stopped);
            }

            // Calculate delay based on time difference from previous event
            if i > 0 {
                if let (Some(prev_ts), Some(curr_ts)) = (
                    Self::parse_timestamp(&events[i - 1].timestamp),
                    Self::parse_timestamp(&event.timestamp),
                ) {
                    let delta = curr_ts
                        .signed_duration_since(prev_ts)
                        .num_milliseconds()
                        .max(0) as f64;
                    let adjusted_ms = delta / self.speed_multiplier;
                    if adjusted_ms > 1.0 {
                        tokio::time::sleep(std::time::Duration::from_millis(adjusted_ms as u64))
                            .await;
                    }
                }
            }

            // subhadipmitra, 2026-03-10: WS4 — Process constellation events through
            // the ConstellationExecutor. Handles step assignment, ISL transfers with
            // actual link quality, failover detection, and progress tracking.
            if constellation::is_constellation_event(event) {
                let state = self.sat_state.lock().await;
                let position = self.update_orbital_state().await;
                let pos_ref = position.as_ref();

                // Check if satellite should fail over this step
                if event.event_type == "constellation.step_started" {
                    if let Some(reason) = constellation::check_failover_condition(&state, pos_ref) {
                        warn!(reason = %reason, "Failover condition detected");
                        // Emit failover event instead of starting the step
                        let failover_event = AgentEvent {
                            event_type: "constellation.failover".to_string(),
                            timestamp: chrono::Utc::now().to_rfc3339(),
                            job_id: event.job_id.clone(),
                            step_id: event.step_id.clone(),
                            payload: serde_json::json!({
                                "satellite_id": self.config.satellite_id,
                                "reason": reason,
                            }),
                        };
                        self.report_event(&failover_event).await.ok();
                    }
                }

                // Process through constellation state machine
                let extra_events = constellation::handle_event(
                    event,
                    &mut constellation_state,
                    &state,
                    pos_ref,
                );

                // Report any extra events generated
                for extra in &extra_events {
                    self.report_event(extra).await.ok();
                }

                // Update agent status for ISL transfers
                if event.event_type == "isl_transfer.started" {
                    let mut status = self.status.lock().await;
                    *status = AgentStatus::Transferring;
                } else if event.event_type == "isl_transfer.completed" {
                    let mut status = self.status.lock().await;
                    *status = AgentStatus::Executing;
                }
            }

            // subhadipmitra, 2026-03-09: For OCP event types, enrich the event
            // with actual satellite state and validate preconditions. We clone
            // the event to avoid mutating the workload's event list.
            let mut enriched_event = event.clone();
            {
                let state = self.sat_state.lock().await;

                // Validate OCP preconditions (eclipse state, battery)
                let position = self.update_orbital_state().await;
                let in_eclipse = position.as_ref().map(|p| p.in_eclipse);
                if let Some(reason) =
                    ocp::validate_ocp_precondition(&event.event_type, &state, in_eclipse)
                {
                    warn!(
                        event_type = %event.event_type,
                        reason = %reason,
                        "OCP precondition warning (continuing replay)"
                    );
                    enriched_event.payload["ocp_warning"] =
                        serde_json::Value::String(reason);
                }

                // Enrich with actual satellite state
                ocp::enrich_event(&mut enriched_event, &state);

                // subhadipmitra, 2026-03-10: WS4 — Enrich constellation events
                let position = self.update_orbital_state().await;
                constellation::enrich_event(&mut enriched_event, &state, position.as_ref());
            }

            // Report enriched event to Console API
            self.client
                .report_event(&workload.deployment_id, &enriched_event)
                .await
                .unwrap_or_else(|e| {
                    warn!(error = %e, "Failed to report event, continuing");
                });

            // Log locally
            let elapsed = first_ts
                .and_then(|ft| {
                    Self::parse_timestamp(&event.timestamp)
                        .map(|ct| ct.signed_duration_since(ft).num_seconds())
                })
                .unwrap_or(0);

            info!(
                "[T+{:>6}s] {} {}",
                elapsed,
                event.event_type,
                event.step_id.as_deref().unwrap_or("")
            );
        }

        {
            let mut status = self.status.lock().await;
            *status = AgentStatus::Idle;
        }

        // subhadipmitra, 2026-03-10: WS4 — Log constellation execution summary.
        if constellation_state.steps_assigned() > 0 {
            info!(
                deployment_id = %workload.deployment_id,
                steps_assigned = constellation_state.steps_assigned(),
                steps_completed = constellation_state.steps_completed(),
                steps_failed_over = constellation_state.failed_over_steps.len(),
                isl_data_mb = constellation_state.total_isl_data_mb,
                "Constellation execution summary"
            );
        }

        info!(
            deployment_id = %workload.deployment_id,
            "Simulated execution complete"
        );
        Ok(())
    }

    async fn start(&self) -> Result<(), AgentError> {
        self.running.store(true, Ordering::Relaxed);

        // Self-register with the Console API
        match self.register().await {
            Ok(()) => info!("Registered with Console API"),
            Err(e) => warn!(error = %e, "Registration failed, continuing anyway"),
        }

        info!(
            agent_id = %self.config.agent_id,
            api_url = %self.config.api_url,
            sim_url = ?self.config.sim_url,
            satellite_name = ?self.config.satellite_name,
            poll_interval = self.config.poll_interval_s,
            "Agent started"
        );

        while self.running.load(Ordering::Relaxed) {
            match self.poll().await {
                Ok(Some(workload)) => {
                    info!(
                        deployment_id = %workload.deployment_id,
                        "Received workload"
                    );
                    if let Err(e) = self.execute(&workload).await {
                        warn!(error = %e, "Workload execution failed");
                    }
                }
                Ok(None) => {
                    // No work available, wait and poll again
                }
                Err(e) => {
                    warn!(error = %e, "Poll failed, will retry");
                }
            }

            if self.running.load(Ordering::Relaxed) {
                tokio::time::sleep(std::time::Duration::from_secs(self.config.poll_interval_s))
                    .await;
            }
        }

        Ok(())
    }

    async fn stop(&self) -> Result<(), AgentError> {
        self.running.store(false, Ordering::Relaxed);
        info!("Agent stopped");
        Ok(())
    }
}

/// Create a SimulatedSatellite wrapped in Arc for use with the telemetry loop.
///
/// This is the preferred way to create an agent when sim service integration
/// is enabled, as the telemetry loop requires shared ownership.
pub async fn start_agent_with_telemetry(
    config: AgentConfig,
    speed_multiplier: f64,
    orbital_elements: Option<OrbitalElements>,
) -> Result<(), AgentError> {
    let mut sat = SimulatedSatellite::new(config, speed_multiplier)?;
    if let Some(elements) = orbital_elements {
        sat = sat.with_orbital_elements(elements);
    }

    let agent = Arc::new(sat);

    // Start telemetry loop if sim service is configured
    if agent.sim_client.is_some() {
        agent.spawn_telemetry_loop();
    }

    agent.start().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn test_config() -> AgentConfig {
        AgentConfig {
            agent_id: "test-sat".into(),
            api_url: "http://127.0.0.1:1".into(), // unreachable, for testing
            api_key: "rs_test".into(),
            poll_interval_s: 1,
            satellite_id: None,
            satellite_name: None,
            sim_url: None,
        }
    }

    #[test]
    fn test_new_creates_satellite() {
        let sat = SimulatedSatellite::new(test_config(), 100.0);
        assert!(sat.is_ok());
        let sat = sat.unwrap();
        assert_eq!(sat.speed_multiplier, 100.0);
        assert!(!sat.running.load(Ordering::Relaxed));
        assert!(sat.sim_client.is_none());
    }

    #[test]
    fn test_new_with_sim_url() {
        let mut config = test_config();
        config.sim_url = Some("https://sim.rotastellar.com".into());
        let sat = SimulatedSatellite::new(config, 100.0).unwrap();
        assert!(sat.sim_client.is_some());
    }

    #[test]
    fn test_with_orbital_elements() {
        let sat = SimulatedSatellite::new(test_config(), 100.0)
            .unwrap()
            .with_orbital_elements(OrbitalElements {
                altitude_km: 550.0,
                inclination_deg: 51.6,
                eccentricity: 0.0001,
                raan_deg: 0.0,
                arg_perigee_deg: 90.0,
                mean_anomaly_deg: 0.0,
                mean_motion: 15.09,
                epoch: "2026-03-08T00:00:00Z".into(),
            });
        assert!(sat.orbital_elements.is_some());
    }

    #[tokio::test]
    async fn test_execute_empty_events() {
        let sat = SimulatedSatellite::new(test_config(), 100.0).unwrap();
        sat.running.store(true, Ordering::Relaxed);

        let workload = WorkloadSpec {
            plan_id: "p1".into(),
            deployment_id: "d1".into(),
            satellite_id: "25544".into(),
            plan_data: json!({}),
            events: vec![],
        };
        let result = sat.execute(&workload).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("No events"));
    }

    #[tokio::test]
    async fn test_execute_processes_events() {
        // Events with failing HTTP client (connection refused) — errors are warned, not fatal
        let sat = SimulatedSatellite::new(test_config(), 10000.0).unwrap();
        sat.running.store(true, Ordering::Relaxed);

        let workload = WorkloadSpec {
            plan_id: "p1".into(),
            deployment_id: "d1".into(),
            satellite_id: "25544".into(),
            plan_data: json!({}),
            events: vec![
                AgentEvent {
                    event_type: "job.accepted".into(),
                    timestamp: "2026-03-07T12:00:00Z".into(),
                    job_id: "j1".into(),
                    step_id: None,
                    payload: json!({}),
                },
                AgentEvent {
                    event_type: "step.started".into(),
                    timestamp: "2026-03-07T12:00:10Z".into(),
                    job_id: "j1".into(),
                    step_id: Some("s1".into()),
                    payload: json!({}),
                },
                AgentEvent {
                    event_type: "job.completed".into(),
                    timestamp: "2026-03-07T12:00:20Z".into(),
                    job_id: "j1".into(),
                    step_id: None,
                    payload: json!({"status": "success"}),
                },
            ],
        };
        let result = sat.execute(&workload).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_execute_low_battery_rejected() {
        let sat = SimulatedSatellite::new(test_config(), 100.0).unwrap();
        sat.running.store(true, Ordering::Relaxed);

        // Drain battery below threshold
        {
            let mut state = sat.sat_state.lock().await;
            state.battery_wh = 10.0;
        }

        let workload = WorkloadSpec {
            plan_id: "p1".into(),
            deployment_id: "d1".into(),
            satellite_id: "25544".into(),
            plan_data: json!({}),
            events: vec![AgentEvent {
                event_type: "job.accepted".into(),
                timestamp: "2026-03-07T12:00:00Z".into(),
                job_id: "j1".into(),
                step_id: None,
                payload: json!({}),
            }],
        };
        let result = sat.execute(&workload).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("battery"));
    }
}
