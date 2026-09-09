//! The read model: what the fleet screen, the host detail screen and the alert list draw.
//!
//! Mirrors `pessimal_client_core::view` — [`FleetView`], [`HostView`], [`MetricView`],
//! [`AlertView`], their four enums, and the [`FreshnessInputs`]/[`Freshness`] pair — plus the three
//! `pessimal_core` enums a view is spelled with and nobody else mirrors: [`MetricUnit`],
//! [`OsFamily`] and [`Liveness`].
//!
//! Nothing here decides anything. Every severity, phase, count and availability in this file was
//! computed by the fold, so that the iOS app and the menu bar app cannot reach different answers
//! from the same data. This module converts the four things UniFFI cannot carry — `DateTime` to
//! epoch milliseconds, `Duration` to seconds, newtype ids to `String`, `BTreeMap` to a sorted
//! `Vec` of records — and stops.
//!
//! # Freshness is an input, not a field
//!
//! [`FleetViewRecord`] carries [`FreshnessInputsRecord`] and **not** a [`FreshnessRecord`]. A
//! freshness verdict is only true at the instant it was computed: between polls it does not move,
//! so a hung request — no error, nothing to fold — or a Swift timer suspended with the app would
//! leave the banner reading "Fresh" while the data underneath it rotted. Recomputing from `now` on
//! every tick is the only shape that greys the screen when *nothing is calling us at all*, which is
//! precisely the failure no error path can report. That is why the session exposes
//! `freshness(now_millis)` as a separate call and why this module offers no way to cache its result.
//!
//! # Names
//!
//! Records and enums share one flat Swift namespace across the whole crate, so everything declared
//! here carries the `Record` suffix — the mirrored *enums* included. Section 4.11 of the M3 design
//! establishes that by spelling an enum mirror `FreshnessRecord` and a warning mirror
//! `TuningWarningRecord`, and `config_records` and `fold_records` follow the same rule. A mixed
//! convention would leave a reader guessing which half of the Swift API dropped the suffix.
//!
//! # What this module does not own
//!
//! Three mirrors a view needs are declared once, elsewhere, and used from here rather than
//! duplicated. A duplicate mirror never *collides*, so nothing ever flags it — and the app then
//! holds two Swift enums for one concept and a conversion function between them:
//!
//! - [`MetricKindRecord`] and [`ComparatorRecord`] belong to `config_records`, which needs them in
//!   both directions for the rule editor. They are re-exported here because this is the module
//!   `fold_records` resolves them through.
//! - [`PollFailureRecord`] belongs to `fold_records`, with the rest of the failure trio. Its doc
//!   names this module's freshness inputs as one of its two field positions.
//! - [`TuningWarningRecord`] belongs to `config_records`; [`FleetViewRecord`] carries the audit so a
//!   settings screen cannot forget to ask for it.

use pessimal_client_core::{
    AlertEvidence, AlertPhase, AlertView, CollectionHealth, FleetCounts, FleetView, Freshness,
    FreshnessInputs, HostView, MetricAvailability, MetricFacts, MetricView, PollFailure, Severity,
};
use pessimal_core::{Liveness, MetricPoint, MetricUnit, OsFamily};

use crate::config_records::TuningWarningRecord;
use crate::convert::{
    AttributeRecord, FfiError, attributes_to_records, datetime_to_millis, duration_to_seconds,
    host_id_to_string, millis_to_datetime, optional_datetime_to_millis,
    optional_millis_to_datetime, seconds_to_duration, urn_to_string,
};
use crate::fold_records::PollFailureRecord;

/// Re-exported from `config_records`, which declares them for the rule editor.
///
/// Not redeclared here: `fold_records` already resolves `MetricKindRecord` through this module, and
/// two mirrors of one domain enum would reach Swift as two unrelated types with no compiler
/// anywhere to notice.
pub use crate::config_records::{ComparatorRecord, MetricKindRecord};

// ---------------------------------------------------------------------------
// Domain enums only a view needs
// ---------------------------------------------------------------------------

/// What a metric's numbers mean, so the app can format them.
///
/// Crossing as an enum rather than as the OTLP unit string is load-bearing: [`MetricUnit::Ratio`]
/// and [`MetricUnit::Load`] both report unit `"1"`, so a formatter keyed on the string renders a
/// run-queue length as a percentage. [`MetricViewRecord::is_rate`] carries the other half of the
/// formatting decision, and neither is derivable from the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, uniffi::Enum)]
pub enum MetricUnitRecord {
    /// Dimensionless ratio in `0.0..=1.0`.
    Ratio,
    Bytes,
    Seconds,
    /// A plain count.
    Count,
    /// Run-queue load average — dimensionless, but not a ratio and not a percentage.
    Load,
}

impl From<MetricUnit> for MetricUnitRecord {
    fn from(unit: MetricUnit) -> Self {
        match unit {
            MetricUnit::Ratio => Self::Ratio,
            MetricUnit::Bytes => Self::Bytes,
            MetricUnit::Seconds => Self::Seconds,
            MetricUnit::Count => Self::Count,
            MetricUnit::Load => Self::Load,
        }
    }
}

impl From<MetricUnitRecord> for MetricUnit {
    fn from(record: MetricUnitRecord) -> Self {
        match record {
            MetricUnitRecord::Ratio => Self::Ratio,
            MetricUnitRecord::Bytes => Self::Bytes,
            MetricUnitRecord::Seconds => Self::Seconds,
            MetricUnitRecord::Count => Self::Count,
            MetricUnitRecord::Load => Self::Load,
        }
    }
}

/// A host's operating system family.
///
/// `Other` is an answer, not a parse failure: the backend reports whatever the agent set, and an
/// unrecognised value must still draw a row. The family is also what separates "this platform never
/// reports load averages" from "this host is not reporting them", which is
/// [`MetricAvailabilityRecord`]'s whole distinction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, uniffi::Enum)]
pub enum OsFamilyRecord {
    Linux,
    Darwin,
    Windows,
    Other,
}

