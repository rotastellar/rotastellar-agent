use serde::{Deserialize, Serialize};

/// A single event in the execution timeline.
///
/// Matches the canonical event format produced by the CAE simulator.
/// Event types: job.accepted, placement.decided, plan.created,
/// step.started, step.progress, step.completed, transfer.started,
/// transfer.pass_started, transfer.progress, transfer.pass_completed,
/// transfer.completed, checkpoint.saved, security.encrypted,
/// security.key_exchange, job.completed, job.failed
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentEvent {
    #[serde(rename = "type")]
    pub event_type: String,
    pub timestamp: String,
    pub job_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step_id: Option<String>,
    pub payload: serde_json::Value,
}

/// A workload specification dispatched to an agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkloadSpec {
    pub plan_id: String,
    pub deployment_id: String,
    pub satellite_id: String,
    pub plan_data: serde_json::Value,
    pub events: Vec<AgentEvent>,
}

/// Configuration for an agent instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    pub agent_id: String,
    pub api_url: String,
    pub api_key: String,
    #[serde(default = "default_poll_interval")]
    pub poll_interval_s: u64,
}

fn default_poll_interval() -> u64 {
    30
}

/// Telemetry data reported by an agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentTelemetry {
    pub agent_id: String,
    pub status: AgentStatus,
    pub timestamp: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_percent: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_mb: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub battery_percent: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature_c: Option<f64>,
}

/// Agent operational status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentStatus {
    Idle,
    Executing,
    Transferring,
    Offline,
}

impl std::fmt::Display for AgentStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentStatus::Idle => write!(f, "idle"),
            AgentStatus::Executing => write!(f, "executing"),
            AgentStatus::Transferring => write!(f, "transferring"),
            AgentStatus::Offline => write!(f, "offline"),
        }
    }
}
