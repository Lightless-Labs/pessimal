//! What one poll produced, in boundary shapes: the result Swift gets back, the transitions worth a
//! notification, the advice that says when to come back, why a request failed, and what a cold
//! start salvaged.
//!
//! These mirror `pessimal_client_core::fold`'s outputs and the failure trio from
//! `pessimal_client_core::error`. Nothing here decides anything: every predicate this module
//! carries — `is_fire` on a transition, `is_actionable` on a failure — is *core's*
//! answer, obtained by calling core's own method, never re-derived from the mirrored fields. The
//! alternative is a copy of a subtle rule on the Swift side, and the two rules this module carries
//! are exactly the two that a re-implementation gets wrong.
//!
//! # Failure data is not the thrown error
//!
//! [`crate::convert::FfiError`] is the *one* `uniffi::Error`, and it appears in return position
//! only. [`PollFailureRecord`] is failure *data* and appears in field position only — on a poll
//! result's view, and inside a `PollAdviceRecord::Stop`. A type cannot be both: `UniFFI` lifts an
//! error and lowers a record through different machinery, and mirroring one enum into both roles
//! is how the two mirrors rot apart. A failed *request* is therefore never an exception on the
//! Swift side; it is a field on a view that still renders.
//!
//! # Names
//!
//! The `PollFailure*` trio carries a `Poll` prefix it does not have in core. Records share one flat
//! Swift namespace across the whole crate, and `FailureSource` — or worse, `Kind` — is the sort of
//! name a later layer collides with by accident. Prefixed, the three read as one family and collide
//! with nothing.
//!
//! [`PollResult`] is the one name here *without* a `Record` suffix, because section 4.11 of the M3
//! plan spells the session's signature `async fn poll(&self, now_millis: i64) ->
//! Result<PollResult, FfiError>`. The boundary was specified there so it would not be invented
//! later, and renaming it here would be inventing it.

use pessimal_client_core::{
    AlertPhase, AlertTransition, FailureSource, PollAdvice, PollFailure, PollFailureKind,
    RestoreReport,
};
use pessimal_core::MetricKind;

use crate::convert::{
    FfiError, datetime_to_millis, duration_to_seconds, host_id_from_string, host_id_to_string,
    millis_to_datetime, optional_datetime_to_millis, optional_millis_to_datetime,
    seconds_to_duration, urn_from_string, urn_to_string,
};
// `MetricKindRecord` is imported from the module that declares it rather than through
// `view_records`' re-export, so this module does not depend on that re-export staying.
use crate::config_records::MetricKindRecord;
use crate::view_records::{AlertPhaseRecord, FleetViewRecord};

/// What kind of thing went wrong, coarse enough for the app to route on.
///
/// Mirrors [`PollFailureKind`]. Kept as four distinct cases rather than collapsed into a flag,
/// because the case *is* the routing decision: `Unauthorized` is the one that offers "Open
/// Settings" and stops the timer, `Unreachable` and `Backend` back off and retry, and `Malformed`
/// is neither — a metric name the backend does not know will still be unknown next poll, and no
/// settings screen repairs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, uniffi::Enum)]
pub enum PollFailureKindRecord {
    /// The credentials are wrong, missing, or expired.
    Unauthorized,
    /// We never reached the backend: DNS, TLS, a dead network.
    Unreachable,
    /// The backend answered, unhappily. A 500, a query rejection.
    Backend,
    /// The backend answered something we could not turn into domain values.
    Malformed,
}

impl From<PollFailureKind> for PollFailureKindRecord {
    fn from(kind: PollFailureKind) -> Self {
        match kind {
            PollFailureKind::Unauthorized => Self::Unauthorized,
            PollFailureKind::Unreachable => Self::Unreachable,
            PollFailureKind::Backend => Self::Backend,
            PollFailureKind::Malformed => Self::Malformed,
        }
    }
}

impl From<PollFailureKindRecord> for PollFailureKind {
    fn from(record: PollFailureKindRecord) -> Self {
        match record {
            PollFailureKindRecord::Unauthorized => Self::Unauthorized,
            PollFailureKindRecord::Unreachable => Self::Unreachable,
            PollFailureKindRecord::Backend => Self::Backend,
            PollFailureKindRecord::Malformed => Self::Malformed,
        }
    }
}

