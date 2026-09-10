//! Alert rules and their evaluation.
//!
//! An alert is a threshold on a metric that must hold continuously for a dwell period before it
//! fires. The dwell (`for_duration`) is what separates a real condition from a single noisy
//! sample, and it is why evaluation is stateful: [`AlertEvaluation`] carries the instant the
//! breach began across polls.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::CoreError;
use crate::host::{HostId, HostSelector};
use crate::metric::{MetricKind, MetricSeries};
use crate::urn::Urn;

/// How a sample is compared against the threshold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Comparator {
    GreaterThan,
    GreaterThanOrEqual,
    LessThan,
    LessThanOrEqual,
}

impl Comparator {
    #[must_use]
    pub fn evaluate(self, value: f64, threshold: f64) -> bool {
        match self {
            Self::GreaterThan => value > threshold,
            Self::GreaterThanOrEqual => value >= threshold,
            Self::LessThan => value < threshold,
            Self::LessThanOrEqual => value <= threshold,
        }
    }

    #[must_use]
    pub fn symbol(self) -> &'static str {
        match self {
            Self::GreaterThan => ">",
            Self::GreaterThanOrEqual => ">=",
            Self::LessThan => "<",
            Self::LessThanOrEqual => "<=",
        }
    }
}

/// A user-configured alert.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "AlertRuleWire")]
pub struct AlertRule {
    id: Urn,
    pub name: String,
    pub metric: MetricKind,
    pub selector: HostSelector,
    pub comparator: Comparator,
    pub threshold: f64,
    /// How long the breach must hold before the rule fires. Zero fires on the first breaching
    /// sample.
    for_duration: Duration,
    pub enabled: bool,
}

impl AlertRule {
    /// The longest dwell a rule may carry: one year.
    ///
    /// The bound exists to keep `since + for_duration` from overflowing, not to police UX. Every
    /// consumer of a `Pending` state adds the dwell to the instant the breach began —
    /// `pessimal_client_core`'s `AlertView::fires_at` is the one users see — and
    /// `DateTime + Duration` panics rather than erroring. `Duration` spans roughly a thousand
    /// times more than `DateTime<Utc>` can represent, so without an absolute ceiling a dwell that
    /// is merely large is indistinguishable from one that is fatal, and the panic crosses UniFFI
    /// as an app crash on every launch: the rule is in `FleetConfig`, the `Pending` evaluation is
    /// in `FleetState`, and both are persisted.
    ///
    /// A year is generous rather than tuned — a dwell measured in months has already stopped
    /// describing anything an operator would wait for — and it sits five orders of magnitude below
    /// the overflow threshold, so no arithmetic downstream of it can get close either.
    pub const MAX_FOR_DURATION: Duration = Duration::days(365);

    /// # Errors
    /// Returns [`CoreError::InvalidRule`] for an empty name, a non-finite threshold, a negative
    /// dwell, or a dwell above [`AlertRule::MAX_FOR_DURATION`]; [`CoreError::InvalidUrn`] if the
    /// environment is not a usable URN segment.
    pub fn new(
        environment: &str,
        name: impl Into<String>,
        metric: MetricKind,
        comparator: Comparator,
        threshold: f64,
        for_duration: Duration,
    ) -> Result<Self, CoreError> {
        let name = name.into();
        if name.trim().is_empty() {
            return Err(CoreError::InvalidRule("name must not be empty".to_owned()));
        }
        if !threshold.is_finite() {
            return Err(CoreError::InvalidRule(format!(
                "threshold {threshold} must be finite"
            )));
        }
        if for_duration < Duration::zero() {
            return Err(CoreError::InvalidRule(
                "for_duration must not be negative".to_owned(),
            ));
        }
        if for_duration > Self::MAX_FOR_DURATION {
            return Err(CoreError::InvalidRule(format!(
                "for_duration {}s exceeds the maximum dwell of {}s, past which `since + \
                 for_duration` overflows the representable range of a timestamp",
                for_duration.num_seconds(),
                Self::MAX_FOR_DURATION.num_seconds()
            )));
        }
        Ok(Self {
            id: Urn::with_id(environment, "alerts", "rule", Uuid::now_v7())?,
            name,
            metric,
            selector: HostSelector::All,
            comparator,
            threshold,
            for_duration,
            enabled: true,
        })
    }