impl From<OsFamily> for OsFamilyRecord {
    fn from(os: OsFamily) -> Self {
        match os {
            OsFamily::Linux => Self::Linux,
            OsFamily::Darwin => Self::Darwin,
            OsFamily::Windows => Self::Windows,
            OsFamily::Other => Self::Other,
        }
    }
}

impl From<OsFamilyRecord> for OsFamily {
    fn from(record: OsFamilyRecord) -> Self {
        match record {
            OsFamilyRecord::Linux => Self::Linux,
            OsFamilyRecord::Darwin => Self::Darwin,
            OsFamilyRecord::Windows => Self::Windows,
            OsFamilyRecord::Other => Self::Other,
        }
    }
}

/// Whether a host is still beating.
///
/// Never travels alone: [`HostViewRecord::liveness_at_millis`] is when this verdict was reached,
/// and it is *frozen* while the roster query is failing. The app renders the age of that instant
/// and never re-judges — a backend unreachable for four minutes must not silently walk every host
/// to `Down` on the Swift side, because that turns "I cannot see your fleet" into "your fleet is
/// down".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, uniffi::Enum)]
pub enum LivenessRecord {
    Alive,
    Stale,
    Down,
    /// No heartbeat has ever been seen. Unjudged, which is not the same as healthy.
    Unknown,
}

impl From<Liveness> for LivenessRecord {
    fn from(liveness: Liveness) -> Self {
        match liveness {
            Liveness::Alive => Self::Alive,
            Liveness::Stale => Self::Stale,
            Liveness::Down => Self::Down,
            Liveness::Unknown => Self::Unknown,
        }
    }
}

impl From<LivenessRecord> for Liveness {
    fn from(record: LivenessRecord) -> Self {
        match record {
            LivenessRecord::Alive => Self::Alive,
            LivenessRecord::Stale => Self::Stale,
            LivenessRecord::Down => Self::Down,
            LivenessRecord::Unknown => Self::Unknown,
        }
    }
}

/// One charted sample.
///
/// The `_millis` suffix is in the field name rather than only in a doc comment because a Swift call
/// site that reads it as seconds draws a chart a thousand times too wide and nothing errors.
#[derive(Debug, Clone, Copy, PartialEq, uniffi::Record)]
pub struct MetricPointRecord {
    pub at_millis: i64,
    pub value: f64,
}

impl From<MetricPoint> for MetricPointRecord {
    fn from(point: MetricPoint) -> Self {
        let MetricPoint { at, value } = point;
        Self {
            at_millis: datetime_to_millis(at),
            value,
        }
    }
}

// ---------------------------------------------------------------------------
// View enums
// ---------------------------------------------------------------------------

/// The single thing the UI colours by.
///
/// Deliberately *not* `Ord` here and deliberately not given a comparison on the Swift side. In core
/// the variant order is the comparison order — with `Unknown` above `Ok`, so nothing unjudged is
/// painted healthy — and core has already used it to fold a host's liveness and its worst alert
/// into the one answer on [`HostViewRecord::severity`]. A second "worse of the two" in the bridge or
/// in the app is how two apps start disagreeing about a colour. The declaration order below is
/// still core's, so a Swift extension that genuinely needs an ordering has a correct case order to
/// build on rather than a guess.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, uniffi::Enum)]
pub enum SeverityRecord {
    Ok,
    /// Nobody could judge this. Above `Ok` on purpose.
    Unknown,
    Warning,
    Critical,
}

impl From<Severity> for SeverityRecord {
    fn from(severity: Severity) -> Self {
        match severity {
            Severity::Ok => Self::Ok,
            Severity::Unknown => Self::Unknown,
            Severity::Warning => Self::Warning,
            Severity::Critical => Self::Critical,
        }
    }
}

impl From<SeverityRecord> for Severity {
    fn from(record: SeverityRecord) -> Self {
        match record {
            SeverityRecord::Ok => Self::Ok,
            SeverityRecord::Unknown => Self::Unknown,
            SeverityRecord::Warning => Self::Warning,
            SeverityRecord::Critical => Self::Critical,
        }
    }
}

/// Why a metric has no current value — four cases a chart must draw four different ways.
///
/// Not an `Option`, because "we could not fetch it", "this host is not reporting it" and "this
/// platform never will" are three different sentences to show a user and an empty series is
/// indistinguishable from all three. Drawing `NotReported` as a zero is the specific bug this enum
/// exists to prevent: an unmounted filesystem is a gap in the line, not a disk that emptied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, uniffi::Enum)]
pub enum MetricAvailabilityRecord {
    Present,
    /// A real gap: an unmounted filesystem, a dropped interface, a Windows agent whose default
    /// filesystem filter matches no `C:\` mount.
    NotReported,
    /// A platform fact: load averages on Windows, where the collector returns empty forever.
    Unsupported,
    /// This poll could not fetch it. The values shown are frozen at `fetched_at_millis`; grey them.
    Unavailable,
}

impl From<MetricAvailability> for MetricAvailabilityRecord {
    fn from(availability: MetricAvailability) -> Self {
        match availability {
            MetricAvailability::Present => Self::Present,
            MetricAvailability::NotReported => Self::NotReported,
            MetricAvailability::Unsupported => Self::Unsupported,
            MetricAvailability::Unavailable => Self::Unavailable,
        }
    }
}

impl From<MetricAvailabilityRecord> for MetricAvailability {
    fn from(record: MetricAvailabilityRecord) -> Self {
        match record {
            MetricAvailabilityRecord::Present => Self::Present,
            MetricAvailabilityRecord::NotReported => Self::NotReported,
            MetricAvailabilityRecord::Unsupported => Self::Unsupported,
            MetricAvailabilityRecord::Unavailable => Self::Unavailable,
        }
    }
}