/// Which request failed, so a banner can name the metric instead of saying "a query".
///
/// Mirrors [`FailureSource`]. The metric travels as a mirrored enum rather than a display string:
/// the app looks its unit, display name and rate-ness up through `describe_metric`, and a string
/// here would force it to parse one back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum PollFailureSourceRecord {
    /// `list_hosts`. The one request whose failure freezes the whole picture.
    Roster,
    /// One metric query.
    Series { metric: MetricKindRecord },
}

impl From<FailureSource> for PollFailureSourceRecord {
    fn from(source: FailureSource) -> Self {
        match source {
            FailureSource::Roster => Self::Roster,
            FailureSource::Series { metric } => Self::Series {
                metric: MetricKindRecord::from(metric),
            },
        }
    }
}

impl From<PollFailureSourceRecord> for FailureSource {
    fn from(record: PollFailureSourceRecord) -> Self {
        match record {
            PollFailureSourceRecord::Roster => Self::Roster,
            PollFailureSourceRecord::Series { metric } => Self::Series {
                metric: MetricKind::from(metric),
            },
        }
    }
}

/// One request's failure, as data rather than as a thrown error.
///
/// Mirrors [`PollFailure`], which is the only failure type in core that ever sits in a struct
/// field. It reaches the app on the view's freshness inputs and inside
/// [`PollAdviceRecord::Stop`] — never as an exception, because a poll that failed still has a last
/// good fleet to draw and blanking it would turn "I cannot see" into "your fleet is down".
///
/// `is_actionable` and `is_transient` are core's predicates, carried rather than left for Swift to
/// re-derive from `kind`. They are not the same question and not each other's negation —
/// `Malformed` answers `false` to both — and a `switch` in the UI that got that three-way split
/// wrong would either burn battery retrying an unfixable query or hide a real auth cliff.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct PollFailureRecord {
    pub kind: PollFailureKindRecord,
    pub source: PollFailureSourceRecord,
    /// The domain error's rendered message, for the banner. Not parsed, on either side.
    pub message: String,
    /// Every failure in one poll is stamped at that poll's single instant.
    pub at_millis: i64,
    /// True only for `Unauthorized`: retrying cannot help, the user must change something.
    pub is_actionable: bool,
    /// True when coming back later could plausibly succeed with nothing else changed.
    pub is_transient: bool,
}

impl From<&PollFailure> for PollFailureRecord {
    fn from(failure: &PollFailure) -> Self {
        let PollFailure {
            kind,
            source,
            message,
            at,
        } = failure;
        Self {
            kind: PollFailureKindRecord::from(*kind),
            source: PollFailureSourceRecord::from(*source),
            message: message.clone(),
            at_millis: datetime_to_millis(*at),
            is_actionable: kind.is_actionable(),
            is_transient: kind.is_transient(),
        }
    }
}

/// By value for callers that own the failure; the work is the `&PollFailure` impl.
impl From<PollFailure> for PollFailureRecord {
    fn from(failure: PollFailure) -> Self {
        Self::from(&failure)
    }
}

impl TryFrom<PollFailureRecord> for PollFailure {
    type Error = FfiError;

    /// The derived predicates are dropped rather than checked: `PollFailureKind` recomputes them,
    /// and comparing them here would be this crate second-guessing core's own answer.
    ///
    /// # Errors
    /// [`FfiError::Internal`] when `at_millis` is not a representable instant.
    fn try_from(record: PollFailureRecord) -> Result<Self, Self::Error> {
        let PollFailureRecord {
            kind,
            source,
            message,
            at_millis,
            is_actionable: _,
            is_transient: _,
        } = record;
        Ok(Self {
            kind: PollFailureKind::from(kind),
            source: FailureSource::from(source),
            message,
            at: millis_to_datetime(at_millis)?,
        })
    }
}

