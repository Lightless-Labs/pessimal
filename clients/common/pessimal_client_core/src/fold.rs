//! The pure fold: one poll in, an entirely new picture out.
//!
//! [`FleetState`] is everything the client remembers between polls — the roster with its liveness
//! verdicts, the stored series, and one [`pessimal_core::AlertEvaluation`] per `(host, rule)` so
//! dwell accumulates. [`FleetState::apply`] consumes a [`PollObservation`] and returns a
//! [`FleetUpdate`]: the next state, the [`FleetView`] the apps draw, the [`AlertTransition`]s
//! worth a notification, and the [`PollAdvice`] that says when to come back. It never mutates,
//! never blocks, and never reads a clock — `observation.observed_at` is the only instant used,
//! which is what lets a test say "it is now T+90s" by typing T+90s.
//!
//! Four rules carry the whole module, and each of them is a bug somebody has already shipped:
//!
//! 1. **A failed poll mutates only the metadata.** The roster, every `Liveness` verdict, every
//!    `liveness_at` and every stored series are left exactly as they were; only `as_of`,
//!    `consecutive_failures`, `last_failure` and `advised_next_poll` move. Re-running
//!    `LivenessPolicy::evaluate` against an advanced clock walks a healthy fleet Alive → Stale →
//!    Down on the strength of a dead Wi-Fi connection. "I cannot see" and "your fleet is down"
//!    must not share a code path.
//! 2. **`observe` is called on `Fresh` and `Empty`, never on `Unavailable`.** `Ok(vec![])` is a
//!    real absence and legitimately clears dwell; an `Err` is not evidence of anything, and
//!    calling `observe` with an empty series on a failed fetch would reset every alert in the
//!    fleet after one transient 500. [`PollObservation::observable_series`] makes that structural:
//!    `None` is "do not judge".
//! 3. **The blind-gap reset compares against the *advised* next poll, not the cadence.** Dwell in
//!    core is wall-clock and survives any unobserved gap, so an hour-suspended app plus one
//!    breaching sample would fire instantly. Measuring lateness against the poll we ourselves
//!    asked for means a deliberate five-minute backoff does not trip its own freeze, while an
//!    unattended gap does.
//! 4. **History is bounded four ways**: the fetch window, replace-not-merge on every successful
//!    outcome, `forget_host_after`, and a hard `max_retained_hosts` cap evicting the oldest
//!    `last_seen_in_roster` first. A backend churning hostnames otherwise accumulates a day of
//!    ghosts, each carrying an evaluation per rule.
//!
//! Every map is a `BTreeMap` and evaluations are keyed `(host, rule-URN-string)` rather than by
//! `Urn` — which is `Hash + Eq` but not `Ord`. Deterministic iteration is what makes a whole-state
//! `assert_eq!` a legitimate test rather than a flake.

use std::collections::BTreeMap;

use chrono::{DateTime, Duration, Utc};
use pessimal_core::{
    AlertEvaluation, AlertRule, AlertState, Host, HostId, Liveness, MetricKind, MetricSeries,
    OsFamily, Urn,
};
use serde::{Deserialize, Serialize};

use crate::config::FleetConfig;
use crate::error::{ClientError, PollFailure, PollFailureKind, Result, worst_failure};
use crate::normalize;
use crate::observation::{Coverage, PollObservation, RosterOutcome};
use crate::rules::{rule_fingerprint, validate_rule};
use crate::view::{
    AlertEvidence, AlertPhase, AlertView, CollectionHealth, FleetCounts, FleetView,
    FreshnessInputs, HostView, MetricAvailability, MetricView, PLACEHOLDER_HOST_ID, Severity,
    expected_on,
};

/// Bumped whenever the persisted shape changes. A cache carrying anything else is discarded whole
/// rather than half-read: a monitoring app must not launch showing a fleet assembled from two
/// different schemas.
pub const FLEET_STATE_SCHEMA_VERSION: u32 = 1;

/// One retained host: what the roster said, plus the verdict we reached about it and when.
///
/// `liveness_at` is stored rather than recomputed on read because it is *frozen* while the roster
/// query is failing. The UI renders age relative to this instant and never re-judges liveness
/// itself.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct HostRecord {
    host: Host,
    liveness: Liveness,
    liveness_at: DateTime<Utc>,
    last_seen_in_roster: DateTime<Utc>,
    in_current_roster: bool,
}

impl HostRecord {
    fn seeded(host: Host, at: DateTime<Utc>) -> Self {
        Self {
            host,
            // Overwritten immediately by the liveness sweep; `Unknown` is the honest placeholder
            // for the instant between the two.
            liveness: Liveness::Unknown,
            liveness_at: at,
            last_seen_in_roster: at,
            in_current_roster: true,
        }
    }

    /// Merges a roster entry FIELD BY FIELD, never wholesale.
    ///
    /// `last_heartbeat` moves only forward: a query window that clipped the newest bucket would
    /// otherwise age a live host backwards. `agent_version` is never overwritten with `None`, and
    /// `os` is never overwritten with `OsFamily::Other` when we already know better — a label set
    /// missing `os.type` otherwise degrades a Windows host and turns its load tiles from
    /// `Unsupported` into an unexplained gap.
    fn merge(&mut self, incoming: &Host) {
        if incoming.last_heartbeat > self.host.last_heartbeat {
            self.host.last_heartbeat = incoming.last_heartbeat;
        }
        if incoming.agent_version.is_some() {
            self.host.agent_version.clone_from(&incoming.agent_version);
        }
        if incoming.os != OsFamily::Other || self.host.os == OsFamily::Other {
            self.host.os = incoming.os;
        }
    }
}

/// One metric's stored samples for one host, already normalised.
///
/// `resets` is keyed by `normalize::series_id` rather than positioned alongside `series`: a
/// parallel `Vec` would silently mis-associate the moment an attribute set appeared or vanished,
/// and JSON needs string map keys anyway.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct StoredMetric {
    series: Vec<MetricSeries>,
    availability: MetricAvailability,
    fetched_at: Option<DateTime<Utc>>,
    resets: BTreeMap<String, Vec<DateTime<Utc>>>,
}

impl StoredMetric {
    fn unfetched() -> Self {
        Self {
            series: Vec::new(),
            availability: MetricAvailability::Unavailable,
            fetched_at: None,
            resets: BTreeMap::new(),
        }
    }
}

/// One rule's running state for one host, plus what the last judgement was made of.
///
/// `fingerprint` is what makes an edited rule drop its dwell with nothing for the app to call:
/// shortening `for_duration` on a `Pending` rule would otherwise put `fires_at` in the past and
/// fire instantly off an old `since`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct StoredEvaluation {
    evaluation: AlertEvaluation,
    fingerprint: u64,
    frozen_since: Option<DateTime<Utc>>,
    latest_value: Option<f64>,
    dominant_label: Option<String>,
}

impl StoredEvaluation {
    fn fresh(rule: &AlertRule, host: &HostId, fingerprint: u64) -> Self {
        Self {
            evaluation: AlertEvaluation::new(rule.id().clone(), host.clone()),
            fingerprint,
            frozen_since: None,
            latest_value: None,
            dominant_label: None,
        }
    }

    /// Back to `NoData` with nothing carried over, keeping the fingerprint: the rule did not
    /// change, we merely stopped watching.
    fn reset(&mut self) {
        self.evaluation = AlertEvaluation::new(
            self.evaluation.rule_id.clone(),
            self.evaluation.host.clone(),
        );
        self.frozen_since = None;
        self.latest_value = None;
        self.dominant_label = None;
    }
}

/// Reads only the version out of a cache, so a schema mismatch is distinguishable from corruption.
///
/// Deserialising straight into [`FleetState`] cannot tell them apart: an older schema fails to
/// parse and would be reported as unreadable, which sends an operator hunting for a disk fault
/// that is not there.
#[derive(Deserialize)]
struct SchemaProbe {
    schema_version: u32,
}

/// Everything the client remembers between polls.
///
/// Opaque on purpose: private fields over nested `BTreeMap`s, moved across the FFI boundary only
/// as a JSON string. `PartialEq` but not `Eq` — `MetricSeries` and `AlertRule` carry `f64`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FleetState {
    schema_version: u32,
    backend_name: String,
    /// `None` until the first fold.
    as_of: Option<DateTime<Utc>>,
    /// `as_of + advice.delay()`. The instant the blind-gap rule measures lateness against.
    advised_next_poll: Option<DateTime<Utc>>,
    /// Last poll with a listed roster and no failure at all.
    last_clean_at: Option<DateTime<Utc>>,
    /// Last poll with a listed roster, whatever the series did.
    last_roster_at: Option<DateTime<Utc>>,
    consecutive_failures: u32,
    last_failure: Option<PollFailure>,
    /// Deliberately not persisted: a restored cache must read `Idle`, never `Fresh`.
    #[serde(skip)]
    polled_this_session: bool,
    hosts: BTreeMap<HostId, HostRecord>,
    series: BTreeMap<HostId, BTreeMap<MetricKind, StoredMetric>>,
    /// Keyed by `Urn::to_string()`, because `Urn` is `Hash + Eq` but not `Ord`.
    evaluations: BTreeMap<HostId, BTreeMap<String, StoredEvaluation>>,
}

impl FleetState {
    #[must_use]
    pub fn new(backend_name: impl Into<String>) -> Self {
        Self {
            schema_version: FLEET_STATE_SCHEMA_VERSION,
            backend_name: backend_name.into(),
            as_of: None,
            advised_next_poll: None,
            last_clean_at: None,
            last_roster_at: None,
            consecutive_failures: 0,
            last_failure: None,
            polled_this_session: false,
            hosts: BTreeMap::new(),
            series: BTreeMap::new(),
            evaluations: BTreeMap::new(),
        }
    }

