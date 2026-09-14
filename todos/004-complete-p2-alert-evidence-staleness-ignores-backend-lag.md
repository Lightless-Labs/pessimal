---
status: complete
priority: p2
issue_id: "004"
tags: [alerts, liveness, backend-lag, signoz, client-core]
dependencies: []
---

# Alert evidence is gated at max_staleness, which does not budget for backend lag

## Problem Statement

`AlertEvaluation::observe` discards a sample older than the bound it is given. Before commit `0521efb`,
`observe_alerts` gave it `max_staleness`, which is 150s at defaults. That bound had no room for backend
ingestion lag.

The alert gate had the same defect that liveness had. The liveness fix in
`docs/solutions/backend-ingestion-lag-breaks-liveness.md` added `backend_lag_allowance`, which is 180s
(3 minutes) at preset. After that fix, the alert gate was the tighter of the two bounds. A host
whose backend ran 160s behind read Alive while every one of its alerts read NoData. A fleet that looks
healthy and has stopped alerting is worse than one that looks stale.

It was not fixed with the liveness change, on purpose. `observe` seeds `breaching_since` from a point's
own timestamp. Widening `overview_window` alone would give it older points and seed dwell earlier. That
could make an alert fire for a breach that had already ended.

## Findings

- The measured ingestion lag is 88s. Against SigNoz Cloud, on a host that exports every 5s, the newest
  queryable heartbeat was 88s behind the clock. SigNoz stamps a 30-second bucket at its start, which adds
  up to 30s, so a healthy host's newest sample is about 118s old.
- The gate is now `PollTuning::evidence_horizon() = max_staleness + backend_lag_allowance`
  (`config.rs:372`). At preset that is 330s (150s + 180s), about 212s above that ~118s age.
- `observe_alerts` passes the horizon to `AlertEvaluation::observe` and keeps `now` as the real instant
  (`fold.rs:789`, `fold.rs:814`).
- The gate uses the same inequality as Option B, written in a different form.
  `now - p.at <= max_staleness + lag` and `(now - lag) - p.at <= max_staleness` are the same inequality.
  The second one explains the gate: it rejects samples nobody is refreshing, and that has to be measured
  from the latest instant the backend could have answered for. The first one is the one that is safe to
  implement.
- Passing `now - backend_lag_allowance` to `observe` would have been wrong. Its `now` has three uses, and
  only the first needs the shift:
  1. the reference instant for the gate;
  2. the ceiling on admissible samples: `MetricSeries::latest_at(now)` discards anything newer;
  3. the anchor for dwell: `now - since >= for_duration`.

  A shifted instant would discard the freshest sample, which is the sample the fix has to admit. It would
  judge on a sample a whole allowance older. It would delay every fire by the allowance. On a loopback
  collector, where the lag is zero but the allowance is not, it would read NoData from good data. The
  `evidence_horizon()` doc comment gives all four results
  (`clients/common/pessimal_client_core/src/config.rs:358-364`). The `observe` doc comment gives the
  three uses of `now`, the lost freshest sample and the delayed fires
  (`common/pessimal_core/src/alert.rs:282-286`).
- Liveness can take the shifted instant because `LivenessPolicy::evaluate` is monotone in it: a heartbeat
  newer than the instant reads Alive and is not discarded. `observe` is not monotone in its instant.
- `overview_window()` is now `evidence_horizon() + 2 * metric_step`, 390s at preset (`config.rs:393`).
  Without this, `overview_window` would be the real evidence bound for the fleet-wide tier, while the
  focused host's wider `detail_window` would leave the gate as the bound. Two hosts with the same data
  would get different verdicts, depending on which one the user had open. The gate is narrower than both
  query windows, so no sample older than the horizon can seed `breaching_since`.
- The view's `latest_value` uses the same gate (`fold.rs:816-823`), so it is `None` whenever the alert is
  NoData.
- `reset_after_blind_gap` still uses the raw `max_staleness`, on purpose. It measures the app's own
  lateness against an instant the app chose, and no backend is in that comparison.
- The interlock `3 * metric_step <= max_staleness <= down_threshold` is unchanged, on purpose. The
  allowance is added to both sides of each half and cancels (`config.rs:518-545`).
  - The upper half, `max_staleness <= down_threshold`, still makes evidence expire no later than
    liveness gives up. Evidence expires `max_staleness + lag` after the last sample. Liveness gives up
    `down_threshold + lag` after the last heartbeat. At preset the two are equal.
  - The lower half, `3 * metric_step <= max_staleness`, is still needed. The allowance can be zero on a
    loopback collector. Then the lower half is the only thing that stops bucket quantisation from reading
    as NoData.
- The default dwell for new rules in both apps is now 600s, not 300s. With a 330s horizon, a 300s dwell
  gets the `SpikeCanFire` warning on every new rule (`config.rs:754-755`).
- `docs/HANDOFF.md:476-477` marks the item done in commit `0521efb`.

## Proposed Solutions

### Option A: Put the lag allowance into `max_staleness`

Raise `max_staleness` so that it includes the backend lag.

- **Pros:** no new bound.
- **Cons:** `max_staleness` is the non-lag part of the budget: bucket quantisation and poll latency. The
  settings field says to raise the lag allowance for a slow backend, not this
  (`clients/ffi/pessimal_ffi/src/config_records.rs:445`). Both sides of the interlock would shift.

### Option B: Pass `now - backend_lag_allowance` to `observe`

Judge the gate from the shifted instant, the way liveness is judged.