    #[must_use]
    pub fn with_selector(mut self, selector: HostSelector) -> Self {
        self.selector = selector;
        self
    }

    #[must_use]
    pub fn disabled(mut self) -> Self {
        self.enabled = false;
        self
    }

    #[must_use]
    pub fn id(&self) -> &Urn {
        &self.id
    }

    #[must_use]
    pub fn for_duration(&self) -> Duration {
        self.for_duration
    }

    /// Whether a single value breaches the threshold, ignoring dwell.
    #[must_use]
    pub fn breaches(&self, value: f64) -> bool {
        self.comparator.evaluate(value, self.threshold)
    }

    /// A one-line description, e.g. `CPU > 0.9 for 5m`.
    #[must_use]
    pub fn describe(&self) -> String {
        format!(
            "{} {} {} for {}s",
            self.metric.display_name(),
            self.comparator.symbol(),
            self.threshold,
            self.for_duration.num_seconds()
        )
    }
}

/// The only shape [`AlertRule`] deserialises through, so a persisted rule is held to the same
/// invariants [`AlertRule::new`] enforces.
///
/// The id is carried through rather than minted: a restored rule is the same rule, and a fresh id
/// would orphan every alert evaluation keyed to it.
#[derive(Deserialize)]
struct AlertRuleWire {
    id: Urn,
    name: String,
    metric: MetricKind,
    selector: HostSelector,
    comparator: Comparator,
    threshold: f64,
    for_duration: Duration,
    enabled: bool,
}

impl TryFrom<AlertRuleWire> for AlertRule {
    type Error = CoreError;

    fn try_from(wire: AlertRuleWire) -> Result<Self, Self::Error> {
        let validated = Self::new(
            wire.id.environment(),
            wire.name,
            wire.metric,
            wire.comparator,
            wire.threshold,
            wire.for_duration,
        )?;
        Ok(Self {
            id: wire.id,
            selector: wire.selector,
            enabled: wire.enabled,
            ..validated
        })
    }
}

/// Where a rule stands for one host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AlertState {
    /// Not breaching.
    Ok,
    /// Breaching, but the dwell period has not elapsed. Carries when the breach started.
    Pending { since: DateTime<Utc> },
    /// Breaching and past the dwell period. Carries when the breach started, not when it fired.
    Firing { since: DateTime<Utc> },
    /// No data for this host and metric — nothing to judge. Never fires; a silent host is
    /// [`crate::Liveness`]'s business, not an alert's.
    NoData,
}

impl AlertState {
    #[must_use]
    pub fn is_firing(self) -> bool {
        matches!(self, Self::Firing { .. })
    }

    /// When the current breach began, for `Pending` and `Firing`.
    #[must_use]
    pub fn breaching_since(self) -> Option<DateTime<Utc>> {
        match self {
            Self::Pending { since } | Self::Firing { since } => Some(since),
            Self::Ok | Self::NoData => None,
        }
    }
}

/// A rule's running state for one host, carried across polls.
///
/// Construct with [`AlertEvaluation::new`] and feed each poll's series to
/// [`AlertEvaluation::observe`]. The evaluation is a value: the caller owns where it is stored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AlertEvaluation {
    pub rule_id: Urn,
    pub host: HostId,
    state: AlertState,
}

impl AlertEvaluation {
    #[must_use]
    pub fn new(rule_id: Urn, host: HostId) -> Self {
        Self {
            rule_id,
            host,
            state: AlertState::NoData,
        }
    }

    #[must_use]
    pub fn state(&self) -> AlertState {
        self.state
    }

