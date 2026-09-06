# Pessimal Handoff

**Updated:** 2026-09-06

## Current state

- **M0 scaffold** — done. Cargo workspace across `common/`, `agents/`, `clients/`, `tools/`.
  MIT, conventional commits via `cog`.
- **M1 domain core** — done. `pessimal_core` covers metric identity, instrument kinds, time ranges
  and series, liveness, alert evaluation, URNs, and ports.
- **M2 host agent** — done. `pessimal_agent_core` (config, backend presets, cached collection,
  resource identity, OTLP export) and `pessimal_agent_host` (sysinfo collector, CLI, run loop).
  Verified end to end against a real OpenTelemetry collector over **both** gRPC and HTTP/protobuf:
  12 metrics arrive with the right names, units, and instrument types.
- 152 tests, `clippy -D warnings` clean, `cargo fmt --check` clean.
- **M3 SigNoz adapter** — `pessimal_query_signoz` implements `TelemetryQuery` against
  `/api/v5/query_range`. Response types taken from SigNoz's own Go source, not guessed; the naming
  convention (dotted for v0.88+/Cloud, underscored before) is a setting. 37 tests including
  HTTP-level ones against a mock server. **Never run against a live SigNoz** — structure is
  authoritative, behaviour is not.
- `pessimal_client_core` and everything from M4 on are unstarted; `clients/` still holds
  placeholders for them.

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
- **SigNoz does not document its query response body.** Take it from
  `pkg/types/querybuildertypes/querybuildertypesv5/resp.go` in their repo. Three details a guess
  gets wrong: `labels[].key` is an object (name at `.key.name`), `value` arrives as the *string*
  `"NaN"`/`"Inf"`/`"-Inf"` for non-finite numbers despite being typed `float64`, and `partial`
  buckets must be dropped or a stale host reads as alive.
- **Do not trust `docker logs` after `docker restart`** when verifying an export: the previous
  run's lines are still there and will happily convince you a broken path works. Recreate the
  container.

## Next up

Confirm the SigNoz adapter against a live instance with one `query_range` call, then
`pessimal_client_core`: polling orchestration, liveness, and alert evaluation across a fleet.
