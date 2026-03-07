use async_trait::async_trait;

use crate::types::{AgentEvent, AgentTelemetry, WorkloadSpec};

/// The core Agent trait defining the satellite-side execution protocol.
///
/// Agents run on satellites (or in simulation) and communicate with
/// the RotaStellar Console API using a pull-based protocol:
///
/// 1. Agent polls for pending workloads during contact windows
/// 2. Agent executes workload steps locally
/// 3. Agent reports events back as they occur
/// 4. Agent sends periodic telemetry heartbeats
#[async_trait]
pub trait Agent: Send + Sync {
    /// Poll the Console API for pending workloads assigned to this agent.
    /// Returns `None` if no work is available.
    async fn poll(&self) -> Result<Option<WorkloadSpec>, AgentError>;

    /// Report an execution event to the Console API.
    async fn report_event(&self, event: &AgentEvent) -> Result<(), AgentError>;

    /// Report telemetry data (heartbeat, resource usage).
    async fn report_telemetry(&self, telemetry: &AgentTelemetry) -> Result<(), AgentError>;

    /// Execute a workload. Implementations should call `report_event` for each
    /// step transition during execution.
    async fn execute(&self, workload: &WorkloadSpec) -> Result<(), AgentError>;

    /// Start the agent run loop: poll → execute → report, repeating at
    /// the configured poll interval.
    async fn start(&self) -> Result<(), AgentError>;

    /// Signal the agent to stop gracefully.
    async fn stop(&self) -> Result<(), AgentError>;
}

/// Errors that can occur during agent operations.
#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("API request failed: {0}")]
    ApiError(String),

    #[error("Network error: {0}")]
    NetworkError(#[from] reqwest::Error),

    #[error("Serialization error: {0}")]
    SerializationError(#[from] serde_json::Error),

    #[error("Execution error: {0}")]
    ExecutionError(String),

    #[error("Agent stopped")]
    Stopped,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_error_api_display() {
        let err = AgentError::ApiError("timeout".into());
        assert_eq!(err.to_string(), "API request failed: timeout");
    }

    #[test]
    fn test_agent_error_execution_display() {
        let err = AgentError::ExecutionError("step failed".into());
        assert_eq!(err.to_string(), "Execution error: step failed");
    }

    #[test]
    fn test_agent_error_stopped_display() {
        let err = AgentError::Stopped;
        assert_eq!(err.to_string(), "Agent stopped");
    }
}
