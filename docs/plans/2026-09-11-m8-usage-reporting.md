# M8 — Usage reporting (Pessimal's own telemetry)

**Created:** 2026-09-11
**Status:** Spike complete; implementation next

## Goal

Pessimal reports on itself. Opt-out usage reporting from **both** the clients and the agents to
Lightless Labs' own SigNoz, so we can see what breaks in the field instead of waiting to be told.
Traces first — a poll cycle is a trace-shaped thing, and spans give SigNoz enough to derive rate,
errors and duration without us designing a metric set up front.

Today the clients emit nothing at all: there is no `tracing` subscriber on the client path, so the
`tracing` macros in `pessimal_client_core` go nowhere, and no analytics SDK in the Swift. The agents
emit `pessimal.agent.heartbeat` and `pessimal.agent.collection_failures`, but only to the
*operator's* backend — we never see them.

## Not the same thing as collecting traces from hosts

The roadmap lists "log and trace collection, metrics only" as out of scope. That still holds: it is
about what the **agents collect from the machines they watch**. This milestone is Pessimal
instrumenting *itself*, on a separate export path, to a separate destination.

## Decisions

| Decision | Rationale |
|---|---|
| **Traces only** in phase 1 | One encoder, one endpoint (`/v1/traces`), one thing to verify. SigNoz derives a service entry with RED metrics from spans, so a metric set is not needed to answer the first questions. A heartbeat counter can follow if spans turn out not to answer "how many installs". |
| A **separate, shared vendor sink**, not a second OTel SDK pipeline | The agent's operator-facing export keeps `opentelemetry-otlp` + tonic, untouched. The vendor path is small, identical on both sides, and independent of whatever the operator configured. One wire format to verify rather than two. |
| **Hand-rolled OTLP/HTTP JSON**, no `opentelemetry-otlp` on the client | `opentelemetry-otlp`'s `reqwest-rustls` routes through reqwest's default provider, i.e. `aws-lc-rs` — the exact thing `pessimal_query_signoz` was written to avoid for iOS. Its `PeriodicReader` is also a forever-task, wrong for a backgrounded app. A handful of spans does not need an SDK. |
| The **core stays pure** | Spans are emitted from `pessimal_ffi::session` around `poll()`/`probe()`, and from the query adapter for HTTP timing. `pessimal_client_core` gains no sink, no reqwest, no tokio. |
| Attributes are an **allowlist enforced by construction** | See [What leaves the device](#what-leaves-the-device). The span builders accept enums and numbers, never a free `String`, so a future edit cannot casually attach someone's hostname. |
| The credential is **injected at build time**, absent from source builds | `--action_env` into a genrule'd plist for the apps, `option_env!` for the agent: kumbaya's and phil-connors' mechanism rather than a third one. A `cargo build` or a `--config=ci` simulator build has no key, so it cannot report — test builds phoning home is designed out rather than configured out. |
| **Direct to SigNoz**, not through an ingestion proxy | Matches the `SIGNOZ_OTLP_ENDPOINT` / `SIGNOZ_OTLP_INGESTION_KEY` pair already in Doppler. A proxy (Worker in front of SigNoz) would be strictly better — rotate the key without an app release, rate-limit abuse, nothing extractable from the IPA — and is the recorded upgrade path, not phase 1. |
| One transport for both sides, installing `ring` **explicitly** | The agent already enables both rustls providers, so crate-feature resolution panics there. Measured in step 4. An explicit install makes one shared adapter safe everywhere and costs a `Once`. |
| **Client first, agent second** | The agent has no CI release path yet, so nothing would inject its key. The shared crates land with the client; the agent's wiring follows. |
| Section is named `[usage_reporting]`, not `[telemetry]` | "Telemetry" is the entire product. A `[telemetry]` block in a telemetry agent's config means the wrong thing. |
| **No on-disk buffer.** Drop on failure | Diagnostics, not billing. A retry queue is a durability promise we do not need and a privacy surface we do not want. |

## What leaves the device

This is the load-bearing part. Traces pull toward useful attributes, and the useful attributes here
are the user's private infrastructure. A shared backend must never see:

- `backendBaseURL` / `export.endpoint` — the user's infra topology
- host names, `resource.attributes`, filesystem mount points
- alert rule contents (thresholds reveal capacity; names reveal services)
- `CoreError::Backend(String)` bodies and any HTTP response body
- API keys, obviously, and anything derived from them

What may leave, and nothing else:

| Attribute | Type | Example |
|---|---|---|
| `pessimal.app.version` | version string from the bundle | `0.1.0` |
| `pessimal.app.build` | integer | `26` |
| `pessimal.platform` | enum | `ios` \| `macos` \| `linux` \| `windows` |
| `pessimal.os.version` | major.minor only | `18.4` |
| `pessimal.device.class` | enum | `phone` \| `tablet` \| `mac` \| `server` |
| `pessimal.backend.kind` | enum, the preset — not the URL | `signoz` |
| `pessimal.hosts.count` | integer | `12` |
| `pessimal.rules.count` | integer | `3` |
| `pessimal.alerts.firing` | integer | `1` |
| `pessimal.outcome` | enum | `ok` \| `partial` \| `unreachable` \| `unauthorized` \| `backend_error` \| `decode_error` |
| `pessimal.http.status_class` | integer | `2`, `4`, `5` |
| span duration | implicit | |

Enforced by giving the pure crate typed span builders whose setters take those enums and integers.
There is no `attribute(key: &str, value: String)` on the public surface. A leak then requires
changing the crate's API, which a reviewer will see.

## Consent

Three structural requirements, none of which is "check a boolean before sending":

1. **The sink does not exist until consent says so.** `VendorSink::new` is fallible on consent;
   nothing holds a transport that could fire before the flag is read. Not a guard at the send site.
2. **Opt-out survives a settings reset.** `SettingsStore.removeAll()` exists for "reset connection".
   If the opt-out lived there, resetting the backend URL would silently re-enable reporting. It gets
   its own key and its own lifecycle, deliberately outside `removeAll()`.
3. **The agent respects the conventions its users already have.** `DO_NOT_TRACK=1` disables.
   `CI=true` disables — nobody wants their build matrix in our span volume.

Client surface: a "Help improve Pessimal" toggle in Settings on both apps, with a disclosure
listing the table above verbatim, and a `PrivacyInfo.xcprivacy` declaring diagnostics data, not
linked to identity, not used for tracking. Opt-out means the toggle starts on; absence of the key
means *enabled*, which inverts `SettingsStore`'s "nil = not configured" convention and so must be
documented at the property.

### Install identity

A stable per-install UUID is a persistent pseudonymous identifier, which is the part of opt-out
reporting that actually carries regulatory weight. **Decision: session-scoped.** `service.instance.id`
is a UUIDv7 minted at launch and never persisted. Traces from one run correlate; two runs do not.
That costs us retention and install counts, and buys not shipping a durable identifier by default.
If install counts turn out to be necessary, the upgrade is an install-scoped id **rotated whenever
the user opts out and back in** — recorded here so the choice is deliberate either way. Never IDFV.

## Shape

```
common/pessimal_usage/            pure: span types, allowlisted builders, consent policy,
                                  resource attrs, OTLP/JSON encoder. serde + uuid + chrono.
                                  no reqwest, no tokio.
common/pessimal_usage_otlp/       adapter: POST /v1/traces, reqwest 0.13 + rustls/ring, with an
                                  explicit provider install. One transport, both sides — see the
                                  spike findings below for why the install is not optional.
```

Both under `common/` because both sides use them, per CLAUDE.md's rule about what belongs where.

Client spans, one trace per poll cycle:

```
pessimal.client.poll                 root; outcome, hosts.count, alerts.firing
├── pessimal.client.plan             pure, microseconds, mostly proves the trace is complete
├── pessimal.client.gather           one child per backend request, from pessimal_query_signoz
│   └── http status_class, duration
└── pessimal.client.fold             outcome
```

Emission is `tokio::spawn`ed from the FFI session — the poll path never awaits an export. Flush on
`scenePhase` `.background`, awaited with a ~2s cap inside a `beginBackgroundTask`. Nothing is sent
while backgrounded.

## Step 0 — Spike: findings

Run 2026-09-11. Five questions, each of which would have wasted the implementation if answered late.

### 1. The dev collector had no traces pipeline

`dev/otelcol/config.yaml` was metrics-only. Added; both pipelines now wired.

Two environment facts fell out of this, both worth knowing before the next person tries:

- **Docker cannot run on this machine.** It is itself a Tart guest (`hw.model = VirtualMac2,1`,
  `Apple M4 (Virtual)`), and Apple Silicon has no nested virtualisation, so colima's `vz` driver
  fails with "Virtualization is not available on this hardware". Use the native
  `otelcol-contrib_*_darwin_arm64` binary instead, which is faster anyway and sidesteps the
  scratchpad bind-mount problem as well.
- **Rust tests have no network egress here; Python does.** A `reqwest` call to a public host times
  out where `urllib` gets a 200. So live verification runs through
  [`scripts/otlp-trace-probe.py`](../../scripts/otlp-trace-probe.py), and the `live_*` Rust tests
  stay CI-only.

### 2. The spec-correct JSON shape is accepted — and the deviations behave unlike the folklore

`scripts/otlp-trace-probe.py` posts a hand-built root-plus-child trace. The collector logged it
whole: hex ids parsed, parent linked to child, `intValue` attributes arriving as `Int` and not
`Str`, both span kinds and `status.code` correct.

The negative controls are the useful part, because the received wisdom is that OTLP/JSON mistakes
are silent. Measured against otelcol-contrib 0.160.0:

| deviation | result |
|---|---|
| base64 ids, i.e. what plain ProtoJSON emits for a `bytes` field | **400**, `ID.UnmarshalJSONIter: length mismatch` — loud |
| int64 as a JSON number rather than a decimal string | accepted, **and exact**: `…123456789` ns round-tripped intact, so jsoniter reads int64 without float64 rounding |
| all-zero `parentSpanId` on a root span | accepted, normalised to no parent |

We emit the spec shape regardless: the leniency belongs to this receiver, not to OTLP, and the next
backend owes us nothing. But the claim in the probe's own docstring has been corrected to what was
measured rather than what was assumed.

### 3. Which SigNoz header — **answered, from the sibling projects**

Not by asking, and not by guessing: `signoz-ingestion-key`, which kumbaya and phil-connors both send
to the same SigNoz, from their Rust servers *and* their Swift clients. Four independent places agree.
The endpoint is `https://ingest.eu2.signoz.cloud`.

This is **not** the `signoz-access-token` that `pessimal_agent_core`'s preset sends. That one goes to
the *operator's* collector — a different endpoint with a different credential — and confusing the two
would be a silent non-delivery. `the_ingestion_header_is_not_the_operators_access_token` asserts the
distinction so it cannot quietly drift.

What the siblings cannot answer is whether a span from *this* code becomes queryable in our SigNoz.
That stays open until a release build runs, and the check is the trace id appearing in the Traces UI
rather than the HTTP status.

### 4. There is no two-reqwest-major problem. There is a provider problem, and it predates this work

`opentelemetry-otlp` 0.32 **already uses reqwest 0.13**, so the feared 0.12/0.13 coexistence does
not exist: 0.13 is the only version in `Cargo.lock`. The `reqwest = { version = "0.12", … }` line in
`[workspace.dependencies]` is dead — no crate references it.

What is real, and is a latent hazard already shipped in the agent: **the agent's `rustls` is compiled
with both provider features at once.** From the real binary's build graph:

```
rustls  ['aws-lc-rs', 'aws_lc_rs', 'log', 'logging', 'ring', 'std', 'tls12']
```

Both come from one line — the workspace's `opentelemetry-otlp` features carry `reqwest-rustls`
(which pulls reqwest's default provider, `aws-lc-rs`) *and* `tls-ring`. With two providers enabled,
rustls 0.23's `from_crate_features()` returns `None` and the generic builder panics:

> Could not automatically determine the process-level CryptoProvider from Rustls crate features.

Measured, not inferred: a throwaway `[[bin]]` inside `pessimal_agent_host` — so the agent's exact
dependency closure, with no dev-dependencies perturbing the feature union — panics on
`rustls::ClientConfig::builder()`, and resolves cleanly once `ring` is installed explicitly first.

The agent does not panic in practice *today* because both of its TLS paths name a provider
explicitly (tonic via `tls-ring`, reqwest via `__rustls-aws-lc-rs`). Nothing reaches the generic
builder. That is luck, not design, and it is exactly the luck `rustls-no-provider` would spend.

**Consequence for this milestone:** one shared transport serves both sides, so the encoder and the
transport stay single-implementation — but `pessimal_usage_otlp` **must install the `ring` provider
explicitly through a `Once`**, the way `pessimal_query_signoz::install_crypto_provider` already
does, rather than relying on crate features resolving. On the client that install is redundant; in
the agent it is what stops a panic. Tightening the agent's own feature set — dropping one of
`reqwest-rustls` / `tls-ring` — is a separate change with its own blast radius, noted as a follow-up
rather than smuggled in here.

### 5. Injection: the siblings' mechanism, not a new one

`rustc_env_files` + `option_env!` was verified to work under `rules_rust` — values reach
`option_env!`, an absent name gives `None`, an empty file also gives `None` rather than `Some("")`,
and a changed value invalidates the cached action. It is **not** what we use, because both sibling
projects already solve this and their way is simpler:

```ruby
# fastlane, guarded so a local build passes no flag at all
signoz_key = ENV["SIGNOZ_OTLP_INGESTION_KEY"].to_s
bazel_args << "--action_env=SIGNOZ_OTLP_INGESTION_KEY=#{signoz_key}" unless signoz_key.empty?
```

A `genrule` then expands it into a plist merged into the bundle — `<key>SIGNOZ_OTLP_INGESTION_KEY</key>`
with `$${SIGNOZ_OTLP_INGESTION_KEY:-}` as its string value — Swift reads it from
`Bundle.main.infoDictionary`, and passes it across FFI to `Destination::from_bundle`.

The credential never enters a Rust compile unit, so there is no build-time env plumbing, nothing to
gitignore, and no tracked file that one `git add -A` could turn into a published key. `$${VAR:-}`
expands to an empty string when unset, which `a_bundle_with_unset_values_yields_no_destination` pins
as "no destination" rather than "a destination that fails every send" — so the `--config=ci`
simulator build cannot phone home by construction.

The agent keeps `option_env!` via `Destination::from_build`, because a daemon has no bundle to read a
plist from. Same type, same validation, two routes in.

The env var names are the Doppler secret names — `SIGNOZ_OTLP_ENDPOINT`,
`SIGNOZ_OTLP_INGESTION_KEY` — rather than `PESSIMAL_`-prefixed, so the release script is a
pass-through with no renaming step to get wrong.

### What we deliberately do *not* copy from the siblings

Their tracers send `device.id` as a stable per-install identifier, `user.id`, and
`exception.message` / `status.message` from caught errors. For kumbaya and phil-connors those are
their own users and their own errors. For Pessimal the equivalents are the *operator's*
infrastructure — a backend error body names their collector, their hosts, their queries — so the
allowlist keeps them out and the encoder emits no `status.message` at all. Same wire format, stricter
payload.

Their client tracing is also in Swift, which is right for them: their app logic is Swift, so that is
where the spans are. Pessimal's client logic is Rust — `plan`, `gather`, `fold`, and the backend's
HTTP status all happen below the FFI boundary — so a Swift tracer would see only the outermost call.
Hence the encoder lives in Rust, where it is also shared with the agent. The wire format is
independently corroborated: their `OTLPExporter.encodeSpan` and our `encode::encode_span` agree on
every detail that matters, having been written from the spec separately.

Their batching is worth copying outright: a buffer of 20 spans with a 5-second debounce, flushed
early when full, fire-and-forget. That lands in the FFI session rather than in the sink.

## Verification ladder

- Encoder unit tests whose expectations come from **what a real receiver accepted**, not from the
  encoder. Done differently, and better, than originally planned: rather than pasting a fixture, the
  `emit_trace` example pipes the encoder's own output into `scripts/otlp-trace-probe.py`, and the
  collector's debug exporter says what arrived. A fixture generated by the code under test cannot
  find a bug in it; a live receiver can. The cost is that this leg is a command, not a checked-in
  test — `cargo run -q -p pessimal_usage --example emit_trace | scripts/otlp-trace-probe.py --stdin
  http://localhost:4318` — so it belongs in the export smoke script rather than in `cargo test`.
- An allowlist test that constructs every span type and asserts the serialised attribute key set is
  a subset of the table above. This is the test that stops the leak regressing.
- A consent test: no transport is constructed and no request is attempted when consent is denied,
  `DO_NOT_TRACK=1` is set, or `CI=true` is set.
- `swift-smoke.sh` gains the export path pointed at `127.0.0.1:9`: reporting fails as *data*, never
  hangs, never throws, and the poll result is byte-identical with and without it.
- `bazelisk build --config=ci` still links; the uniffi symbol check still passes; the macOS bundle
  script still builds, since it links the FFI staticlib that now pulls the new crates.
- Live: a span from a real device visible in our SigNoz, with the attribute set matching the table
  and nothing else.

## Phases

**Phase 1 — client.** Spike and the two crates are done. Remaining: session instrumentation and
batching, FFI records, Swift consent + UI + privacy manifest, and the `--action_env` → `genrule` →
plist → `infoDictionary` → FFI chain in the Buildkite iOS release step. Ends with a span from
TestFlight visible in our SigNoz.

**Phase 2 — agent.** `[usage_reporting]` config, `DO_NOT_TRACK` / `CI`, agent-side spans (startup,
export cycle outcome, collection failure kind). Blocked on an agent release path existing, since
that is what would inject the key.

## Follow-ups, deliberately not in phase 1

- Ingestion proxy in front of SigNoz; key rotation without a release.
- A heartbeat metric, if spans do not answer the install-count question.
- `NSPrivacyAccessedAPITypes` if later work touches `systemUptime`, `UserDefaults` or file
  timestamps. TestFlight accepted 0.1.0 without a manifest; App Store review will not.
- Path-filtered dynamic pipeline upload, so a docs push stops queueing for the single macOS slot.
