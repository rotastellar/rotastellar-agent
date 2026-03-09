# RotaStellar Agent Protocol Specification

**Version:** 0.1.0
**Status:** Draft
**Last updated:** 2026-03-07

## Overview

The RotaStellar Agent Protocol defines how satellite-side agents communicate with the RotaStellar Console API. It is a **pull-based** protocol designed for intermittent satellite connectivity — agents operate autonomously and sync during contact windows.

## Authentication

All agent requests authenticate via API key in the `X-API-Key` header. The agent also identifies itself via the `X-Agent-ID` header.

```
X-API-Key: rs_live_...
X-Agent-ID: sat-25544
```

API keys are created in Mission Control under Developer > API Keys. The key is hashed (SHA-256) server-side and compared against stored hashes. Keys can be revoked at any time.

## Agent Lifecycle

```
┌─────────┐     ┌─────────┐     ┌──────────┐     ┌──────────┐
│ Register │────▶│  Poll   │────▶│ Execute  │────▶│ Complete │
└─────────┘     └────┬────┘     └────┬─────┘     └──────────┘
                     │               │
                     │ No work       │ Report events
                     │               │ Report telemetry
                     ▼               ▼
                  (sleep)         (continue)
```

### 1. Register

The agent registers with the Console on first startup. This creates an `mc_agents` record.

```
POST /api/agent/register
```

**Request:**
```json
{
  "agent_id": "sat-25544",
  "satellite_id": "25544",
  "satellite_name": "ISS (ZARYA)",
  "agent_version": "0.1.0"
}
```

**Response (201):**
```json
{
  "id": "sat-25544",
  "status": "idle"
}
```

If the agent_id already exists for this user, the record is updated (upsert).

### 2. Poll for Workloads

The agent polls periodically for pending deployments assigned to it.

```
GET /api/agent/workloads
```

**Response (200) — Work available:**
```json
{
  "plan_id": "abc-123",
  "deployment_id": "dep-456",
  "satellite_id": "25544",
  "plan_data": { ... },
  "events": [
    {
      "type": "job.accepted",
      "timestamp": "2026-03-07T12:00:00Z",
      "job_id": "preset-001",
      "payload": { "preset": "split-learning", "steps": 8 }
    },
    ...
  ]
}
```

**Response (204) — No work available:**

Empty response. Agent should sleep for `poll_interval_s` and retry.

The server returns the oldest pending deployment where:
- `mode = 'live'` (not simulated)
- `satellite_id` matches the agent's registered satellite
- `status = 'pending'`

On dispatch, the server updates the deployment status to `'dispatched'`.

### 3. Report Events

During execution, the agent reports events as they occur.

```
POST /api/deployments/{deployment_id}/events
```

**Request:**
```json
{
  "type": "step.completed",
  "timestamp": "2026-03-07T14:23:45Z",
  "job_id": "preset-001",
  "step_id": "feature_extraction",
  "payload": {
    "duration_s": 180,
    "location": "onboard",
    "data_output_mb": 10.5
  }
}
```

**Response (201):**
```json
{
  "id": "evt-789"
}
```

The server stores the event in `mc_deployment_events` and updates the deployment status based on event type:
- `job.completed` → deployment status = `'completed'`
- `job.failed` → deployment status = `'failed'`

### 4. Report Telemetry

Agents send periodic heartbeats with health data.

```
POST /api/agent/telemetry
```

**Request:**
```json
{
  "agent_id": "sat-25544",
  "status": "executing",
  "timestamp": "2026-03-07T14:23:45Z",
  "cpu_percent": 67.5,
  "memory_mb": 128.0,
  "battery_percent": 82.0,
  "temperature_c": 34.2
}
```

**Response (200):**
```json
{
  "ok": true
}
```

The server updates `mc_agents.status` and `mc_agents.last_heartbeat_at`.

## Event Types

The protocol uses the same event format as the CAE simulator. All events have this structure:

