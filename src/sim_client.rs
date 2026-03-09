//! HTTP client for the RotaStellar Simulation Service.
//!
//! Calls `sim.rotastellar.com` for stateless orbital state computation.
//! The agent uses this to get its current position and eclipse status,
//! then maintains power/thermal state locally.

use chrono::{DateTime, Utc};
use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::agent::AgentError;

/// Orbital elements passed to the simulation service.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrbitalElements {
    pub altitude_km: f64,
    pub inclination_deg: f64,
    #[serde(default = "default_eccentricity")]
    pub eccentricity: f64,
    #[serde(default)]
    pub raan_deg: f64,
    #[serde(default = "default_arg_perigee")]
    pub arg_perigee_deg: f64,
    #[serde(default)]
    pub mean_anomaly_deg: f64,
    #[serde(default)]
    pub mean_motion: f64,
    pub epoch: String,
}

fn default_eccentricity() -> f64 {
    0.0001
}
fn default_arg_perigee() -> f64 {
    90.0
}

/// Response from the simulation service's /v1/state endpoint.
#[derive(Debug, Clone, Deserialize)]
pub struct OrbitalState {
    pub lat: f64,
    pub lon: f64,
    pub altitude_km: f64,
    pub velocity_km_s: f64,
    pub in_eclipse: bool,
    pub orbit_fraction: f64,
}

/// Client for the RotaStellar Simulation Service.
pub struct SimClient {
    http: Client,
    sim_url: String,
}

impl SimClient {
    pub fn new(sim_url: String) -> Result<Self, AgentError> {
        let http = Client::builder()
            .user_agent(format!(
                "rotastellar-agent/{}",
                env!("CARGO_PKG_VERSION")
            ))
            .build()?;
        Ok(Self { http, sim_url })
    }

    /// Get the orbital state at the given timestamp.
    pub async fn get_state(
        &self,
        elements: &OrbitalElements,
        timestamp: DateTime<Utc>,
    ) -> Result<OrbitalState, AgentError> {
        let url = format!("{}/v1/state", self.sim_url);

        #[derive(Serialize)]
        struct StateRequest<'a> {
            elements: &'a OrbitalElements,
            timestamp: String,
        }

        let resp = self
            .http
            .post(&url)
            .json(&StateRequest {
                elements,
                timestamp: timestamp.to_rfc3339(),
            })
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(AgentError::ApiError(format!(
                "Sim service {status}: {body}"
            )));
        }

        let state: OrbitalState = resp.json().await?;
        Ok(state)
    }
}

/// Local satellite state model — power and thermal.
/// The sim service provides position/eclipse; this struct tracks
/// stateful quantities that evolve between sim calls.
pub struct SatelliteState {
    pub battery_wh: f64,
    pub temperature_c: f64,
    pub last_update: DateTime<Utc>,
}

impl Default for SatelliteState {
    fn default() -> Self {
        Self::new()
    }
}

impl SatelliteState {
    pub fn new() -> Self {
        Self {
            battery_wh: 80.0, // start at 80%
            temperature_c: 25.0,
            last_update: Utc::now(),
        }
    }

    /// Update power and thermal state based on elapsed time and eclipse status.
    pub fn update(&mut self, now: DateTime<Utc>, in_eclipse: bool, is_computing: bool) {
        let dt_s = (now - self.last_update).num_milliseconds() as f64 / 1000.0;
        if dt_s <= 0.0 {
            return;
        }
        let dt_min = dt_s / 60.0;

        // Power model (Wh, dt in hours)
        let dt_h = dt_s / 3600.0;
        let charge_rate_w = if in_eclipse { -250.0 } else { 750.0 };
        let compute_draw_w = if is_computing { -200.0 } else { 0.0 };
        let net_power_w = charge_rate_w + compute_draw_w;
        self.battery_wh = (self.battery_wh + net_power_w * dt_h).clamp(0.0, 100.0);

        // Thermal model
        let target_temp = if in_eclipse { -20.0 } else { 40.0 };
        let compute_heat = if is_computing { 5.0 } else { 0.0 };
        let rate = 0.5; // °C per minute
        let delta = (target_temp + compute_heat - self.temperature_c) * (rate * dt_min / 60.0).min(1.0);
        self.temperature_c += delta;

        self.last_update = now;
    }

    /// Battery percentage (0-100).
    pub fn battery_percent(&self) -> f64 {
        self.battery_wh
    }

    /// Whether compute is available (battery > 15%).
    pub fn compute_available(&self) -> bool {
        self.battery_wh > 15.0
    }

    /// Compute capacity fraction (0.0-1.0).
    pub fn compute_capacity(&self) -> f64 {
        if self.battery_wh < 15.0 {
            0.0
        } else if self.battery_wh < 30.0 {
            0.5
        } else {
            1.0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    #[test]
    fn test_satellite_state_sunlit_charging() {
        let mut state = SatelliteState::new();
        state.battery_wh = 50.0;
        let now = state.last_update;
        let later = now + Duration::minutes(30);
        state.update(later, false, false);
        assert!(state.battery_wh > 50.0, "Battery should charge in sunlight");
    }

    #[test]
    fn test_satellite_state_eclipse_drain() {
        let mut state = SatelliteState::new();
        state.battery_wh = 50.0;
        let now = state.last_update;
        let later = now + Duration::minutes(30);
        state.update(later, true, false);
        assert!(state.battery_wh < 50.0, "Battery should drain in eclipse");
    }

    #[test]
    fn test_compute_available() {
        let mut state = SatelliteState::new();
        state.battery_wh = 20.0;
        assert!(state.compute_available());
        state.battery_wh = 10.0;
        assert!(!state.compute_available());
    }

    #[test]
    fn test_compute_capacity() {
        let mut state = SatelliteState::new();
        state.battery_wh = 50.0;
        assert_eq!(state.compute_capacity(), 1.0);
        state.battery_wh = 25.0;
        assert_eq!(state.compute_capacity(), 0.5);
        state.battery_wh = 10.0;
        assert_eq!(state.compute_capacity(), 0.0);
    }
}