/// Whether the agent is collecting, as opposed to merely beating.
///
/// The only tell there is. A failed collection re-exports the previous snapshot with fresh
/// timestamps, so the data shows a flat line rather than a gap and an `Alive` host can be
/// collecting nothing for hours. `Failing` carries its evidence — how much the failure counter
/// added over how long — because a badge with no numbers cannot be distinguished from a stale badge
/// nobody cleared.
#[derive(Debug, Clone, Copy, PartialEq, uniffi::Enum)]
pub enum CollectionHealthRecord {
    Healthy,
    Failing {
        added: f64,
        over_seconds: i64,
    },
    /// Fewer than two samples, or the metric was not fetched. Unjudged, not a problem.
    Unknown,
}

impl From<CollectionHealth> for CollectionHealthRecord {
    fn from(health: CollectionHealth) -> Self {
        match health {
            CollectionHealth::Healthy => Self::Healthy,
            CollectionHealth::Failing { added, over } => Self::Failing {
                added,
                over_seconds: duration_to_seconds(over),
            },
            CollectionHealth::Unknown => Self::Unknown,
        }
    }
}

impl TryFrom<CollectionHealthRecord> for CollectionHealth {
    type Error = FfiError;

    /// # Errors
    /// [`FfiError::Internal`] when `over_seconds` is not a representable span; see
    /// [`seconds_to_duration`].
    fn try_from(record: CollectionHealthRecord) -> Result<Self, Self::Error> {
        Ok(match record {
            CollectionHealthRecord::Healthy => Self::Healthy,
            CollectionHealthRecord::Failing {
                added,
                over_seconds,
            } => Self::Failing {
                added,
                over: seconds_to_duration(over_seconds)?,
            },
            CollectionHealthRecord::Unknown => Self::Unknown,
        })
    }
}

/// An alert's phase, with `pessimal_core::AlertState`'s `DateTime` payload stripped.
///
/// The payload travels beside it as [`AlertViewRecord::breaching_since_millis`] and
/// [`AlertTransitionRecord::breaching_since_millis`](crate::fold_records::AlertTransitionRecord).
/// That split is not a convenience: UniFFI cannot carry a `chrono` value inside an enum variant
/// without a custom type, and a custom type here would mean the one enum every Swift `switch` in
/// both apps matches on is also the one needing hand-written lifting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, uniffi::Enum)]
pub enum AlertPhaseRecord {
    Ok,
    Pending,
    Firing,
    /// Nothing to judge on. Sorts above `Ok` in core's alert list order — an unjudged rule is worth
    /// looking at — which is *not* the same order [`SeverityRecord`] uses.
    NoData,
}

impl From<AlertPhase> for AlertPhaseRecord {
    fn from(phase: AlertPhase) -> Self {
        match phase {
            AlertPhase::Ok => Self::Ok,
            AlertPhase::Pending => Self::Pending,
            AlertPhase::Firing => Self::Firing,
            AlertPhase::NoData => Self::NoData,
        }
    }
}

impl From<AlertPhaseRecord> for AlertPhase {
    fn from(record: AlertPhaseRecord) -> Self {
        match record {
            AlertPhaseRecord::Ok => Self::Ok,
            AlertPhaseRecord::Pending => Self::Pending,
            AlertPhaseRecord::Firing => Self::Firing,
            AlertPhaseRecord::NoData => Self::NoData,
        }
    }
}

/// Whether the phase was judged on this poll's data, or is being held.
///
/// `Frozen { since_millis }` is what lets a row read "held since 12:04" instead of presenting a
/// four-minute-old verdict as current. Without it a failed metric query and a healthy one look
/// identical on screen, which is the failure the whole partial-poll design exists to make visible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, uniffi::Enum)]
pub enum AlertEvidenceRecord {
    Fresh,
    /// The query for this metric failed; the state is held, not re-judged.
    Frozen {
        since_millis: i64,
    },
}

impl From<AlertEvidence> for AlertEvidenceRecord {
    fn from(evidence: AlertEvidence) -> Self {
        match evidence {
            AlertEvidence::Fresh => Self::Fresh,
            AlertEvidence::Frozen { since } => Self::Frozen {
                since_millis: datetime_to_millis(since),
            },
        }
    }
}

impl TryFrom<AlertEvidenceRecord> for AlertEvidence {
    type Error = FfiError;

    /// # Errors
    /// [`FfiError::Internal`] when `since_millis` is not a representable instant; see
    /// [`millis_to_datetime`].
    fn try_from(record: AlertEvidenceRecord) -> Result<Self, Self::Error> {
        Ok(match record {
            AlertEvidenceRecord::Fresh => Self::Fresh,
            AlertEvidenceRecord::Frozen { since_millis } => Self::Frozen {
                since: millis_to_datetime(since_millis)?,
            },
        })
    }
}

/// How much the picture on screen deserves to be believed, at one instant.
///
/// The output of core's `freshness_at(inputs, now)`, returned by the session's `freshness` call and
/// never stored on a view — see this module's header for why a cached freshness is a banner that
/// lies. The four cases are four different screens: `Fresh` has no banner, `Idle` reads "as of 6
/// hours ago" for a cache restored but not yet polled, `Degraded` keeps the values and adds a
/// mandatory banner, and `Unusable` says the data is past the age at which core would call a *host*
/// down.
///
/// `last_success` is the last *clean* poll, falling back to the last poll that at least listed a
/// roster — otherwise a fleet with one permanently broken metric could never name a success and the
/// banner would have nothing to date itself by.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum FreshnessRecord {
    Fresh {
        at_millis: i64,
    },
    /// Restored from disk, not yet polled this session. A six-hour-old cache must never claim a
    /// live connection this process has not made.
    Idle {
        last_success_millis: i64,
    },
    /// Real data, visibly older than it should be. The banner is mandatory; the values stay.
    Degraded {
        last_success_millis: i64,
        consecutive_failures: u32,
        failure: Option<PollFailureRecord>,
    },
    /// `None` for a state that has never seen a roster at all.
    Unusable {
        last_success_millis: Option<i64>,
        consecutive_failures: u32,
        failure: Option<PollFailureRecord>,
    },
}

