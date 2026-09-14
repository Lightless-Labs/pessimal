---
status: pending
priority: p3
issue_id: "006"
tags: [clients, agent, architecture, liveness]
dependencies: []
---

# Poll agents directly over Tailscale

## Problem Statement

The clients poll the telemetry backend (SigNoz), not the agents. That was a deliberate choice: the
agents run on private hosts that a phone cannot reach, and the backend already receives their data.

The repository description is "Personal infrastructure monitoring over Tailscale". On a tailnet, every
client can reach every agent by a stable name, and Tailscale authenticates the connection at the network
layer. There, the reason for polling the backend does not apply.

Polling agents directly gives three things the current design cannot:

- **Liveness without the backend.** The ~88 s ingestion lag is a property of the read path through
  SigNoz. It is the reason `backend_lag_allowance` exists. A direct poll has no ingestion lag, so
  liveness is accurate to the round trip.
- **No operator query key in the clients.** The SigNoz query key is in the iOS app and the macOS menu
  bar app only because they query SigNoz.
- **Monitoring that works when the backend is down.**

## Findings

- `pessimal_core::TelemetryQuery` (`common/pessimal_core/src/ports.rs:50`) is the port.
  `pessimal_query_signoz` is its only implementation: `clients/common/query/` holds nothing else. A
  direct-to-agent adapter is a second implementation of the same trait, not a rewrite.
- `plan_poll`, `execute_plan` (`clients/common/pessimal_client_core/src/gather.rs:49`, takes
  `&dyn TelemetryQuery`) and `FleetState::apply` (`clients/common/pessimal_client_core/src/fold.rs:252`)
  do not depend on the adapter.
- The FFI and both apps do depend on it. `FleetSession::new`
  (`clients/ffi/pessimal_ffi/src/session.rs:245-284`) takes `base_url` and `api_key` (and, since 3aa5ad1,
  `usage`) among its arguments, and builds the SigNoz adapter itself with `SignozConfig::for_policy`
  and `SignozQuery::with_client`. A direct adapter needs a new `FleetSession` constructor or config
  shape, and new settings in both apps.
- The agent has no HTTP server. No crate in `agents/`, `common/` or `clients/`, and nothing in the
  workspace `Cargo.toml`, uses axum, a hyper server, `TcpListener`, tiny_http, warp or actix.
- The agent already holds the current snapshot in `CachedCollector`
  (`agents/common/pessimal_agent_core/src/collector.rs:94`). Only the OTLP exporter uses it today.
  Serving it over HTTP exposes data the agent already has. It adds no collection.
- Direct polling removes the operator's query key (the `SIGNOZ-API-KEY` header,
  `clients/common/query/pessimal_query_signoz/src/client.rs:24`, stored through
  `clients/apple/PessimalKit/Sources/Platform/APIKeyStore.swift`). It does not remove every SigNoz
  credential from the iOS app. Release builds of the iOS app carry `SIGNOZ_OTLP_INGESTION_KEY` for
  usage reporting (`clients/apple/ios/BUILD.bazel:109-110`, read at
  `clients/apple/PessimalKit/Sources/Platform/UsageConsentStore.swift:83`, passed in by
  `scripts/release-ios-testflight-buildkite.sh:268-271`). That is Pessimal's own ingestion key, for a
  different endpoint, and it is optional. The macOS menu bar app does not carry it:
  `scripts/build-macos-app.sh` builds it from `clients/apple/macos/Resources/Info.plist`, which has no
  `SIGNOZ_*` keys, and neither that script nor `scripts/release-macos-app.sh` adds any. On macOS,
  direct polling removes every SigNoz credential.
- `PollTuning` has one `backend_lag_allowance` (`clients/common/pessimal_client_core/src/config.rs:148`).
  The preset is 3 minutes (`PRESET_BACKEND_LAG_ALLOWANCE_MINUTES`, `config.rs:89` and `:225`). Four
  things use it: liveness, the alert evidence gate `evidence_horizon()` (`config.rs:372`, which is
  `max_staleness + backend_lag_allowance`), the roster window `host_window()` (`config.rs:318`) and the
  detail query window `detail_window()` (`config.rs:337`).
- A backend poll sends one request per planned metric with `HostSelector::All`, never one per host per
  metric. It also sends one request per detail metric for the focused host
  (`clients/common/pessimal_client_core/src/plan.rs:9-14`). A direct poll sends one request per host
  per poll, and on a phone that is over the radio.
- `docs/HANDOFF.md` does not mention Tailscale or direct polling. Its next query backends are Honeycomb
  and ClickStack.

## Proposed Solutions

### Option A: A second `TelemetryQuery` adapter that polls agents

Add a small HTTP endpoint to the agent that serves the `CachedCollector` snapshot. Add a client adapter
next to `pessimal_query_signoz` that reads it. Decide these before starting:

