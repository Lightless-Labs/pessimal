//! The render model: everything the apps draw, and nothing about how they draw it.
//!
//! Every type here crosses UniFFI eventually, so all of it is deliberately concrete — no
//! generics, no lifetimes, no borrowed strings, no hash maps with an unstable iteration order.
//! Every collection is an already-sorted `Vec` and every list element carries a stable `String`
//! id, so `pessimal_ffi` is a mechanical `From` mapping and nothing else, and a `SwiftUI`
//! `ForEach` has an identity that survives a mount appearing or an interface going away.
//!
//! The module holds one real decision, [`freshness_at`], and it is a *function of `now`* rather
//! than a stored field. [`FleetView`] therefore carries [`FreshnessInputs`], never a
//! [`Freshness`]. That shape is load-bearing: see [`freshness_at`].
//!
//! Absolute instants only. An age ("4 minutes ago") is presentation's business, and one frozen
//! at fold time starts lying the instant the fold returns.

use std::collections::BTreeMap;

use chrono::{DateTime, Duration, Utc};
use pessimal_core::{
    AlertState, Comparator, HostId, Liveness, MetricKind, MetricPoint, MetricUnit, OsFamily, Urn,
};
use serde::{Deserialize, Serialize};

use crate::config::TuningWarning;
use crate::error::PollFailure;
use crate::normalize;

/// The id the host agent falls back to when it cannot determine a hostname.
///
/// Several unrelated machines collide into this one row, and `HostSelector::matches` will happily
/// apply a rule to all of them. Nothing below this crate can detect that, which is why
/// [`HostView::id_is_placeholder`] exists; this constant is the single place the literal lives so
/// the fold does not spell it out a second time.
pub const PLACEHOLDER_HOST_ID: &str = "unknown-host";

/// Why a metric has no current value — the distinction between "we did not get it" and "it is
/// not there", which a chart must draw differently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MetricAvailability {
    Present,
    /// A real gap: an unmounted filesystem, a dropped interface, a Windows agent whose default
    /// `filesystems = ["/"]` filter matches no `C:\` mount. Render as a gap, never as zero.
    NotReported,
    /// A platform fact, not a configuration one: load averages on Windows, where the collector
    /// returns empty forever even though the instrument is registered.
    Unsupported,
    /// This poll could not fetch it; the values shown are frozen at `fetched_at`. Grey them.
    Unavailable,
}

/// Whether `kind` is one this platform can ever report.
///
/// False only for `LoadAverage1m`/`5m`/`15m` on [`pessimal_core::OsFamily::Windows`]. This is what
/// separates [`MetricAvailability::Unsupported`] from [`MetricAvailability::NotReported`] on an
/// empty-but-successful response: "not supported on this platform" is an answer, "no data" is a
/// question.
#[must_use]
pub fn expected_on(kind: MetricKind, os: OsFamily) -> bool {
    !matches!(
        (kind, os),
        (
            MetricKind::LoadAverage1m | MetricKind::LoadAverage5m | MetricKind::LoadAverage15m,
            OsFamily::Windows,
        )
    )
}

/// Whether a rule may be written against `kind`.
///
/// False for `AgentHeartbeat`: a silent host is `Liveness`'s business, never an alert's. Alerting
/// on the heartbeat's *value* would also be meaningless — liveness reads its bucket timestamps.
#[must_use]
pub fn alertable(kind: MetricKind) -> bool {
    !matches!(kind, MetricKind::AgentHeartbeat)
}

/// The domain facts about a metric that two apps must not disagree about.
///
/// Deliberately not a presentation catalogue: no formatted strings, no slider bounds, no default
/// thresholds. `alertable` and `is_rate` are facts about the metric; English is not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetricFacts {
    pub kind: MetricKind,
    pub display_name: String,
    pub unit: MetricUnit,
    /// True when values are a derived rate or delta rather than a raw reading. Label the axis
    /// from this and from `kind` — never from the unit string: `MetricUnit::Ratio` and
    /// `MetricUnit::Load` both report OTLP unit `"1"`.
    pub is_rate: bool,
    pub alertable: bool,
}