```json
{
  "type": "<event_type>",
  "timestamp": "<ISO 8601>",
  "job_id": "<string>",
  "step_id": "<string | null>",
  "payload": { ... }
}
```

### Lifecycle Events

| Type | Description | Payload |
|------|-------------|---------|
| `job.accepted` | Workload received and queued | `preset`, `category`, `steps`, `security` |
| `plan.created` | Execution plan finalized | `segments`, `windows_used`, `total_duration_s` |
| `job.completed` | All steps finished successfully | `total_duration_s`, `status`, `delivery_confidence` |
| `job.failed` | Execution failed | `total_duration_s`, `status` |

### Placement Events

| Type | Description | Payload |
|------|-------------|---------|
| `placement.decided` | Step placement decision | `location` (onboard/ground), `reason` |

### Compute Events

| Type | Description | Payload |
|------|-------------|---------|
| `step.started` | Compute step begins | `location`, `window`, `window_label` |
| `step.progress` | Progress update | `percent` (25, 50, 75) |
| `step.completed` | Compute step finished | `duration_s`, `location`, `data_output_mb` |

### Transfer Events

| Type | Description | Payload |
|------|-------------|---------|
| `transfer.started` | Data transfer initiated | `type`, `raw_data_mb`, `total_transfer_mb`, `fec_scheme` |
| `transfer.pass_started` | Ground station pass begins | `ground_station`, `station_name`, `elevation_peak_deg` |
| `transfer.progress` | Transfer progress | `data_transferred_mb`, `total_mb` |
| `transfer.pass_completed` | Pass finished | `data_transferred_mb`, `ground_station` |
| `transfer.completed` | All transfers done | `total_transferred_mb`, `duration_s` |
| `transfer.retransmission` | Blocks retransmitted (BER) | `blocks_retransmitted`, `ber` |

### Security Events

| Type | Description | Payload |
|------|-------------|---------|
| `security.encrypted` | Data encrypted | `algorithm`, `data_mb` |
| `security.key_exchange` | Key exchange performed | `duration_s`, `encryption` |

### Checkpoint Events

| Type | Description | Payload |
|------|-------------|---------|
| `checkpoint.saved` | State persisted | `checkpoint_number`, `progress_fraction` |
| `checkpoint.predicted` | Hazard prediction generated (I-4) | `hazards_count`, `checkpoints_count`, `next_hazard`, `max_safe_window_s`, `overhead_fraction`, `hazards`, `checkpoint_schedule` |

### Orbital Compute Primitive Events (I-1)

<!-- subhadipmitra, 2026-03-09: OCP events are emitted by the CAE simulator
     and enriched by the agent with actual satellite state at execution time. -->

**Eclipse-Step Events** — Steps that run during battery-only eclipse phases.

| Type | Description | Payload |
|------|-------------|---------|
| `eclipse_step.started` | Eclipse step begins | `energy_budget_j`, `eclipse_policy`, `actual_battery_wh`, `max_available_energy_j` |
| `eclipse_step.energy_check` | Energy at 50% budget | `energy_remaining_j`, `actual_battery_wh` |
| `eclipse_step.completed` | Eclipse step finished | `energy_consumed_j`, `actual_battery_wh`, `actual_temperature_c` |

**Window-Step Events** — Adaptive quality steps that select tiers based on available window time.

| Type | Description | Payload |
|------|-------------|---------|
| `window_step.started` | Window step begins with tier | `planned_tier`, `quality_tiers`, `actual_battery_percent`, `compute_capacity` |
| `window_step.degraded` | Tier downgraded at runtime | `from_tier`, `to_tier`, `reason` |
| `window_step.upgraded` | Tier upgraded (more time available) | `from_tier`, `to_tier` |
| `window_step.completed` | Window step finished | `achieved_tier`, `output_quality`, `degradations` |

**Pass-Step Events** — Atomic steps decomposed around ground contact windows.

| Type | Description | Payload |
|------|-------------|---------|
| `pass_step.started` | Pass step begins | `sequence_index`, `sequence_total`, `actual_battery_percent` |
| `pass_step.completed` | Pass step finished | `sequence_index`, `sequence_total`, `merge_strategy` |

