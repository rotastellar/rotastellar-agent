use reqwest::Client;

use crate::agent::AgentError;
use crate::types::{AgentConfig, AgentEvent, AgentTelemetry, WorkloadSpec};

/// HTTP client for communicating with the RotaStellar Console API.
pub struct ConsoleClient {
    http: Client,
    config: AgentConfig,
}

impl ConsoleClient {
    pub fn new(config: AgentConfig) -> Result<Self, AgentError> {
        let http = Client::builder()
            .user_agent(format!("rotastellar-agent/{}", env!("CARGO_PKG_VERSION")))
            .build()?;

        Ok(Self { http, config })
    }

    /// Register the agent with the Console API.
    pub async fn register(
        &self,
        satellite_id: Option<&str>,
        satellite_name: Option<&str>,
        orbit_altitude_km: Option<f64>,
        agent_version: Option<&str>,
    ) -> Result<(), AgentError> {
        let url = format!("{}/api/agent/register", self.config.api_url);

        let mut body = serde_json::json!({
            "agent_id": self.config.agent_id,
        });

        if let Some(sid) = satellite_id {
            body["satellite_id"] = serde_json::json!(sid);
        }
        if let Some(name) = satellite_name {
            body["satellite_name"] = serde_json::json!(name);
        }
        if let Some(alt) = orbit_altitude_km {
            body["orbit_altitude_km"] = serde_json::json!(alt);
        }
        if let Some(ver) = agent_version {
            body["agent_version"] = serde_json::json!(ver);
        }

        let resp = self
            .http
            .post(&url)
            .header("X-API-Key", &self.config.api_key)
            .header("X-Agent-ID", &self.config.agent_id)
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(AgentError::ApiError(format!("{status}: {body}")));
        }

        Ok(())
    }

    /// Poll for pending workloads assigned to this agent.
    pub async fn poll_workloads(&self) -> Result<Option<WorkloadSpec>, AgentError> {
        let url = format!("{}/api/agent/workloads", self.config.api_url);
        let resp = self
            .http
            .get(&url)
            .header("X-API-Key", &self.config.api_key)
            .header("X-Agent-ID", &self.config.agent_id)
            .send()
            .await?;

        if resp.status() == reqwest::StatusCode::NO_CONTENT {
            return Ok(None);
        }

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(AgentError::ApiError(format!("{status}: {body}")));
        }

        let workload: WorkloadSpec = resp.json().await?;
        Ok(Some(workload))
    }

    /// Report an execution event.
    pub async fn report_event(
        &self,
        deployment_id: &str,
        event: &AgentEvent,
    ) -> Result<(), AgentError> {
        let url = format!(
            "{}/api/deployments/{}/events",
            self.config.api_url, deployment_id
        );
        let resp = self
            .http
            .post(&url)
            .header("X-API-Key", &self.config.api_key)
            .header("X-Agent-ID", &self.config.agent_id)
            .json(event)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(AgentError::ApiError(format!("{status}: {body}")));
        }

        Ok(())
    }

    /// Report telemetry / heartbeat.
    pub async fn report_telemetry(&self, telemetry: &AgentTelemetry) -> Result<(), AgentError> {
        let url = format!("{}/api/agent/telemetry", self.config.api_url);
        let resp = self
            .http
            .post(&url)
            .header("X-API-Key", &self.config.api_key)
            .header("X-Agent-ID", &self.config.agent_id)
            .json(telemetry)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(AgentError::ApiError(format!("{status}: {body}")));
        }

        Ok(())
    }
}