    /// Folds one poll's worth of data into the state and returns the new state.
    ///
    /// `series` is the data fetched for this rule's metric and host and `now` is the evaluation
    /// instant. `max_age` is how old the newest sample may be before it stops counting as
    /// evidence: past that, the rule reports [`AlertState::NoData`] rather than judging the host
    /// on a reading that has stopped being refreshed.
    ///
    /// That bound is not optional. Polling returns whatever the backend still holds, so an agent
    /// that dies mid-breach would otherwise leave its last breaching sample in every subsequent
    /// response — and the rule would dutifully fire, and keep firing, on a host nobody has heard
    /// from since. A silent host is [`crate::Liveness`]'s business to report, not an alert's.
    ///
    /// The bound is supplied rather than derived because only the caller knows its read path.
    /// `max_age` is measured from `now`, and `now` for most backends is not an instant a query can
    /// observe: a columnar metrics backend batches on write, so its newest *queryable* sample is
    /// already some constant behind wall clock however healthy the agent is. A caller that passes
    /// only the age it would tolerate from a *local* collector refuses every sample a remote one
    /// can give it. `pessimal_client_core` passes `PollTuning::evidence_horizon`, which is its
    /// staleness bound plus its backend lag allowance.
    ///
    /// `now` itself is three things at once — the ceiling on admissible samples
    /// ([`MetricSeries::latest_at`] discards anything newer), the instant `max_age` counts back
    /// from, and the instant dwell is measured to — so it must stay the real evaluation instant. A
    /// caller tempted to hand it a backdated instant to buy staleness headroom would throw away
    /// the freshest sample it has and delay every fire by however far it backdated.
    ///
    /// A disabled rule resets to [`AlertState::Ok`] so it cannot linger as firing.
    pub fn observe(
        &mut self,
        rule: &AlertRule,
        series: &MetricSeries,
        now: DateTime<Utc>,
        max_age: Duration,
    ) -> AlertState {
        if !rule.enabled || !rule.selector.matches(&self.host) {
            self.state = AlertState::Ok;
            return self.state;
        }

        let Some(point) = series.latest_at(now) else {
            self.state = AlertState::NoData;
            return self.state;
        };

        if now - point.at > max_age {
            self.state = AlertState::NoData;
            return self.state;
        }

        self.state = if rule.breaches(point.value) {
            // A breach already under way keeps its original start, so dwell accumulates across
            // polls rather than restarting on every observation.
            let since = self.state.breaching_since().unwrap_or(point.at);
            if now - since >= rule.for_duration() {
                AlertState::Firing { since }
            } else {
                AlertState::Pending { since }
            }
        } else {
            AlertState::Ok
        };
        self.state
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metric::MetricPoint;

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_760_000_000 + secs, 0).expect("valid timestamp")
    }

    fn host() -> HostId {
        HostId::new("web-1")
    }

    fn rule(for_secs: i64) -> AlertRule {
        AlertRule::new(
            "prod",
            "CPU hot",
            MetricKind::CpuUtilization,
            Comparator::GreaterThan,
            0.9,
            Duration::seconds(for_secs),
        )
        .expect("valid rule")
    }

    fn series(points: Vec<MetricPoint>) -> MetricSeries {
        MetricSeries::new(host(), MetricKind::CpuUtilization, points)
    }

    fn evaluation(rule: &AlertRule) -> AlertEvaluation {
        AlertEvaluation::new(rule.id().clone(), host())
    }

    /// A staleness bound wide enough that only the tests about staleness feel it.
    fn fresh() -> Duration {
        Duration::days(1)
    }

    #[test]
    fn comparators_evaluate_their_boundaries() {
        assert!(Comparator::GreaterThan.evaluate(0.91, 0.9));
        assert!(!Comparator::GreaterThan.evaluate(0.9, 0.9));
        assert!(Comparator::GreaterThanOrEqual.evaluate(0.9, 0.9));
        assert!(Comparator::LessThan.evaluate(0.1, 0.9));
        assert!(!Comparator::LessThan.evaluate(0.9, 0.9));
        assert!(Comparator::LessThanOrEqual.evaluate(0.9, 0.9));
    }

    #[test]
    fn rules_carry_a_sortable_uuid_v7_identity() {
        assert_eq!(rule(0).id().id().get_version_num(), 7);
    }

    #[test]
    fn rejects_an_empty_name() {
        assert!(
            AlertRule::new(
                "prod",
                "   ",
                MetricKind::CpuUtilization,
                Comparator::GreaterThan,
                0.9,
                Duration::zero(),
            )
            .is_err()
        );
    }

    #[test]
    fn rejects_a_non_finite_threshold() {
        assert!(
            AlertRule::new(
                "prod",
                "NaN",
                MetricKind::CpuUtilization,
                Comparator::GreaterThan,
                f64::NAN,
                Duration::zero(),
            )
            .is_err()
        );
    }

    #[test]
    fn rejects_a_negative_dwell() {
        assert!(
            AlertRule::new(
                "prod",
                "Backwards",
                MetricKind::CpuUtilization,
                Comparator::GreaterThan,
                0.9,
                Duration::seconds(-1),
            )
            .is_err()
        );
    }

    #[test]
    fn accepts_the_maximum_dwell_and_rejects_a_second_past_it() {
        let with_dwell = |dwell| {
            AlertRule::new(
                "prod",
                "Patient",
                MetricKind::CpuUtilization,
                Comparator::GreaterThan,
                0.9,
                dwell,
            )
        };

        assert!(
            with_dwell(AlertRule::MAX_FOR_DURATION).is_ok(),
            "the boundary itself is legal; the ceiling is a guard, not a preference"
        );
        assert!(matches!(
            with_dwell(AlertRule::MAX_FOR_DURATION + Duration::seconds(1)),
            Err(CoreError::InvalidRule(_))
        ));

        // The value from the reproduction: legal as a `Duration`, fatal as an addend to a
        // timestamp. Without the ceiling this rule reached `since + for_duration` and panicked.
        assert!(matches!(
            with_dwell(Duration::seconds(10_000_000_000_000)),
            Err(CoreError::InvalidRule(_))
        ));
    }

    #[test]
    fn a_dwell_at_the_ceiling_cannot_overflow_a_realistic_breach_instant() {
        assert!(
            at(0)
                .checked_add_signed(AlertRule::MAX_FOR_DURATION)
                .is_some(),
            "the ceiling exists precisely so this addition is total"
        );
    }

    #[test]
    fn an_empty_series_is_no_data_not_ok() {
        let rule = rule(0);
        let mut eval = evaluation(&rule);
        assert_eq!(
            eval.observe(&rule, &series(vec![]), at(0), fresh()),
            AlertState::NoData
        );
    }

    #[test]
    fn a_zero_dwell_rule_fires_on_the_first_breach() {
        let rule = rule(0);
        let mut eval = evaluation(&rule);
        let state = eval.observe(
            &rule,
            &series(vec![MetricPoint::new(at(0), 0.95)]),
            at(0),
            fresh(),
        );
        assert_eq!(state, AlertState::Firing { since: at(0) });
    }

    #[test]
    fn a_breach_dwells_as_pending_before_it_fires() {
        let rule = rule(120);
        let mut eval = evaluation(&rule);

        let data = series(vec![MetricPoint::new(at(0), 0.95)]);
        assert_eq!(
            eval.observe(&rule, &data, at(0), fresh()),
            AlertState::Pending { since: at(0) }
        );
        assert_eq!(
            eval.observe(&rule, &data, at(60), fresh()),
            AlertState::Pending { since: at(0) }
        );
        assert_eq!(
            eval.observe(&rule, &data, at(120), fresh()),
            AlertState::Firing { since: at(0) }
        );
    }

    #[test]
    fn dwell_accumulates_across_polls_rather_than_restarting() {
        let rule = rule(120);
        let mut eval = evaluation(&rule);

        eval.observe(
            &rule,
            &series(vec![MetricPoint::new(at(0), 0.95)]),
            at(0),
            fresh(),
        );
        // A newer breaching sample arrives; the breach start must stay at the original point.
        let state = eval.observe(
            &rule,
            &series(vec![MetricPoint::new(at(130), 0.99)]),
            at(130),
            fresh(),
        );
        assert_eq!(state, AlertState::Firing { since: at(0) });
    }

    #[test]
    fn recovery_clears_the_dwell_so_the_next_breach_starts_over() {
        let rule = rule(120);
        let mut eval = evaluation(&rule);

        eval.observe(
            &rule,
            &series(vec![MetricPoint::new(at(0), 0.95)]),
            at(0),
            fresh(),
        );
        assert_eq!(
            eval.observe(
                &rule,
                &series(vec![MetricPoint::new(at(60), 0.1)]),
                at(60),
                fresh()
            ),
            AlertState::Ok
        );

        let state = eval.observe(
            &rule,
            &series(vec![MetricPoint::new(at(120), 0.95)]),
            at(120),
            fresh(),
        );
        assert_eq!(
            state,
            AlertState::Pending { since: at(120) },
            "the earlier breach must not count towards this one"
        );
    }

    #[test]
    fn a_disabled_rule_never_fires_and_clears_an_existing_breach() {
        let rule = rule(0);
        let mut eval = evaluation(&rule);
        eval.observe(
            &rule,
            &series(vec![MetricPoint::new(at(0), 0.95)]),
            at(0),
            fresh(),
        );
        assert!(eval.state().is_firing());

        let disabled = rule.disabled();
        assert_eq!(
            eval.observe(
                &disabled,
                &series(vec![MetricPoint::new(at(0), 0.95)]),
                at(0),
                fresh()
            ),
            AlertState::Ok
        );
    }

    #[test]
    fn a_rule_scoped_elsewhere_does_not_fire_on_this_host() {
        let rule = rule(0).with_selector(HostSelector::Host(HostId::new("db-1")));
        let mut eval = evaluation(&rule);
        assert_eq!(
            eval.observe(
                &rule,
                &series(vec![MetricPoint::new(at(0), 0.95)]),
                at(0),
                fresh()
            ),
            AlertState::Ok
        );
    }

    #[test]
    fn evaluation_ignores_samples_from_after_the_evaluation_instant() {
        let rule = rule(0);
        let mut eval = evaluation(&rule);
        let data = series(vec![MetricPoint::new(at(300), 0.99)]);
        assert_eq!(
            eval.observe(&rule, &data, at(0), fresh()),
            AlertState::NoData
        );
    }

    #[test]
    fn a_sample_older_than_the_staleness_bound_is_no_data_not_a_breach() {
        let rule = rule(0);
        let mut eval = evaluation(&rule);
        let data = series(vec![MetricPoint::new(at(0), 0.95)]);

        assert_eq!(
            eval.observe(&rule, &data, at(0), Duration::seconds(150)),
            AlertState::Firing { since: at(0) }
        );
        // The agent goes quiet. The backend keeps returning that same last sample.
        assert_eq!(
            eval.observe(&rule, &data, at(600), Duration::seconds(150)),
            AlertState::NoData,
            "a rule must not keep firing on a host nobody has heard from"
        );
    }

    #[test]
    fn the_staleness_bound_is_inclusive() {
        let rule = rule(0);
        let mut eval = evaluation(&rule);
        let data = series(vec![MetricPoint::new(at(0), 0.95)]);

        assert!(
            eval.observe(&rule, &data, at(150), Duration::seconds(150))
                .is_firing()
        );
        assert_eq!(
            eval.observe(&rule, &data, at(151), Duration::seconds(150)),
            AlertState::NoData
        );
    }

    #[test]
    fn a_stale_sample_clears_the_dwell_so_recovery_starts_over() {
        let rule = rule(120);
        let mut eval = evaluation(&rule);
        let bound = Duration::seconds(150);

        eval.observe(
            &rule,
            &series(vec![MetricPoint::new(at(0), 0.95)]),
            at(0),
            bound,
        );
        eval.observe(
            &rule,
            &series(vec![MetricPoint::new(at(0), 0.95)]),
            at(600),
            bound,
        );
        assert_eq!(eval.state(), AlertState::NoData);

        // The agent comes back, still hot. The dwell must restart, not resume from at(0).
        let state = eval.observe(
            &rule,
            &series(vec![MetricPoint::new(at(600), 0.95)]),
            at(600),
            bound,
        );
        assert_eq!(state, AlertState::Pending { since: at(600) });
    }

    #[test]
    fn describe_reads_as_a_sentence() {
        assert_eq!(rule(300).describe(), "CPU > 0.9 for 300s");
    }
}
