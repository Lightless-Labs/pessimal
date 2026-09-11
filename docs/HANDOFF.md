# Pessimal Handoff

**Updated:** 2026-09-10

## Current state

- **M0 scaffold** — done. Cargo workspace across `common/`, `agents/`, `clients/`, `tools/`.
  AGPL-3.0-or-later, conventional commits via `cog`.
- **M1 domain core** — done. `pessimal_core` covers metric identity, instrument kinds, time ranges
  and series, liveness, alert evaluation, URNs, and ports.
- **M2 host agent** — done. `pessimal_agent_core` (config, backend presets, cached collection,
  resource identity, OTLP export) and `pessimal_agent_host` (sysinfo collector, CLI, run loop).
  Verified end to end against a real OpenTelemetry collector over **both** gRPC and HTTP/protobuf:
  12 metrics arrive with the right names, units, and instrument types.
- 156 tests, `clippy -D warnings` clean, `cargo fmt --check` clean.
- **M3 SigNoz adapter** — `pessimal_query_signoz` implements `TelemetryQuery` against
  `/api/v5/query_range`. Response types taken from SigNoz's own Go source, not guessed; the naming
  convention (dotted for v0.88+/Cloud, underscored before) is a setting. **Never run against a live
  SigNoz** — structure is authoritative, behaviour is not.
- **M3 client core** — `pessimal_client_core` is plan-gather-fold, 7,774 lines and 120 tests, per
  [`docs/plans/2026-09-07-m3-client-core.md`](plans/2026-09-07-m3-client-core.md). The hexagon holds:
  no reqwest, no tokio outside dev-dependencies, no query adapter, no uniffi.
- **M4 FFI bridge** — `pessimal_ffi` mirrors every client_core type as a UniFFI Record and exposes
  `FleetSession`, per section 4.11 of the client-core design. 6,011 lines, 96 tests, 77 public Swift
  types with no name collisions. Generated bindings are committed at
  `clients/apple/PessimalFFI/Sources/` and CI fails if they go stale.
- **CI is green on all three platforms**, first run: ubuntu-latest, macos-15, windows-latest, plus
  the OTLP export smoke test against a real collector and the Swift smoke test. The two risks flagged
  earlier did not materialise — Windows built `aws-lc-rs` without needing NASM, and the collector
  service container was reachable.
- **The whole read path is verified against production.** Agent → OTLP/TLS → SigNoz Cloud → query
  adapter → fold → view, confirmed with a real instance: the roster finds the host, it reads Alive,
  and five metrics arrive with real values. Two opt-in tests keep it honest, both skipped without
  `PESSIMAL_LIVE_SIGNOZ_URL` / `PESSIMAL_LIVE_SIGNOZ_KEY`:
  `pessimal_query_signoz --test live_signoz` for the adapter, and
  `pessimal_ffi --test live_round_trip` for the whole chain.
- **M5 macOS menu bar app** — 23 Swift files at `clients/apple/macos/`, built to a real `.app` by
  `scripts/build-macos-app.sh` with plain `swiftc`. Verified launching as a `UIElement` with no Dock
  icon and a real status item. CI builds the bundle and asserts it is statically linked, has no
  leftover plist placeholders, and keeps `LSUIElement`.
- **Bazel builds the Apple clients**, on `rules_rust` 0.74.0 (not the siblings' 0.68.1, which
  predates the 1.95 MSRV). The agents stay on Cargo. Both app builds coexist: Bazel for iOS,
  `scripts/build-macos-app.sh` for macOS.
- **M6 iOS app** — 17 Swift files at `clients/apple/ios/`, built end to end by Bazel to
  `Pessimal.ipa`: Rust through crate_universe, the staticlib, the bindings as real objc/swift
  modules, PessimalKit, the app library, the bundle. Verified arm64 with 40 uniffi symbols linked in.
- **`clients/apple/PessimalKit/`** holds the Swift both apps share: `FleetModel`, the platform
  stores, and the composition root. Its imports of the bindings are guarded with `canImport`, which
  is load-bearing — Bazel compiles `PessimalFFI` as a real module while the macOS script compiles
  everything into one module where it does not exist.
