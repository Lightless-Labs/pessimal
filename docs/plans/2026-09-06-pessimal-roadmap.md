# Pessimal Roadmap

**Created:** 2026-09-06
**Status:** In progress

## Goal

An open-source host telemetry stack: cross-platform Rust agents exporting OTLP to a collector the
operator already runs, and iOS + macOS menu bar clients that read those metrics back, show liveness,
and raise configurable alerts. Rust everywhere except the UI layer, which is native Swift over a
UniFFI bridge.

## Decisions

| Decision | Rationale |
|---|---|
| Clients query the **backend**, not the agents | Agents on private hosts are not reachable from a phone. The backend already is. Decided 2026-09-06 with the owner. |
| **Polling only** | Push needs a server component and a notification path. Deferred, not designed out — the query port has no subscribe method rather than a stubbed one. |
| Export is plain **OTLP**; backends differ only by auth header | Makes SigNoz / ClickStack / Honeycomb / anything-else a config choice, not a code path. |
| Read-back needs a **per-backend adapter** | There is no OTLP for queries. SigNoz first (the owner's shared collector), then Honeycomb and ClickStack. |
| Agents build with **Cargo**, not Bazel | Bazel here exists for the Apple toolchain. Linux and Windows agents cross-compile far more simply with Cargo in CI. |
| Bazel pinned to **8.2.1** | rules_apple is not yet Bazel 9 compatible. Matches the toolchain proven in the sibling projects. |

## Milestones

### M0 — Scaffold and CI ✅
Repo layout, Cargo workspace, licence, conventional commits, CI that builds and tests the Rust
tree on Linux/macOS/Windows.

### M1 — Domain core ✅
`pessimal_core`: metric identity mapped to OTel semconv names, time ranges and series, liveness
policy over heartbeat age, alert rules with dwell-period evaluation, URN identity, and the ports
adapters implement. No I/O, fully unit-tested.

### M2 — Host agent
`pessimal_agent_core` (collection trait, resource attributes, OTLP exporter with backend presets,
TOML config) and `pessimal_agent_host` (sysinfo-backed collector, CLI, run loop). Verified against a
real OTLP endpoint, not just unit tests.

### M3 — SigNoz query adapter and client core
`pessimal_query_signoz` implementing `TelemetryQuery` against SigNoz's query-range API, tested
against recorded responses. `pessimal_client_core` orchestrating polling, liveness, and alert
evaluation across a fleet.

### M4 — FFI bridge
`pessimal_ffi` exposing the client core to Swift. Bindgen wired up, generated bindings checked in,
CI verifying they are not stale.

### M5 — macOS menu bar app
`MenuBarExtra` + `LSUIElement`, showing fleet liveness and per-host metrics. New ground: none of the
sibling projects has a `macos_application` Bazel target.

### M6 — iOS app
Fleet list, host detail, alert configuration. Shares `PessimalKit` with the menu bar app.

### M7 — Additional backends
Honeycomb and ClickStack query adapters behind the same port.

## Out of scope for now

- Push notifications and any server component.
- Log and trace collection. Metrics only.
- Agent auto-update and packaged installers.