/// An alert crossing a phase boundary — the only thing worth waking a user for.
///
/// Mirrors [`AlertTransition`]. `host` becomes `host_id` because the boundary carries the id's
/// string and the name should say so; everything else keeps core's field names.
///
/// `is_fire` and `is_resolve` are core's predicates, carried deliberately. `is_resolve` is
/// `Firing -> Ok` and *nothing looser*: a blind-gap reset drives a firing evaluation to `NoData`
/// and then back to `Pending`, so a Swift-side `from == .firing` test would post a spurious
/// "resolved" for every still-breaching alert on every app resume and re-fire one dwell later.
/// That rule belongs in exactly one place, and this is how it gets there.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct AlertTransitionRecord {
    /// The rule's URN as its canonical string.
    pub rule_id: String,
    pub rule_name: String,
    pub host_id: String,
    pub from: AlertPhaseRecord,
    pub to: AlertPhaseRecord,
    /// When the BREACH started, not when the alert fired.
    pub breaching_since_millis: Option<i64>,
    /// The poll instant the crossing was observed at.
    pub at_millis: i64,
    /// `to` is `Firing` and `from` is not. The edge worth a notification.
    pub is_fire: bool,
    /// `from` was `Firing` and `to` is `Ok`. The edge worth withdrawing one.
    pub is_resolve: bool,
}

impl From<&AlertTransition> for AlertTransitionRecord {
    fn from(transition: &AlertTransition) -> Self {
        // Asked before the destructure, because they are methods on the whole transition rather
        // than functions of any one field.
        let is_fire = transition.is_fire();
        let is_resolve = transition.is_resolve();
        let AlertTransition {
            rule_id,
            rule_name,
            host,
            from,
            to,
            breaching_since,
            at,
        } = transition;
        Self {
            rule_id: urn_to_string(rule_id),
            rule_name: rule_name.clone(),
            host_id: host_id_to_string(host),
            from: AlertPhaseRecord::from(*from),
            to: AlertPhaseRecord::from(*to),
            breaching_since_millis: optional_datetime_to_millis(*breaching_since),
            at_millis: datetime_to_millis(*at),
            is_fire,
            is_resolve,
        }
    }
}

/// By value for callers that own the transition; the work is the `&AlertTransition` impl.
impl From<AlertTransition> for AlertTransitionRecord {
    fn from(transition: AlertTransition) -> Self {
        Self::from(&transition)
    }
}

impl TryFrom<AlertTransitionRecord> for AlertTransition {
    type Error = FfiError;

    /// # Errors
    /// [`FfiError::InvalidUrn`] when `rule_id` is not a URN; [`FfiError::Internal`] when either
    /// instant is not representable.
    fn try_from(record: AlertTransitionRecord) -> Result<Self, Self::Error> {
        let AlertTransitionRecord {
            rule_id,
            rule_name,
            host_id,
            from,
            to,
            breaching_since_millis,
            at_millis,
            is_fire: _,
            is_resolve: _,
        } = record;
        Ok(Self {
            rule_id: urn_from_string(&rule_id)?,
            rule_name,
            host: host_id_from_string(host_id),
            from: AlertPhase::from(from),
            to: AlertPhase::from(to),
            breaching_since: optional_millis_to_datetime(breaching_since_millis)?,
            at: millis_to_datetime(at_millis)?,
        })
    }
}

/// One poll's transitions, in the order the fold emitted them.
///
/// A named function rather than a `map` at the call site so the ordering has somewhere to be
/// stated: the fold sorts by `(host, rule_id)` and the app's notification scheduler depends on
/// that being stable between polls. Nothing here re-sorts.
#[must_use]
pub fn transitions_to_records(transitions: &[AlertTransition]) -> Vec<AlertTransitionRecord> {
    transitions
        .iter()
        .map(AlertTransitionRecord::from)
        .collect()
}

/// When to come back, and whether to come back at all.
///
/// Mirrors [`PollAdvice`]. Core owns the policy and Swift owns the timer, so this crosses as three
/// cases the app switches on rather than as a nullable delay: "poll again in n seconds" and
/// "stop, and tell the user why" are different instructions, and a single `Option<i64>` would
/// make the second one indistinguishable from a missing value.
///
/// Durations cross as whole seconds. `Poll { after_seconds: 0 }` is load-bearing rather than a
/// degenerate case: a state that has not folded this session — a relaunch, a cold start — advises
/// zero so the app shows data instead of sleeping an interval first.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum PollAdviceRecord {
    /// A clean poll, or one where the roster listed and some series failed. Only a failed roster
    /// backs off; a single broken metric must never slow liveness to a five-minute cadence.
    Poll { after_seconds: i64 },
    /// The roster failed. The delay is core's deterministic, unjittered backoff; Swift may add
    /// jitter, core stays reproducible so a schedule is exact in a test.
    Retry {
        after_seconds: i64,
        consecutive_failures: u32,
    },
    /// Retrying cannot help. Emitted whenever any failure this poll carried `Unauthorized`.
    Stop {
        reason: PollFailureKindRecord,
        message: String,
    },
}