- **Buildkite builds the Apple clients on the mini.** Pipeline `la-bande-a-bonnot/pessimal`, green
  end to end for the non-release jobs: the Rust workspace on a Linux guest, the iOS app through Bazel
  with the uniffi symbols asserted, and the macOS bundle with the Swift smoke test. GitHub Actions
  keeps the cross-platform matrix, which hosted runners do free on a public repo.
- **M7 iOS release — done.** Build `0.1.0.26` reached TestFlight on 2026-09-11 from the mini and was
  processed by App Store Connect. Tag `pessimal-ios-v*`, a block step, then sign, build and upload.
  What it took is in
  [`solutions/the-ios-release-path-end-to-end.md`](solutions/the-ios-release-path-end-to-end.md).
- **`scripts/asc.py` is a read-only App Store Connect client** for Python 3.9 with no third-party
  packages: ES256 JWTs are signed by shelling out to `openssl`. It replaced sigh's profile fetch and
  worked on first contact where sigh reported only "no matching profile found".
- **Secrets live in Doppler project `lightless-labs-pessimal`**, on the same service account as
  Pocket Companion: `prd_ios_deployment` for the TestFlight lanes (plus `GH_TOKEN`) and
  `prd_macos_notarisation` for `scripts/release-macos-app.sh`. There is deliberately no app-runtime
  config: the iOS app takes its backend URL and key from the user and keeps the key in the Keychain,
  rather than baking them into the bundle with a genrule the way kumbaya does. Adding one would mean
  shipping a shared credential to every install.

## Verifying the agent locally

```bash
docker run --rm -p 4317:4317 -p 4318:4318 \
  -v "$PWD/dev/otelcol:/etc/otelcol-conf" \
  otel/opentelemetry-collector-contrib:latest --config /etc/otelcol-conf/config.yaml

cargo run -p pessimal_agent_host -- --config dev/pessimal.dev.toml
cargo run -p pessimal_agent_host -- --config dev/pessimal.dev.toml --sample   # no export
cargo run -p pessimal_agent_host -- --config dev/pessimal.dev.toml --check    # validate only
```

## Known issues

- None open.

## Gotchas found the hard way

**`--platforms` does not decide device versus simulator.** rules_apple reads the legacy Apple CPU
configuration, so `--config=ios_device` with only `--platforms=//platforms:ios_arm64` built an arm64
bundle whose Info.plist said `DTPlatformName=iphonesimulator`. It signs nothing, installs nowhere, and
— worst of all — *succeeds*, which is how a missing `provisioning_profile` attribute went unnoticed.
`--ios_multi_cpus=arm64` is the flag that decides it.

**`--embed_label` is inert without an `apple_bundle_version` target.** fastlane passed the label, the
build succeeded, and `CFBundleVersion` stayed at whatever Info.plist said. The failure would have
arrived from App Store Connect on the *second* upload, after signing and uploading both times.


- **A collector on loopback verifies almost nothing.** Four real bugs survived every local run, the
  CI export job and the smoke script, and all four fell out of the first contact with a real
  backend: no TLS roots on the gRPC path, the response envelope parsed one level too shallow, the
  wrong temporality for counters, and no budget for ingestion lag. `http://localhost:4317`
  exercises neither certificates, nor auth, nor lag, nor the real response shape.
- **A fixture built from the same source as the parser cannot find a parser bug.** The mock bodies
  and the wire types were both written from SigNoz's Go `QueryRangeResponse`, which is only the
  inner half of the HTTP envelope. They agreed with each other and both disagreed with the server,
  and no number of assertions between them would ever have noticed.
- **`#[serde(default)]` on every field turns a wrong shape into an empty answer.** That is what made
  the envelope bug silent: a fleet that was reporting normally looked like a fleet with no hosts.
  Required fields where the shape is load-bearing.