- **Pros:** the same instant as liveness.
- **Cons:** `now` is also the sample ceiling and the dwell anchor. It would discard the freshest sample,
  delay every fire by the allowance, and read NoData on a loopback collector.

### Option C: Gate at `evidence_horizon()` and keep `now`

Pass `max_staleness + backend_lag_allowance` as the bound. Keep `now` as the real instant.

- **Pros:** the same inequality as Option B, with none of its side effects. The interlocks do not change.
- **Cons:** a host that has gone silent holds its last alert verdict for `max_staleness + lag` instead
  of `max_staleness`. `overview_window` has to derive from the horizon. The new-rule dwell default has to
  rise above the horizon.

## Recommended Action

Done. Option C was implemented in commit `0521efb` on 2026-09-10 (`fix(clients): stop a lagging backend
silently disabling every alert`). The code at HEAD still has it: `config.rs:372` defines
`evidence_horizon()`, and `fold.rs:789` and `fold.rs:814` pass it to `observe`. Nothing remains.

## Technical Details

- `clients/common/pessimal_client_core/src/config.rs`: `evidence_horizon()`, `overview_window()`, the
  interlocks, the `SpikeCanFire` check, and tests
- `clients/common/pessimal_client_core/src/fold.rs`: `observe_alerts`, the `latest_value` gate, and tests
- `clients/common/pessimal_client_core/src/plan.rs`: the overview window doc and test
- `common/pessimal_core/src/alert.rs`: the `observe` doc comment
- `clients/ffi/pessimal_ffi/src/config_records.rs`: the `max_staleness_seconds` doc
- `clients/ffi/pessimal_ffi/src/session.rs`: the `SpikeCanFire` test docs
- `clients/apple/PessimalFFI/Sources/PessimalFFI.swift`: regenerated bindings (doc comments only)
- `clients/apple/ios/Sources/Views/SettingsAlertRules.swift:190-194`: new-rule dwell default 600s
- `clients/apple/macos/Sources/Views/AlertRulesSettings.swift:168-172`: new-rule dwell default 600s

## Acceptance Criteria

- [x] The alert gate allows for backend lag: `observe` gets `evidence_horizon()`, and `now` stays the
  real instant.
- [x] A breaching host behind the backend's lag fires instead of reading NoData
  (`a_breaching_host_behind_the_backends_lag_fires_rather_than_reading_no_data`, `fold.rs:1834`).
- [x] A sample nobody refreshes is still refused, at the instant liveness gives up
  (`a_sample_nobody_refreshes_is_still_refused_at_the_instant_liveness_gives_up`, `fold.rs:1877`).
- [x] Liveness is Down exactly when evidence is NoData at ages 0, 118, 160, 180, 270, 271, 330, 331 and
  600s (`a_host_liveness_has_not_given_up_on_always_has_alertable_evidence`, `fold.rs:1931`). The test
  failed when the gate was reverted.
- [x] `evidence_horizon()` is 330s and `overview_window()` is 390s at preset
  (`derived_windows_are_not_settable_and_match_their_formulas`, `config.rs:1190`).
- [x] Both query windows reach past the horizon
  (`both_query_tiers_reach_past_the_evidence_horizon_so_the_gate_is_what_binds`, `config.rs:1259`).
- [x] `SpikeCanFire` warns when dwell is below the horizon
  (`spike_can_fire_warns_when_dwell_is_below_the_evidence_horizon`, `config.rs:1317`), and the new-rule
  default dwell does not trigger it.
- [x] The interlock `3 * metric_step <= max_staleness <= down_threshold` was reviewed. It stays as it is.

## Work Log

### 2026-09-09

- Created in `08bc5ca` (`docs(todos): file the alert-evidence gap the lag fix leaves behind`), minutes
  after the liveness lag fix `b9ccb69`, the same morning.
- At that time `observe_alerts` passed `max_staleness` (150s at defaults) to `observe`. The measured lag
  plus bucket quantisation was about 118s, so the margin was about 30s. A backend 160s behind gave Alive
  hosts with every alert at NoData.
- Two candidate fixes recorded: put `backend_lag_allowance` into `max_staleness`, or compare the gate
  against `now - backend_lag_allowance` the way liveness does. The second was marked as probably right.
  The note said the interlock `3 * metric_step <= max_staleness <= down_threshold` needed revisiting.
- `ea8ac27` (`docs: record what live verification found, and that it is done`) added it to
  `docs/HANDOFF.md` as next step 3.

### 2026-09-10

- Fixed in `0521efb`. Added `PollTuning::evidence_horizon()` and passed it to `observe` with `now`
  unchanged. Did not pass the shifted instant, for the reasons in Findings.
- Derived `overview_window()` from the horizon. Made `latest_value` use the same gate. Left the
  interlocks unchanged.
- Added the fold tests and the config tests listed in Acceptance Criteria.
- Changed the new-rule dwell default in both apps from 300s to 600s.
- Regenerated the Swift bindings (doc comments only). Both apps rebuilt. 419 tests.
- Appended the resolution notes to this todo.
- `74af91d` (`docs: record the real Doppler coordinates and the iOS release state`) struck the item
  through in `docs/HANDOFF.md`.

### 2026-09-12

- `68b2271` (`feat(client): derive used-of-total bytes for memory and every filesystem`) changed
  `config.rs` and `fold.rs`. It did not change the gate, the horizon or the lag allowance.

## Resources

- `docs/solutions/backend-ingestion-lag-breaks-liveness.md`
- Commit `0521efb`: the fix and its reasoning
- Commit `08bc5ca`: the original note
- `docs/HANDOFF.md`, next steps, item 3