    /// The pure fold. Seven steps, each its own private function and its own test.
    ///
    /// 1. **Blind gap.** Every evaluation is replaced with a fresh `NoData` one *before* anything
    ///    is observed, iff we are more than a staleness bound late for the poll we ourselves asked
    ///    for. Skipped on the first fold, when there is no advised instant.
    /// 2. **Roster.** Merge each `Host` field by field, then recompute `Liveness` for EVERY
    ///    retained host — including ones absent from this roster, which keep their old
    ///    `last_heartbeat` and therefore read `Down` naturally. Nothing happens on a failed or
    ///    unattempted roster.
    /// 3. **Retention.** Forget hosts unseen for `forget_host_after`, then evict the oldest
    ///    sightings down to `max_retained_hosts`. Series and evaluations go with them.
    /// 4. **Evaluation reconciliation.** The set is exactly `{(host, rule) : rule.enabled &&
    ///    rule.selector.matches(host) && host retained && validate_rule(rule).is_ok()}`. A changed
    ///    `rule_fingerprint` replaces the evaluation rather than reusing it.
    /// 5. **Series.** Per retained host and planned metric, switch on `PollObservation::coverage`.
    /// 6. **Alerts.** Per reconciled pair, `observe` on the SAME evaluation value as last poll so
    ///    dwell accumulates — or, on `Unavailable`, no `observe` at all.
    /// 7. **Bookkeeping.** Freshness inputs, the advice, and the advised instant the next fold's
    ///    step 1 will measure against.
    #[must_use]
    pub fn apply(&self, config: &FleetConfig, observation: &PollObservation) -> FleetUpdate {
        let now = observation.observed_at;
        let tuning = config.tuning;

        // Captured BEFORE step 1. A transition's `from` is the phase this poll started at, so a
        // blind-gap reset of a still-breaching alert reads Firing -> Pending: neither a fire nor a
        // resolve, which is exactly what an app resume should emit. It also means raising a
        // threshold on a firing rule reads Firing -> Ok, i.e. a resolve. That is correct — the
        // condition the user was told about no longer holds — so do not special-case it.
        let before = self.phases();

        let mut next = self.clone();
        next.reset_after_blind_gap(now, tuning.max_staleness());
        next.apply_roster(observation, config);
        next.reconcile_evaluations(config);
        next.store_series(config, observation);
        next.observe_alerts(config, observation);
        next.record_bookkeeping(observation);

        let transitions = next.transitions_since(&before, config, now);
        let advice = next.advice(config);
        // `Stop` carries no delay, and leaving this unset would disable the blind-gap reset
        // exactly when it is most needed: a key fixed three hours later would fire every frozen
        // `Pending` off a three-hour-old `since` on the first manual poll.
        // Checked: `backoff_after` caps the delay at five minutes, so this can only saturate when
        // `now` itself sits at the end of representable time — but falling back to `now` keeps the
        // instant armed, and an armed instant is what step 1 measures against.
        next.advised_next_poll = Some(
            now.checked_add_signed(advice.delay().unwrap_or_else(Duration::zero))
                .unwrap_or(now),
        );

        let view = next.view(config);
        FleetUpdate {
            state: next,
            view,
            transitions,
            advice,
        }
    }

    /// A pure projection. [`FleetUpdate`] carries one already built, so the pair cannot disagree.
    #[must_use]
    pub fn view(&self, config: &FleetConfig) -> FleetView {
        let tuning = config.tuning;
        let mut hosts = Vec::with_capacity(self.hosts.len());
        let mut alerts = Vec::new();
        let mut counts = FleetCounts::default();

        for (id, record) in &self.hosts {
            let host_alerts = self.alert_views(config, id);
            let firing = phase_count(&host_alerts, AlertPhase::Firing);
            let pending = phase_count(&host_alerts, AlertPhase::Pending);
            let liveness_severity = Severity::from_liveness(record.liveness);
            // "The worse of the two" is `Ord::max`, not a hand-written table each app writes
            // differently. `Severity` orders Ok < Unknown < Warning < Critical.
            let severity = host_alerts
                .iter()
                .map(|alert| alert.severity)
                .max()
                .map_or(liveness_severity, |worst| liveness_severity.max(worst));

            hosts.push(HostView {
                id: id.clone(),
                os: record.host.os,
                agent_version: record.host.agent_version.clone(),
                last_heartbeat: record.host.last_heartbeat,
                liveness: record.liveness,
                liveness_at: record.liveness_at,
                severity,
                collection: self.collection_health(id, tuning.metric_step()),
                metrics: self.metric_views(id, tuning.metric_step()),
                firing_alerts: firing,
                pending_alerts: pending,
                in_current_roster: record.in_current_roster,
                id_is_placeholder: id.as_str() == PLACEHOLDER_HOST_ID,
            });

            counts.hosts = counts.hosts.saturating_add(1);
            match record.liveness {
                Liveness::Alive => counts.alive = counts.alive.saturating_add(1),
                Liveness::Stale => counts.stale = counts.stale.saturating_add(1),
                Liveness::Down => counts.down = counts.down.saturating_add(1),
                Liveness::Unknown => counts.unknown = counts.unknown.saturating_add(1),
            }
            counts.firing_alerts = counts.firing_alerts.saturating_add(firing);
            counts.pending_alerts = counts.pending_alerts.saturating_add(pending);
            alerts.extend(host_alerts);
        }

        // `AlertPhase::sort_rank` rather than `Severity`: an unjudged rule is worth looking at, so
        // `NoData` sorts above `Ok` here. Keeping the key in `view.rs` is what stops this order
        // and its doc comment drifting apart. `Urn` is not `Ord`, hence the string tie-break.
        alerts.sort_by(|left, right| {
            left.phase
                .sort_rank()
                .cmp(&right.phase.sort_rank())
                .then_with(|| left.host.cmp(&right.host))
                .then_with(|| left.rule_id.to_string().cmp(&right.rule_id.to_string()))
        });

        let severity = hosts
            .iter()
            .map(|host| host.severity)
            .max()
            .unwrap_or(Severity::Ok);

        FleetView {
            backend_name: self.backend_name.clone(),
            as_of: self.as_of,
            freshness: self.freshness_inputs(config),
            severity,
            hosts,
            alerts,
            counts,
            warnings: config.audit(),
        }
    }

    /// When to come back, and whether to come back at all.
    ///
    /// Order matters. A state that has not folded this session polls immediately whatever it
    /// remembers — a relaunch must try the key once, because the user may well have fixed it.
    /// After that, `Unauthorized` anywhere in the poll stops the timer: one expired key does not
    /// fail selectively, and burning battery on a problem only the user can fix is wrong. Only a
    /// failed *roster* backs off; a single broken metric must never slow liveness to a
    /// five-minute cadence.
    #[must_use]
    pub fn advice(&self, config: &FleetConfig) -> PollAdvice {
        if !self.polled_this_session {
            return PollAdvice::Poll {
                after: Duration::zero(),
            };
        }
        // `worst_failure` ranks `Unauthorized` above everything, so the worst failure of a poll
        // containing one *is* the `Unauthorized` one. Keying off the kind rather than
        // `!is_transient` is deliberate: `Malformed` is neither transient nor user-fixable, and
        // stopping the poller on a metric-name mismatch would hide the rest of the fleet.
        if let Some(failure) = &self.last_failure
            && failure.kind == PollFailureKind::Unauthorized
        {
            return PollAdvice::Stop {
                reason: failure.kind,
                message: failure.message.clone(),
            };
        }
        // `last_roster_at` is stamped with `observed_at` exactly when the roster listed, and
        // `as_of` is that same instant, so "the roster listed this poll" is their equality. One
        // poll, one instant; there is no second observation folded at the same `observed_at`.
        if self.as_of != self.last_roster_at {
            return PollAdvice::Retry {
                after: config.tuning.backoff_after(self.consecutive_failures),
                consecutive_failures: self.consecutive_failures,
            };
        }
        PollAdvice::Poll {
            after: config.tuning.poll_interval(),
        }
    }

    /// Drops a host with its series and evaluations — an operator decommissioning a machine.
    #[must_use]
    pub fn forget_host(&self, host: &HostId) -> Self {
        let mut next = self.clone();
        next.hosts.remove(host);
        next.series.remove(host);
        next.evaluations.remove(host);
        next
    }

    #[must_use]
    pub fn backend_name(&self) -> &str {
        &self.backend_name
    }

    #[must_use]
    pub fn as_of(&self) -> Option<DateTime<Utc>> {
        self.as_of
    }

    #[must_use]
    pub fn last_clean_at(&self) -> Option<DateTime<Utc>> {
        self.last_clean_at
    }

    #[must_use]
    pub fn consecutive_failures(&self) -> u32 {
        self.consecutive_failures
    }

    #[must_use]
    pub fn known_hosts(&self) -> Vec<HostId> {
        self.hosts.keys().cloned().collect()
    }

    #[must_use]
    pub fn evaluation(&self, rule_id: &Urn, host: &HostId) -> Option<AlertState> {
        self.evaluations
            .get(host)?
            .get(&rule_id.to_string())
            .map(|stored| stored.evaluation.state())
    }

    /// # Errors
    /// [`ClientError::Backend`] if serialisation fails.
    pub fn to_json(&self) -> Result<String> {
        serde_json::to_string(self).map_err(|error| ClientError::Backend(error.to_string()))
    }

    /// Cold start from a cache. NEVER returns `Err` for a bad cache.
    ///
    /// A monitoring app must not refuse to launch because its state is a schema behind; what could
    /// not be salvaged is reported instead. `polled_this_session` is `#[serde(skip)]` and comes
    /// back `false`, which is what makes the restored view read `Idle` rather than `Fresh`. Stale
    /// dwell is deliberately NOT handled here: `advised_next_poll` survives the round trip, so
    /// relaunch is covered by the same blind-gap rule and the same test as an app resume.
    #[must_use]
    pub fn restore(backend_name: &str, json: &str) -> (Self, RestoreReport) {
        let Ok(probe) = serde_json::from_str::<SchemaProbe>(json) else {
            return (
                Self::new(backend_name),
                RestoreReport {
                    discarded_unreadable: true,
                    ..RestoreReport::default()
                },
            );
        };
        if probe.schema_version != FLEET_STATE_SCHEMA_VERSION {
            return (
                Self::new(backend_name),
                RestoreReport {
                    discarded_incompatible: true,
                    ..RestoreReport::default()
                },
            );
        }
        let Ok(state) = serde_json::from_str::<Self>(json) else {
            return (
                Self::new(backend_name),
                RestoreReport {
                    discarded_unreadable: true,
                    ..RestoreReport::default()
                },
            );
        };
        let report = RestoreReport {
            discarded_incompatible: false,
            discarded_unreadable: false,
            hosts_restored: count(state.hosts.len()),
            evaluations_restored: count(
                state.evaluations.values().map(BTreeMap::len).sum::<usize>(),
            ),
            saved_as_of: state.as_of,
        };
        // The caller's backend name wins: the session knows which backend it is talking to and the
        // cache only knows which one it was written against.
        (
            Self {
                backend_name: backend_name.to_owned(),
                ..state
            },
            report,
        )
    }

    // ---- step 0: what every alert looked like before this poll -------------------------------