impl From<&PollAdvice> for PollAdviceRecord {
    fn from(advice: &PollAdvice) -> Self {
        match advice {
            PollAdvice::Poll { after } => Self::Poll {
                after_seconds: duration_to_seconds(*after),
            },
            PollAdvice::Retry {
                after,
                consecutive_failures,
            } => Self::Retry {
                after_seconds: duration_to_seconds(*after),
                consecutive_failures: *consecutive_failures,
            },
            PollAdvice::Stop { reason, message } => Self::Stop {
                reason: PollFailureKindRecord::from(*reason),
                message: message.clone(),
            },
        }
    }
}

/// By value for callers that own the advice; the work is the `&PollAdvice` impl.
impl From<PollAdvice> for PollAdviceRecord {
    fn from(advice: PollAdvice) -> Self {
        Self::from(&advice)
    }
}

impl TryFrom<PollAdviceRecord> for PollAdvice {
    type Error = FfiError;

    /// # Errors
    /// [`FfiError::Internal`] when a delay is too large for `chrono::Duration`.
    fn try_from(record: PollAdviceRecord) -> Result<Self, Self::Error> {
        match record {
            PollAdviceRecord::Poll { after_seconds } => Ok(Self::Poll {
                after: seconds_to_duration(after_seconds)?,
            }),
            PollAdviceRecord::Retry {
                after_seconds,
                consecutive_failures,
            } => Ok(Self::Retry {
                after: seconds_to_duration(after_seconds)?,
                consecutive_failures,
            }),
            PollAdviceRecord::Stop { reason, message } => Ok(Self::Stop {
                reason: PollFailureKind::from(reason),
                message,
            }),
        }
    }
}

/// What a cold start could and could not salvage.
///
/// Mirrors [`RestoreReport`]. The session holds it and hands it over on request, because
/// `FleetState::restore` returns a tuple and tuples do not cross the boundary.
///
/// Reported rather than raised: a monitoring app must not refuse to launch because its cache is a
/// schema behind. The two `discarded_*` flags are separate cases because they mean different
/// things to whoever reads a bug report — one says we shipped a schema change, the other says the
/// file was truncated or corrupted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct RestoreReportRecord {
    /// True when the cache's schema version did not match and everything was discarded.
    pub discarded_incompatible: bool,
    /// True when the cache was not valid JSON.
    pub discarded_unreadable: bool,
    pub hosts_restored: u32,
    pub evaluations_restored: u32,
    /// The `as_of` the discarded-or-restored cache was written with. `None`, not `0`: a cache that
    /// never polled and a cache written at the Unix epoch must not render the same.
    pub saved_as_of_millis: Option<i64>,
}

impl From<RestoreReport> for RestoreReportRecord {
    fn from(report: RestoreReport) -> Self {
        let RestoreReport {
            discarded_incompatible,
            discarded_unreadable,
            hosts_restored,
            evaluations_restored,
            saved_as_of,
        } = report;
        Self {
            discarded_incompatible,
            discarded_unreadable,
            hosts_restored,
            evaluations_restored,
            saved_as_of_millis: optional_datetime_to_millis(saved_as_of),
        }
    }
}

impl TryFrom<RestoreReportRecord> for RestoreReport {
    type Error = FfiError;

    /// # Errors
    /// [`FfiError::Internal`] when `saved_as_of_millis` is not a representable instant.
    fn try_from(record: RestoreReportRecord) -> Result<Self, Self::Error> {
        let RestoreReportRecord {
            discarded_incompatible,
            discarded_unreadable,
            hosts_restored,
            evaluations_restored,
            saved_as_of_millis,
        } = record;
        Ok(Self {
            discarded_incompatible,
            discarded_unreadable,
            hosts_restored,
            evaluations_restored,
            saved_as_of: optional_millis_to_datetime(saved_as_of_millis)?,
        })
    }
}