- **`swiftc` needs the modulemap passed explicitly.** The generated `PessimalFFI.swift` guards its
  import with `#if canImport(PessimalFFIFFI)`, which silently compiles to nothing without
  `-Xcc -fmodule-map-file=…/PessimalFFIFFI.modulemap`. The error is
  `cannot find type 'RustBuffer' in scope`, which never mentions modules. See
  [`solutions/uniffi-tokio-runtime-verified-from-swift.md`](solutions/uniffi-tokio-runtime-verified-from-swift.md).
- **Swift Record and enum names share one flat namespace** across every Rust module. A collision
  compiles in Rust, generates bindings fine, and fails the Xcode build with an unrelated-looking
  redeclaration error. `grep -oE "^public (struct|enum) [A-Za-z0-9_]+" …/PessimalFFI.swift | sort |
  uniq -d` is the check.

- **OTLP/HTTP needs the signal path appended.** `with_endpoint()` is treated as a *signal-specific*
  endpoint by `opentelemetry-otlp`, so it posts to `/` and every collector answers 404. Pessimal
  treats `export.endpoint` as a base URL and appends `/v1/metrics` itself.
- **The gRPC and HTTP exporters want opposite things from Tokio.** tonic needs a runtime alive for
  the process lifetime and captures a handle when the channel is built; the HTTP path uses a
  *blocking* reqwest client, which will not run inside a runtime context. So the agent constructs
  the provider under a short-lived `runtime.enter()` guard and runs `shutdown()` outside any
  runtime context.
- **Derived `Deserialize` walks around a validating constructor.** Every validated type in core had
  private fields, a fallible constructor, and a derived `Deserialize` that bypassed it — an inverted
  liveness policy and an unsorted `MetricSeries` whose `latest()` returned the wrong sample were both
  reachable from JSON. Fixed with `#[serde(try_from = "…Wire")]` throughout. Apply the same pattern
  to any new validated type; `pessimal_client_core` already does.
- **A coarse query step destroys liveness.** SigNoz timestamps a bucket at its *start*, so
  querying a 30-minute window in one bucket reports every host's last heartbeat as 30 minutes old
  and marks a healthy fleet down. `list_hosts` queries at the heartbeat interval for that reason,
  and `LivenessPolicy`'s default stale threshold is 3 intervals rather than 2 to leave headroom for
  the quantisation that remains.
- **SigNoz does not document its query response body.** Take it from
  `pkg/types/querybuildertypes/querybuildertypesv5/resp.go` in their repo. Three details a guess
  gets wrong: `labels[].key` is an object (name at `.key.name`), `value` arrives as the *string*
  `"NaN"`/`"Inf"`/`"-Inf"` for non-finite numbers despite being typed `float64`, and `partial`
  buckets must be dropped or a stale host reads as alive.
- **Do not trust `docker logs` after `docker restart`** when verifying an export: the previous
  run's lines are still there and will happily convince you a broken path works. Recreate the
  container.

## Next up

M7, additional query backends (Honeycomb, ClickStack), or the open items below. Two things first:

1. ~~Read `todos/bazel-toolchain-must-provide-rust-1-95.md` before writing `MODULE.bazel`~~ — done.
   `MODULE.bazel` pins `rules_rust` 0.74.0 and Rust 1.95.0, and the iOS app builds. A
   `macos_application` target is still new ground; macOS ships through
   `scripts/build-macos-app.sh` for now.
2. ~~Confirm the SigNoz adapter against a live instance~~ — done 2026-09-09, and it found four bugs.
   See [`solutions/backend-ingestion-lag-breaks-liveness.md`](solutions/backend-ingestion-lag-breaks-liveness.md)
   and the fixes either side of it in the log.
3. ~~Pick up `todos/alert-evidence-staleness-ignores-backend-lag.md`~~ — done, commit `0521efb`:
   the evidence gate now uses `evidence_horizon()` rather than `max_staleness`.
4. Run the first TestFlight upload by hand before writing a workflow for it. The lane loads and the
   build resolves its profile; what has never happened is the portal fetch and the upload.

One thing the client core cannot check and M4 must not forget: nothing detects an unwired
`SignozConfig::for_policy`, and the symptom is a healthy fleet silently reading stale or down with
no error anywhere. Build the config from the policy, never alongside it.