    fn phases(&self) -> BTreeMap<HostId, BTreeMap<String, AlertPhase>> {
        self.evaluations
            .iter()
            .map(|(host, per_rule)| {
                let phases = per_rule
                    .iter()
                    .map(|(rule_id, stored)| {
                        (
                            rule_id.clone(),
                            AlertPhase::from_state(stored.evaluation.state()),
                        )
                    })
                    .collect();
                (host.clone(), phases)
            })
            .collect()
    }

    // ---- step 1: blind gap -------------------------------------------------------------------

    /// We are more than a staleness bound late for the poll we ourselves asked for, so we did not
    /// watch the interval and must not judge it.
    ///
    /// Measured against `advised_next_poll` rather than the cadence: a deliberate five-minute
    /// backoff, or a `Stop`, must not trip its own freeze, while an unattended gap must. Skipped
    /// entirely on the first fold, when there is no advised instant — which is also what makes
    /// persisting a `FleetState` across a relaunch safe.
    fn reset_after_blind_gap(&mut self, now: DateTime<Utc>, max_staleness: Duration) {
        let Some(advised) = self.advised_next_poll else {
            return;
        };
        // Checked, because `advised_next_poll` is persisted JSON and a restored instant near the
        // end of representable time would panic on the addition. Overflow means the deadline is
        // beyond any `now` that could exist, so "we are not yet past it" is the exact answer.
        if advised
            .checked_add_signed(max_staleness)
            .is_none_or(|deadline| now <= deadline)
        {
            return;
        }
        for per_rule in self.evaluations.values_mut() {
            for stored in per_rule.values_mut() {
                stored.reset();
            }
        }
    }

    // ---- steps 2 and 3: roster and retention --------------------------------------------------

    /// Nothing here runs unless the roster actually listed.
    ///
    /// Retention is gated with it, not merely liveness: `forget_host_after` measured through a
    /// day-long outage would delete the entire fleet and render an empty screen, which is the one
    /// fabrication this crate promises never to commit. Hosts are only ever added on a listed
    /// roster, so the cap has nothing to do on a failed one either.
    fn apply_roster(&mut self, observation: &PollObservation, config: &FleetConfig) {
        let RosterOutcome::Listed(listed) = &observation.hosts else {
            return;
        };
        let now = observation.observed_at;
        let tuning = config.tuning;

        for record in self.hosts.values_mut() {
            record.in_current_roster = false;
        }
        for host in listed {
            let record = self
                .hosts
                .entry(host.id.clone())
                .or_insert_with(|| HostRecord::seeded(host.clone(), now));
            record.merge(host);
            record.last_seen_in_roster = now;
            record.in_current_roster = true;
        }

        // EVERY retained host, not just the listed ones. A host that dropped out keeps its old
        // `last_heartbeat` and therefore reads `Down` naturally, which is a positive judgement;
        // `Unknown` is left to mean "in the roster, never beat".
        let liveness = tuning.liveness();
        for record in self.hosts.values_mut() {
            record.liveness = liveness.evaluate(record.host.last_heartbeat, now);
            record.liveness_at = now;
        }

        let horizon = tuning.forget_host_after();
        self.hosts
            .retain(|_, record| now - record.last_seen_in_roster <= horizon);

        let cap = usize::try_from(tuning.max_retained_hosts()).unwrap_or(usize::MAX);
        if self.hosts.len() > cap {
            let mut by_sighting: Vec<(DateTime<Utc>, HostId)> = self
                .hosts
                .iter()
                .map(|(id, record)| (record.last_seen_in_roster, id.clone()))
                .collect();
            by_sighting.sort_unstable();
            let excess = self.hosts.len() - cap;
            for (_, id) in by_sighting.into_iter().take(excess) {
                self.hosts.remove(&id);
            }
        }

        let retained = &self.hosts;
        self.series.retain(|host, _| retained.contains_key(host));
        self.evaluations
            .retain(|host, _| retained.contains_key(host));
    }

    // ---- step 4: evaluation reconciliation ----------------------------------------------------

    /// The reconciled set is exactly the pairs worth judging, so core's quirk — `observe` returns
    /// `Ok`, not `NoData`, for a disabled or out-of-scope rule, and an unjudged host would read
    /// healthy — never reaches the view.
    fn reconcile_evaluations(&mut self, config: &FleetConfig) {
        let host_ids: Vec<HostId> = self.hosts.keys().cloned().collect();
        for host in host_ids {
            let wanted: Vec<(&AlertRule, String, u64)> = config
                .rules
                .iter()
                .filter(|rule| {
                    // An invalid rule is shown and flagged by the view, never evaluated: a NaN
                    // threshold makes every comparator return false and the rule silently never
                    // fires.
                    rule.enabled && rule.selector.matches(&host) && validate_rule(rule).is_ok()
                })
                .map(|rule| (rule, rule.id().to_string(), rule_fingerprint(rule)))
                .collect();

            if wanted.is_empty() {
                self.evaluations.remove(&host);
                continue;
            }

            let slot = self.evaluations.entry(host.clone()).or_default();
            slot.retain(|key, _| wanted.iter().any(|(_, wanted_key, _)| wanted_key == key));
            for (rule, key, fingerprint) in wanted {
                let current = slot
                    .get(&key)
                    .is_some_and(|stored| stored.fingerprint == fingerprint);
                if !current {
                    slot.insert(key, StoredEvaluation::fresh(rule, &host, fingerprint));
                }
            }
        }
    }

    // ---- step 5: series storage ---------------------------------------------------------------

    /// Replace, never merge. Replacing outright kills timestamp alignment bugs, duplicate points
    /// and drifting windows at the cost of bandwidth — affordable only because the fleet-wide
    /// window is about seven buckets.
    fn store_series(&mut self, config: &FleetConfig, observation: &PollObservation) {
        let now = observation.observed_at;
        let targets: Vec<(HostId, OsFamily)> = self
            .hosts
            .iter()
            .map(|(id, record)| (id.clone(), record.host.os))
            .collect();

        for (host, os) in targets {
            let planned = planned_metrics_for(config, &host);
            let mut slot = self.series.remove(&host).unwrap_or_default();
            // A metric that has stopped being planned — the focus moved to another host — must not
            // sit at `Present` forever with a fetched_at nobody is refreshing.
            slot.retain(|kind, _| planned.contains(kind));

            for metric in planned {
                match observation.coverage(&host, metric) {
                    Coverage::Fresh => {
                        let mut stored = Vec::new();
                        let mut resets = BTreeMap::new();
                        for series in observation.series_for(&host, metric) {
                            // On the RAW series: on an already-normalised one this reports every
                            // ordinary decrease in rate as an agent restart.
                            let marks = normalize::counter_resets(series);
                            if !marks.is_empty() {
                                resets.insert(
                                    normalize::series_id(&host, metric, &series.attributes),
                                    marks,
                                );
                            }
                            stored.push(normalize::normalize(series));
                        }
                        slot.insert(
                            metric,
                            StoredMetric {
                                series: stored,
                                availability: MetricAvailability::Present,
                                fetched_at: Some(now),
                                resets,
                            },
                        );
                    }
                    Coverage::Empty => {
                        // A successful empty answer is current knowledge, so `fetched_at` advances
                        // and the old points go: "render as a gap, never as zero" means there must
                        // be nothing left to render.
                        let availability = if expected_on(metric, os) {
                            MetricAvailability::NotReported
                        } else {
                            MetricAvailability::Unsupported
                        };
                        slot.insert(
                            metric,
                            StoredMetric {
                                series: Vec::new(),
                                availability,
                                fetched_at: Some(now),
                                resets: BTreeMap::new(),
                            },
                        );
                    }
                    Coverage::Unavailable => {
                        // Values and `fetched_at` untouched: the gap between `fetched_at` and the
                        // poll's `as_of` is the whole point.
                        slot.entry(metric)
                            .or_insert_with(StoredMetric::unfetched)
                            .availability = MetricAvailability::Unavailable;
                    }
                }
            }

            if !slot.is_empty() {
                self.series.insert(host, slot);
            }
        }
    }

    // ---- step 6: alerts -----------------------------------------------------------------------

    /// `observe` runs on the SAME evaluation value as last poll, which is how dwell accumulates.
    fn observe_alerts(&mut self, config: &FleetConfig, observation: &PollObservation) {
        let now = observation.observed_at;
        let max_staleness = config.tuning.max_staleness();
        let by_key = rules_by_key(config);

        for (host, per_rule) in &mut self.evaluations {
            for (rule_key, stored) in per_rule.iter_mut() {
                let Some(rule) = by_key.get(rule_key) else {
                    continue;
                };
                let Some(raw) = observation.observable_series(host, rule.metric) else {
                    // `Unavailable`: no value exists to hand to `observe`, so a caller physically
                    // cannot reset a fleet's dwell on the strength of one transient 500.
                    stored.frozen_since.get_or_insert(now);
                    continue;
                };
                stored.frozen_since = None;

                // Normalise each attribute set FIRST, then reduce. Reducing raw cumulative
                // counters across interfaces or mounts and differencing afterwards fabricates a
                // reset — or a spike — whenever the dominant label set changes.
                let normalized: Vec<MetricSeries> =
                    raw.into_iter().map(normalize::normalize).collect();
                let reduction = normalize::reduction_for(rule.comparator);
                let candidate = normalize::reduce(rule.metric, host, &normalized, reduction)
                    .unwrap_or_else(|| MetricSeries::new(host.clone(), rule.metric, Vec::new()));

                stored
                    .evaluation
                    .observe(rule, &candidate, now, max_staleness);

                // Exactly `observe`'s own gate, so `latest_value` is the value the phase was
                // judged on and is `None` whenever the phase is `NoData`.
                let judged = candidate
                    .latest_at(now)
                    .filter(|point| now - point.at <= max_staleness);
                stored.latest_value = judged.map(|point| point.value);
                // `dominant_at` matches on the exact instant of the reduced point, so the label can
                // never name a series that did not produce the value. `reduce` returns empty
                // attributes even for a single input, so the label cannot come from `candidate`.
                stored.dominant_label = judged
                    .and_then(|point| normalize::dominant_at(&normalized, point.at, reduction))
                    .and_then(normalize::series_label);
            }
        }
    }

    // ---- step 7: bookkeeping ------------------------------------------------------------------