#[must_use]
pub fn describe_metric(kind: MetricKind) -> MetricFacts {
    MetricFacts {
        kind,
        display_name: kind.display_name().to_owned(),
        unit: kind.unit(),
        is_rate: normalize::is_rate(kind),
        alertable: alertable(kind),
    }
}

/// Every modelled metric, in `MetricKind::ALL` display order.
#[must_use]
pub fn metric_facts() -> Vec<MetricFacts> {
    MetricKind::ALL.into_iter().map(describe_metric).collect()
}

/// The single thing the UI colours by, computed once here so two apps cannot disagree.
///
/// Liveness maps `Alive -> Ok`, `Stale -> Warning`, `Down -> Critical`, `Unknown -> Unknown`;
/// alerts map `Ok -> Ok`, `Pending -> Warning`, `Firing -> Critical`, `NoData -> Unknown`; a host
/// is the max of its liveness severity and its worst alert severity.
///
/// The variant order is the comparison order, and `Unknown` sits *above* `Ok` on purpose: a host
/// or a rule nobody could judge must not be coloured as healthy. `Ord` is what makes "the worse
/// of the two" a `max` rather than a hand-written table each app writes differently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Severity {
    Ok,
    Unknown,
    Warning,
    Critical,
}

impl Severity {
    #[must_use]
    pub fn from_liveness(liveness: Liveness) -> Self {
        match liveness {
            Liveness::Alive => Self::Ok,
            Liveness::Stale => Self::Warning,
            Liveness::Down => Self::Critical,
            Liveness::Unknown => Self::Unknown,
        }
    }

    #[must_use]
    pub fn from_phase(phase: AlertPhase) -> Self {
        match phase {
            AlertPhase::Ok => Self::Ok,
            AlertPhase::Pending => Self::Warning,
            AlertPhase::Firing => Self::Critical,
            AlertPhase::NoData => Self::Unknown,
        }
    }
}

/// Everything [`freshness_at`] needs, carried on the view so the UI can recompute as time passes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FreshnessInputs {
    pub as_of: Option<DateTime<Utc>>,
    /// Last poll with a listed roster and no failure at all.
    pub last_clean_at: Option<DateTime<Utc>>,
    /// Last poll with a listed roster, whatever the series did. `Unusable` keys off this, not off
    /// `last_clean_at`: one permanently broken metric — a naming mismatch on exactly one name —
    /// must not walk the whole banner to `Unusable` and leave it there.
    pub last_roster_at: Option<DateTime<Utc>>,
    pub consecutive_failures: u32,
    pub last_failure: Option<PollFailure>,
    /// False for a state restored from disk on which no fold has yet run.
    ///
    /// Set by the FIRST fold whatever its outcome — not by the first *successful* one. A relaunch
    /// whose first poll 401s must show the 401 with an Open Settings button, not `Idle` hiding a
    /// dead key behind a cache age.
    pub polled_this_session: bool,
    /// `PollTuning::staleness_tolerance()`. Past this, `Degraded`.
    pub tolerance: Duration,
    /// `PollTuning::freshness_budget()`. Past this, `Unusable`.
    pub budget: Duration,
}

/// How much the picture on screen deserves to be believed.
///
/// `last_success` is the last poll that was *clean*, falling back to the last poll that at least
/// listed a roster when no poll was ever clean — otherwise a fleet with one permanently broken
/// metric could never name a success at all, and the banner would have nothing to date itself by.
/// A clean poll always listed its roster, so the fallback only ever widens the answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Freshness {
    Fresh {
        at: DateTime<Utc>,
    },
    /// Restored from disk, not yet polled this session. A six-hour-old cache must never claim a
    /// live connection this process has not made.
    Idle {
        last_success: DateTime<Utc>,
    },
    /// Real data, visibly older than it should be. The banner is mandatory; the values stay.
    Degraded {
        last_success: DateTime<Utc>,
        consecutive_failures: u32,
        failure: Option<PollFailure>,
    },
    Unusable {
        last_success: Option<DateTime<Utc>>,
        consecutive_failures: u32,
        failure: Option<PollFailure>,
    },
}

