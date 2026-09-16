# Pessimal

Pessimal collects metrics from your hosts and shows them in an iOS app and a macOS menu bar app.

> A.E. Pessimal is the Patrician's inspector in Terry Pratchett's *Thud!* — a neat little man with
> very shiny shoes, sent to audit the City Watch, whose forensic accounting later became "legendary
> and feared throughout Ankh-Morpork". This is that, for your hosts.

- **Agent.** A Rust program that runs on each host. It reads CPU, memory, disk, network and load
  average, and sends them over OTLP (gRPC or HTTP/protobuf) to your telemetry backend. It also sends a
  heartbeat, so the apps can tell when a host stops reporting.
- **Apps.** The iOS app and the macOS menu bar app read the metrics from your backend, not from the
  hosts. They show which hosts are up and raise the alerts you configure.
- **Core.** Both apps share one Rust library through UniFFI. It decides liveness, alert state and what
  to poll.

## Status

No release is published yet. Build from source.

| Part | State |
|---|---|
| Agent | Works on Linux and macOS. Not supported on Windows. |
| iOS app | On TestFlight. |
| macOS app | Builds and runs. Not published yet. |
| Backends | The agent sends to any OTLP endpoint. The apps read from SigNoz only. |

## Backends

The agent sends plain OTLP. A preset only chooses the header that carries your key.

| Backend | Preset | Header |
|---|---|---|
| SigNoz, self-hosted | `signoz` | none |
| SigNoz Cloud | `signoz` | `signoz-access-token` |
| ClickStack / HyperDX | `clickstack` | `authorization` |
| Honeycomb | `honeycomb` | `x-honeycomb-team`, `x-honeycomb-dataset` |
| Other | `otlp` | the headers you set |

The apps need a query adapter for each backend. Only SigNoz has one. A new backend implements
`pessimal_core::TelemetryQuery`.

## Run the agent

Build it and copy the example config:

```bash
cargo build --release -p pessimal_agent_host      # target/release/pessimal-agent
cp pessimal.example.toml pessimal.toml
```

Set these in `pessimal.toml`:

| Field | Value |
|---|---|
| `export.endpoint` | Your OTLP endpoint, with the scheme and the port. |
| `export.preset` | Your backend, from the table above. |
| `resource.environment` | A name for this deployment. The agent adds it to every metric as `deployment.environment.name`. |

Each field has an environment variable that overrides it: `PESSIMAL_ENDPOINT`, `PESSIMAL_PRESET`,
`PESSIMAL_PROTOCOL`, `PESSIMAL_API_KEY`, `PESSIMAL_INTERVAL_SECONDS`, `PESSIMAL_SERVICE_NAME`,
`PESSIMAL_ENVIRONMENT` and `PESSIMAL_HOST_NAME`. Put the key in `PESSIMAL_API_KEY`, not in the file. An
empty variable counts as unset.

Check the config and print one sample. Neither command sends data:

```bash
./target/release/pessimal-agent --config pessimal.toml --check
./target/release/pessimal-agent --config pessimal.toml --sample
```

Run the agent:

```bash
PESSIMAL_LOG=info,opentelemetry_sdk=debug ./target/release/pessimal-agent --config pessimal.toml
```

Each export logs `export_result="Ok(())"` or an error. The agent keeps running when an export fails, so
read these lines, not the exit code. If your endpoint needs a key, use `grpc`. Over `http/protobuf`, a
rejected key shows only as `HTTP export failed: network error`.

### macOS service

```bash
scripts/install-agent-launchd.sh \
  --environment production \
  --preset signoz --endpoint https://ingest.eu2.signoz.cloud:443 \
  --key-file ~/.config/pessimal/ingestion-key
```

This installs a LaunchAgent for your user. It starts at login and restarts if it stops. The script
checks that one export succeeds before it installs. It puts the key in the LaunchAgent's environment,
not in the config file. The log is `~/Library/Logs/pessimal/agent.log`.

To remove it:

```bash
scripts/install-agent-launchd.sh --uninstall
```

### Linux service

Use [`packaging/systemd/pessimal-agent.service`](packaging/systemd/pessimal-agent.service). The steps
are in [`packaging/tarball/README.md`](packaging/tarball/README.md).

## Install a release

