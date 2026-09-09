# A backend's ingestion lag makes healthy hosts read as stale

**Measured:** 2026-09-09 against SigNoz Cloud (eu2).

## What was measured

With the agent exporting every 5 seconds and still running, the newest *queryable* heartbeat was
**88 seconds behind wall clock**. Part of that is bucket quantisation — a 30-second bucket is stamped
at its start — but most of it is the backend's own ingestion-to-queryable delay.

That number is not a SigNoz defect. Every columnar metrics backend batches on write. It is a property
of the read path that Pessimal has to budget for.

## Why it matters

Two things break, and both were invisible against a local collector, which has effectively zero lag.

**Liveness misjudges a healthy host.** `LivenessPolicy::default()` is a 30-second beat, stale at 3
intervals (90s) and down at 5 (150s). A host beating perfectly has a *measured* age of roughly
`ingestion_lag + quantisation` ≈ 88s + up to 30s, which is already past the stale threshold and
approaching down. The earlier analysis budgeted one interval for quantisation and nothing for the
backend, so the margin it reasoned about does not exist.

**The roster comes back empty.** `plan_poll`'s host window is derived from the down threshold, so it
is ~150s wide ending at `now`. With 88s of lag the newest data sits near the far edge of that window
and intermittently outside it: `list_hosts` returns nothing and the fleet reads as absent, not as
stale. Observed directly — a 30-minute window found the host, the production window found nothing,
with the same data.

## The shape of the fix

The measurement path has a known, roughly constant delay, so say so rather than pretending `now` is
observable:

1. A configurable `backend_lag_allowance` on `PollTuning`, defaulting generously (the measured 88s
   wants real headroom, not a tight fit).
2. The roster and chart windows widen by it, so the newest data is comfortably inside.
3. Liveness is judged as of `now - backend_lag_allowance` — the latest instant the backend could
   have told us about — rather than as of `now`.

Point 3 is the conceptual correction. `LivenessPolicy` is right to compare a heartbeat against an
instant; the bug was handing it an instant the backend cannot yet have data for.

## The lesson worth keeping

A collector on loopback exercises neither the certificate path, nor the auth path, nor the lag. All
three bugs found on the first contact with a real backend were in behaviour that a local collector
makes look perfect. Verify against something you did not write.