impl Freshness {
    #[must_use]
    pub fn is_fresh(&self) -> bool {
        matches!(self, Self::Fresh { .. })
    }

    #[must_use]
    pub fn last_success(&self) -> Option<DateTime<Utc>> {
        match self {
            Self::Fresh { at } => Some(*at),
            Self::Idle { last_success } | Self::Degraded { last_success, .. } => {
                Some(*last_success)
            }
            Self::Unusable { last_success, .. } => *last_success,
        }
    }

    #[must_use]
    pub fn severity(&self) -> Severity {
        match self {
            Self::Fresh { .. } => Severity::Ok,
            Self::Idle { .. } => Severity::Unknown,
            Self::Degraded { .. } => Severity::Warning,
            Self::Unusable { .. } => Severity::Critical,
        }
    }
}

/// Freshness is a FUNCTION OF `now`, never a field frozen at fold time.
///
/// That is the whole point of this function existing. A `Freshness` computed during the fold is
/// only correct at the instant it was computed: between polls it does not move, so a hung
/// `execute_plan` — no error, nothing to fold — or a Swift timer that stopped when the app was
/// suspended would leave the banner reading `Fresh` indefinitely while the data underneath it
/// rots. Recomputing from `now` is the only shape that greys the screen when *nothing is calling
/// us at all*, which is precisely the failure no error path can report. Do not cache the result
/// on the view.
///
/// In order:
/// 1. no `last_roster_at` — we have never seen a roster — `Unusable`;
/// 2. `!polled_this_session` — a restored cache — `Idle`, checked before the budget so a
///    six-hour-old cache reads "as of 6 hours ago" rather than a failure nobody has had yet;
/// 3. `now - last_roster_at > budget` — `Unusable`; past the age at which we would call a *host*
///    down, the whole view is past the age at which it deserves to be believed;
/// 4. `consecutive_failures == 0 && now - last_clean_at <= tolerance` — `Fresh`;
/// 5. otherwise `Degraded`.
#[must_use]
pub fn freshness_at(inputs: &FreshnessInputs, now: DateTime<Utc>) -> Freshness {
    let Some(last_roster_at) = inputs.last_roster_at else {
        return Freshness::Unusable {
            last_success: inputs.last_clean_at,
            consecutive_failures: inputs.consecutive_failures,
            failure: inputs.last_failure.clone(),
        };
    };

    // A clean poll implies a listed roster, so this only falls back when no poll was ever clean.
    let last_success = inputs.last_clean_at.unwrap_or(last_roster_at);

    if !inputs.polled_this_session {
        return Freshness::Idle { last_success };
    }

    if now - last_roster_at > inputs.budget {
        return Freshness::Unusable {
            last_success: Some(last_success),
            consecutive_failures: inputs.consecutive_failures,
            failure: inputs.last_failure.clone(),
        };
    }

    if inputs.consecutive_failures == 0
        && let Some(clean_at) = inputs.last_clean_at
        && now - clean_at <= inputs.tolerance
    {
        return Freshness::Fresh { at: clean_at };
    }

    Freshness::Degraded {
        last_success,
        consecutive_failures: inputs.consecutive_failures,
        failure: inputs.last_failure.clone(),
    }
}

