# Alert evidence is gated at max_staleness, which does not budget for backend lag

`observe_alerts` passes `max_staleness` (150s at defaults) to
`AlertEvaluation::observe`, which discards a sample older than that. The measured
SigNoz Cloud ingestion lag plus bucket quantisation is ~118s, so the margin is
about thirty seconds.

This is the same defect class that
[`backend-ingestion-lag-breaks-liveness.md`](../docs/solutions/backend-ingestion-lag-breaks-liveness.md)
fixed for liveness, and after that fix it is now the *tighter* of the two bounds:
with a 3-minute lag allowance, a host whose backend is running 160s behind reads
**Alive** while every one of its alerts reads **NoData**. A fleet that looks healthy
and silently stops alerting is worse than one that looks stale.

Not fixed with the liveness change on purpose. `observe` seeds `breaching_since`
from a point's own timestamp, so simply widening `overview_window` would hand it
older points and re-seed dwell earlier — a behaviour change well outside that
defect, and one that could make an alert fire for a breach that had already ended.

**What to work out:** whether `max_staleness` should itself include
`backend_lag_allowance`, or whether the evidence gate should compare against
`now - backend_lag_allowance` the way liveness now does. The second is probably
right, by the same argument: the gate's job is to reject samples nobody is
refreshing, and "nobody is refreshing it" has to be measured from the latest
instant the backend could have answered for.

Either way the interlock `3 * metric_step <= max_staleness <= down_threshold` needs
revisiting, since both sides of it shift.
