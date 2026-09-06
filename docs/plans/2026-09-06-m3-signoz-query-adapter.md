# M3 — SigNoz query adapter

**Created:** 2026-09-06
**Status:** Blocked on a real SigNoz to read from

## Goal

`pessimal_query_signoz` implements `pessimal_core::TelemetryQuery` against SigNoz, so the clients can
read back what the agents export. First of the query adapters; Honeycomb and ClickStack follow the
same port in M7.

## What is established

The request contract is documented and unambiguous.

- **Endpoint:** `POST {base}/api/v5/query_range`
- **Auth:** header `SIGNOZ-API-KEY: {key}`, from a service account under Settings → Service Accounts.
  Creating one needs the `Admin` role.
- **Request body** (verified against the published examples):

  ```json
  {
    "start": 1742602572000,
    "end": 1742604372000,
    "requestType": "time_series",
    "compositeQuery": {
      "queries": [{
        "type": "builder_query",
        "spec": {
          "name": "A",
          "signal": "metrics",
          "stepInterval": 60,
          "aggregations": [{
            "metricName": "system.cpu.utilization",
            "temporality": "Unspecified",
            "timeAggregation": "avg",
            "spaceAggregation": "sum"
          }],
          "filter": { "expression": "host.name IN ['web-1', 'db-1']" },
          "groupBy": [{ "name": "host.name" }],
          "disabled": false
        }
      }]
    }
  }
  ```

  `start`/`end` are epoch **milliseconds**; `stepInterval` is **seconds**. `filter.expression` is
  SigNoz's own expression language, not PromQL.

- **Metric naming depends on the instance's version.** Before v0.88 SigNoz normalised names to
  underscores (`system_memory_usage`, `host_name`); v0.88 migrated to dot-based names aligned with
  the OpenTelemetry semantic conventions (`system.memory.usage`, `host.name`). The v5 API examples
  all use dots. Attribute keys moved too, not just metric names.

  This is a real fork in the adapter, not a detail: on a pre-0.88 instance every name Pessimal
  queries by is wrong. Whichever way it goes, `MetricKind::otel_name()` is not automatically the
  query key, so the adapter needs a naming strategy rather than passing the semconv name straight
  through.

## What is not established

**The response body shape is not in the documentation.** Field names for series, labels, points, and
timestamps are unknown, and that is precisely the part a parser is made of. Building it from
recall would encode guesses as fixtures and the tests would then defend the guesses.

## Decisions taken

| Decision | Rationale |
|---|---|
| Naming is a strategy, not a constant | Pre/post-0.88 instances disagree, and Honeycomb and ClickStack will disagree again. The adapter maps `MetricKind` to a backend name rather than assuming semconv passes through. |
| Fixtures come from a real instance | Wiremock fixtures written from recall test the fiction, not the backend. |
| The adapter picks its own reqwest TLS features | The agent inherits `aws-lc-rs` from reqwest's default rustls provider. The clients must not: it is the usual source of iOS cross-compilation trouble, and the two are separate build graphs. |

## Next step

Get real `query_range` responses. In preference order:

1. **Stand up SigNoz locally in Docker**, point the agent at it, and capture genuine responses for
   the metrics Pessimal actually emits. No credentials involved, reproducible, and the same compose
   file can later back an integration test.
2. A read-only service-account key for the owner's existing instance.
3. The owner runs one `curl` and pastes the response.

Option 1 is the default unless the owner says otherwise.