The agent enriches all OCP events with `actual_battery_percent` and (for eclipse steps) `actual_temperature_c`. If OCP preconditions don't match actual satellite state, an `ocp_warning` field is added to the event payload.

### ISL Transfer Events (I-3)

<!-- subhadipmitra, 2026-03-09: I-3 Constellation DAG events for ISL data
     routing and multi-satellite workflow coordination. -->

| Type | Description | Payload |
|------|-------------|---------|
| `isl_transfer.started` | ISL data transfer initiated | `src_satellite`, `dst_satellite`, `data_mb`, `hops`, `route` |
| `isl_transfer.hop_completed` | One ISL hop completed | `from`, `to`, `data_mb`, `hop_index`, `hop_total` |
| `isl_transfer.completed` | Full ISL transfer done | `total_time_s`, `total_data_mb`, `hops`, `reliability` |
| `constellation.step_assigned` | Step assigned to this satellite | `step_id`, `step_name`, `satellite_id`, `window_id` |
| `constellation.failover` | Step failed, reassigning | `step_id`, `from_satellite`, `to_satellite`, `reason` |

## Error Handling

All error responses follow this format:

```json
{
  "error": "Human-readable error message"
}
```

| Status | Meaning |
|--------|---------|
| 400 | Invalid request body |
| 401 | Missing or invalid API key |
| 403 | API key valid but insufficient permissions |
| 404 | Resource not found |
| 409 | Conflict (e.g., deployment already dispatched) |
| 429 | Rate limited |
| 500 | Server error |

## Rate Limits

- **Poll**: Max 1 request per 10 seconds per agent
- **Events**: Max 100 events per minute per deployment
- **Telemetry**: Max 1 request per 30 seconds per agent

## Agent Modes

Deployments have a `mode` field:

| Mode | Description |
|------|-------------|
| `simulated` | Server generates events from CAE plan data. No agent involved. |
| `live` | Agent polls, executes, and reports events. Real or hardware-in-loop execution. |

## Versioning

The protocol version is included in the `User-Agent` header:

```
User-Agent: rotastellar-agent/0.1.0
```

Breaking changes will increment the minor version until 1.0. After 1.0, semantic versioning applies.

## Constellation Execution (WS4)

When executing a constellation DAG workload, the agent tracks multi-satellite coordination:

### Constellation Events

| Event Type | Direction | Description |
|-----------|-----------|-------------|
| `constellation.step_assigned` | CAE → Agent | Step from DAG assigned to this satellite |
| `constellation.step_started` | Agent → Console | Agent began executing a DAG step |
| `constellation.step_completed` | Agent → Console | DAG step finished successfully |
| `constellation.failover` | Agent → Console | Step failed, needs reassignment |
| `constellation.failover_acknowledged` | Agent → Console | Satellite confirmed step handoff |
| `constellation.satellite_complete` | Agent → Console | All assigned steps done on this satellite |

### ISL Transfer Events

| Event Type | Direction | Description |
|-----------|-----------|-------------|
| `isl_transfer.started` | CAE → Agent | ISL data transfer initiated |
| `isl_transfer.hop_completed` | Agent → Console | One ISL hop completed with quality metrics |
| `isl_transfer.completed` | Agent → Console | Full ISL transfer done |
| `isl_transfer.quality_report` | Agent → Console | Link quality summary (avg quality, latency, bandwidth) |

### Failover Conditions

The agent automatically triggers failover when:
- Battery below 10% (critical power loss)
- Temperature above 75°C (thermal protection)
- Compute unavailable (battery below 15%)

### ISL Link Quality Model

ISL hop quality is computed from actual satellite state:
- **Distance factor**: `1 - (distance_km / 5000) * 0.6`
- **Eclipse penalty**: `0.9` if either endpoint is in eclipse
- **Effective bandwidth**: `100 Mbps * quality`
- **Latency**: `distance_km / c + 2ms` (processing overhead)