/// What the session returns from one `poll`.
///
/// `FleetUpdate`'s `state` field, the next `FleetState`, is deliberately absent: it goes back into
/// the session's mutex and never crosses. Everything the app needs is here.
///
/// `skipped` is a field rather than an error because a skipped poll is a *normal* outcome, not a
/// failure. A menu-bar timer tick and a pull-to-refresh will collide, and the session answers the
/// loser with the current view instead of folding twice onto the same base state and silently
/// discarding one poll's HTTP. A thrown error there would make the app show a banner for
/// something that went right.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct PollResult {
    /// The fleet as it now stands. Present on a skipped poll too — the app redraws from it either
    /// way, and handing back nothing would blank a screen that has perfectly good data on it.
    pub view: FleetViewRecord,
    /// Phase crossings worth a notification, sorted by `(host, rule_id)`. Empty when nothing
    /// crossed, and empty on a skipped poll.
    pub transitions: Vec<AlertTransitionRecord>,
    /// When to come back. Always present, including on a skipped poll: the timer has to be
    /// rescheduled from something, and the advice the unchanged state already carries is the
    /// honest answer.
    pub advice: PollAdviceRecord,
    /// True when another poll was already in flight and this call folded nothing.
    pub skipped: bool,
}

impl PollResult {
    /// A poll that ran: core's view, core's transitions, core's advice.
    #[must_use]
    pub fn folded(
        view: FleetViewRecord,
        transitions: Vec<AlertTransitionRecord>,
        advice: PollAdviceRecord,
    ) -> Self {
        Self {
            view,
            transitions,
            advice,
            skipped: false,
        }
    }