1. **Source of truth when both paths are configured.** Probably direct polling for liveness and the
   backend for history. That is a per-metric answer, not a global one.
2. **Lag allowance per source.** Zero for a direct poll, the measured lag for the backend. The split
   must cover liveness, `evidence_horizon()`, `host_window()` and `detail_window()`.
3. **Fan-out.** One request per host per poll. `plan_poll` would need to account for it.
4. **Exposure.** Serving metrics over the tailnet opens a port. Tailscale ACLs are the control. Make
   exposure an explicit setting, not a default.

- **Pros:** liveness is not delayed by ingestion lag. Clients that poll directly need no SigNoz query
  key. Liveness still works when the backend is down. `plan_poll`, `execute_plan` and
  `FleetState::apply` stay as they are.
- **Cons:** the agent opens a port. One request per host per poll instead of one per metric.
  `PollTuning`, `FleetSession::new` and both apps' settings change. It only helps where clients and
  agents share a tailnet.

### Option B: Keep polling only the backend

- **Pros:** no change. No port on the agent. Request count does not grow with the fleet.
- **Cons:** liveness stays behind the ingestion lag. The query key stays in the apps. Liveness fails
  when the backend is down.

## Recommended Action

Not now. When it is picked up, write a plan in `docs/plans/` first that answers the four questions in
Option A, then build Option A.

## Technical Details

- `common/pessimal_core/src/ports.rs`: the `TelemetryQuery` port
- `clients/common/query/`: where the new adapter goes, next to `pessimal_query_signoz`
- `agents/common/pessimal_agent_core/src/collector.rs`: `CachedCollector`, the snapshot to serve
- `agents/host/`: the host agent binary that would serve it
- `clients/common/pessimal_client_core/src/config.rs`: `PollTuning` and `backend_lag_allowance`
- `clients/common/pessimal_client_core/src/plan.rs`: `plan_poll` and fan-out
- `clients/ffi/pessimal_ffi/src/session.rs`: `FleetSession::new`
- `clients/apple/PessimalKit/Sources/Platform/APIKeyStore.swift`: where the query key is stored
- `clients/apple/ios/`, `clients/apple/macos/`: settings for direct polling in both apps

## Acceptance Criteria

- [ ] A plan in `docs/plans/` answers source of truth, per-source lag allowance, fan-out and exposure.
- [ ] The agent serves its current snapshot over HTTP only when configured to.
- [ ] A second `TelemetryQuery` implementation reads that snapshot.
- [ ] Direct polling uses a lag allowance of zero for liveness, the evidence gate, the roster window and
  the detail window. The backend path keeps its measured allowance.
- [ ] `plan_poll`, `execute_plan` and `FleetState::apply` need no adapter-specific change.
- [ ] Both apps can poll agents directly without a SigNoz query key.
- [ ] Liveness still works when the backend is unreachable.

## Work Log

### 2026-09-10

- Created in ae5363f (`feat(release): wire macOS signing and notarization to Doppler`), deferred. The
  owner called it a good future improvement.
- 0521efb (`fix(clients): stop a lagging backend silently disabling every alert`) added
  `PollTuning::evidence_horizon()`, which also uses `backend_lag_allowance`. A per-source allowance
  must cover it too.

### 2026-09-11

- M8 usage reporting (plan 0d27166; de7facb, 1f6cb0e, 3aa5ad1, 5e02535) added a `usage` argument to
  `FleetSession::new` (3aa5ad1) and put `SIGNOZ_OTLP_INGESTION_KEY` into iOS release builds (5e02535).
  After this, direct polling removes only the query key from the iOS app, not every SigNoz credential.
  The macOS menu bar app does not carry the ingestion key, so there direct polling still removes every
  SigNoz credential.

### 2026-09-14

- Checked against the repo. No agent HTTP endpoint and no direct-to-agent adapter exist.
  `git log --all -S tailscale -i` and `--grep=tailscale` match only ae5363f. Still pending.
- Corrected two claims from the old file. The FFI and both apps need changes, because
  `FleetSession::new` builds the SigNoz adapter itself. A backend poll sends one request per planned
  metric plus the focused host's detail requests, not one request for the fleet. The code was already
  like this at ae5363f, when `plan.rs` was last changed in b9ccb69 (2026-09-09).
- Moved to this format from `todos/agent-direct-polling-over-tailscale.md`.

## Resources

- `docs/solutions/backend-ingestion-lag-breaks-liveness.md`: the 88 s ingestion lag
- `docs/plans/2026-09-07-m3-client-core.md`: the lag measured on SigNoz Cloud
- `docs/plans/2026-09-06-m3-signoz-query-adapter.md`: item 4 of the live-backend findings, the lag