impl From<Freshness> for FreshnessRecord {
    fn from(freshness: Freshness) -> Self {
        match freshness {
            Freshness::Fresh { at } => Self::Fresh {
                at_millis: datetime_to_millis(at),
            },
            Freshness::Idle { last_success } => Self::Idle {
                last_success_millis: datetime_to_millis(last_success),
            },
            Freshness::Degraded {
                last_success,
                consecutive_failures,
                failure,
            } => Self::Degraded {
                last_success_millis: datetime_to_millis(last_success),
                consecutive_failures,
                failure: failure.map(PollFailureRecord::from),
            },
            Freshness::Unusable {
                last_success,
                consecutive_failures,
                failure,
            } => Self::Unusable {
                last_success_millis: optional_datetime_to_millis(last_success),
                consecutive_failures,
                failure: failure.map(PollFailureRecord::from),
            },
        }
    }
}

impl TryFrom<FreshnessRecord> for Freshness {
    type Error = FfiError;

    /// # Errors
    /// [`FfiError::Internal`] when any instant is outside the representable range; see
    /// [`millis_to_datetime`].
    fn try_from(record: FreshnessRecord) -> Result<Self, Self::Error> {
        Ok(match record {
            FreshnessRecord::Fresh { at_millis } => Self::Fresh {
                at: millis_to_datetime(at_millis)?,
            },
            FreshnessRecord::Idle {
                last_success_millis,
            } => Self::Idle {
                last_success: millis_to_datetime(last_success_millis)?,
            },
            FreshnessRecord::Degraded {
                last_success_millis,
                consecutive_failures,
                failure,
            } => Self::Degraded {
                last_success: millis_to_datetime(last_success_millis)?,
                consecutive_failures,
                failure: failure.map(PollFailure::try_from).transpose()?,
            },
            FreshnessRecord::Unusable {
                last_success_millis,
                consecutive_failures,
                failure,
            } => Self::Unusable {
                last_success: optional_millis_to_datetime(last_success_millis)?,
                consecutive_failures,
                failure: failure.map(PollFailure::try_from).transpose()?,
            },
        })
    }
}

// ---------------------------------------------------------------------------
// View records
// ---------------------------------------------------------------------------

/// The domain facts about a metric that two apps must not disagree about.
///
/// Read once to build a metric picker and to label an axis. Pointedly *not* a presentation
/// catalogue: no slider bounds, no default thresholds, no localised copy. `alertable` is false only
/// for the heartbeat, whose silence is liveness's business and whose *value* means nothing.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct MetricFactsRecord {
    pub kind: MetricKindRecord,
    pub display_name: String,
    pub unit: MetricUnitRecord,
    /// True when values are a derived rate or delta rather than a raw reading. Label the axis from
    /// this and from `kind`, never from the unit.
    pub is_rate: bool,
    /// False for the agent heartbeat: a rule may not be written against it.
    pub alertable: bool,
}

impl From<MetricFacts> for MetricFactsRecord {
    fn from(facts: MetricFacts) -> Self {
        let MetricFacts {
            kind,
            display_name,
            unit,
            is_rate,
            alertable,
        } = facts;
        Self {
            kind: MetricKindRecord::from(kind),
            display_name,
            unit: MetricUnitRecord::from(unit),
            is_rate,
            alertable,
        }
    }
}

/// Everything core's `freshness_at` needs, so the app can recompute the banner as time passes.
///
/// Absolute instants and the two derived windows, never an age: an age computed at fold time is a
/// number that starts lying the moment the poll ends. `tolerance_seconds` is the span past which
/// the banner is mandatory, `budget_seconds` the span past which the whole view stops deserving to
/// be believed; both are derived inside core's tuning and arrive here so the UI never has to know
/// their formulas.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FreshnessInputsRecord {
    pub as_of_millis: Option<i64>,
    /// Last poll with a listed roster and no failure at all.
    pub last_clean_at_millis: Option<i64>,
    /// Last poll with a listed roster, whatever the series did. `Unusable` keys off this one, so a
    /// single permanently broken metric cannot walk the whole banner to `Unusable` and leave it
    /// there.
    pub last_roster_at_millis: Option<i64>,
    pub consecutive_failures: u32,
    pub last_failure: Option<PollFailureRecord>,
    /// False for a state restored from disk on which no fold has yet run. Set by the FIRST fold
    /// whatever its outcome, so a relaunch whose first poll is rejected shows the rejection rather
    /// than hiding a dead key behind a cache age.
    pub polled_this_session: bool,
    pub tolerance_seconds: i64,
    pub budget_seconds: i64,
}

impl From<FreshnessInputs> for FreshnessInputsRecord {
    fn from(inputs: FreshnessInputs) -> Self {
        let FreshnessInputs {
            as_of,
            last_clean_at,
            last_roster_at,
            consecutive_failures,
            last_failure,
            polled_this_session,
            tolerance,
            budget,
        } = inputs;
        Self {
            as_of_millis: optional_datetime_to_millis(as_of),
            last_clean_at_millis: optional_datetime_to_millis(last_clean_at),
            last_roster_at_millis: optional_datetime_to_millis(last_roster_at),
            consecutive_failures,
            last_failure: last_failure.map(PollFailureRecord::from),
            polled_this_session,
            tolerance_seconds: duration_to_seconds(tolerance),
            budget_seconds: duration_to_seconds(budget),
        }
    }
}