/// One attribute set of one metric on one host, ready to chart.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MetricView {
    /// `series_id(host, kind, attributes)`. Stable `ForEach` identity across mounts appearing and
    /// vanishing.
    pub id: String,
    pub kind: MetricKind,
    pub unit: MetricUnit,
    pub display_name: String,
    /// From `series_label`: `"/"`, `"en0 receive"`. `None` for host-wide metrics.
    pub label: Option<String>,
    pub attributes: BTreeMap<String, String>,
    pub latest: Option<MetricPoint>,
    pub points: Vec<MetricPoint>,
    pub availability: MetricAvailability,
    pub is_rate: bool,
    /// `tuning.metric_step()`. A gap wider than this must be drawn as a break — never
    /// interpolated, never carried to zero.
    pub expected_step: Duration,
    /// Timestamps where a counter reset was detected and the point dropped. Draw a marker, not a
    /// spike.
    pub resets: Vec<DateTime<Utc>>,
    /// When these values were last actually fetched. Older than the poll's `as_of` while
    /// `availability` is [`MetricAvailability::Unavailable`] — that gap is the whole point.
    pub fetched_at: Option<DateTime<Utc>>,
}

/// Whether the agent is collecting, as opposed to merely beating.
///
/// Derived from the newest delta of `AgentCollectionFailures`: a failed collection re-exports the
/// previous snapshot with fresh timestamps, so the data itself shows a flat line rather than a
/// gap and this is the only tell.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum CollectionHealth {
    Healthy,
    Failing {
        added: f64,
        over: Duration,
    },
    /// Fewer than two samples, or the metric was not fetched. Unjudged, not a problem.
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HostView {
    pub id: HostId,
    pub os: OsFamily,
    pub agent_version: Option<String>,
    pub last_heartbeat: Option<DateTime<Utc>>,
    pub liveness: Liveness,
    /// When this verdict was computed. Frozen while the roster query is failing — the UI renders
    /// age relative to this, and never re-judges liveness itself.
    pub liveness_at: DateTime<Utc>,
    pub severity: Severity,
    pub collection: CollectionHealth,
    /// Sorted by `(kind, id)`, one entry per attribute set.
    pub metrics: Vec<MetricView>,
    pub firing_alerts: u32,
    pub pending_alerts: u32,
    /// False for a retained host absent from the last listed roster.
    pub in_current_roster: bool,
    /// True when the id is [`PLACEHOLDER_HOST_ID`], the agent's fallback, which several unrelated
    /// machines can collide into. Flag it; nothing else can detect it.
    pub id_is_placeholder: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct FleetCounts {
    pub hosts: u32,
    pub alive: u32,
    pub stale: u32,
    pub down: u32,
    pub unknown: u32,
    pub firing_alerts: u32,
    pub pending_alerts: u32,
}

/// `AlertState` without its `DateTime` payload, which travels as sibling fields — UniFFI cannot
/// carry a `chrono` payload inside an enum variant without a custom type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AlertPhase {
    Ok,
    Pending,
    Firing,
    NoData,
}

impl AlertPhase {
    /// Drops the payload; `breaching_since` carries it alongside.
    #[must_use]
    pub fn from_state(state: AlertState) -> Self {
        match state {
            AlertState::Ok => Self::Ok,
            AlertState::Pending { .. } => Self::Pending,
            AlertState::Firing { .. } => Self::Firing,
            AlertState::NoData => Self::NoData,
        }
    }

    /// Primary sort key for [`FleetView::alerts`]: `Firing` 0, `Pending` 1, `NoData` 2, `Ok` 3.
    ///
    /// Not the `Severity` order — `NoData` sorts above `Ok` here because an unjudged rule is
    /// worth looking at, while `Severity::Unknown` also outranks `Severity::Ok`. Keeping the key
    /// here rather than inline in the fold is what stops the list order and this doc drifting
    /// apart.
    #[must_use]
    pub fn sort_rank(self) -> u8 {
        match self {
            Self::Firing => 0,
            Self::Pending => 1,
            Self::NoData => 2,
            Self::Ok => 3,
        }
    }
}

