# RotaStellar Operator Agent

[![Rust](https://img.shields.io/badge/rust-1.75%2B-orange?logo=rust)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-MPL--2.0-blue)](LICENSE)
[![GitHub](https://img.shields.io/github/stars/rotastellar/rotastellar-agent?style=social)](https://github.com/rotastellar/rotastellar-agent)

The execution layer for orbital compute. A Rust SDK for running workloads on satellites using a pull-based agent protocol.

## Architecture

```
Satellite                          RotaStellar Console
┌───────────────────┐               ┌────────────────────────────┐
│ rotastellar-agent │◄── poll ────► │ GET /api/agent/workloads   │
│                   │               │                            │
│  ┌────────────┐   │               │                            │
│  │ Execute    │   │──  events ──► │ POST /api/deployments      │
│  │ workload   │   │               │      /{id}/events          │
│  └────────────┘   │               │                            │
│                   │── telemetry─► │ POST /api/agent/           │
│                   │               │      telemetry             │
└───────────────────┘               └────────────────────────────┘
```

## Protocol

1. **Poll** — Agent checks for pending workloads during contact windows
2. **Execute** — Agent runs workload steps locally on the satellite
3. **Report** — Agent streams execution events back to Console
4. **Telemetry** — Agent sends periodic health/status heartbeats

## Event Types

| Event | Description |
|-------|------------|
| `job.accepted` | Workload received and queued |
| `placement.decided` | Step placement decision (on-board vs ground) |
| `plan.created` | Execution plan finalized |
| `step.started` | Compute step begins |
| `step.progress` | Progress update (25%, 50%, 75%) |
| `step.completed` | Compute step finished |
| `transfer.started` | Data transfer initiated |
| `transfer.completed` | Data transfer finished |
| `job.completed` | All steps finished successfully |
| `job.failed` | Execution failed |

## Usage

### As a library

```rust
use rotastellar_agent::{AgentConfig, SimulatedSatellite, Agent};

#[tokio::main]
async fn main() {
    let config = AgentConfig {
        agent_id: "sat-25544".into(),
        api_url: "https://console.rotastellar.com".into(),
        api_key: "rs_...".into(),
        poll_interval_s: 30,
    };

    let agent = SimulatedSatellite::new(config, 100.0).unwrap();
    agent.start().await.unwrap();
}
```

### CLI — Simulate execution

```bash
# Simulate a CAE plan at 100x speed
rotastellar-agent simulate \
  --plan plan.json \
  --speed 100 \
  --api-url https://console.rotastellar.com \
  --api-key rs_...

# Run in poll mode (long-running daemon)
rotastellar-agent run \
  --agent-id sat-25544 \
  --api-url https://console.rotastellar.com \
  --api-key rs_...
```

### Implement a custom agent

```rust
use async_trait::async_trait;
use rotastellar_agent::{Agent, AgentError, AgentEvent, AgentTelemetry, WorkloadSpec};

struct MyAgent { /* ... */ }

#[async_trait]
impl Agent for MyAgent {
    async fn poll(&self) -> Result<Option<WorkloadSpec>, AgentError> {
        // Check for pending workloads
        todo!()
    }

    async fn report_event(&self, event: &AgentEvent) -> Result<(), AgentError> {
        // Report execution events
        todo!()
    }

    async fn report_telemetry(&self, telemetry: &AgentTelemetry) -> Result<(), AgentError> {
        // Report health data
        todo!()
    }

    async fn execute(&self, workload: &WorkloadSpec) -> Result<(), AgentError> {
        // Run the actual computation on satellite hardware
        for event in &workload.events {
            // Execute each step...
            self.report_event(event).await?;
        }
        Ok(())
    }

    async fn start(&self) -> Result<(), AgentError> { todo!() }
    async fn stop(&self) -> Result<(), AgentError> { todo!() }
}
```

## Building

```bash
cargo build --release
```

## License

MPL-2.0
