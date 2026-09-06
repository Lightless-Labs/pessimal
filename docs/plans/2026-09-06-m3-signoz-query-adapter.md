# M3 — SigNoz query adapter

**Created:** 2026-09-06
**Status:** Built and tested at the HTTP level. Not yet run against a live instance.
**Updated:** 2026-09-06

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

## The response shape, and how it was obtained

The prose documentation does not publish the response body. Rather than guess it — or spend five
containers and several gigabytes standing up a local SigNoz to observe it — it was taken from
SigNoz's own Go types, which are authoritative:
`pkg/types/querybuildertypes/querybuildertypesv5/resp.go`.

```
QueryRangeResponse
  data.results[]            -> TimeSeriesData
    queryName
    aggregations[]          -> AggregationBucket
      index, alias, meta.unit
      series[]              -> TimeSeries
        labels[]            -> {key: {name, signal, fieldContext, fieldDataType}, value: any}
        values[]            -> {timestamp: int64 ms, value: float64, partial: bool}
```

Three things there that a reasonable guess gets wrong:

1. **`labels[].key` is an object, not a string.** The name is at `labels[].key.name`.
2. **`value` is not reliably a number.** It is declared `float64` in Go, but every value passes
   through a sanitiser that renders non-finite numbers as the *strings* `"NaN"`, `"Inf"`, `"-Inf"`.
   Deserialising straight into `f64` fails exactly when a host has a gap in its data — which is
   when a monitoring client most needs to keep working.
3. **`partial` datapoints must be dropped.** SigNoz's own comment says a partial bucket does not
   cover its whole step and "should be ignored". The first bucket of any window usually is one.
   Charting them puts a false dip at the edge of every window; worse, letting one through in
   `list_hosts` reports a heartbeat later than the one actually recorded, which makes a stale host
   look alive.

Aggregation and temporality enum values come from `pkg/types/metrictypes/metrictypes.go`:
`latest | sum | avg | min | max | count | count_distinct | rate | increase` for time, and
`sum | avg | min | max | count` for space.

## What is still unverified

The adapter has never spoken to a real SigNoz. The *structure* is authoritative, but the
*behaviour* is not: which labels are actually populated, how an unknown metric name is reported,
and whether Cloud differs from self-hosted in any of it. One `query_range` call against the owner's
Cloud instance would settle all three.

## Decisions taken

| Decision | Rationale |
|---|---|
| Naming is a strategy, not a constant | Pre/post-0.88 instances disagree, and Honeycomb and ClickStack will disagree again. The adapter maps `MetricKind` to a backend name rather than assuming semconv passes through. |
| Fixtures come from a real instance | Wiremock fixtures written from recall test the fiction, not the backend. |
| The adapter picks its own reqwest TLS features | The agent inherits `aws-lc-rs` from reqwest's default rustls provider. The clients must not: it is the usual source of iOS cross-compilation trouble, and the two are separate build graphs. |

## Why not a local SigNoz

The plan was to stand one up and capture real responses. On inspection that costs five containers
(ClickHouse, ClickHouse Keeper, Postgres, the collector, and SigNoz itself), 4GB of RAM, a
piped-shell `foundryctl` install, and UI-driven account creation — all to learn a schema that the
source code states outright. The source was the better answer, and cheaper by several gigabytes on
a machine with 44GB free.

## Next step

Confirm against the owner's SigNoz Cloud instance with a single request. Everything else is done.
