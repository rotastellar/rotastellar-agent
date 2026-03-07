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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_agent_event_roundtrip() {
        let event = AgentEvent {
            event_type: "step.completed".into(),
            timestamp: "2026-03-07T14:23:45Z".into(),
            job_id: "preset-001".into(),
            step_id: Some("feature_extraction".into()),
            payload: json!({"duration_s": 180}),
        };
        let json_str = serde_json::to_string(&event).unwrap();
        assert!(json_str.contains("\"type\":\"step.completed\""));
        assert!(!json_str.contains("\"event_type\""));
        let decoded: AgentEvent = serde_json::from_str(&json_str).unwrap();
        assert_eq!(decoded.event_type, "step.completed");
        assert_eq!(decoded.step_id, Some("feature_extraction".into()));
    }

    #[test]
    fn test_agent_event_optional_step_id() {
        let event = AgentEvent {
            event_type: "job.accepted".into(),
            timestamp: "2026-03-07T12:00:00Z".into(),
            job_id: "preset-001".into(),
            step_id: None,
            payload: json!({}),
        };
        let json_str = serde_json::to_string(&event).unwrap();
        assert!(!json_str.contains("step_id"));
    }

    #[test]
    fn test_workload_spec_roundtrip() {
        let spec = WorkloadSpec {
            plan_id: "plan-abc".into(),
            deployment_id: "dep-456".into(),
            satellite_id: "25544".into(),
            plan_data: json!({"segments": []}),
            events: vec![AgentEvent {
                event_type: "job.accepted".into(),
                timestamp: "2026-03-07T12:00:00Z".into(),
                job_id: "j1".into(),
                step_id: None,
                payload: json!({}),
            }],
        };
        let json_str = serde_json::to_string(&spec).unwrap();
        let decoded: WorkloadSpec = serde_json::from_str(&json_str).unwrap();
        assert_eq!(decoded.plan_id, "plan-abc");
        assert_eq!(decoded.events.len(), 1);
    }

    #[test]
    fn test_agent_config_default_poll() {
        let json_str = r#"{"agent_id":"sat-1","api_url":"http://localhost","api_key":"rs_test"}"#;
        let config: AgentConfig = serde_json::from_str(json_str).unwrap();
        assert_eq!(config.poll_interval_s, 30);
    }

    #[test]
    fn test_agent_config_custom_poll() {
        let json_str =
            r#"{"agent_id":"sat-1","api_url":"http://localhost","api_key":"rs_test","poll_interval_s":60}"#;
        let config: AgentConfig = serde_json::from_str(json_str).unwrap();
        assert_eq!(config.poll_interval_s, 60);
    }

    #[test]
    fn test_agent_telemetry_roundtrip() {
        let full = AgentTelemetry {
            agent_id: "sat-1".into(),
            status: AgentStatus::Executing,
            timestamp: "2026-03-07T14:00:00Z".into(),
            cpu_percent: Some(67.5),
            memory_mb: Some(128.0),
            battery_percent: Some(82.0),
            temperature_c: Some(34.2),
        };
        let json_str = serde_json::to_string(&full).unwrap();
        assert!(json_str.contains("\"cpu_percent\":67.5"));
        let decoded: AgentTelemetry = serde_json::from_str(&json_str).unwrap();
        assert_eq!(decoded.cpu_percent, Some(67.5));

        // With optional fields omitted
        let minimal = AgentTelemetry {
            agent_id: "sat-1".into(),
            status: AgentStatus::Idle,
            timestamp: "2026-03-07T14:00:00Z".into(),
            cpu_percent: None,
            memory_mb: None,
            battery_percent: None,
            temperature_c: None,
        };
        let json_str = serde_json::to_string(&minimal).unwrap();
        assert!(!json_str.contains("cpu_percent"));
    }

    #[test]
    fn test_agent_status_serde() {
        assert_eq!(
            serde_json::to_string(&AgentStatus::Idle).unwrap(),
            "\"idle\""
        );
        assert_eq!(
            serde_json::to_string(&AgentStatus::Executing).unwrap(),
            "\"executing\""
        );
        assert_eq!(
            serde_json::to_string(&AgentStatus::Transferring).unwrap(),
            "\"transferring\""
        );
        assert_eq!(
            serde_json::to_string(&AgentStatus::Offline).unwrap(),
            "\"offline\""
        );

        let decoded: AgentStatus = serde_json::from_str("\"executing\"").unwrap();
        assert_eq!(decoded, AgentStatus::Executing);
    }
}