/// One attribute set of one metric on one host, ready to chart.
///
/// `id` is core's `series_id` and is the stable `SwiftUI` `ForEach` identity across mounts appearing
/// and interfaces vanishing. Do not re-derive it from `attributes` on the Swift side: core's
/// definition is the one the list's sort order already agrees with.
///
/// `expected_step_seconds` and `resets_millis` are the two fields that keep a chart honest. A gap
/// wider than the step is a break in the line, never an interpolation and never a fall to zero; a
/// reset is a marker, never the spike a raw counter delta would draw.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct MetricViewRecord {
    pub id: String,
    pub kind: MetricKindRecord,
    pub unit: MetricUnitRecord,
    pub display_name: String,
    /// `"/"`, `"en0 receive"`. `None` for host-wide metrics.
    pub label: Option<String>,
    /// In key order, which is what keeps a `ForEach` from animating rows that did not change.
    pub attributes: Vec<AttributeRecord>,
    pub latest: Option<MetricPointRecord>,
    pub points: Vec<MetricPointRecord>,
    pub availability: MetricAvailabilityRecord,
    pub is_rate: bool,
    pub expected_step_seconds: i64,
    /// Instants where a counter reset was detected and the point dropped.
    pub resets_millis: Vec<i64>,
    /// When these values were last actually fetched — older than the fleet's `as_of_millis` while
    /// `availability` is `Unavailable`. That gap is the entire point of the field: it is the
    /// timestamp a greyed tile shows.
    pub fetched_at_millis: Option<i64>,
}

impl From<MetricView> for MetricViewRecord {
    fn from(view: MetricView) -> Self {
        let MetricView {
            id,
            kind,
            unit,
            display_name,
            label,
            attributes,
            latest,
            points,
            availability,
            is_rate,
            expected_step,
            resets,
            fetched_at,
        } = view;
        Self {
            id,
            kind: MetricKindRecord::from(kind),
            unit: MetricUnitRecord::from(unit),
            display_name,
            label,
            attributes: attributes_to_records(&attributes),
            latest: latest.map(MetricPointRecord::from),
            points: points.into_iter().map(MetricPointRecord::from).collect(),
            availability: MetricAvailabilityRecord::from(availability),
            is_rate,
            expected_step_seconds: duration_to_seconds(expected_step),
            resets_millis: resets.into_iter().map(datetime_to_millis).collect(),
            fetched_at_millis: optional_datetime_to_millis(fetched_at),
        }
    }
}

/// One host's row, already coloured.
///
/// `severity` is the worse of this host's liveness severity and its worst alert severity, folded by
/// core. A host beating perfectly with a firing alert is `Critical`, which is why the app must not
/// colour from `liveness` alone.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct HostViewRecord {
    pub id: String,
    pub os: OsFamilyRecord,
    pub agent_version: Option<String>,
    pub last_heartbeat_millis: Option<i64>,
    pub liveness: LivenessRecord,
    /// When the liveness verdict was computed. Frozen while the roster query is failing: render its
    /// age, never re-judge from it.
    pub liveness_at_millis: i64,
    pub severity: SeverityRecord,
    pub collection: CollectionHealthRecord,
    /// Sorted by `(kind, id)`, one entry per attribute set.
    pub metrics: Vec<MetricViewRecord>,
    pub firing_alerts: u32,
    pub pending_alerts: u32,
    /// False for a retained host absent from the last listed roster. Such a host is a positive
    /// `Down` with a badge, not a row that silently vanished at the moment the operator needed it.
    pub in_current_roster: bool,
    /// True when the id is the agent's `"unknown-host"` fallback, which several unrelated machines
    /// can collide into and which a host selector then treats as one machine. Nothing above this
    /// flag can detect that, so the row gets a warning badge.
    pub id_is_placeholder: bool,
}

impl From<HostView> for HostViewRecord {
    fn from(view: HostView) -> Self {
        let HostView {
            id,
            os,
            agent_version,
            last_heartbeat,
            liveness,
            liveness_at,
            severity,
            collection,
            metrics,
            firing_alerts,
            pending_alerts,
            in_current_roster,
            id_is_placeholder,
        } = view;
        Self {
            id: host_id_to_string(&id),
            os: OsFamilyRecord::from(os),
            agent_version,
            last_heartbeat_millis: optional_datetime_to_millis(last_heartbeat),
            liveness: LivenessRecord::from(liveness),
            liveness_at_millis: datetime_to_millis(liveness_at),
            severity: SeverityRecord::from(severity),
            collection: CollectionHealthRecord::from(collection),
            metrics: metrics.into_iter().map(MetricViewRecord::from).collect(),
            firing_alerts,
            pending_alerts,
            in_current_roster,
            id_is_placeholder,
        }
    }
}

/// The fleet tallies a summary row shows.
///
/// Carried rather than counted on the Swift side, so a header and the list beneath it cannot
/// disagree once a filter is applied to one and not the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct FleetCountsRecord {
    pub hosts: u32,
    pub alive: u32,
    pub stale: u32,
    pub down: u32,
    pub unknown: u32,
    pub firing_alerts: u32,
    pub pending_alerts: u32,
}

impl From<FleetCounts> for FleetCountsRecord {
    fn from(counts: FleetCounts) -> Self {
        let FleetCounts {
            hosts,
            alive,
            stale,
            down,
            unknown,
            firing_alerts,
            pending_alerts,
        } = counts;
        Self {
            hosts,
            alive,
            stale,
            down,
            unknown,
            firing_alerts,
            pending_alerts,
        }
    }
}

/// One alert row: a rule against a host, with the evidence its phase was judged on.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct AlertViewRecord {
    /// `"{rule_urn}|{host}"`. Stable identity for a list that mixes rules and hosts.
    pub id: String,
    pub rule_id: String,
    pub rule_name: String,
    /// Core's `AlertRule::describe()`. Show it; do not parse it on either side.
    pub description: String,
    pub host: String,
    pub metric: MetricKindRecord,
    pub comparator: ComparatorRecord,
    /// Compared against the normalised *rate* when `is_rate` is true, not against the cumulative
    /// total the backend returns. The editor must label the field accordingly.
    pub threshold: f64,
    pub phase: AlertPhaseRecord,
    pub severity: SeverityRecord,
    /// When the BREACH started, not when the alert fired.
    pub breaching_since_millis: Option<i64>,
    /// `breaching_since + for_duration`; already in the past once `Firing`.
    pub fires_at_millis: Option<i64>,
    /// The reduced value the phase was judged on.
    pub latest_value: Option<f64>,
    /// Which mount or interface held it.
    pub series_label: Option<String>,
    pub evidence: AlertEvidenceRecord,
    pub is_rate: bool,
    /// Set when the stored rule failed core's validation — a `NaN` threshold makes every comparator
    /// return false, so the rule would otherwise sit in the list silently never firing. Such a rule
    /// is SHOWN and NOT evaluated: badging it is the only way an operator ever learns it is broken.
    pub invalid_reason: Option<String>,
}