    fn record_bookkeeping(&mut self, observation: &PollObservation) {
        let now = observation.observed_at;
        self.as_of = Some(now);
        if matches!(observation.hosts, RosterOutcome::Listed(_)) {
            self.last_roster_at = Some(now);
        }
        if observation.is_clean() {
            self.last_clean_at = Some(now);
        }
        let failures = observation.failures();
        if failures.is_empty() {
            self.consecutive_failures = 0;
        } else {
            self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        }
        // The last poll's worst failure, not a historical one: a banner that keeps naming a fault
        // that has since cleared is worse than no banner.
        self.last_failure = worst_failure(&failures).cloned();
        // Set by the FIRST fold whatever its outcome. A relaunch whose first poll 401s must read
        // `Unusable { failure: Unauthorized }` with an Open Settings button, not `Idle` hiding a
        // dead key behind a cache age.
        self.polled_this_session = true;
    }

    fn transitions_since(
        &self,
        before: &BTreeMap<HostId, BTreeMap<String, AlertPhase>>,
        config: &FleetConfig,
        at: DateTime<Utc>,
    ) -> Vec<AlertTransition> {
        let by_key = rules_by_key(config);
        let mut transitions = Vec::new();
        // Nested `BTreeMap`s iterate host-sorted then rule-sorted, which is the pinned
        // `(host, rule_id)` order for free.
        for (host, per_rule) in &self.evaluations {
            for (rule_key, stored) in per_rule {
                let state = stored.evaluation.state();
                let to = AlertPhase::from_state(state);
                // A pair with no prior evaluation started at `NoData`: that is what a fresh
                // `AlertEvaluation` is, so a first fold straight to `Firing` still emits a fire.
                let from = before
                    .get(host)
                    .and_then(|per_rule| per_rule.get(rule_key))
                    .copied()
                    .unwrap_or(AlertPhase::NoData);
                if from == to {
                    continue;
                }
                let Some(rule) = by_key.get(rule_key) else {
                    continue;
                };
                transitions.push(AlertTransition {
                    rule_id: stored.evaluation.rule_id.clone(),
                    rule_name: rule.name.clone(),
                    host: host.clone(),
                    from,
                    to,
                    breaching_since: state.breaching_since(),
                    at,
                });
            }
        }
        transitions
    }

    // ---- view construction --------------------------------------------------------------------

    fn freshness_inputs(&self, config: &FleetConfig) -> FreshnessInputs {
        FreshnessInputs {
            as_of: self.as_of,
            last_clean_at: self.last_clean_at,
            last_roster_at: self.last_roster_at,
            consecutive_failures: self.consecutive_failures,
            last_failure: self.last_failure.clone(),
            polled_this_session: self.polled_this_session,
            tolerance: config.tuning.staleness_tolerance(),
            budget: config.tuning.freshness_budget(),
        }
    }

    fn metric_views(&self, host: &HostId, step: Duration) -> Vec<MetricView> {
        let mut views = Vec::new();
        let Some(per_metric) = self.series.get(host) else {
            return views;
        };
        for (&kind, stored) in per_metric {
            if stored.series.is_empty() {
                // A metric with no samples still needs a row: `NotReported`, `Unsupported` and
                // `Unavailable` are answers the UI must render, and a missing tile renders none of
                // them.
                let attributes = BTreeMap::new();
                views.push(MetricView {
                    id: normalize::series_id(host, kind, &attributes),
                    kind,
                    unit: kind.unit(),
                    display_name: kind.display_name().to_owned(),
                    label: None,
                    attributes,
                    latest: None,
                    points: Vec::new(),
                    availability: stored.availability,
                    is_rate: normalize::is_rate(kind),
                    expected_step: step,
                    resets: Vec::new(),
                    fetched_at: stored.fetched_at,
                });
                continue;
            }
            for series in &stored.series {
                let id = normalize::series_id(host, kind, &series.attributes);
                let resets = stored.resets.get(&id).cloned().unwrap_or_default();
                views.push(MetricView {
                    id,
                    kind,
                    unit: kind.unit(),
                    display_name: kind.display_name().to_owned(),
                    label: normalize::series_label(series),
                    attributes: series.attributes.clone(),
                    latest: series.latest(),
                    points: series.points().to_vec(),
                    availability: stored.availability,
                    is_rate: normalize::is_rate(kind),
                    expected_step: step,
                    resets,
                    fetched_at: stored.fetched_at,
                });
            }
        }
        views.sort_by(|left, right| {
            left.kind
                .cmp(&right.kind)
                .then_with(|| left.id.cmp(&right.id))
        });
        views
    }

    /// Whether the agent is collecting, as opposed to merely beating.
    ///
    /// The stored series are already `Delta`-normalised, so the newest point *is* "failures added
    /// since the previous sample" and `over` is the bucket it spans. A failed collection re-exports
    /// the previous snapshot with fresh timestamps, so the data itself shows a flat line rather
    /// than a gap and this is the only tell.
    fn collection_health(&self, host: &HostId, step: Duration) -> CollectionHealth {
        let Some(stored) = self
            .series
            .get(host)
            .and_then(|per_metric| per_metric.get(&MetricKind::AgentCollectionFailures))
        else {
            return CollectionHealth::Unknown;
        };
        // Frozen values are not evidence of current collection health.
        if stored.availability != MetricAvailability::Present {
            return CollectionHealth::Unknown;
        }
        let reduced = normalize::reduce(
            MetricKind::AgentCollectionFailures,
            host,
            &stored.series,
            normalize::Reduction::Max,
        );
        let Some(latest) = reduced.and_then(|series| series.latest()) else {
            return CollectionHealth::Unknown;
        };
        if latest.value > 0.0 {
            CollectionHealth::Failing {
                added: latest.value,
                over: step,
            }
        } else {
            CollectionHealth::Healthy
        }
    }

    fn alert_views(&self, config: &FleetConfig, host: &HostId) -> Vec<AlertView> {
        let mut views = Vec::new();
        for rule in &config.rules {
            if !rule.enabled || !rule.selector.matches(host) {
                continue;
            }
            let stored = self
                .evaluations
                .get(host)
                .and_then(|per_rule| per_rule.get(&rule.id().to_string()));
            let state = stored.map_or(AlertState::NoData, |held| held.evaluation.state());
            let phase = AlertPhase::from_state(state);
            let breaching_since = state.breaching_since();
            views.push(AlertView {
                id: format!("{}|{host}", rule.id()),
                rule_id: rule.id().clone(),
                rule_name: rule.name.clone(),
                description: rule.describe(),
                host: host.clone(),
                metric: rule.metric,
                comparator: rule.comparator,
                threshold: rule.threshold,
                phase,
                severity: Severity::from_phase(phase),
                breaching_since,
                // Checked, and `None` on overflow, so a dwell that somehow slipped past both
                // `AlertRule::new` and `validate_rule` renders as "no fire time known" instead of
                // crashing the app. This is the last layer: `since` comes out of a persisted
                // `FleetState` and the rule out of a persisted `FleetConfig`, so a value that
                // reaches here reaches here on every launch, through the synchronous `view()`
                // path as well as the fold.
                fires_at: breaching_since
                    .and_then(|since| since.checked_add_signed(rule.for_duration())),
                latest_value: stored.and_then(|held| held.latest_value),
                series_label: stored.and_then(|held| held.dominant_label.clone()),
                evidence: stored
                    .and_then(|held| held.frozen_since)
                    .map_or(AlertEvidence::Fresh, |since| AlertEvidence::Frozen {
                        since,
                    }),
                is_rate: normalize::is_rate(rule.metric),
                // One decision, one place: the full `Display` string, prefixed "invalid alert
                // rule: …", so no caller has to re-prefix it and none of them disagree.
                invalid_reason: validate_rule(rule).err().map(|error| error.to_string()),
            });
        }
        views
    }
}

/// Overview metrics union every enabled rule's metric, plus the detail set for the one focused
/// host. Anything outside this is a pair the plan never asked for, and marking it `Unavailable`
/// would be reporting a failure that never happened.
fn planned_metrics_for(config: &FleetConfig, host: &HostId) -> Vec<MetricKind> {
    let mut metrics = config.planned_metrics();
    if config.focus.as_ref() == Some(host) {
        metrics.extend(config.detail_metrics.iter().copied());
        metrics.sort_unstable();
        metrics.dedup();
    }
    metrics
}

fn rules_by_key(config: &FleetConfig) -> BTreeMap<String, &AlertRule> {
    config
        .rules
        .iter()
        .map(|rule| (rule.id().to_string(), rule))
        .collect()
}

fn phase_count(alerts: &[AlertView], phase: AlertPhase) -> u32 {
    count(alerts.iter().filter(|alert| alert.phase == phase).count())
}

/// `usize` counts cross the FFI boundary as `u32`. Saturating rather than wrapping: a fleet of
/// four billion hosts is not a number worth being precisely wrong about.
fn count(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

/// What one fold produced. `pessimal_ffi` destructures it: the state goes back into the session's
/// mutex, the rest crosses the boundary as a `PollResult`.
#[derive(Debug, Clone, PartialEq)]
pub struct FleetUpdate {
    pub state: FleetState,
    pub view: FleetView,
    /// Only where `from != to`, sorted by `(host, rule_id)`.
    pub transitions: Vec<AlertTransition>,
    pub advice: PollAdvice,
}

/// An alert crossing a phase boundary — the only thing worth waking a user for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlertTransition {
    pub rule_id: Urn,
    pub rule_name: String,
    pub host: HostId,
    /// [`AlertPhase`], not `pessimal_core::AlertState`: the payload `DateTime` travels beside it,
    /// for the same reason `AlertView` carries a phase.
    pub from: AlertPhase,
    pub to: AlertPhase,
    pub breaching_since: Option<DateTime<Utc>>,
    pub at: DateTime<Utc>,
}

impl AlertTransition {
    /// `to` is `Firing` and `from` is not. The edge worth a notification.
    #[must_use]
    pub fn is_fire(&self) -> bool {
        self.to == AlertPhase::Firing && self.from != AlertPhase::Firing
    }

    /// `from` was `Firing` and `to` is `Ok` — nothing looser.
    ///
    /// A blind-gap reset drives a `Firing` evaluation to `NoData` and then to `Pending`; if this
    /// matched anything but `Ok`, every app resume would emit a spurious resolve for every
    /// still-breaching alert and re-fire one dwell later.
    #[must_use]
    pub fn is_resolve(&self) -> bool {
        self.from == AlertPhase::Firing && self.to == AlertPhase::Ok
    }
}

