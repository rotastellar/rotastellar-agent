//! # RotaStellar Operator Agent SDK
//!
//! Execute orbital compute workloads on satellites using a pull-based agent protocol.
//!
//! ## Architecture
//!
//! The agent runs on a satellite (or in simulation) and communicates with the
//! RotaStellar Console API:
//!
//! 1. **Poll** — Agent checks for pending workloads during contact windows
//! 2. **Execute** — Agent runs workload steps locally on the satellite
//! 3. **Report** — Agent streams execution events back to Console
//! 4. **Telemetry** — Agent sends periodic health/status heartbeats
//!
//! ## Quick Start
//!
//! ```rust,no_run
//! use rotastellar_agent::{AgentConfig, SimulatedSatellite, Agent};
//!
//! #[tokio::main]
//! async fn main() {
//!     let config = AgentConfig {
//!         agent_id: "sat-001".into(),
//!         api_url: "https://runtime.rotastellar.com".into(),
//!         api_key: "rs_...".into(),
//!         poll_interval_s: 30,
//!         satellite_id: Some("99901".into()),
//!         satellite_name: Some("RS-LEO-1".into()),
//!         sim_url: Some("https://sim.rotastellar.com".into()),
//!     };
//!
//!     let agent = SimulatedSatellite::new(config, 100.0).unwrap();
//!     agent.start().await.unwrap();
//! }
//! ```

pub mod agent;
pub mod client;
// subhadipmitra, 2026-03-09: Orbital Compute Primitives runtime module.
pub mod ocp;
// subhadipmitra, 2026-03-09: I-4 HazardPredictor — deterministic eclipse-boundary
// state preservation via hazard-predictive checkpointing.
pub mod hazard;
pub mod sim_client;
pub mod simulated;
pub mod types;

// Re-export core types for convenience
pub use agent::{Agent, AgentError};
pub use client::ConsoleClient;
pub use sim_client::{OrbitalElements, OrbitalState, SatelliteState, SimClient};
pub use simulated::SimulatedSatellite;
pub use types::{
    AgentConfig, AgentEvent, AgentStatus, AgentTelemetry, Position, WorkloadSpec,
};
