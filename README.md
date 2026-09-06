# Pessimal

Host telemetry for people who would rather be told than have to look.

Pessimal is a small Rust agent that collects host metrics and ships them to any OpenTelemetry
endpoint, plus an iOS app and a macOS menu bar app that read those metrics back, show liveness at a
glance, and raise configurable alerts.

> A.E. Pessimal is the Patrician's inspector in Terry Pratchett's *Thud!* — a neat little man with
> very shiny shoes, sent to audit the City Watch, whose forensic accounting later became "legendary
> and feared throughout Ankh-Morpork". This is that, for your hosts.

**Status: early.** The domain core and the host agent work and are tested end to end against a
real OpenTelemetry collector. The query adapters and both apps are not written yet. See
[`docs/plans/`](docs/plans/) for the roadmap.

## What it does

- **Agent** — one Rust binary for Linux, macOS, and Windows. Samples CPU, memory, filesystem,
  network, and load average, names them with the [OpenTelemetry system semantic conventions][semconv],
  and exports them over OTLP (gRPC or HTTP/protobuf). Emits a heartbeat so silence is detectable.
- **Apps** — iOS and a macOS menu bar app, sharing one Rust core through UniFFI. They *query the
  telemetry backend*, not the agents: nothing has to be reachable from your phone except the
  backend you already run.
- **Alerts** — thresholds with a dwell period, evaluated client-side. Polling only for now; push is
  a later milestone.

## Backends

Export is plain OTLP, so anything that speaks it works. The named presets exist only to fill in the
right auth header:

| Backend | Preset | Header |
|---|---|---|
| SigNoz (self-hosted) | `signoz` | none |
| SigNoz (cloud) | `signoz` | `signoz-access-token` |
| ClickStack / HyperDX | `clickstack` | `authorization` |
| Honeycomb | `honeycomb` | `x-honeycomb-team`, `x-honeycomb-dataset` |
| Anything else | `otlp` | whatever you configure |

Reading metrics back is not standardised the way OTLP export is, so each backend needs a query
adapter. SigNoz ships first; ClickStack and Honeycomb follow.

## Layout

```
common/          Shared by agents and clients
  pessimal_core/     Domain: metrics, liveness, alerts, ports. No I/O.
agents/
  common/            Shared among agents: collection traits, OTLP export, config
  host/              The host telemetry agent binary
clients/
  common/            Shared among clients: query adapters, polling orchestration
  ffi/               UniFFI bridge (Rust <-> Swift)
  apple/             Generated bindings, shared SwiftUI, macOS menu bar, iOS app
tools/uniffi/    Bindgen CLI, version-locked to the workspace uniffi crate
docs/            Plans, architecture notes, solution docs
```

## Building

Everything that exists today builds with Cargo, on Linux, macOS, and Windows:

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Run the agent against a throwaway collector to see what it exports:

```bash
docker run --rm -p 4317:4317 -p 4318:4318 \
  -v "$PWD/dev/otelcol:/etc/otelcol-conf" \
  otel/opentelemetry-collector-contrib:latest --config /etc/otelcol-conf/config.yaml

cargo run -p pessimal_agent_host -- --config dev/pessimal.dev.toml
```

Or just look at what it would send, without exporting anything:

```bash
cargo run -p pessimal_agent_host -- --config pessimal.example.toml --sample
```

The Apple apps will build with Bazel once they exist (M5 onward); there is no `MODULE.bazel` yet.

## Licence

MIT. See [LICENSE](LICENSE).

[semconv]: https://opentelemetry.io/docs/specs/semconv/system/system-metrics/