/// Whether the phase was judged on data from this poll, or is being held.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AlertEvidence {
    Fresh,
    /// The query for this metric failed; the state is held, not re-judged.
    Frozen {
        since: DateTime<Utc>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AlertView {
    /// `"{rule_urn}|{host}"`. Stable identity for a list that mixes rules and hosts.
    pub id: String,
    pub rule_id: Urn,
    pub rule_name: String,
    /// `AlertRule::describe()`. Do not parse it.
    pub description: String,
    pub host: HostId,
    pub metric: MetricKind,
    pub comparator: Comparator,
    pub threshold: f64,
    pub phase: AlertPhase,
    pub severity: Severity,
    /// When the BREACH started, not when the alert fired.
    pub breaching_since: Option<DateTime<Utc>>,
    /// `breaching_since + rule.for_duration()`; already in the past once `Firing`.
    pub fires_at: Option<DateTime<Utc>>,
    /// The reduced value the phase was judged on.
    pub latest_value: Option<f64>,
    /// Which mount or interface held it, from `dominant_at`.
    pub series_label: Option<String>,
    pub evidence: AlertEvidence,
    pub is_rate: bool,
    /// Set when the stored rule failed `validate_rule` — a `NaN` threshold makes every comparator
    /// return false and the rule silently never fires. Such a rule is SHOWN and NOT evaluated.
    pub invalid_reason: Option<String>,
}

/// The whole picture, as of one fold.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FleetView {
    pub backend_name: String,
    /// `None` until the first fold.
    pub as_of: Option<DateTime<Utc>>,
    /// Absolute instants only. Ages are presentation's business and freezing one at fold time
    /// would make it lie between polls.
    pub freshness: FreshnessInputs,
    pub severity: Severity,
    /// Sorted by `HostId`.
    pub hosts: Vec<HostView>,
    /// Sorted by [`AlertPhase::sort_rank`], then by host, then by rule id string — `Urn` is not
    /// `Ord`. Invalid rules sort with `NoData`.
    pub alerts: Vec<AlertView>,
    pub counts: FleetCounts,
    /// From `FleetConfig::audit()`, carried so a settings screen cannot forget to ask.
    pub warnings: Vec<TuningWarning>,
}

impl FleetView {
    #[must_use]
    pub fn host(&self, id: &HostId) -> Option<&HostView> {
        self.hosts.iter().find(|host| &host.id == id)
    }

    #[must_use]
    pub fn firing(&self) -> Vec<&AlertView> {
        self.alerts
            .iter()
            .filter(|alert| alert.phase == AlertPhase::Firing)
            .collect()
    }

    /// Hosts worth the operator's attention, by [`Severity`] rather than by [`Liveness`] alone.
    ///
    /// A host beating perfectly with a firing alert belongs in this list, and severity is already
    /// defined as "the single thing the UI colours by" — using `Liveness::is_degraded` here would
    /// give the list and the colour two different answers.
    #[must_use]
    pub fn degraded_hosts(&self) -> Vec<&HostView> {
        self.hosts
            .iter()
            .filter(|host| host.severity >= Severity::Warning)
            .collect()
    }

    /// Recompute the banner as time passes; see [`freshness_at`] for why this is not a field.
    #[must_use]
    pub fn freshness_at(&self, now: DateTime<Utc>) -> Freshness {
        freshness_at(&self.freshness, now)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AlertPhase, Freshness, FreshnessInputs, MetricAvailability, Severity, alertable,
        describe_metric, expected_on, freshness_at,
    };
    use chrono::{DateTime, Duration, Utc};
    use pessimal_core::{AlertState, CoreError, Liveness, MetricKind, MetricUnit, OsFamily};

    use crate::error::{FailureSource, PollFailure};