impl From<AlertView> for AlertViewRecord {
    fn from(view: AlertView) -> Self {
        let AlertView {
            id,
            rule_id,
            rule_name,
            description,
            host,
            metric,
            comparator,
            threshold,
            phase,
            severity,
            breaching_since,
            fires_at,
            latest_value,
            series_label,
            evidence,
            is_rate,
            invalid_reason,
        } = view;
        Self {
            id,
            rule_id: urn_to_string(&rule_id),
            rule_name,
            description,
            host: host_id_to_string(&host),
            metric: MetricKindRecord::from(metric),
            comparator: ComparatorRecord::from(comparator),
            threshold,
            phase: AlertPhaseRecord::from(phase),
            severity: SeverityRecord::from(severity),
            breaching_since_millis: optional_datetime_to_millis(breaching_since),
            fires_at_millis: optional_datetime_to_millis(fires_at),
            latest_value,
            series_label,
            evidence: AlertEvidenceRecord::from(evidence),
            is_rate,
            invalid_reason,
        }
    }
}

/// The whole picture, as of one fold.
///
/// `hosts` arrives sorted by host id and `alerts` by phase rank, then host, then rule id — all
/// core's orders. Re-sorting on the Swift side is how a list animates rows that did not change.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FleetViewRecord {
    pub backend_name: String,
    /// `None` until the first fold. Not `0`: a fleet nobody has polled and a fleet polled at the
    /// Unix epoch must not render the same.
    pub as_of_millis: Option<i64>,
    /// The banner's *inputs*. Recompute the verdict per tick through the session's `freshness`
    /// call; see this module's header for why the verdict is not a field here.
    pub freshness: FreshnessInputsRecord,
    pub severity: SeverityRecord,
    pub hosts: Vec<HostViewRecord>,
    pub alerts: Vec<AlertViewRecord>,
    pub counts: FleetCountsRecord,
    /// Core's configuration audit, carried so a settings screen cannot forget to ask for it.
    pub warnings: Vec<TuningWarningRecord>,
}

impl From<FleetView> for FleetViewRecord {
    fn from(view: FleetView) -> Self {
        let FleetView {
            backend_name,
            as_of,
            freshness,
            severity,
            hosts,
            alerts,
            counts,
            warnings,
        } = view;
        Self {
            backend_name,
            as_of_millis: optional_datetime_to_millis(as_of),
            freshness: FreshnessInputsRecord::from(freshness),
            severity: SeverityRecord::from(severity),
            hosts: hosts.into_iter().map(HostViewRecord::from).collect(),
            alerts: alerts.into_iter().map(AlertViewRecord::from).collect(),
            counts: FleetCountsRecord::from(counts),
            warnings: warnings
                .into_iter()
                .map(TuningWarningRecord::from)
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use chrono::{DateTime, Duration, Utc};
    use pessimal_client_core::{
        AlertEvidence, AlertPhase, AlertView, CollectionHealth, FailureSource, FleetCounts,
        FleetView, Freshness, FreshnessInputs, HostView, MetricAvailability, MetricView,
        PLACEHOLDER_HOST_ID, PollFailure, Severity, metric_facts,
    };
    use pessimal_core::{
        Comparator, CoreError, HostId, Liveness, MetricKind, MetricPoint, MetricUnit, OsFamily, Urn,
    };

    use super::{
        AlertEvidenceRecord, AlertPhaseRecord, AlertViewRecord, CollectionHealthRecord,
        ComparatorRecord, FleetCountsRecord, FleetViewRecord, FreshnessInputsRecord,
        FreshnessRecord, HostViewRecord, LivenessRecord, MetricAvailabilityRecord,
        MetricFactsRecord, MetricKindRecord, MetricPointRecord, MetricUnitRecord, MetricViewRecord,
        OsFamilyRecord, SeverityRecord,
    };

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_757_000_000 + secs, 0).expect("a valid fixture instant")
    }

    fn unauthorized() -> PollFailure {
        PollFailure::from_core(&CoreError::Unauthorized, FailureSource::Roster, at(0))
    }

    fn rule_urn() -> Urn {
        "pessimal::prod::client::alertrule::018f3c4e-0000-7000-8000-000000000001"
            .parse()
            .expect("a valid fixture URN")
    }

    // --- Enum round trips.
    //
    // Each one exists so that adding a variant to `pessimal_client_core` or `pessimal_core` fails
    // `cargo test` in this crate rather than every `switch` statement in two Swift apps at iOS
    // build time. The `From` impls already refuse to compile on a new variant; these assert the
    // mapping is a bijection rather than two variants quietly collapsing into one.

    #[test]
    fn severity_round_trips() {
        for original in [
            Severity::Ok,
            Severity::Unknown,
            Severity::Warning,
            Severity::Critical,
        ] {
            let record = SeverityRecord::from(original);
            assert_eq!(original, Severity::from(record));
        }
        assert_ne!(
            SeverityRecord::Ok,
            SeverityRecord::Unknown,
            "a host nobody could judge must not colour as healthy"
        );
    }

    #[test]
    fn metric_availability_round_trips() {
        for original in [
            MetricAvailability::Present,
            MetricAvailability::NotReported,
            MetricAvailability::Unsupported,
            MetricAvailability::Unavailable,
        ] {
            let record = MetricAvailabilityRecord::from(original);
            assert_eq!(original, MetricAvailability::from(record));
        }
        assert_ne!(
            MetricAvailabilityRecord::Unsupported,
            MetricAvailabilityRecord::NotReported,
            "a platform fact and a configuration gap are different sentences"
        );
    }

    #[test]
    fn alert_phase_round_trips() {
        for original in [
            AlertPhase::Ok,
            AlertPhase::Pending,
            AlertPhase::Firing,
            AlertPhase::NoData,
        ] {
            let record = AlertPhaseRecord::from(original);
            assert_eq!(original, AlertPhase::from(record));
        }
    }