/// When to come back. Swift owns the timer; core owns the policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PollAdvice {
    /// A clean poll, or a partial failure — a broken single metric must never slow liveness to a
    /// five-minute cadence, so only a failed roster backs off. Any state that has not folded this
    /// session, restored or brand new, advises `after = zero` so neither a relaunch nor a cold
    /// start sleeps an interval before showing data.
    Poll { after: Duration },
    /// The roster failed. `after = tuning.backoff_after(consecutive_failures)`, deterministic and
    /// unjittered; Swift may add jitter, core stays reproducible.
    Retry {
        after: Duration,
        consecutive_failures: u32,
    },
    /// Retrying cannot help. Emitted whenever ANY failure this poll — roster or a single series
    /// query — carried [`PollFailureKind::Unauthorized`].
    Stop {
        reason: PollFailureKind,
        message: String,
    },
}

impl PollAdvice {
    #[must_use]
    pub fn delay(&self) -> Option<Duration> {
        match self {
            Self::Poll { after } | Self::Retry { after, .. } => Some(*after),
            Self::Stop { .. } => None,
        }
    }

    #[must_use]
    pub fn should_poll(&self) -> bool {
        !matches!(self, Self::Stop { .. })
    }
}

/// What a cold start could and could not salvage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RestoreReport {
    /// True when the cache's `schema_version` did not match and everything was discarded.
    pub discarded_incompatible: bool,
    /// True when the cache was not valid JSON.
    pub discarded_unreadable: bool,
    pub hosts_restored: u32,
    pub evaluations_restored: u32,
    pub saved_as_of: Option<DateTime<Utc>>,
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use chrono::{DateTime, Duration, Utc};
    use pessimal_core::{
        AlertRule, AlertState, Comparator, CoreError, Host, HostId, HostSelector, Liveness,
        MetricKind, MetricPoint, MetricSeries, OsFamily,
    };

    use super::{AlertPhase, AlertTransition, FleetState, PollAdvice};
    use crate::config::{FleetConfig, PollTuning};
    use crate::error::{FailureSource, PollFailure, PollFailureKind};
    use crate::observation::{PollObservation, RosterOutcome, SeriesResult};
    use crate::plan::plan_poll;
    use crate::view::{AlertEvidence, CollectionHealth, MetricAvailability, PLACEHOLDER_HOST_ID};

    const BACKEND: &str = "signoz";

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_760_000_000 + secs, 0).expect("valid timestamp")
    }

    fn config() -> FleetConfig {
        FleetConfig::new("prod", PollTuning::default()).expect("prod is a usable environment")
    }

    fn cpu_rule(for_secs: i64) -> AlertRule {
        AlertRule::new(
            "prod",
            "CPU hot",
            MetricKind::CpuUtilization,
            Comparator::GreaterThan,
            0.9,
            Duration::seconds(for_secs),
        )
        .expect("a valid rule")
    }

    fn host(name: &str, beat: DateTime<Utc>) -> Host {
        Host::new(HostId::new(name), OsFamily::Linux).with_last_heartbeat(beat)
    }

    fn cpu(name: &str, samples: &[(DateTime<Utc>, f64)]) -> MetricSeries {
        series(name, MetricKind::CpuUtilization, samples)
    }

    fn mount(path: &str) -> BTreeMap<String, String> {
        let mut attributes = BTreeMap::new();
        attributes.insert("system.filesystem.mountpoint".to_owned(), path.to_owned());
        attributes
    }

    fn series(name: &str, kind: MetricKind, samples: &[(DateTime<Utc>, f64)]) -> MetricSeries {
        MetricSeries::new(
            HostId::new(name),
            kind,
            samples
                .iter()
                .map(|&(at, value)| MetricPoint::new(at, value))
                .collect(),
        )
    }

    /// Builds the observation a real poll of `config` at `now` would produce: the plan is asked
    /// for its queries, so spec ordering — which `series_for`'s last-wins dedup depends on — is
    /// the one the gather stage would really see.
    fn poll_with(
        config: &FleetConfig,
        now: DateTime<Utc>,
        roster: RosterOutcome,
        available: &[MetricSeries],
        failing: &[(MetricKind, CoreError)],
    ) -> PollObservation {
        let plan = plan_poll(config, now).expect("a constructor-built tuning always plans");
        let mut observation = PollObservation::new(now).with_hosts(roster);
        for spec in plan.queries {
            let result = match failing.iter().find(|(metric, _)| *metric == spec.metric) {
                Some((_, error)) => SeriesResult::Failed(PollFailure::from_core(
                    error,
                    FailureSource::Series {
                        metric: spec.metric,
                    },
                    now,
                )),
                None => SeriesResult::Returned(
                    available
                        .iter()
                        .filter(|series| spec.covers(&series.host, series.kind))
                        .cloned()
                        .collect(),
                ),
            };
            observation = observation.with_series(spec, result);
        }
        observation
    }

    fn clean_poll(
        config: &FleetConfig,
        now: DateTime<Utc>,
        roster: Vec<Host>,
        available: &[MetricSeries],
    ) -> PollObservation {
        poll_with(config, now, RosterOutcome::Listed(roster), available, &[])
    }

    fn every_metric(error: &CoreError) -> Vec<(MetricKind, CoreError)> {
        MetricKind::ALL
            .into_iter()
            .map(|kind| (kind, error.clone()))
            .collect()
    }

    /// A dead network: the roster and every query fail together.
    fn dead_poll(config: &FleetConfig, now: DateTime<Utc>, error: &CoreError) -> PollObservation {
        poll_with(
            config,
            now,
            RosterOutcome::Failed(PollFailure::from_core(error, FailureSource::Roster, now)),
            &[],
            &every_metric(error),
        )
    }

    fn unreachable() -> CoreError {
        CoreError::Unreachable("dns".to_owned())
    }

    #[test]
    fn a_failed_roster_does_not_age_hosts_toward_down() {
        let config = config();
        let first = FleetState::new(BACKEND).apply(
            &config,
            &clean_poll(&config, at(0), vec![host("web-1", at(0))], &[]),
        );

        // Far past `down_threshold`. Re-judging liveness here would walk a healthy fleet down on
        // the strength of a dead Wi-Fi connection.
        let second = first
            .state
            .apply(&config, &dead_poll(&config, at(400), &unreachable()));

        let view = second.view.host(&HostId::new("web-1")).expect("retained");
        assert_eq!(view.liveness, Liveness::Alive);
        assert_eq!(view.liveness_at, at(0));
        assert_eq!(view.last_heartbeat, Some(at(0)));
    }

    #[test]
    fn a_failed_series_query_does_not_clear_dwell() {
        let rule = cpu_rule(300);
        let config = config().with_rules(vec![rule.clone()]);
        let roster = vec![host("web-1", at(0))];

        let first = FleetState::new(BACKEND).apply(
            &config,
            &clean_poll(
                &config,
                at(0),
                roster.clone(),
                &[cpu("web-1", &[(at(0), 0.95)])],
            ),
        );
        assert_eq!(
            first.state.evaluation(rule.id(), &HostId::new("web-1")),
            Some(AlertState::Pending { since: at(0) })
        );

        let second = first.state.apply(
            &config,
            &poll_with(
                &config,
                at(30),
                RosterOutcome::Listed(roster),
                &[],
                &[(MetricKind::CpuUtilization, unreachable())],
            ),
        );

        assert_eq!(
            second.state.evaluation(rule.id(), &HostId::new("web-1")),
            Some(AlertState::Pending { since: at(0) }),
            "an Err is not evidence of anything and must not clear dwell"
        );
        assert_eq!(
            second.view.alerts[0].evidence,
            AlertEvidence::Frozen { since: at(30) }
        );
    }

    #[test]
    fn an_empty_but_successful_series_clears_dwell_to_nodata() {
        let rule = cpu_rule(300);
        let config = config().with_rules(vec![rule.clone()]);
        let roster = vec![host("web-1", at(0))];

        let first = FleetState::new(BACKEND).apply(
            &config,
            &clean_poll(
                &config,
                at(0),
                roster.clone(),
                &[cpu("web-1", &[(at(0), 0.95)])],
            ),
        );
        let second = first
            .state
            .apply(&config, &clean_poll(&config, at(30), roster, &[]));

        assert_eq!(
            second.state.evaluation(rule.id(), &HostId::new("web-1")),
            Some(AlertState::NoData),
            "Ok(vec![]) is a real absence and legitimately clears dwell"
        );
    }

    #[test]
    fn dwell_accumulates_across_three_polls_to_firing() {
        let rule = cpu_rule(60);
        let config = config().with_rules(vec![rule.clone()]);
        let roster = vec![host("web-1", at(0))];
        let mut state = FleetState::new(BACKEND);

        for offset in [0, 30, 60] {
            let observation = clean_poll(
                &config,
                at(offset),
                roster.clone(),
                &[cpu("web-1", &[(at(offset), 0.95)])],
            );
            state = state.apply(&config, &observation).state;
        }

        assert_eq!(
            state.evaluation(rule.id(), &HostId::new("web-1")),
            Some(AlertState::Firing { since: at(0) })
        );
    }

    #[test]
    fn a_failure_streak_under_backoff_does_not_reset_dwell() {
        let rule = cpu_rule(600);
        let config = config().with_rules(vec![rule.clone()]);
        let host_id = HostId::new("web-1");

        let mut state = FleetState::new(BACKEND)
            .apply(
                &config,
                &clean_poll(
                    &config,
                    at(0),
                    vec![host("web-1", at(0))],
                    &[cpu("web-1", &[(at(0), 0.95)])],
                ),
            )
            .state;

        // Every observation lands exactly on the instant the previous fold advised: 30s, then the
        // backoff ladder 60s, 120s. A deliberate backoff must not trip its own blind-gap freeze.
        for offset in [30, 90, 210] {
            let update = state.apply(&config, &dead_poll(&config, at(offset), &unreachable()));
            state = update.state;
        }

        assert_eq!(
            state.evaluation(rule.id(), &host_id),
            Some(AlertState::Pending { since: at(0) })
        );
        assert_eq!(state.consecutive_failures(), 3);
    }

    #[test]
    fn an_unattended_gap_past_the_advised_poll_resets_dwell() {
        let rule = cpu_rule(60);
        let config = config().with_rules(vec![rule.clone()]);
        let roster = vec![host("web-1", at(0))];
        let host_id = HostId::new("web-1");

        let mut state = FleetState::new(BACKEND);
        for offset in [0, 60] {
            let observation = clean_poll(
                &config,
                at(offset),
                roster.clone(),
                &[cpu("web-1", &[(at(offset), 0.95)])],
            );
            state = state.apply(&config, &observation).state;
        }
        assert_eq!(
            state.evaluation(rule.id(), &host_id),
            Some(AlertState::Firing { since: at(0) })
        );

        // Advised at 90s; more than `max_staleness` (150s) past it.
        let resumed = state.apply(
            &config,
            &clean_poll(
                &config,
                at(360),
                roster,
                &[cpu("web-1", &[(at(360), 0.95)])],
            ),
        );

        assert_eq!(
            resumed.state.evaluation(rule.id(), &host_id),
            Some(AlertState::Pending { since: at(360) }),
            "we did not watch the interval, so we must not judge it"
        );
    }

    #[test]
    fn a_stop_followed_by_a_late_manual_poll_resets_dwell() {
        let rule = cpu_rule(300);
        let config = config().with_rules(vec![rule.clone()]);
        let roster = vec![host("web-1", at(0))];
        let host_id = HostId::new("web-1");

        let first = FleetState::new(BACKEND).apply(
            &config,
            &clean_poll(
                &config,
                at(0),
                roster.clone(),
                &[cpu("web-1", &[(at(0), 0.95)])],
            ),
        );

        // A 401 on the series query alone: the roster still lists, so the Pending state freezes
        // rather than being re-judged, and the advice is `Stop`, which carries no delay.
        let stopped = first.state.apply(
            &config,
            &poll_with(
                &config,
                at(30),
                RosterOutcome::Listed(roster.clone()),
                &[],
                &[(MetricKind::CpuUtilization, CoreError::Unauthorized)],
            ),
        );
        assert!(matches!(stopped.advice, PollAdvice::Stop { .. }));
        assert_eq!(
            stopped.state.evaluation(rule.id(), &host_id),
            Some(AlertState::Pending { since: at(0) })
        );

        // `advised_next_poll` was still armed at the Stop instant, so a manual poll more than a
        // staleness bound later is a blind gap like any other.
        let manual = stopped.state.apply(
            &config,
            &clean_poll(
                &config,
                at(181),
                roster,
                &[cpu("web-1", &[(at(181), 0.95)])],
            ),
        );
        assert_eq!(
            manual.state.evaluation(rule.id(), &host_id),
            Some(AlertState::Pending { since: at(181) })
        );
    }

    #[test]
    fn the_first_fold_never_triggers_the_blind_gap_reset() {
        let rule = cpu_rule(600);
        let config = config().with_rules(vec![rule.clone()]);
        let roster = vec![host("web-1", at(0))];
        let host_id = HostId::new("web-1");

        let first = FleetState::new(BACKEND).apply(
            &config,
            &clean_poll(
                &config,
                at(0),
                roster.clone(),
                &[cpu("web-1", &[(at(0), 0.95)])],
            ),
        );

        // A cache written before `advised_next_poll` existed, or one whose advice was never
        // recorded: no advised instant means no evidence we were ever late.
        let mut cached: serde_json::Value =
            serde_json::from_str(&first.state.to_json().expect("serialisable"))
                .expect("valid JSON");
        cached["advised_next_poll"] = serde_json::Value::Null;
        let (restored, report) = FleetState::restore(BACKEND, &cached.to_string());
        assert_eq!(report.evaluations_restored, 1);

        let resumed = restored.apply(
            &config,
            &clean_poll(
                &config,
                at(100_000),
                roster,
                &[cpu("web-1", &[(at(100_000), 0.95)])],
            ),
        );

        // `since` is untouched, which is the whole assertion: with no advised instant there is
        // nothing to be late for, so step 1 does not run. Note what that costs — the rule fires
        // off a `since` a day old, which is exactly the hazard the armed instant exists to
        // prevent. That is why `apply` arms `advised_next_poll` on EVERY fold, `Stop` included.
        assert_eq!(
            resumed.state.evaluation(rule.id(), &host_id),
            Some(AlertState::Firing { since: at(0) })
        );
    }

    #[test]
    fn a_breach_instant_too_close_to_the_end_of_time_renders_no_fire_time() {
        // The last layer, tested where the earlier layers cannot reach it. A dwell this long is
        // legal — it is exactly `AlertRule::MAX_FOR_DURATION` — so nothing upstream rejects the
        // rule; what makes `since + for_duration` overflow is the `since`, and `since` comes out
        // of a persisted `FleetState` rather than out of any constructor.
        let rule = AlertRule::new(
            "prod",
            "CPU hot",
            MetricKind::CpuUtilization,
            Comparator::GreaterThan,
            0.9,
            AlertRule::MAX_FOR_DURATION,
        )
        .expect("the ceiling itself is a legal dwell");
        let config = config().with_rules(vec![rule.clone()]);
        let host_id = HostId::new("web-1");

        let folded = FleetState::new(BACKEND).apply(
            &config,
            &clean_poll(
                &config,
                at(0),
                vec![host("web-1", at(0))],
                &[cpu("web-1", &[(at(0), 0.95)])],
            ),
        );
        assert_eq!(
            folded.state.evaluation(rule.id(), &host_id),
            Some(AlertState::Pending { since: at(0) })
        );

        // Within a dwell of the end of representable time. Nothing in the crate mints an instant
        // like this, and nothing has to: `DateTime<Utc>`'s serde impl accepts the whole range, so
        // a synced or hand-edited cache can carry one.
        let brink = DateTime::<Utc>::MAX_UTC - Duration::days(200);
        assert!(
            brink
                .checked_add_signed(AlertRule::MAX_FOR_DURATION)
                .is_none(),
            "the fixture must actually overflow, or this test proves nothing"
        );

        let mut cached: serde_json::Value =
            serde_json::from_str(&folded.state.to_json().expect("serialisable"))
                .expect("valid JSON");
        cached["evaluations"]["web-1"][rule.id().to_string()]["evaluation"]["state"] =
            serde_json::json!({ "Pending": { "since": brink } });
        let (restored, report) = FleetState::restore(BACKEND, &cached.to_string());
        assert_eq!(report.evaluations_restored, 1);

        // `view()` is the synchronous path an app calls on launch, before any poll. It used to
        // panic here, which crossed UniFFI as a crash on every single launch.
        let view = restored.view(&config);
        let alert = view
            .alerts
            .first()
            .expect("the rule is enabled and matches");
        assert_eq!(alert.phase, AlertPhase::Pending);
        assert_eq!(
            alert.breaching_since,
            Some(brink),
            "the breach is still reported; only the arithmetic that cannot be done is dropped"
        );
        assert_eq!(
            alert.fires_at, None,
            "an unrepresentable fire time reads as unknown, never as a crash"
        );
    }

    #[test]
    fn blind_gap_reset_emits_no_resolve_transition() {
        let rule = cpu_rule(60);
        let config = config().with_rules(vec![rule.clone()]);
        let roster = vec![host("web-1", at(0))];

        let mut state = FleetState::new(BACKEND);
        for offset in [0, 60] {
            let observation = clean_poll(
                &config,
                at(offset),
                roster.clone(),
                &[cpu("web-1", &[(at(offset), 0.95)])],
            );
            state = state.apply(&config, &observation).state;
        }

        let resumed = state.apply(
            &config,
            &clean_poll(
                &config,
                at(360),
                roster,
                &[cpu("web-1", &[(at(360), 0.95)])],
            ),
        );

        assert_eq!(resumed.transitions.len(), 1);
        let transition = &resumed.transitions[0];
        assert_eq!(transition.from, AlertPhase::Firing);
        assert_eq!(transition.to, AlertPhase::Pending);
        assert!(!transition.is_resolve());
        assert!(!transition.is_fire());
        assert!(resumed.transitions.iter().all(|edge| !edge.is_resolve()));
    }

    #[test]
    fn is_resolve_requires_to_ok() {
        let rule = cpu_rule(60);
        let edge = |from: AlertPhase, to: AlertPhase| AlertTransition {
            rule_id: rule.id().clone(),
            rule_name: rule.name.clone(),
            host: HostId::new("web-1"),
            from,
            to,
            breaching_since: None,
            at: at(0),
        };

        assert!(edge(AlertPhase::Firing, AlertPhase::Ok).is_resolve());
        assert!(!edge(AlertPhase::Firing, AlertPhase::NoData).is_resolve());
        assert!(!edge(AlertPhase::Firing, AlertPhase::Pending).is_resolve());
        assert!(!edge(AlertPhase::Pending, AlertPhase::Ok).is_resolve());
        assert!(edge(AlertPhase::Pending, AlertPhase::Firing).is_fire());
        assert!(!edge(AlertPhase::Firing, AlertPhase::Firing).is_fire());
    }

    #[test]
    fn a_host_absent_from_the_roster_reads_down_not_unknown() {
        let config = config();
        let first = FleetState::new(BACKEND).apply(
            &config,
            &clean_poll(&config, at(0), vec![host("web-1", at(0))], &[]),
        );
        let second = first.state.apply(
            &config,
            &clean_poll(&config, at(400), vec![host("web-2", at(400))], &[]),
        );

        let dropped = second.view.host(&HostId::new("web-1")).expect("retained");
        assert_eq!(dropped.liveness, Liveness::Down);
        assert!(!dropped.in_current_roster);
        assert_eq!(
            second
                .view
                .host(&HostId::new("web-2"))
                .expect("listed")
                .liveness,
            Liveness::Alive
        );
    }

    #[test]
    fn roster_merge_never_regresses_last_heartbeat() {
        let config = config();
        let first = FleetState::new(BACKEND).apply(
            &config,
            &clean_poll(&config, at(0), vec![host("web-1", at(0))], &[]),
        );
        // A clipped window returning an older bucket must not age a live host backwards.
        let second = first.state.apply(
            &config,
            &clean_poll(&config, at(30), vec![host("web-1", at(-60))], &[]),
        );

        assert_eq!(
            second
                .view
                .host(&HostId::new("web-1"))
                .expect("retained")
                .last_heartbeat,
            Some(at(0))
        );
    }

    #[test]
    fn roster_merge_does_not_clobber_os_to_other() {
        let config = config();
        let windows = Host::new(HostId::new("win-1"), OsFamily::Windows).with_last_heartbeat(at(0));
        let first = FleetState::new(BACKEND)
            .apply(&config, &clean_poll(&config, at(0), vec![windows], &[]));

        // A label set missing `os.type` arrives as `Other` and would otherwise turn this host's
        // load tiles from `Unsupported` into an unexplained gap.
        let unlabelled =
            Host::new(HostId::new("win-1"), OsFamily::Other).with_last_heartbeat(at(30));
        let second = first
            .state
            .apply(&config, &clean_poll(&config, at(30), vec![unlabelled], &[]));

        assert_eq!(
            second
                .view
                .host(&HostId::new("win-1"))
                .expect("retained")
                .os,
            OsFamily::Windows
        );
    }

    #[test]
    fn deleting_a_rule_drops_its_evaluations() {
        let rule = cpu_rule(60);
        let with_rule = config().with_rules(vec![rule.clone()]);
        let roster = vec![host("web-1", at(0))];
        let host_id = HostId::new("web-1");

        let first = FleetState::new(BACKEND).apply(
            &with_rule,
            &clean_poll(
                &with_rule,
                at(0),
                roster.clone(),
                &[cpu("web-1", &[(at(0), 0.95)])],
            ),
        );
        assert!(first.state.evaluation(rule.id(), &host_id).is_some());

        let without = config();
        let second = first
            .state
            .apply(&without, &clean_poll(&without, at(30), roster, &[]));

        assert_eq!(second.state.evaluation(rule.id(), &host_id), None);
        assert!(second.view.alerts.is_empty());
    }

    #[test]
    fn disabling_a_rule_drops_its_evaluation_rather_than_resetting_it_to_ok() {
        let rule = cpu_rule(60);
        let enabled = config().with_rules(vec![rule.clone()]);
        let roster = vec![host("web-1", at(0))];
        let host_id = HostId::new("web-1");

        let first = FleetState::new(BACKEND).apply(
            &enabled,
            &clean_poll(
                &enabled,
                at(0),
                roster.clone(),
                &[cpu("web-1", &[(at(0), 0.95)])],
            ),
        );

        // Core's `observe` returns `Ok` for a disabled rule, which would read as healthy. Dropping
        // the evaluation instead is what keeps that quirk out of the view.
        let disabled = config().with_rules(vec![rule.clone().disabled()]);
        let second = first.state.apply(
            &disabled,
            &clean_poll(
                &disabled,
                at(30),
                roster,
                &[cpu("web-1", &[(at(30), 0.95)])],
            ),
        );

        assert_eq!(second.state.evaluation(rule.id(), &host_id), None);
        assert!(second.view.alerts.is_empty());
    }

    #[test]
    fn editing_a_rule_drops_its_dwell() {
        let rule = cpu_rule(600);
        let before = config().with_rules(vec![rule.clone()]);
        let roster = vec![host("web-1", at(0))];
        let host_id = HostId::new("web-1");

        let first = FleetState::new(BACKEND).apply(
            &before,
            &clean_poll(
                &before,
                at(0),
                roster.clone(),
                &[cpu("web-1", &[(at(0), 0.95)])],
            ),
        );
        assert_eq!(
            first.state.evaluation(rule.id(), &host_id),
            Some(AlertState::Pending { since: at(0) })
        );

        let mut edited = rule.clone();
        edited.threshold = 0.5;
        let after = config().with_rules(vec![edited]);
        let second = first.state.apply(
            &after,
            &clean_poll(&after, at(30), roster, &[cpu("web-1", &[(at(30), 0.95)])]),
        );

        assert_eq!(
            second.state.evaluation(rule.id(), &host_id),
            Some(AlertState::Pending { since: at(30) }),
            "an edited rule must not inherit an old `since`"
        );
    }

    #[test]
    fn an_invalid_rule_shows_invalid_reason_and_is_not_evaluated() {
        // `AlertRule`'s fields are public and it derives `Deserialize`, so a NaN threshold arrives
        // intact from storage, makes every comparator return false, and the rule silently never
        // fires.
        let mut rule = cpu_rule(60);
        rule.threshold = f64::NAN;
        let config = config().with_rules(vec![rule.clone()]);

        let update = FleetState::new(BACKEND).apply(
            &config,
            &clean_poll(
                &config,
                at(0),
                vec![host("web-1", at(0))],
                &[cpu("web-1", &[(at(0), 0.95)])],
            ),
        );

        assert_eq!(
            update.state.evaluation(rule.id(), &HostId::new("web-1")),
            None
        );
        assert_eq!(update.view.alerts.len(), 1);
        let shown = &update.view.alerts[0];
        assert_eq!(shown.phase, AlertPhase::NoData);
        assert!(shown.invalid_reason.is_some());
    }

    #[test]
    fn forgetting_a_host_drops_its_series_and_evaluations() {
        let rule = cpu_rule(60);
        let config = config().with_rules(vec![rule.clone()]);
        let roster = vec![host("web-1", at(0)), host("web-2", at(0))];

        let first = FleetState::new(BACKEND).apply(
            &config,
            &clean_poll(&config, at(0), roster, &[cpu("web-1", &[(at(0), 0.95)])]),
        );

        let pruned = first.state.forget_host(&HostId::new("web-1"));

        assert_eq!(pruned.known_hosts(), vec![HostId::new("web-2")]);
        assert_eq!(pruned.evaluation(rule.id(), &HostId::new("web-1")), None);
        let view = pruned.view(&config);
        assert!(view.host(&HostId::new("web-1")).is_none());
        assert!(
            view.alerts
                .iter()
                .all(|alert| alert.host != HostId::new("web-1"))
        );
    }

    #[test]
    fn retained_hosts_are_capped_evicting_oldest_roster_sighting() {
        let tuning = PollTuning::new(
            pessimal_core::LivenessPolicy::default(),
            Duration::seconds(30),
            Duration::seconds(30),
            Duration::hours(1),
            Duration::seconds(150),
            Duration::hours(24),
            2,
        )
        .expect("every interlock holds");
        let config = FleetConfig::new("prod", tuning).expect("prod is usable");

        let first = FleetState::new(BACKEND).apply(
            &config,
            &clean_poll(
                &config,
                at(0),
                vec![host("web-1", at(0)), host("web-2", at(0))],
                &[],
            ),
        );
        // web-1 is absent from the second roster, so its sighting stays at t0 and it is the oldest.
        let second = first.state.apply(
            &config,
            &clean_poll(
                &config,
                at(30),
                vec![host("web-2", at(30)), host("web-3", at(30))],
                &[],
            ),
        );

        assert_eq!(
            second.state.known_hosts(),
            vec![HostId::new("web-2"), HostId::new("web-3")]
        );
    }

    #[test]
    fn a_partial_failure_keeps_the_roster_cadence() {
        let config = config();
        let update = FleetState::new(BACKEND).apply(
            &config,
            &poll_with(
                &config,
                at(0),
                RosterOutcome::Listed(vec![host("web-1", at(0))]),
                &[],
                &[(MetricKind::MemoryUtilization, unreachable())],
            ),
        );

        assert_eq!(
            update.advice,
            PollAdvice::Poll {
                after: Duration::seconds(30)
            },
            "a broken single metric must not slow liveness to a five-minute cadence"
        );
    }

    #[test]
    fn unauthorized_advises_stop_not_retry() {
        let config = config();
        let update = FleetState::new(BACKEND).apply(
            &config,
            &dead_poll(&config, at(0), &CoreError::Unauthorized),
        );

        match update.advice {
            PollAdvice::Stop { reason, .. } => assert_eq!(reason, PollFailureKind::Unauthorized),
            other => panic!("expected Stop, got {other:?}"),
        }
        assert!(!update.advice.should_poll());
        assert_eq!(update.advice.delay(), None);
    }

    #[test]
    fn unauthorized_on_a_series_query_alone_advises_stop() {
        let config = config();
        // One expired key does not fail selectively, so a 401 on a single query stops the poller
        // even though the roster listed happily.
        let update = FleetState::new(BACKEND).apply(
            &config,
            &poll_with(
                &config,
                at(0),
                RosterOutcome::Listed(vec![host("web-1", at(0))]),
                &[],
                &[(MetricKind::CpuUtilization, CoreError::Unauthorized)],
            ),
        );

        assert!(matches!(
            update.advice,
            PollAdvice::Stop {
                reason: PollFailureKind::Unauthorized,
                ..
            }
        ));
    }

    #[test]
    fn worst_failure_is_independent_of_outcome_order() {
        let config = config();
        let plan = plan_poll(&config, at(0)).expect("plans");
        let failure_for = |metric: MetricKind| {
            let error = if metric == MetricKind::CpuUtilization {
                unreachable()
            } else {
                CoreError::Backend("500".to_owned())
            };
            SeriesResult::Failed(PollFailure::from_core(
                &error,
                FailureSource::Series { metric },
                at(0),
            ))
        };

        let mut forward = PollObservation::new(at(0)).with_hosts(RosterOutcome::Listed(Vec::new()));
        let mut reverse = PollObservation::new(at(0)).with_hosts(RosterOutcome::Listed(Vec::new()));
        for spec in &plan.queries {
            forward = forward.with_series(spec.clone(), failure_for(spec.metric));
        }
        for spec in plan.queries.iter().rev() {
            reverse = reverse.with_series(spec.clone(), failure_for(spec.metric));
        }

        let state = FleetState::new(BACKEND);
        let one = state.apply(&config, &forward).view.freshness.last_failure;
        let other = state.apply(&config, &reverse).view.freshness.last_failure;

        assert_eq!(
            one, other,
            "the banner must not depend on which future finished first"
        );
        assert_eq!(one.expect("a failure").kind, PollFailureKind::Unreachable);
    }

    #[test]
    fn a_restored_state_advises_polling_immediately() {
        let config = config();
        let folded = FleetState::new(BACKEND)
            .apply(
                &config,
                &clean_poll(&config, at(0), vec![host("web-1", at(0))], &[]),
            )
            .state;

        let (restored, report) =
            FleetState::restore(BACKEND, &folded.to_json().expect("serialisable"));

        assert_eq!(report.hosts_restored, 1);
        assert_eq!(report.saved_as_of, Some(at(0)));
        assert_eq!(
            restored.advice(&config),
            PollAdvice::Poll {
                after: Duration::zero()
            },
            "neither a relaunch nor a cold start should sleep an interval before showing data"
        );
        assert_eq!(
            FleetState::new(BACKEND).advice(&config),
            PollAdvice::Poll {
                after: Duration::zero()
            }
        );
    }

    #[test]
    fn restoring_an_incompatible_state_starts_fresh_and_reports_it() {
        let (state, report) = FleetState::restore(BACKEND, r#"{"schema_version":999,"hosts":{}}"#);
        assert!(report.discarded_incompatible);
        assert!(!report.discarded_unreadable);
        assert!(state.known_hosts().is_empty());
        assert_eq!(state.backend_name(), BACKEND);

        let (also_fresh, unreadable) = FleetState::restore(BACKEND, "not json at all");
        assert!(unreadable.discarded_unreadable);
        assert!(!unreadable.discarded_incompatible);
        assert!(also_fresh.known_hosts().is_empty());
    }

    #[test]
    fn applying_a_clean_observation_twice_is_idempotent() {
        let rule = cpu_rule(600);
        let config = config().with_rules(vec![rule]);
        let observation = clean_poll(
            &config,
            at(0),
            vec![host("web-1", at(0))],
            &[cpu("web-1", &[(at(0), 0.95)])],
        );

        let first = FleetState::new(BACKEND).apply(&config, &observation);
        let second = first.state.apply(&config, &observation);

        assert_eq!(first.state, second.state);
        assert!(
            second.transitions.is_empty(),
            "only the clean case is idempotent, and nothing moved"
        );
    }

    // ---- deferred from `view.rs`, which cannot build a fold ------------------------------------

    #[test]
    fn a_frozen_metric_keeps_its_older_fetched_at() {
        let config = config();
        let roster = vec![host("web-1", at(0))];
        let first = FleetState::new(BACKEND).apply(
            &config,
            &clean_poll(
                &config,
                at(0),
                roster.clone(),
                &[cpu("web-1", &[(at(0), 0.42)])],
            ),
        );

        let second = first.state.apply(
            &config,
            &poll_with(
                &config,
                at(30),
                RosterOutcome::Listed(roster),
                &[],
                &[(MetricKind::CpuUtilization, unreachable())],
            ),
        );

        let host_view = second.view.host(&HostId::new("web-1")).expect("retained");
        let cpu_view = host_view
            .metrics
            .iter()
            .find(|metric| metric.kind == MetricKind::CpuUtilization)
            .expect("a CPU tile");
        assert_eq!(cpu_view.availability, MetricAvailability::Unavailable);
        assert_eq!(cpu_view.fetched_at, Some(at(0)));
        assert_eq!(cpu_view.points.len(), 1);
        assert_eq!(second.view.as_of, Some(at(30)));
    }

    #[test]
    fn collection_health_is_failing_when_the_delta_is_positive() {
        let config = config();
        let failures = series(
            "web-1",
            MetricKind::AgentCollectionFailures,
            &[(at(-30), 1.0), (at(0), 4.0)],
        );

        let update = FleetState::new(BACKEND).apply(
            &config,
            &clean_poll(&config, at(0), vec![host("web-1", at(0))], &[failures]),
        );

        let host_view = update.view.host(&HostId::new("web-1")).expect("listed");
        match host_view.collection {
            CollectionHealth::Failing { added, over } => {
                assert!((added - 3.0).abs() < f64::EPSILON);
                assert_eq!(over, Duration::seconds(30));
            }
            other => panic!("expected Failing, got {other:?}"),
        }
    }

    #[test]
    fn the_alert_sort_is_firing_first_and_stable() {
        let hot = cpu_rule(0);
        let memory = AlertRule::new(
            "prod",
            "Memory hot",
            MetricKind::MemoryUtilization,
            Comparator::GreaterThan,
            0.9,
            Duration::zero(),
        )
        .expect("a valid rule");
        let config = config().with_rules(vec![memory.clone(), hot.clone()]);

        // web-1 breaches CPU (Firing) and reports nothing for memory (NoData); web-2 reports a
        // healthy CPU (Ok) and nothing for memory.
        let update = FleetState::new(BACKEND).apply(
            &config,
            &clean_poll(
                &config,
                at(0),
                vec![host("web-1", at(0)), host("web-2", at(0))],
                &[
                    cpu("web-1", &[(at(0), 0.99)]),
                    cpu("web-2", &[(at(0), 0.10)]),
                ],
            ),
        );

        let ordering: Vec<(AlertPhase, String)> = update
            .view
            .alerts
            .iter()
            .map(|alert| (alert.phase, alert.host.as_str().to_owned()))
            .collect();
        assert_eq!(
            ordering,
            vec![
                (AlertPhase::Firing, "web-1".to_owned()),
                (AlertPhase::NoData, "web-1".to_owned()),
                (AlertPhase::NoData, "web-2".to_owned()),
                (AlertPhase::Ok, "web-2".to_owned()),
            ]
        );
        assert_eq!(update.view.alerts[0].rule_id, *hot.id());
        assert_eq!(update.view.alerts[1].rule_id, *memory.id());
    }

    #[test]
    fn views_carry_stable_ids() {
        let rule = cpu_rule(60);
        let config = config().with_rules(vec![rule]);
        let observation = clean_poll(
            &config,
            at(0),
            vec![host("web-1", at(0)), host("web-2", at(0))],
            &[
                cpu("web-1", &[(at(0), 0.95)]),
                cpu("web-2", &[(at(0), 0.10)]),
            ],
        );

        let first = FleetState::new(BACKEND).apply(&config, &observation);
        let second = first.state.apply(&config, &observation);

        let ids = |update: &super::FleetUpdate| -> Vec<String> {
            let mut ids: Vec<String> = update
                .view
                .hosts
                .iter()
                .flat_map(|host| host.metrics.iter().map(|metric| metric.id.clone()))
                .collect();
            ids.extend(update.view.alerts.iter().map(|alert| alert.id.clone()));
            ids
        };

        let before = ids(&first);
        assert_eq!(before, ids(&second), "ids must not drift between folds");
        assert_eq!(
            before.iter().collect::<BTreeSet<_>>().len(),
            before.len(),
            "every id is unique"
        );
    }

    #[test]
    fn fleet_counts_match_the_host_list() {
        let rule = cpu_rule(0);
        let config = config().with_rules(vec![rule]);
        let roster = vec![
            host("alive-1", at(0)),
            host("stale-1", at(-120)),
            host("down-1", at(-200)),
            Host::new(HostId::new("silent-1"), OsFamily::Linux),
        ];

        let update = FleetState::new(BACKEND).apply(
            &config,
            &clean_poll(&config, at(0), roster, &[cpu("alive-1", &[(at(0), 0.99)])]),
        );

        let counts = update.view.counts;
        assert_eq!(counts.hosts, 4);
        assert_eq!(counts.alive, 1);
        assert_eq!(counts.stale, 1);
        assert_eq!(counts.down, 1);
        assert_eq!(counts.unknown, 1);
        assert_eq!(
            counts.hosts,
            super::count(update.view.hosts.len()),
            "the counts and the list are the same fleet"
        );
        assert_eq!(counts.firing_alerts, 1);
        assert_eq!(
            counts.firing_alerts,
            super::count(update.view.firing().len())
        );
        assert_eq!(counts.pending_alerts, 0);
    }

    #[test]
    fn unknown_host_in_roster_raises_a_warning() {
        let config = config();
        let update = FleetState::new(BACKEND).apply(
            &config,
            &clean_poll(
                &config,
                at(0),
                vec![host(PLACEHOLDER_HOST_ID, at(0)), host("web-1", at(0))],
                &[],
            ),
        );

        assert!(
            update
                .view
                .host(&HostId::new(PLACEHOLDER_HOST_ID))
                .expect("retained")
                .id_is_placeholder,
            "several unrelated machines collide into this row and nothing else can detect it"
        );
        assert!(
            !update
                .view
                .host(&HostId::new("web-1"))
                .expect("retained")
                .id_is_placeholder
        );
    }

    #[test]
    fn a_selector_that_matches_no_host_creates_no_evaluations() {
        let rule = cpu_rule(60).with_selector(HostSelector::AnyOf(vec![HostId::new("other")]));
        let config = config().with_rules(vec![rule.clone()]);

        let update = FleetState::new(BACKEND).apply(
            &config,
            &clean_poll(
                &config,
                at(0),
                vec![host("web-1", at(0))],
                &[cpu("web-1", &[(at(0), 0.95)])],
            ),
        );

        assert_eq!(
            update.state.evaluation(rule.id(), &HostId::new("web-1")),
            None
        );
        assert!(update.view.alerts.is_empty());
    }

    #[test]
    fn a_multi_mount_alert_names_the_dominant_series() {
        let disk = AlertRule::new(
            "prod",
            "Disk full",
            MetricKind::FilesystemUtilization,
            Comparator::GreaterThan,
            0.9,
            Duration::zero(),
        )
        .expect("a valid rule");
        let config = config().with_rules(vec![disk]);

        let mut root = BTreeMap::new();
        root.insert("system.filesystem.mountpoint".to_owned(), "/".to_owned());
        let mut data = BTreeMap::new();
        data.insert(
            "system.filesystem.mountpoint".to_owned(),
            "/data".to_owned(),
        );

        let update = FleetState::new(BACKEND).apply(
            &config,
            &clean_poll(
                &config,
                at(0),
                vec![host("web-1", at(0))],
                &[
                    series("web-1", MetricKind::FilesystemUtilization, &[(at(0), 0.40)])
                        .with_attributes(root),
                    series("web-1", MetricKind::FilesystemUtilization, &[(at(0), 0.95)])
                        .with_attributes(data),
                ],
            ),
        );

        let alert = &update.view.alerts[0];
        assert_eq!(alert.phase, AlertPhase::Firing);
        assert_eq!(alert.series_label, Some("/data".to_owned()));
        assert!((alert.latest_value.expect("a judged value") - 0.95).abs() < f64::EPSILON);
    }

    #[test]
    fn tied_mounts_name_the_same_series_however_the_backend_ordered_them() {
        let disk = AlertRule::new(
            "prod",
            "Disk full",
            MetricKind::FilesystemUtilization,
            Comparator::GreaterThan,
            0.9,
            Duration::zero(),
        )
        .expect("a valid rule");
        let config = config().with_rules(vec![disk]);

        // A bind mount reports `/` and `/data` identically. `query_series` promises no order, so
        // the two arrive either way round and the firing row must still name one mount.
        let root = series("web-1", MetricKind::FilesystemUtilization, &[(at(0), 0.95)])
            .with_attributes(mount("/"));
        let data = series("web-1", MetricKind::FilesystemUtilization, &[(at(0), 0.95)])
            .with_attributes(mount("/data"));

        let first = FleetState::new(BACKEND).apply(
            &config,
            &clean_poll(
                &config,
                at(0),
                vec![host("web-1", at(0))],
                &[root.clone(), data.clone()],
            ),
        );
        let second = FleetState::new(BACKEND).apply(
            &config,
            &clean_poll(&config, at(0), vec![host("web-1", at(0))], &[data, root]),
        );

        assert_eq!(first.view.alerts[0].phase, AlertPhase::Firing);
        assert_eq!(
            first.view.alerts[0].series_label, second.view.alerts[0].series_label,
            "the subtitle must not alternate between two mounts that never changed"
        );
        assert_eq!(first.view.alerts[0].series_label, Some("/".to_owned()));
    }
}