    /// A poll that found another already in flight.
    ///
    /// Takes no transitions, and the signature is what enforces that: no fold happened, so there
    /// is no crossing to report, and a caller cannot accidentally pass the previous poll's list
    /// and have the app notify twice for one edge.
    #[must_use]
    pub fn skipped_in_flight(view: FleetViewRecord, advice: PollAdviceRecord) -> Self {
        Self {
            view,
            transitions: Vec::new(),
            advice,
            skipped: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, Duration, Utc};
    use pessimal_client_core::{
        AlertPhase, AlertTransition, FailureSource, PollAdvice, PollFailure, PollFailureKind,
        RestoreReport,
    };
    use pessimal_core::{HostId, MetricKind, Urn};

    use crate::view_records::AlertPhaseRecord;

    use super::{
        AlertTransitionRecord, PollAdviceRecord, PollFailureKindRecord, PollFailureRecord,
        PollFailureSourceRecord, RestoreReportRecord, transitions_to_records,
    };

    const ALL_KINDS: [PollFailureKind; 4] = [
        PollFailureKind::Unauthorized,
        PollFailureKind::Unreachable,
        PollFailureKind::Backend,
        PollFailureKind::Malformed,
    ];

    const ALL_PHASES: [AlertPhase; 4] = [
        AlertPhase::Ok,
        AlertPhase::Pending,
        AlertPhase::Firing,
        AlertPhase::NoData,
    ];

    fn at() -> DateTime<Utc> {
        DateTime::from_timestamp(1_757_000_000, 0).expect("a valid fixture instant")
    }

    fn rule_urn() -> Urn {
        "pessimal::prod::fleet::rule::0192f000-0000-7000-8000-000000000001"
            .parse::<Urn>()
            .expect("a valid fixture URN")
    }

    fn transition(from: AlertPhase, to: AlertPhase) -> AlertTransition {
        AlertTransition {
            rule_id: rule_urn(),
            rule_name: "cpu hot".to_owned(),
            host: HostId::from("web-1".to_owned()),
            from,
            to,
            breaching_since: Some(at() - Duration::minutes(5)),
            at: at(),
        }
    }

    /// Every [`PollFailureKind`] variant must survive the trip, and the bitmask catches the other
    /// half of the problem: a variant named in the `match` but missing from `ALL_KINDS`. Adding a
    /// variant to core then fails here rather than in an app build, where it surfaces as a
    /// non-exhaustive Swift `switch` in a file nobody touched.
    #[test]
    fn every_poll_failure_kind_round_trips() {
        let mut covered = 0_u8;
        for kind in ALL_KINDS {
            let record = PollFailureKindRecord::from(kind);
            assert_eq!(PollFailureKind::from(record), kind, "{kind:?} round trips");
            covered |= match kind {
                PollFailureKind::Unauthorized => 0b0001_u8,
                PollFailureKind::Unreachable => 0b0010,
                PollFailureKind::Backend => 0b0100,
                PollFailureKind::Malformed => 0b1000,
            };
        }
        assert_eq!(covered, 0b1111, "the fixture list must name every kind");
    }

    /// The derived predicates must be core's answers, not a plausible re-derivation. Asserted
    /// against core's own methods for every kind so the three-way split — actionable, transient,
    /// neither — cannot drift.
    #[test]
    fn derived_failure_predicates_are_cores() {
        for kind in ALL_KINDS {
            let record = PollFailureRecord::from(&failure(kind, FailureSource::Roster, "boom"));
            assert_eq!(record.is_actionable, kind.is_actionable(), "{kind:?}");
            assert_eq!(record.is_transient, kind.is_transient(), "{kind:?}");
        }
    }

    fn failure(kind: PollFailureKind, source: FailureSource, message: &str) -> PollFailure {
        PollFailure {
            kind,
            source,
            message: message.to_owned(),
            at: at(),
        }
    }

    /// `Roster` and a `Series` for every [`MetricKind`], because the payload is the half of this
    /// enum a hand-written mirror drops.
    #[test]
    fn every_failure_source_round_trips() {
        let mut covered = 0_u8;
        let sources = std::iter::once(FailureSource::Roster).chain(
            MetricKind::ALL
                .into_iter()
                .map(|metric| FailureSource::Series { metric }),
        );
        for source in sources {
            let record = PollFailureSourceRecord::from(source);
            assert_eq!(
                FailureSource::from(record),
                source,
                "{source:?} round trips"
            );
            covered |= match source {
                FailureSource::Roster => 0b01_u8,
                FailureSource::Series { .. } => 0b10,
            };
        }
        assert_eq!(covered, 0b11, "both source shapes must be exercised");
    }

    #[test]
    fn a_poll_failure_round_trips() {
        for source in [
            FailureSource::Roster,
            FailureSource::Series {
                metric: MetricKind::FilesystemUtilization,
            },
        ] {
            let original = failure(PollFailureKind::Backend, source, "500 from SigNoz");
            let record = PollFailureRecord::from(&original);
            let restored =
                PollFailure::try_from(record.clone()).expect("a fixture instant is representable");
            assert_eq!(restored, original);
            // By value and by reference must agree; one is implemented in terms of the other and
            // that is worth pinning rather than assuming.
            assert_eq!(PollFailureRecord::from(original), record);
        }
    }

    /// All sixteen phase pairs, not only the four interesting ones: `is_resolve` is the predicate
    /// a Swift re-implementation gets wrong, and it is wrong on `Firing -> NoData`, which only a
    /// full sweep catches.
    #[test]
    fn an_alert_transition_carries_cores_edges_and_round_trips() {
        for from in ALL_PHASES {
            for to in ALL_PHASES {
                let original = transition(from, to);
                let record = AlertTransitionRecord::from(&original);
                assert_eq!(
                    record.is_fire,
                    original.is_fire(),
                    "{from:?} -> {to:?} fire"
                );
                assert_eq!(
                    record.is_resolve,
                    original.is_resolve(),
                    "{from:?} -> {to:?} resolve"
                );
                let restored = AlertTransition::try_from(record)
                    .expect("a fixture URN and instant are representable");
                assert_eq!(restored, original, "{from:?} -> {to:?} round trips");
            }
        }
    }

    /// The two edges the app actually notifies on, spelled out so the mapping cannot be satisfied
    /// by a pair of always-false booleans that round-trip perfectly well.
    #[test]
    fn only_the_intended_edges_fire_and_resolve() {
        let fires =
            AlertTransitionRecord::from(&transition(AlertPhase::Pending, AlertPhase::Firing));
        assert!(fires.is_fire);
        assert!(!fires.is_resolve);

        let resolves = AlertTransitionRecord::from(&transition(AlertPhase::Firing, AlertPhase::Ok));
        assert!(resolves.is_resolve);
        assert!(!resolves.is_fire);

        // A blind-gap reset. Neither edge: notifying here would withdraw an alert that is still
        // breaching and re-post it one dwell later.
        let reset =
            AlertTransitionRecord::from(&transition(AlertPhase::Firing, AlertPhase::NoData));
        assert!(!reset.is_fire);
        assert!(!reset.is_resolve);
    }

    /// `from` and `to` must not be swapped. A swap survives a round trip and every count, and
    /// shows up only as a notification that says "resolved" when the alert just started.
    #[test]
    fn an_alert_transitions_direction_is_not_swapped() {
        let record = AlertTransitionRecord::from(&transition(AlertPhase::Ok, AlertPhase::Firing));
        assert_eq!(record.from, AlertPhaseRecord::from(AlertPhase::Ok));
        assert_eq!(record.to, AlertPhaseRecord::from(AlertPhase::Firing));
    }

    #[test]
    fn transitions_keep_the_folds_order() {
        let first = transition(AlertPhase::Ok, AlertPhase::Pending);
        let second = transition(AlertPhase::Firing, AlertPhase::Ok);
        let records = transitions_to_records(&[first.clone(), second.clone()]);
        assert_eq!(
            records,
            vec![
                AlertTransitionRecord::from(&first),
                AlertTransitionRecord::from(&second)
            ]
        );
        assert!(transitions_to_records(&[]).is_empty());
    }

    /// Every [`PollAdvice`] variant, with the bitmask guarding the fixture list the same way.
    #[test]
    fn every_poll_advice_variant_round_trips() {
        let mut covered = 0_u8;
        // Whole seconds throughout: the boundary's unit is the second, so a fixture with
        // millisecond precision would fail for the representation rather than for the mapping.
        for advice in [
            PollAdvice::Poll {
                after: Duration::zero(),
            },
            PollAdvice::Poll {
                after: Duration::seconds(60),
            },
            PollAdvice::Retry {
                after: Duration::seconds(30),
                consecutive_failures: 3,
            },
            PollAdvice::Stop {
                reason: PollFailureKind::Unauthorized,
                message: "not authorised for the telemetry backend".to_owned(),
            },
        ] {
            let record = PollAdviceRecord::from(&advice);
            let restored =
                PollAdvice::try_from(record.clone()).expect("a fixture delay is representable");
            assert_eq!(restored, advice, "{advice:?} round trips");
            covered |= match &advice {
                PollAdvice::Poll { .. } => 0b001_u8,
                PollAdvice::Retry { .. } => 0b010,
                PollAdvice::Stop { .. } => 0b100,
            };
            assert_eq!(PollAdviceRecord::from(advice), record);
        }
        assert_eq!(covered, 0b111, "every advice variant must be exercised");
    }

    /// The immediate-poll case, pinned on its own. `after = zero` is what stops a relaunch or a
    /// cold start sleeping an interval before it shows anything, and a conversion that rounded it
    /// to the poll interval would be invisible in every other test here.
    #[test]
    fn an_immediate_poll_stays_immediate() {
        assert_eq!(
            PollAdviceRecord::from(&PollAdvice::Poll {
                after: Duration::zero()
            }),
            PollAdviceRecord::Poll { after_seconds: 0 }
        );
    }

    #[test]
    fn a_restore_report_round_trips() {
        for report in [
            RestoreReport::default(),
            RestoreReport {
                discarded_incompatible: true,
                discarded_unreadable: false,
                hosts_restored: 0,
                evaluations_restored: 0,
                saved_as_of: None,
            },
            RestoreReport {
                discarded_incompatible: false,
                discarded_unreadable: false,
                hosts_restored: 12,
                evaluations_restored: 34,
                saved_as_of: Some(at()),
            },
        ] {
            let record = RestoreReportRecord::from(report);
            let restored =
                RestoreReport::try_from(record).expect("a fixture instant is representable");
            assert_eq!(restored, report);
        }
    }

    /// A nothing-salvaged report must not read as a cache written at the epoch.
    #[test]
    fn an_unreadable_cache_reports_no_saved_instant() {
        let record = RestoreReportRecord::from(RestoreReport {
            discarded_unreadable: true,
            ..RestoreReport::default()
        });
        assert!(record.discarded_unreadable);
        assert_eq!(record.saved_as_of_millis, None);
        assert_eq!(record.hosts_restored, 0);
    }
}