Each [GitHub release](https://github.com/Lightless-Labs/pessimal/releases) has these files:

| File | For |
|---|---|
| `pessimal-agent-<version>-aarch64-apple-darwin.tar.gz` | macOS, Apple silicon |
| `pessimal-agent-<version>-x86_64-apple-darwin.tar.gz` | macOS, Intel |
| `pessimal-agent-<version>-aarch64-unknown-linux-gnu.tar.gz` | Linux, arm64, glibc 2.28 or later |
| `pessimal-agent-<version>-x86_64-unknown-linux-gnu.tar.gz` | Linux, x86_64, glibc 2.28 or later |
| `Pessimal-<version>-macos.zip` | The menu bar app, Apple silicon only |
| `SHA256SUMS` | Checksums for the files above |

Install the agent with Homebrew or mise, or from a tarball:

```bash
brew install lightless-labs/tap/pessimal-agent

mise use -g github:Lightless-Labs/pessimal

shasum -a 256 -c --ignore-missing SHA256SUMS
tar -xzf pessimal-agent-<version>-<target>.tar.gz
```

Then set it up:

```bash
pessimal-agent init
```

`init` asks where to send metrics, which environment this host belongs to, and for the API key,
which it does not echo. It writes the config where the service on this machine reads it, at mode
0600, sends one batch to check the backend accepts it, and offers to start the service. Run it again
to change any answer; the previous config is kept as `pessimal.toml.bak`.

For a configuration management tool, the same without questions:

```bash
pessimal-agent init --non-interactive --preset signoz \
  --endpoint https://ingest.eu2.signoz.cloud:443 --environment production \
  --api-key-stdin < key.txt
```

There is no `--api-key` flag on purpose: command-line arguments are visible in `ps` to everyone on
the machine.

On macOS, `scripts/install-agent-launchd.sh --tarball <file>` installs a downloaded tarball as a
service.

Install the menu bar app with Homebrew, which also updates it on `brew upgrade`:

```bash
brew install --cask lightless-labs/tap/pessimal
open /Applications/Pessimal.app
```

Or download `Pessimal-<version>-macos.zip` from the release, unzip it, and move `Pessimal.app` to
Applications. It needs Apple silicon and macOS 14 or later. It has no Dock icon and no window: click
the icon in the menu bar, open Settings, and set the backend address, the API key and the
environment. A downloaded copy opens without a Gatekeeper warning, because the app is notarized and
its ticket is stapled.

To publish a release, follow [`docs/runbooks/cutting-a-release.md`](docs/runbooks/cutting-a-release.md).

## Build and test

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

The apps need macOS and Xcode:

```bash
./scripts/build-macos-app.sh                                 # .build/macos/Pessimal.app
bazelisk build --config=ios_sim //clients/apple/ios:Pessimal
bazelisk run //:xcodeproj                                    # products/ios/Pessimal.xcodeproj
```

After you change `pessimal_ffi`, regenerate the Swift bindings and commit both generated files. CI
fails if they are out of date.

```bash
./tools/uniffi/regen.sh
./scripts/swift-smoke.sh
```

To see what the agent sends, run a local collector:

```bash
docker run --rm -p 4317:4317 -p 4318:4318 \
  -v "$PWD/dev/otelcol:/etc/otelcol-conf" \
  otel/opentelemetry-collector-contrib:latest --config /etc/otelcol-conf/config.yaml

cargo run -p pessimal_agent_host -- --config dev/pessimal.dev.toml
```

To test against a live SigNoz instance, set both variables. Without them, these tests do nothing.

```bash
PESSIMAL_LIVE_SIGNOZ_URL=https://your-org.region.signoz.cloud \
PESSIMAL_LIVE_SIGNOZ_KEY=... \
  cargo test -p pessimal_query_signoz --test live_signoz -- --nocapture

PESSIMAL_LIVE_SIGNOZ_URL=... PESSIMAL_LIVE_SIGNOZ_KEY=... \
  cargo test -p pessimal_ffi --test live_round_trip -- --nocapture
```

CI runs on Buildkite ([`.buildkite/pipeline.yml`](.buildkite/pipeline.yml)). Pull requests from forks
get no CI.

## Layout

```
common/pessimal_core/                 Domain types, liveness, alerts. No I/O.
agents/
  common/pessimal_agent_core/         Agent config, OTLP export, collection
  host/pessimal_agent_host/           The agent binary
clients/
  common/pessimal_client_core/        Polling, liveness and alert state for the apps
  common/query/pessimal_query_signoz/ The SigNoz query adapter
  ffi/pessimal_ffi/                   The UniFFI bridge
  apple/
    PessimalFFI/                      Generated Swift bindings
    PessimalKit/                      Swift code for both apps
    macos/                            The menu bar app
    ios/                              The iOS app
tools/uniffi/                         The bindings generator
platforms/                            Bazel platforms for the Apple builds
packaging/                            Release files: systemd unit, Homebrew formula, tarball README
scripts/                              Build, CI and release scripts
dev/                                  A local collector config
docs/, todos/                         Plans, notes and open work
```

The agent builds with Cargo. The apps build with Bazel.

## Licence

GNU AGPL-3.0-or-later. See [LICENSE](LICENSE). If you change Pessimal and let other people use it,
including as a hosted service, you must publish your changes.
