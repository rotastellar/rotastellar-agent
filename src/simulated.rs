use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use tokio::sync::Mutex;
use tracing::{info, warn};

use crate::agent::{Agent, AgentError};
use crate::client::ConsoleClient;
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

            // Report event to Console API
            self.client
                .report_event(&workload.deployment_id, event)
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
