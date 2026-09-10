# Future: poll agents directly over Tailscale

**Deferred, deliberately, 2026-09-10.** Not now; recorded because the owner called it a good future
improvement and it would be a shame to rediscover it.

## The idea

Today the clients poll the telemetry backend, which is what was chosen when the question was put:
agents sit on private hosts a phone cannot reach, the backend already can. The repository
description, though, says *"personal infrastructure monitoring over Tailscale"* — and on a tailnet
the original objection evaporates. Every agent is reachable from every client, by stable name,
authenticated at the network layer.

That buys three things the current design cannot:

- **No backend dependency for liveness.** The ~88s ingestion lag that forced
  `backend_lag_allowance` is a property of the read path through SigNoz. Asking an agent directly is
  immediate, so liveness would be accurate to the round trip rather than to the lag.
- **No backend credential on the device.** The API key on the phone exists only because the phone
  talks to SigNoz.
- **It still works when the backend is down** — arguably when you need it most.

## Why it is a second implementation, not a rewrite

`pessimal_core::TelemetryQuery` is the port, and `pessimal_query_signoz` is one implementation of it.
A direct-to-agent implementation satisfies the same trait, and everything above it —
`plan_poll`, `execute_plan`, `FleetState::apply`, the FFI, both apps — is unchanged. That is the
shape the hexagon was for.

What it does need on the agent side: a small HTTP endpoint serving the current snapshot. The agent
already holds exactly that in `CachedCollector`, so it is an exposure rather than new collection.

## What to think about before starting

- **Who is the source of truth** when both paths are configured? Probably direct for liveness and
  the backend for history, which is a per-metric answer rather than a global one.
- **`backend_lag_allowance` becomes per-source.** Zero for a direct poll; the measured lag for the
  backend. `PollTuning` currently has one value.
- **Fan-out.** Polling the backend is one request for the whole fleet; polling agents is one per
  host, on a phone radio. The plan stage would need to care about that.
- Serving metrics over the tailnet is still an open port. Tailscale ACLs are the answer, but it
  should be a decision rather than a default.
