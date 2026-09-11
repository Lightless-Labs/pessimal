# Pessimal Handoff

**Updated:** 2026-09-11

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
- **M7 iOS release — done, and now continuous.** Build `0.1.0.26` reached TestFlight on 2026-09-11
  from the mini and was processed by App Store Connect. **Every push to main that passes the three
  verification steps now ships to TestFlight** — no tag, no block step. `[skip release]` in the commit
  message opts a push out; a commit marker rather than a path filter, because a path filter that is
  wrong stops shipping silently.

  The tag gate it replaced was not merely friction: every verification step was `if: build.tag == null`,
  so a tag build ran the release and *nothing else*, and the signed upload depended on nothing having
  been verified. The release now `depends_on` all three. Tags are free for a future App Store
  submission path, which should stay a deliberate act.

  The marketing version comes from `[workspace.package] version` in `Cargo.toml` — already the single
  source of truth for every crate — and the build number from `BUILDKITE_BUILD_NUMBER`, which cannot go
  backwards. **Bumping that version is now the one manual step in a release**, and forgetting it means
  TestFlight keeps accumulating builds under the same marketing version, which is valid but unhelpful.
  What it took is in
  [`solutions/the-ios-release-path-end-to-end.md`](solutions/the-ios-release-path-end-to-end.md).
- **`scripts/asc.py` is a read-only App Store Connect client** for Python 3.9 with no third-party
  packages: ES256 JWTs are signed by shelling out to `openssl`. It replaced sigh's profile fetch and
  worked on first contact where sigh reported only "no matching profile found".
- **Secrets live in Doppler project `lightless-labs-pessimal`**, on the same service account as
  Pocket Companion: `prd_ios_deployment` for the TestFlight lanes (plus `GH_TOKEN`) and
  `prd_macos_notarisation` for `scripts/release-macos-app.sh`.
- **The query credential is still the user's**, taken from the settings screen and kept in the
  Keychain — never baked into the bundle. The *usage-reporting* credential is the one exception, and
  a deliberate reversal of the earlier "no app-runtime config" position, decided with the owner on
  2026-09-11: opt-out reporting to our own backend needs a credential we supply, so
  `SIGNOZ_OTLP_ENDPOINT` / `SIGNOZ_OTLP_INGESTION_KEY` from `prd_ios_deployment` are injected at
  build time. It is write-only and ingestion-only, like a Sentry DSN, and extractable from any IPA —
  which is why an ingestion proxy is the recorded upgrade path. Only official release builds carry
  it; see M8 below.
- **M8 usage reporting — phase 1 built, one verification outstanding.** Opt-out traces from the
  clients to our own SigNoz. `common/pessimal_usage` (pure: span types, the attribute allowlist,
  consent, the OTLP/JSON encoder), `common/pessimal_usage_otlp` (the transport), `pessimal_ffi::usage`
  (instance id, batching, the `PollFailureKind` → `Outcome` mapping), `FleetSession` instrumented
  around `poll` with `flush_usage()` and `usage_diagnostics()`, the Swift consent store and the opt-out
  section on both apps, `PrivacyInfo.xcprivacy`, the `usage_plist` genrule, and the guarded
  `--action_env` flags in the fastlane lane. The spike and the design are in
  [`plans/2026-09-11-m8-usage-reporting.md`](plans/2026-09-11-m8-usage-reporting.md).

  **Still unproven: a span from a release build appearing in our SigNoz.** Everything local passes; a
  loopback collector verified the encoding and nothing else, which is the lesson this repo already
  learnt once. The credential flows from Doppler `prd_ios_deployment`
  (`SIGNOZ_OTLP_ENDPOINT`, `SIGNOZ_OTLP_INGESTION_KEY`) and both are *optional* in the release script,
  so a release without them reports nothing rather than failing.

  Two follow-ups, both additive: child spans for `gather` (needs a sink handle inside
  `pessimal_query_signoz`), and a span for `probe()` (its session is a throwaway and is given no
  reporter on purpose).

## Verifying the agent locally

The dev collector config at `dev/otelcol/config.yaml` now has **both** a metrics and a traces
pipeline — the latter for the usage spans of M8.

**Docker does not work on the development mini.** It is itself a Tart guest (`hw.model =
VirtualMac2,1`, `Apple M4 (Virtual)`), and Apple Silicon has no nested virtualisation, so colima's
`vz` driver fails with "Virtualization is not available on this hardware". Use the native binary,
which is faster anyway:

```bash
# https://github.com/open-telemetry/opentelemetry-collector-releases/releases
#   otelcol-contrib_<version>_darwin_arm64.tar.gz
./otelcol-contrib --config dev/otelcol/config.yaml

# And to put a trace in front of it, including the Rust encoder's own output:
scripts/otlp-trace-probe.py http://localhost:4318
cargo run -q -p pessimal_usage --example emit_trace \
  | scripts/otlp-trace-probe.py --stdin http://localhost:4318
```

