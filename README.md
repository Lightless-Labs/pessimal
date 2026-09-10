# Pessimal

Host telemetry for people who would rather be told than have to look.

Pessimal is a small Rust agent that collects host metrics and ships them to any OpenTelemetry
endpoint, plus an iOS app and a macOS menu bar app that read those metrics back, show liveness at a
glance, and raise configurable alerts.

> A.E. Pessimal is the Patrician's inspector in Terry Pratchett's *Thud!* — a neat little man with
> very shiny shoes, sent to audit the City Watch, whose forensic accounting later became "legendary
> and feared throughout Ankh-Morpork". This is that, for your hosts.

## Status

Working, unreleased, and not yet packaged for anyone but its author. There are no binaries to
download and the macOS app is not notarized yet.

| | |
|---|---|
| **Agent** | Works. Linux, macOS and Windows are built and tested in CI. Verified exporting over both gRPC and HTTP/protobuf to a real OpenTelemetry collector, and to SigNoz Cloud over TLS. |
| **Read path** | Works, verified end to end against a live SigNoz Cloud instance: agent → OTLP → backend → query adapter → fold → rendered view. |
| **macOS app** | Builds and runs. Menu bar only, no Dock icon. Not notarized. |
| **iOS app** | **Builds** to an `.ipa` via Bazel, with the Rust linked in and checked in CI. Not yet run on a simulator or a device. |
| **Backends** | SigNoz only. Honeycomb and ClickStack are the next piece of work. |

See [`docs/plans/`](docs/plans/) for the roadmap and [`todos/`](todos/) for what is deliberately
deferred.

## What it does

- **Agent** — one Rust binary for Linux, macOS and Windows. Samples CPU, memory, filesystem,
  network and load average, names them with the
  [OpenTelemetry system semantic conventions][semconv], and exports them over OTLP (gRPC or
  HTTP/protobuf). Emits a heartbeat so silence is detectable, and a separate counter for sampling
  failures — an agent can be beating happily while failing to read the host, and those are different
  problems.
- **Apps** — iOS and a macOS menu bar app over one shared Rust core through UniFFI. They query the
  *telemetry backend*, not the agents, so nothing has to be reachable from your phone except the
  backend you already run.
- **Alerts** — thresholds with a dwell period, evaluated client-side. Polling only; push would need a
  server component and is not planned yet.

Every product decision — liveness, severity, alert state, freshness, what to poll and when to poll
again — is made in Rust. Swift renders the answer and owns the timer, and decides nothing the core
could decide.

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

Reading metrics back is not standardised the way OTLP export is, so each backend needs its own query
adapter. **SigNoz is implemented and verified against a live instance**; Honeycomb and ClickStack are
not written yet. Anything new implements one trait, `pessimal_core::TelemetryQuery`, and everything
above it is unchanged.

## Layout

```
common/pessimal_core/            Domain: metrics, liveness, alerts, ports. No I/O, no async.
agents/
  common/pessimal_agent_core/      Shared by all agents: config, OTLP export, collection traits
  host/pessimal_agent_host/        The host telemetry agent binary
clients/
  common/pessimal_client_core/     Plan, gather, fold: polling, liveness, alert state
  common/query/pessimal_query_signoz/   The SigNoz query adapter
  ffi/pessimal_ffi/                UniFFI bridge. Translation only, no logic.
  apple/
    PessimalFFI/                     Generated Swift bindings (committed; CI fails if stale)
    PessimalKit/                     Swift shared by both apps: the model and the platform stores
    macos/                           The menu bar app
    ios/                             The iOS app
tools/uniffi/                    Bindgen CLI, version-locked to the workspace uniffi crate
platforms/                       Bazel target platforms for the Apple builds
scripts/                         Build, release and verification scripts
dev/                             A throwaway collector config for local work
docs/, todos/                    Plans, solution notes, deferred work
```

Two build systems, on purpose. The agents use **Cargo**, because cross-compiling to Linux and Windows
is far simpler that way and no Apple toolchain is involved. The Apple clients use **Bazel**, which is
where `rules_apple`, `rules_swift` and the generated Xcode project live.

## Building

The Rust, on any platform:

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

See what the agent would send, without exporting anything:

```bash
cargo run -p pessimal_agent_host -- --config pessimal.example.toml --sample
```

Run it against a throwaway collector and watch what arrives:

```bash
docker run --rm -p 4317:4317 -p 4318:4318 \
  -v "$PWD/dev/otelcol:/etc/otelcol-conf" \
  otel/opentelemetry-collector-contrib:latest --config /etc/otelcol-conf/config.yaml

cargo run -p pessimal_agent_host -- --config dev/pessimal.dev.toml
```

The apps (macOS only, needs Xcode):

```bash
./scripts/build-macos-app.sh           # assembles .build/macos/Pessimal.app
bazelisk build --config=ios_sim //clients/apple/ios:Pessimal
bazelisk run //:xcodeproj              # generate products/ios/Pessimal.xcodeproj
```

After changing anything in `pessimal_ffi`, regenerate the committed bindings — CI fails if they go
stale, because a stale `.swift` means your Rust change silently has no effect:

```bash
./tools/uniffi/regen.sh
./scripts/swift-smoke.sh               # drives the FFI from Swift's own executor
```

### Verifying against a real backend

The mock-based tests cannot catch a backend disagreeing with you, and on this project they did not:
the first contact with a live SigNoz found four bugs, including one where the fixtures and the parser
had been derived from the same incomplete source and so agreed with each other. Two opt-in tests
exist for that, skipped unless credentials are present, so CI never needs them:

```bash
PESSIMAL_LIVE_SIGNOZ_URL=https://your-org.region.signoz.cloud \
PESSIMAL_LIVE_SIGNOZ_KEY=... \
  cargo test -p pessimal_query_signoz --test live_signoz -- --nocapture

PESSIMAL_LIVE_SIGNOZ_URL=... PESSIMAL_LIVE_SIGNOZ_KEY=... \
  cargo test -p pessimal_ffi --test live_round_trip -- --nocapture
```

## Licence

GNU AGPL-3.0-or-later. See [LICENSE](LICENSE).

The agent is something you run on your own hosts and the apps talk only to your own backend, so the
network clause costs an ordinary user nothing. It does mean that if you modify Pessimal and offer it
to others — including as a hosted service — those changes have to be published too.

[semconv]: https://opentelemetry.io/docs/specs/semconv/system/system-metrics/
