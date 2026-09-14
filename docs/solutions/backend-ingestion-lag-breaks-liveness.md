# A backend's ingestion lag makes healthy hosts look stale

**Measured:** 2026-09-09, against SigNoz Cloud (eu2).

## The measurement

The agent exported every 5 seconds and continued to operate. The newest heartbeat that a query could
find was **88 seconds behind the clock**. Bucket quantisation causes part of this delay, because SigNoz
gives a 30-second bucket the time at its start. The backend causes most of the delay. It needs time
before a query can find new data.

This delay is not a SigNoz defect. All columnar metrics backends write data in batches. The delay is a
property of the read path, and Pessimal must include it in its calculations.

## Why this is important

Two things go wrong. A local collector does not show either problem, because its lag is almost zero.

**Liveness gives the wrong result for a healthy host.** `LivenessPolicy::default()` sets a 30-second
beat. A host becomes stale after 3 intervals (90 s) and down after 5 intervals (150 s). A host that
sends each heartbeat correctly has a *measured* age of about `ingestion_lag + quantisation`. That is
88 s plus a maximum of 30 s. This age is already more than the stale threshold, and it is near the down
threshold.

The first analysis gave one interval to quantisation and zero to the backend. Thus, the
safety margin in that analysis did not exist.

**The roster is empty.** `plan_poll` calculates its host window from the down threshold. The window is
about 150 s wide, and it ends at `now`. With 88 s of lag, the newest data is near the far edge of the
window, and sometimes outside it. Then `list_hosts` returns no hosts, and the fleet shows as absent, not
as stale.

We saw this directly. With the same data, a 30-minute window found the host, and the
production window found nothing.

## The fix

The measurement path has a known delay that is almost constant. Pessimal must include this delay. It
must not act as if a query can see the data at `now`.

1. `PollTuning` has a `backend_lag_allowance` that you can configure. Its default is large, because
   the measured 88 s needs a good margin.
2. The roster window and the chart windows increase by this allowance. Thus, the newest data is well
   inside them.
3. Liveness uses `now - backend_lag_allowance` as its reference instant, not `now`. This is the latest
   instant for which the backend can have data.

Item 3 is the important correction. `LivenessPolicy` is correct when it compares a heartbeat to an
instant. The bug was the instant that Pessimal gave to it: the backend cannot have data for that
instant yet.

## The lesson

A collector on loopback does not test the certificate path, the authentication path or the lag. The
first contact with a real backend found three bugs. All three bugs were in behaviour that a local
collector makes look correct. Always verify against a system that you did not write.