    #[test]
    fn collection_health_round_trips() {
        for original in [
            CollectionHealth::Healthy,
            CollectionHealth::Failing {
                added: 3.0,
                over: Duration::seconds(120),
            },
            CollectionHealth::Unknown,
        ] {
            let record = CollectionHealthRecord::from(original);
            assert_eq!(
                original,
                CollectionHealth::try_from(record).expect("a representable span")
            );
        }
    }

    #[test]
    fn alert_evidence_round_trips() {
        for original in [
            AlertEvidence::Fresh,
            AlertEvidence::Frozen { since: at(60) },
        ] {
            let record = AlertEvidenceRecord::from(original);
            assert_eq!(
                original,
                AlertEvidence::try_from(record).expect("a representable instant")
            );
        }
    }

    #[test]
    fn freshness_round_trips() {
        for original in [
            Freshness::Fresh { at: at(0) },
            Freshness::Idle {
                last_success: at(-60),
            },
            Freshness::Degraded {
                last_success: at(-90),
                consecutive_failures: 2,
                failure: Some(unauthorized()),
            },
            Freshness::Unusable {
                last_success: None,
                consecutive_failures: 7,
                failure: None,
            },
        ] {
            let record = FreshnessRecord::from(original.clone());
            assert_eq!(
                original,
                Freshness::try_from(record).expect("a representable instant")
            );
        }
    }

    #[test]
    fn metric_unit_round_trips() {
        for original in [
            MetricUnit::Ratio,
            MetricUnit::Bytes,
            MetricUnit::Seconds,
            MetricUnit::Count,
            MetricUnit::Load,
        ] {
            let record = MetricUnitRecord::from(original);
            assert_eq!(original, MetricUnit::from(record));
        }
        assert_ne!(
            MetricUnitRecord::Ratio,
            MetricUnitRecord::Load,
            "both report OTLP unit \"1\"; only the enum keeps a load average off a percent axis"
        );
    }

    #[test]
    fn os_family_round_trips() {
        for original in [
            OsFamily::Linux,
            OsFamily::Darwin,
            OsFamily::Windows,
            OsFamily::Other,
        ] {
            let record = OsFamilyRecord::from(original);
            assert_eq!(original, OsFamily::from(record));
        }
    }

    #[test]
    fn liveness_round_trips() {
        for original in [
            Liveness::Alive,
            Liveness::Stale,
            Liveness::Down,
            Liveness::Unknown,
        ] {
            let record = LivenessRecord::from(original);
            assert_eq!(original, Liveness::from(record));
        }
    }

    /// Re-exported from `config_records` rather than redeclared, so this asserts the re-export is
    /// the real mirror and not an accidental second one.
    #[test]
    fn the_reexported_domain_enums_round_trip() {
        for original in MetricKind::ALL {
            let record = MetricKindRecord::from(original);
            assert_eq!(original, MetricKind::from(record));
        }
        for original in [
            Comparator::GreaterThan,
            Comparator::GreaterThanOrEqual,
            Comparator::LessThan,
            Comparator::LessThanOrEqual,
        ] {
            let record = ComparatorRecord::from(original);
            assert_eq!(original, Comparator::from(record));
        }
    }

    // --- Struct conversions. The view is output-only, so the assertions are on the converted
    // --- fields rather than on a reverse nobody calls.

    #[test]
    fn a_metric_view_crosses_as_millis_seconds_and_sorted_attributes() {
        let attributes = BTreeMap::from([
            ("mount".to_owned(), "/".to_owned()),
            ("device".to_owned(), "disk1s1".to_owned()),
        ]);
        let view = MetricView {
            id: "host-a|filesystem.utilization|device=disk1s1,mount=/".to_owned(),
            kind: MetricKind::FilesystemUtilization,
            unit: MetricUnit::Ratio,
            display_name: "Filesystem utilisation".to_owned(),
            label: Some("/".to_owned()),
            attributes,
            latest: Some(MetricPoint::new(at(30), 0.82)),
            points: vec![
                MetricPoint::new(at(0), 0.80),
                MetricPoint::new(at(30), 0.82),
            ],
            availability: MetricAvailability::Unavailable,
            is_rate: false,
            expected_step: Duration::seconds(30),
            resets: vec![at(15)],
            fetched_at: Some(at(-60)),
        };

        let record = MetricViewRecord::from(view);

        assert_eq!(record.expected_step_seconds, 30);
        assert_eq!(record.resets_millis, vec![at(15).timestamp_millis()]);
        assert_eq!(
            record.fetched_at_millis,
            Some(at(-60).timestamp_millis()),
            "a greyed tile shows when it was last fetched, not when the poll ran"
        );
        assert_eq!(
            record.latest,
            Some(MetricPointRecord {
                at_millis: at(30).timestamp_millis(),
                value: 0.82,
            })
        );
        assert_eq!(record.points.len(), 2);
        assert_eq!(
            record
                .attributes
                .iter()
                .map(|attribute| attribute.key.as_str())
                .collect::<Vec<_>>(),
            vec!["device", "mount"],
            "key order is what keeps a ForEach from animating rows that did not change"
        );
        assert_eq!(record.availability, MetricAvailabilityRecord::Unavailable);
        assert_eq!(record.unit, MetricUnitRecord::Ratio);
    }

