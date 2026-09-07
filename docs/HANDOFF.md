# Pessimal Handoff

**Updated:** 2026-09-07

## Current state

- **M0 scaffold** — done. Cargo workspace across `common/`, `agents/`, `clients/`, `tools/`.
  MIT, conventional commits via `cog`.
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
- M4 onward (the FFI bridge, Bazel, the two apps) is unstarted; `clients/ffi` and `clients/apple`
  still hold placeholders.

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

M4, the UniFFI bridge. Two things to do first, both recorded:

1. Read [`todos/bazel-toolchain-must-provide-rust-1-95.md`](../todos/bazel-toolchain-must-provide-rust-1-95.md)
   before writing `MODULE.bazel`. MSRV is 1.95 and the sibling projects pin a `rules_rust` that
   predates it.
2. Confirm the SigNoz adapter against a live instance with one `query_range` call — see the open
   questions in [`plans/2026-09-06-m3-signoz-query-adapter.md`](plans/2026-09-06-m3-signoz-query-adapter.md).

One thing the client core cannot check and M4 must not forget: nothing detects an unwired
`SignozConfig::for_policy`, and the symptom is a healthy fleet silently reading stale or down with
no error anywhere. Build the config from the policy, never alongside it.
