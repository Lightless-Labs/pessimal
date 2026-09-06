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
    /// # Errors
    /// Returns [`CoreError::InvalidRule`] for an empty name, a non-finite threshold, or a negative
    /// dwell; [`CoreError::InvalidUrn`] if the environment is not a usable URN segment.
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
    /// `series` is the data fetched for this rule's metric and host; `now` is the evaluation
    /// instant. A disabled rule resets to [`AlertState::Ok`] so it cannot linger as firing.
    pub fn observe(
        &mut self,
        rule: &AlertRule,
        series: &MetricSeries,
        now: DateTime<Utc>,
    ) -> AlertState {
        if !rule.enabled || !rule.selector.matches(&self.host) {
            self.state = AlertState::Ok;
            return self.state;
        }

        let Some(point) = series.latest_at(now) else {
            self.state = AlertState::NoData;
            return self.state;
        };

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
    fn an_empty_series_is_no_data_not_ok() {
        let rule = rule(0);
        let mut eval = evaluation(&rule);
        assert_eq!(
            eval.observe(&rule, &series(vec![]), at(0)),
            AlertState::NoData
        );
    }

    #[test]
    fn a_zero_dwell_rule_fires_on_the_first_breach() {
        let rule = rule(0);
        let mut eval = evaluation(&rule);
        let state = eval.observe(&rule, &series(vec![MetricPoint::new(at(0), 0.95)]), at(0));
        assert_eq!(state, AlertState::Firing { since: at(0) });
    }

    #[test]
    fn a_breach_dwells_as_pending_before_it_fires() {
        let rule = rule(120);
        let mut eval = evaluation(&rule);

        let data = series(vec![MetricPoint::new(at(0), 0.95)]);
        assert_eq!(
            eval.observe(&rule, &data, at(0)),
            AlertState::Pending { since: at(0) }
        );
        assert_eq!(
            eval.observe(&rule, &data, at(60)),
            AlertState::Pending { since: at(0) }
        );
        assert_eq!(
            eval.observe(&rule, &data, at(120)),
            AlertState::Firing { since: at(0) }
        );
    }

    #[test]
    fn dwell_accumulates_across_polls_rather_than_restarting() {
        let rule = rule(120);
        let mut eval = evaluation(&rule);

        eval.observe(&rule, &series(vec![MetricPoint::new(at(0), 0.95)]), at(0));
        // A newer breaching sample arrives; the breach start must stay at the original point.
        let state = eval.observe(
            &rule,
            &series(vec![MetricPoint::new(at(130), 0.99)]),
            at(130),
        );
        assert_eq!(state, AlertState::Firing { since: at(0) });
    }

    #[test]
    fn recovery_clears_the_dwell_so_the_next_breach_starts_over() {
        let rule = rule(120);
        let mut eval = evaluation(&rule);

        eval.observe(&rule, &series(vec![MetricPoint::new(at(0), 0.95)]), at(0));
        assert_eq!(
            eval.observe(&rule, &series(vec![MetricPoint::new(at(60), 0.1)]), at(60)),
            AlertState::Ok
        );

        let state = eval.observe(
            &rule,
            &series(vec![MetricPoint::new(at(120), 0.95)]),
            at(120),
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
        eval.observe(&rule, &series(vec![MetricPoint::new(at(0), 0.95)]), at(0));
        assert!(eval.state().is_firing());

        let disabled = rule.disabled();
        assert_eq!(
            eval.observe(
                &disabled,
                &series(vec![MetricPoint::new(at(0), 0.95)]),
                at(0)
            ),
            AlertState::Ok
        );
    }

    #[test]
    fn a_rule_scoped_elsewhere_does_not_fire_on_this_host() {
        let rule = rule(0).with_selector(HostSelector::Host(HostId::new("db-1")));
        let mut eval = evaluation(&rule);
        assert_eq!(
            eval.observe(&rule, &series(vec![MetricPoint::new(at(0), 0.95)]), at(0)),
            AlertState::Ok
        );
    }

    #[test]
    fn evaluation_ignores_samples_from_after_the_evaluation_instant() {
        let rule = rule(0);
        let mut eval = evaluation(&rule);
        let data = series(vec![MetricPoint::new(at(300), 0.99)]);
        assert_eq!(eval.observe(&rule, &data, at(0)), AlertState::NoData);
    }

    #[test]
    fn describe_reads_as_a_sentence() {
        assert_eq!(rule(300).describe(), "CPU > 0.9 for 300s");
    }
}