    #[test]
    fn a_host_view_keeps_its_frozen_verdict_and_its_placeholder_flag() {
        let host = HostView {
            id: HostId::from(PLACEHOLDER_HOST_ID),
            os: OsFamily::Windows,
            agent_version: Some("0.1.0".to_owned()),
            last_heartbeat: Some(at(-45)),
            liveness: Liveness::Stale,
            liveness_at: at(-30),
            severity: Severity::Warning,
            collection: CollectionHealth::Failing {
                added: 4.0,
                over: Duration::seconds(60),
            },
            metrics: Vec::new(),
            firing_alerts: 0,
            pending_alerts: 1,
            in_current_roster: false,
            id_is_placeholder: true,
        };

        let record = HostViewRecord::from(host);

        assert_eq!(record.id, PLACEHOLDER_HOST_ID);
        assert!(
            record.id_is_placeholder,
            "several machines collide into this id and nothing above the flag can tell"
        );
        assert!(!record.in_current_roster);
        assert_eq!(record.liveness, LivenessRecord::Stale);
        assert_eq!(
            record.liveness_at_millis,
            at(-30).timestamp_millis(),
            "the verdict's instant is frozen; the app renders its age, it does not re-judge"
        );
        assert_eq!(
            record.collection,
            CollectionHealthRecord::Failing {
                added: 4.0,
                over_seconds: 60,
            }
        );
        assert_eq!(record.os, OsFamilyRecord::Windows);
    }

    #[test]
    fn an_alert_view_carries_its_urn_as_a_string_and_its_broken_reason() {
        let rule_id = rule_urn();
        let alert = AlertView {
            id: format!("{rule_id}|host-a"),
            rule_id: rule_id.clone(),
            rule_name: "CPU hot".to_owned(),
            description: "CPU utilisation > 0.9 for 5m".to_owned(),
            host: HostId::from("host-a"),
            metric: MetricKind::CpuUtilization,
            comparator: Comparator::GreaterThan,
            threshold: f64::NAN,
            phase: AlertPhase::NoData,
            severity: Severity::Unknown,
            breaching_since: None,
            fires_at: None,
            latest_value: None,
            series_label: None,
            evidence: AlertEvidence::Frozen { since: at(-120) },
            is_rate: false,
            invalid_reason: Some("threshold is not a number".to_owned()),
        };

        let record = AlertViewRecord::from(alert);

        assert_eq!(record.rule_id, rule_id.to_string());
        assert_eq!(record.host, "host-a");
        assert_eq!(record.metric, MetricKindRecord::CpuUtilization);
        assert_eq!(record.comparator, ComparatorRecord::GreaterThan);
        assert_eq!(
            record.evidence,
            AlertEvidenceRecord::Frozen {
                since_millis: at(-120).timestamp_millis()
            },
            "a held phase must be able to say since when"
        );
        assert!(record.threshold.is_nan(), "a NaN threshold crosses as NaN");
        assert_eq!(
            record.invalid_reason.as_deref(),
            Some("threshold is not a number"),
            "a rule that can never fire is shown badged, never hidden"
        );
    }

    #[test]
    fn a_fleet_view_carries_its_freshness_inputs_rather_than_a_frozen_verdict() {
        let view = FleetView {
            backend_name: "signoz".to_owned(),
            as_of: Some(at(0)),
            freshness: FreshnessInputs {
                as_of: Some(at(0)),
                last_clean_at: Some(at(-30)),
                last_roster_at: Some(at(0)),
                consecutive_failures: 1,
                last_failure: Some(unauthorized()),
                polled_this_session: true,
                tolerance: Duration::seconds(90),
                budget: Duration::seconds(150),
            },
            severity: Severity::Critical,
            hosts: Vec::new(),
            alerts: Vec::new(),
            counts: FleetCounts {
                hosts: 3,
                alive: 1,
                stale: 1,
                down: 1,
                unknown: 0,
                firing_alerts: 2,
                pending_alerts: 0,
            },
            warnings: Vec::new(),
        };

        let record = FleetViewRecord::from(view);

        assert_eq!(record.as_of_millis, Some(at(0).timestamp_millis()));
        assert_eq!(record.severity, SeverityRecord::Critical);
        assert_eq!(
            record.freshness,
            FreshnessInputsRecord {
                as_of_millis: Some(at(0).timestamp_millis()),
                last_clean_at_millis: Some(at(-30).timestamp_millis()),
                last_roster_at_millis: Some(at(0).timestamp_millis()),
                consecutive_failures: 1,
                last_failure: Some(unauthorized().into()),
                polled_this_session: true,
                tolerance_seconds: 90,
                budget_seconds: 150,
            },
            "the banner is recomputed from these per tick; a verdict field would stop moving"
        );
        assert_eq!(
            record.counts,
            FleetCountsRecord {
                hosts: 3,
                alive: 1,
                stale: 1,
                down: 1,
                unknown: 0,
                firing_alerts: 2,
                pending_alerts: 0,
            }
        );
    }

    #[test]
    fn the_freshness_verdict_carries_the_failure_that_routes_the_settings_button() {
        let record = FreshnessRecord::from(Freshness::Unusable {
            last_success: Some(at(-21_600)),
            consecutive_failures: 1,
            failure: Some(unauthorized()),
        });

        let FreshnessRecord::Unusable {
            last_success_millis,
            consecutive_failures,
            failure,
        } = record
        else {
            panic!("an unusable verdict must mirror as Unusable");
        };

        assert_eq!(last_success_millis, Some(at(-21_600).timestamp_millis()));
        assert_eq!(consecutive_failures, 1);
        let failure = failure.expect("the failure must reach the banner");
        assert!(
            failure.is_actionable,
            "only an actionable failure offers Open Settings, and core decides which"
        );
    }

    #[test]
    fn metric_facts_cross_for_every_modelled_metric() {
        let facts: Vec<MetricFactsRecord> = metric_facts()
            .into_iter()
            .map(MetricFactsRecord::from)
            .collect();

        assert_eq!(facts.len(), MetricKind::ALL.len());

        let heartbeat = facts
            .iter()
            .find(|fact| fact.kind == MetricKindRecord::AgentHeartbeat)
            .expect("the heartbeat is modelled");
        assert!(
            !heartbeat.alertable,
            "a silent host is liveness's business, never an alert's"
        );

        let network = facts
            .iter()
            .find(|fact| fact.kind == MetricKindRecord::NetworkIo)
            .expect("network io is modelled");
        assert!(network.is_rate);
        assert_eq!(network.unit, MetricUnitRecord::Bytes);
    }
}
