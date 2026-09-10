# RESOLVED 2026-09-10 — see below for what the fix actually had to be

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


---

## Resolved, and the recommendation above was wrong in one important way

Fixed by `PollTuning::evidence_horizon() = max_staleness + backend_lag_allowance`, which is the
*substance* of candidate (b): `now - p.at <= max_staleness + lag` and
`(now - lag) - p.at <= max_staleness` are the same inequality. The second explains it; the first is
the one that is safe to implement.

**The literal form this note recommended would have introduced a second bug.** It said to measure the
gate from `now - backend_lag_allowance` "the way liveness now does" — i.e. pass the shifted instant to
`AlertEvaluation::observe`. But that one argument does three jobs, and only the gate wants the shift:

1. the gate's reference instant;
2. the **ceiling on admissible samples** — `MetricSeries::latest_at(now)` discards anything newer;
3. the **anchor for dwell** — `now - since >= for_duration`.

Backdating it would therefore have discarded the freshest sample, which is precisely the sample the
fix exists to admit; judged on one a whole allowance older; delayed every fire by the allowance; and,
on a loopback collector where the lag is genuinely zero but the allowance is not, read `NoData` off
perfect data.

Liveness could take the shifted instant safely only because `LivenessPolicy::evaluate` is monotone in
it: a heartbeat newer than the instant reads Alive rather than being discarded. `observe` is not
monotone in its instant. The asymmetry is the whole lesson.

## The consequence that had to be handled too

Widening the gate without widening `overview_window` would have made the *window* the real evidence
bound for the fleet-wide tier while the focused host's wider detail window left the gate binding —
two hosts with identical data reaching different verdicts depending on which one the user happened to
have open. `overview_window()` now derives from the horizon, and the gate is deliberately the
narrower of the two, so nothing older than the horizon can ever seed `breaching_since`.

## Interlocks

`3 * metric_step <= max_staleness <= down_threshold` is unchanged, and that is a conclusion rather
than an oversight: the allowance lands on both sides of each half and cancels. The ceiling still
orders the two effective bounds (evidence expires `max_staleness + lag` after the last sample;
liveness gives up `down_threshold + lag` after the last heartbeat), which at preset coincide exactly —
pinned by a test sweep asserting `liveness == Down` iff `evidence == NoData`. The floor stays
necessary because the allowance may legitimately be zero on a loopback collector, where it is the
only thing keeping bucket quantisation from reading as NoData.