On a machine where Docker does work:

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

- **An OTLP endpoint may already carry its signal path, and appending a second one 404s silently.**
  Doppler's `SIGNOZ_OTLP_ENDPOINT` holds `https://ingest.eu2.signoz.cloud:443/v1/traces`, so build 34
  shipped posting to `…/v1/traces/v1/traces`. `Destination::new` now strips a trailing
  `/v1/{traces,metrics,logs}`. Measured against the real endpoint **without a credential**, which is
  the cheap way to answer it: the corrected URL answers **401 "No key"** — the path exists, it wants a
  key — and the doubled URL answers **404 "not found"**. A status that distinguishes "wrong path" from
  "wrong key" needs no secret to read.
- **A commit-message marker is tripped by the commit that documents it.** The release opt-out matched
  against the full `BUILDKITE_MESSAGE`, and the commit introducing it necessarily *explained* the
  marker in its own body — so builds 32 and 33 both declined to ship, #33 reporting **success with no
  ipa**, which is the most expensive shape of failure because it looks like a release. The marker is
  now honoured on the **subject line only**, which also means it cannot live in the step's `if:`:
  Buildkite's `build.message` is the whole message and cannot be narrowed. Buildkite's own `[skip ci]`
  has the same trap.
- **`broken_reason: conditional_failed` does not say which clause was false.** One-clause probe steps
  on a cheap queue answer it in one build; guessing does not. Doing that disproved the theory that
  `build.pull_request.id == null` fails when `build.pull_request` is itself null — it evaluates **true**,
  and was a red herring for two builds.
- **`cargo tree` shows a workspace-wide feature resolve, which is not what a single binary builds.**
  To get the feature set a binary *actually* compiles with, use
  `cargo build -p <pkg> --message-format=json` and read the `features` array on each
  `compiler-artifact`. And to exercise a dependency in a package's exact closure, add a temporary
  `[[bin]]` to that package rather than a test — dev-dependencies unify into the test build and
  change the answer.
- **The agent enables both rustls providers at once, and that is a loaded gun.**
  `opentelemetry-otlp`'s feature list carries `reqwest-rustls` (→ `aws-lc-rs`) and `tls-ring` (→
  `ring`), so `rustls` compiles with both and `from_crate_features()` returns `None`. Any generic
  `ClientConfig::builder()` in that graph panics with "Could not automatically determine the
  process-level CryptoProvider". Nothing hits it today only because both of the agent's TLS paths
  name a provider explicitly. Any crate using `reqwest/rustls-no-provider` that is linked into the
  agent must install one itself first.
- **Rust tests have no network egress on the dev mini; Python does.** A `reqwest` call to a public
  host times out where `urllib` gets a 200. So anything that has to reach a real backend from here
  goes through a Python script, and the `live_*` Rust tests are effectively CI-only.
- **A 200 from an OTLP endpoint does not mean the span was stored**, and the folklore about *how*
  OTLP/JSON fails is wrong in detail. Measured against otelcol-contrib 0.160.0: base64 ids are a
  **400** with a clear message; an int64 sent as a JSON number is accepted *and exact*; a zeroed
  `parentSpanId` on a root is accepted and normalised away. Emit the spec shape regardless — that
  leniency is one receiver's, not OTLP's — and confirm delivery by finding the trace id in the
  backend, not by reading the status.

## Next up

**M8 phase 1 is built.** What remains:

1. **Verify live.** Cut a TestFlight build with the two Doppler secrets present and find the trace in
   our SigNoz. `scripts/otlp-trace-probe.py` against the real ingest endpoint is the faster first
   check — it answers "does the credential work and is the header right" in seconds, without a release.
2. Child spans for `gather`, which needs `pessimal_query_signoz` to hold a sink handle.
3. **M8 phase 2, the agent.** `[usage_reporting]` in the config, `DO_NOT_TRACK` and `CI` honoured
   (both already implemented in `pessimal_usage::consent`), agent-side span variants, and
   `Destination::from_build` fed by `option_env!`. Blocked on an agent release path existing, since
   that is what would inject the credential.
4. Fix `FleetStoreBridge` persisting 3 of its 6 fields. Untouched by M8 — the consent store is
   deliberately separate from it — but still outstanding.
5. Check whether `env!("CARGO_PKG_VERSION")` is `0.0.0` under rules_rust, since no BUILD file here
   passes `version`. It only affects the reported scope version, so it is cosmetic, but it would differ
   between the Cargo and Bazel builds.

Then additional query backends (Honeycomb, ClickStack), or the open items below.

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
