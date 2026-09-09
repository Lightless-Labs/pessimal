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
| Client crates pick their **own reqwest TLS features** | The agent inherits `aws-lc-rs` from reqwest's default rustls provider. Harmless server-side, but it is the usual cause of iOS cross-compilation trouble, and the two are separate build graphs so they need not agree. |
| **MSRV 1.95** | Set by `sysinfo` 0.39, not by choice. Recorded because the Bazel Rust toolchain has to match it. |
| Bazel pinned to **8.2.1** | rules_apple is not yet Bazel 9 compatible. Matches the toolchain proven in the sibling projects. |

## Milestones

### M0 — Scaffold and CI ✅
Repo layout, Cargo workspace, licence, conventional commits. CI builds and tests the Rust tree on
`ubuntu-latest`, `macos-15`, and `windows-latest`, lints once on Linux, and runs an OTLP export
smoke test over both protocols against a real collector service container.

PR CI runs on hosted runners only. Standard hosted runners are free and unmetered for public
repositories, so this costs nothing, and it means a fork's pull request can never execute on the
owner's machine. The self-hosted `getmac-tahoe` runner is reserved for pushes to `main` and for
release workflows, which forks cannot trigger.

### M1 — Domain core ✅
`pessimal_core`: metric identity mapped to OTel semconv names, time ranges and series, liveness
policy over heartbeat age, alert rules with dwell-period evaluation, URN identity, and the ports
adapters implement. No I/O, fully unit-tested.

### M2 — Host agent ✅
`pessimal_agent_core` (collection trait, resource attributes, OTLP exporter with backend presets,
TOML config) and `pessimal_agent_host` (sysinfo-backed collector, CLI, run loop). Verified against a
real OTLP endpoint, not just unit tests.

### M3 — SigNoz query adapter and client core ✅
`pessimal_query_signoz` implements `TelemetryQuery` against `/api/v5/query_range`, with the naming
convention (dotted vs underscored) as a setting rather than an assumption. Tested at the HTTP level
against a mock server; not yet run against a live instance. `pessimal_client_core` is
plan-gather-fold: pure planning, one async gather that never fails, and a pure fold returning the
next state, the view, the alert transitions, and polling advice. See
[`2026-09-07-m3-client-core.md`](2026-09-07-m3-client-core.md).

### M4 — FFI bridge ✅
`pessimal_ffi` exposes the client core to Swift: a Record per client_core type and a `FleetSession`
object, per section 4.11 of the client-core design. Bindgen wired up via `tools/uniffi/regen.sh`,
generated bindings committed, and CI both fails on stale bindings and drives the boundary from Swift
via `scripts/swift-smoke.sh`.

`#[uniffi::export(async_runtime = "tokio")]` proved sufficient — no hand-rolled runtime, unlike all
three sibling projects. See
[`../solutions/uniffi-tokio-runtime-verified-from-swift.md`](../solutions/uniffi-tokio-runtime-verified-from-swift.md).

### M5 — macOS menu bar app ✅
`MenuBarExtra` + `LSUIElement`, showing fleet liveness, per-host metrics, alerts and a freshness
banner, with a settings window for the backend, poll interval and alert rules.

Built with plain `swiftc` and a hand-assembled bundle, following the sibling project Descartes,
rather than with Bazel and `rules_apple`. Bazel could not be made to run in the build environment at
all, and Descartes already ships a notarized macOS app this way. **This departs from the original
brief**; see [`../../todos/bazel-toolchain-must-provide-rust-1-95.md`](../../todos/bazel-toolchain-must-provide-rust-1-95.md).

### M6 — iOS app
Fleet list, host detail, alert configuration. Shares `PessimalKit` with the menu bar app.

### M7 — Additional backends
Honeycomb and ClickStack query adapters behind the same port.

## Out of scope for now

- Push notifications and any server component.
- Log and trace collection. Metrics only.
- Agent auto-update and packaged installers.