    /// Six hours, the "restored from a cold cache" gap the design keeps citing.
    const SIX_HOURS: i64 = 6 * 60 * 60;

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_757_000_000 + secs, 0).expect("a valid fixture instant")
    }

    /// The default tuning's derived windows: `2 * poll_interval + metric_step` = 90 s tolerance,
    /// `down_threshold()` = 150 s budget.
    fn folded(clean_at: DateTime<Utc>) -> FreshnessInputs {
        FreshnessInputs {
            as_of: Some(clean_at),
            last_clean_at: Some(clean_at),
            last_roster_at: Some(clean_at),
            consecutive_failures: 0,
            last_failure: None,
            polled_this_session: true,
            tolerance: Duration::seconds(90),
            budget: Duration::seconds(150),
        }
    }

    fn unauthorized() -> PollFailure {
        PollFailure::from_core(&CoreError::Unauthorized, FailureSource::Roster, at(0))
    }

    fn broken_metric() -> PollFailure {
        PollFailure::from_core(
            &CoreError::UnknownMetric("system.cpu.utilisation".to_owned()),
            FailureSource::Series {
                metric: MetricKind::CpuUtilization,
            },
            at(600),
        )
    }

    #[test]
    fn freshness_degrades_as_now_advances_without_a_fold() {
        // The reason freshness is a function of `now`: NOTHING FOLDS between these three
        // assertions. The same inputs walk Fresh -> Degraded -> Unusable purely because time
        // passed. A `Freshness` stored on the view at fold time would answer `Fresh` to all
        // three, which is exactly what a hung poll or a stopped Swift timer looks like.
        let inputs = folded(at(0));

        assert_eq!(
            freshness_at(&inputs, at(90)),
            Freshness::Fresh { at: at(0) }
        );
        assert_eq!(
            freshness_at(&inputs, at(91)),
            Freshness::Degraded {
                last_success: at(0),
                consecutive_failures: 0,
                failure: None,
            },
            "one second past the tolerance the banner is mandatory"
        );
        assert_eq!(
            freshness_at(&inputs, at(151)),
            Freshness::Unusable {
                last_success: Some(at(0)),
                consecutive_failures: 0,
                failure: None,
            },
            "past the age at which we would call a host down"
        );
    }

    #[test]
    fn a_permanently_broken_metric_reads_degraded_not_unusable() {
        // One metric name the backend does not know, failing since the first poll: the roster
        // keeps coming back, so `last_roster_at` keeps advancing while `last_clean_at` never
        // moves off `None`. `Unusable` keys off the roster precisely so one misconfigured
        // request out of five cannot dim the whole screen forever.
        let inputs = FreshnessInputs {
            as_of: Some(at(600)),
            last_clean_at: None,
            last_roster_at: Some(at(600)),
            consecutive_failures: 20,
            last_failure: Some(broken_metric()),
            polled_this_session: true,
            tolerance: Duration::seconds(90),
            budget: Duration::seconds(150),
        };

        assert_eq!(
            freshness_at(&inputs, at(600)),
            Freshness::Degraded {
                last_success: at(600),
                consecutive_failures: 20,
                failure: Some(broken_metric()),
            }
        );
    }

    #[test]
    fn a_relaunch_whose_first_poll_fails_reads_unusable_not_idle() {
        // `polled_this_session` is set by the FIRST fold whatever its outcome, so the 401
        // surfaces instead of hiding behind a six-hour-old cache age.
        let inputs = FreshnessInputs {
            as_of: Some(at(0)),
            last_clean_at: Some(at(-SIX_HOURS)),
            last_roster_at: Some(at(-SIX_HOURS)),
            consecutive_failures: 1,
            last_failure: Some(unauthorized()),
            polled_this_session: true,
            tolerance: Duration::seconds(90),
            budget: Duration::seconds(150),
        };

        let freshness = freshness_at(&inputs, at(0));

        assert_eq!(
            freshness,
            Freshness::Unusable {
                last_success: Some(at(-SIX_HOURS)),
                consecutive_failures: 1,
                failure: Some(unauthorized()),
            },
            "the failure must reach the banner so it can offer Open Settings"
        );
        assert_eq!(freshness.severity(), Severity::Critical);
    }

    #[test]
    fn a_restored_state_reads_idle_not_fresh() {
        // A cache this process has not refreshed must never claim a live connection, and `Idle`
        // is checked before the budget so it reads "as of 6 hours ago" rather than reporting a
        // failure that has not happened yet.
        let inputs = FreshnessInputs {
            polled_this_session: false,
            ..folded(at(-SIX_HOURS))
        };

        let freshness = freshness_at(&inputs, at(0));

        assert_eq!(
            freshness,
            Freshness::Idle {
                last_success: at(-SIX_HOURS),
            }
        );
        assert!(!freshness.is_fresh());
        assert_eq!(freshness.severity(), Severity::Unknown);
    }

    #[test]
    fn first_successful_fold_clears_idle() {
        let restored = FreshnessInputs {
            polled_this_session: false,
            ..folded(at(-SIX_HOURS))
        };
        assert!(matches!(
            freshness_at(&restored, at(0)),
            Freshness::Idle { .. }
        ));

        let after_fold = folded(at(0));

        assert_eq!(
            freshness_at(&after_fold, at(0)),
            Freshness::Fresh { at: at(0) }
        );
    }

    #[test]
    fn windows_load_average_is_unsupported_not_a_gap() {
        // The collector registers the instrument and returns empty forever, so an empty-but-
        // successful response is a platform fact. The fold reads this to store
        // `MetricAvailability::Unsupported` rather than `NotReported`.
        for kind in [
            MetricKind::LoadAverage1m,
            MetricKind::LoadAverage5m,
            MetricKind::LoadAverage15m,
        ] {
            assert!(!expected_on(kind, OsFamily::Windows));
            assert!(expected_on(kind, OsFamily::Linux));
            assert!(expected_on(kind, OsFamily::Darwin));
        }
        assert_ne!(
            MetricAvailability::Unsupported,
            MetricAvailability::NotReported
        );
    }

    #[test]
    fn windows_filesystem_is_not_reported_not_unsupported() {
        // Windows absolutely can report filesystems; a Windows agent left on the default
        // `filesystems = ["/"]` filter simply matches no mount. That is a configuration gap the
        // operator can fix, so it must not be dressed up as a platform limitation.
        assert!(expected_on(
            MetricKind::FilesystemUtilization,
            OsFamily::Windows
        ));
        assert!(expected_on(MetricKind::FilesystemUsage, OsFamily::Windows));
    }

    #[test]
    fn host_severity_is_the_worse_of_liveness_and_alerts() {
        // `Ord` on `Severity` is what makes "the worse of the two" a `max`.
        assert_eq!(
            Severity::from_liveness(Liveness::Alive).max(Severity::from_phase(AlertPhase::Firing)),
            Severity::Critical,
            "a perfectly healthy host with a firing alert is still critical"
        );
        assert_eq!(
            Severity::from_liveness(Liveness::Down).max(Severity::from_phase(AlertPhase::Ok)),
            Severity::Critical,
            "a down host with no alerts is still critical"
        );
        assert_eq!(
            Severity::from_liveness(Liveness::Stale).max(Severity::from_phase(AlertPhase::NoData)),
            Severity::Warning
        );
        assert!(
            Severity::Ok < Severity::Unknown && Severity::Unknown < Severity::Warning,
            "unjudged must not colour as healthy, but must not outrank a real warning"
        );
        assert_eq!(
            AlertPhase::from_state(AlertState::Firing { since: at(0) }),
            AlertPhase::Firing
        );
    }

    #[test]
    fn a_counter_metric_view_is_flagged_is_rate() {
        // `NetworkIo` arrives as a cumulative total and is normalised to bytes per second, so a
        // threshold on it is a rate. The axis label must come from this flag and the kind, never
        // from the unit string.
        let network = describe_metric(MetricKind::NetworkIo);
        assert!(network.is_rate);
        assert_eq!(network.unit, MetricUnit::Bytes);
        assert!(network.alertable);

        let collection_failures = describe_metric(MetricKind::AgentCollectionFailures);
        assert!(collection_failures.is_rate);

        let cpu = describe_metric(MetricKind::CpuUtilization);
        assert!(!cpu.is_rate, "a gauge is a reading, not a rate");

        assert!(
            !alertable(MetricKind::AgentHeartbeat),
            "a silent host is liveness's business, never an alert's"
        );
    }
}
