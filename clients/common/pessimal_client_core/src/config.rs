//! What to poll, how often, and over what windows.
//!
//! Two values: [`PollTuning`], the arithmetic every other module reads a duration out of, and
//! [`FleetConfig`], the user-facing set of rules and metrics wrapped around it.
//!
//! Both deserialise through a private wire type rather than over their own fields. That is the
//! load-bearing part of this module. `pessimal_core::LivenessPolicy` used to derive `Deserialize`
//! straight onto its private fields, so a persisted policy could come back with
//! `stale_after_intervals >= down_after_intervals` and invert the entire liveness model with no
//! error anywhere; core has since closed it with a validating wire type and these two are built
//! the same way from the start. A validated constructor that `serde` walks around is a doc
//! comment, not an invariant — and both of these values are persisted and may be synced.
//!
//! The interlocks in [`PollTuning::new`] are not taste. Each one exists because violating it
//! produces a *plausible-looking* fleet that is wrong: a `max_staleness` under three metric steps
//! makes `AlertEvaluation::observe` return `NoData` on a perfectly successful poll, because the
//! freshest complete SigNoz bucket is already `[step, 2*step)` old before any poll latency.

use chrono::Duration;
use pessimal_core::{AlertRule, HostId, HostSelector, LivenessPolicy, MetricKind};
use serde::{Deserialize, Serialize};

use crate::error::{ClientError, Result};

/// CPU, Memory, Disk, Load (1m), Collection failures. Five fleet-wide requests per poll, not
/// twelve.
///
/// Collection failures is in the default set because it is the only signal that catches an agent
/// beating happily while its sampling fails every cycle: a failed collection re-exports the
/// previous snapshot with fresh timestamps, so every other metric looks like a flat line rather
/// than a gap.
pub const DEFAULT_OVERVIEW_METRICS: [MetricKind; 5] = [
    MetricKind::CpuUtilization,
    MetricKind::MemoryUtilization,
    MetricKind::FilesystemUtilization,
    MetricKind::LoadAverage1m,
    MetricKind::AgentCollectionFailures,
];

/// Every modelled metric except [`MetricKind::AgentHeartbeat`], whose value nothing reads —
/// liveness comes from the *timestamp* of the newest heartbeat bucket, never from its count.
pub const DEFAULT_DETAIL_METRICS: [MetricKind; 11] = [
    MetricKind::CpuUtilization,
    MetricKind::MemoryUtilization,
    MetricKind::MemoryUsage,
    MetricKind::FilesystemUtilization,
    MetricKind::FilesystemUsage,
    MetricKind::NetworkIo,
    MetricKind::LoadAverage1m,
    MetricKind::LoadAverage5m,
    MetricKind::LoadAverage15m,
    MetricKind::SystemUptime,
    MetricKind::AgentCollectionFailures,
];

/// The chart window the preset picks: long enough for a readable line at any sane step, short
/// enough that refetching it whole for one focused host stays affordable.
const PRESET_CHART_WINDOW_HOURS: i64 = 1;

/// How long a host survives after it stops appearing in the roster. Long enough that an overnight
/// reboot is still the same machine in the morning.
const PRESET_FORGET_HOST_AFTER_HOURS: i64 = 24;

/// The hard cap on retained hosts. A backend churning hostnames — containers, CI runners —
/// otherwise accumulates a day of ghosts, each carrying an evaluation per rule.
const PRESET_MAX_RETAINED_HOSTS: u32 = 256;

/// The ceiling on [`PollTuning::backoff_after`]. Past five minutes a monitoring app has stopped
/// monitoring.
const BACKOFF_CEILING_MINUTES: i64 = 5;

/// The largest doubling `backoff_after` applies, so the multiplier stays inside `i32`.
const BACKOFF_MAX_DOUBLINGS: u32 = 5;

/// The URN segment separator. An environment containing it would produce a rule id that cannot be
/// parsed back from its own `Display`.
const URN_SEPARATOR: &str = "::";

/// Checked multiplication saturating at the widest representable span.
///
/// These feed `#[must_use]` getters the whole crate calls, and `chrono::Duration`'s `Mul` panics
/// on overflow. A panic here would cross UniFFI as an app crash rather than as an error, and
/// saturating is monotone-correct for every comparison the callers make. Reaching the bound needs
/// a heartbeat interval measured in millions of years.
fn scaled(duration: Duration, factor: i32) -> Duration {
    duration.checked_mul(factor).unwrap_or(Duration::MAX)
}

/// Checked addition saturating at the widest representable span. See [`scaled`].
fn summed(left: Duration, right: Duration) -> Duration {
    left.checked_add(&right).unwrap_or(Duration::MAX)
}

/// Every duration this crate measures a poll, a window, or a dwell against.
///
/// Fields are private and the derived windows are functions, not fields, so a caller cannot set
/// `overview_window` to something inconsistent with `metric_step`. Construct it with
/// [`PollTuning::from_liveness`] or [`PollTuning::new`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "PollTuningWire", into = "PollTuningWire")]
pub struct PollTuning {
    liveness: LivenessPolicy,
    poll_interval: Duration,
    metric_step: Duration,
    chart_window: Duration,
    max_staleness: Duration,
    forget_host_after: Duration,
    max_retained_hosts: u32,
}

impl PollTuning {
    /// # Errors
    /// [`ClientError::InvalidTuning`] unless every interlock holds:
    /// `poll_interval > 0`; `metric_step >= 1s` (the adapter truncates with
    /// `num_seconds().max(1)`); `3 * metric_step <= max_staleness` (the freshest complete SigNoz
    /// bucket is already `[step, 2*step)` old before poll latency);
    /// `max_staleness <= liveness.down_threshold()` (an alert must not outlive liveness);
    /// `chart_window >= max_staleness + metric_step`;
    /// `forget_host_after >= liveness.down_threshold()`;
    /// `staleness_tolerance() <= freshness_budget()` (so the freshness ladder cannot invert);
    /// `max_retained_hosts >= 1`; and, on the embedded policy itself,
    /// `liveness.heartbeat_interval() > 0`,
    /// `liveness.stale_threshold() < liveness.down_threshold()`, and
    /// `liveness.stale_threshold() >= 3 * liveness.heartbeat_interval()`, which is the only way
    /// to check the documented `stale_after_intervals >= 3` given the multipliers are private.
    pub fn new(
        liveness: LivenessPolicy,
        poll_interval: Duration,
        metric_step: Duration,
        chart_window: Duration,
        max_staleness: Duration,
        forget_host_after: Duration,
        max_retained_hosts: u32,
    ) -> Result<Self> {
        let tuning = Self {
            liveness,
            poll_interval,
            metric_step,
            chart_window,
            max_staleness,
            forget_host_after,
            max_retained_hosts,
        };
        tuning.check()?;
        Ok(tuning)
    }

    /// The preset that satisfies every interlock: `poll_interval` and `metric_step` =
    /// `heartbeat_interval()`, `max_staleness` = `down_threshold()`, `chart_window` = 1 hour,
    /// `forget_host_after` = 24 hours, `max_retained_hosts` = 256.
    ///
    /// Infallible by design — this is the value a client falls back to, and a panic or an error
    /// here would be a monitoring app that refuses to start. The formulas hold for any policy a
    /// user would plausibly write, but they are *derived from* the policy rather than checked
    /// against it: a pathological one (a sub-second beat, `down_after_intervals == 2`, a
    /// heartbeat interval so long that `down_threshold()` exceeds 24 hours) yields a tuning that
    /// [`PollTuning::new`] would reject. Route an untrusted or user-edited policy through `new`,
    /// which validates; use this for the defaults.
    #[must_use]
    pub fn from_liveness(liveness: LivenessPolicy) -> Self {
        let interval = liveness.heartbeat_interval();
        Self {
            liveness,
            poll_interval: interval,
            metric_step: interval,
            chart_window: Duration::hours(PRESET_CHART_WINDOW_HOURS),
            max_staleness: liveness.down_threshold(),
            forget_host_after: Duration::hours(PRESET_FORGET_HOST_AFTER_HOURS),
            max_retained_hosts: PRESET_MAX_RETAINED_HOSTS,
        }
    }

    #[must_use]
    pub fn liveness(&self) -> LivenessPolicy {
        self.liveness
    }

    /// The step the adapter must be built with for `list_hosts`. Nothing here can enforce it —
    /// wire `SignozConfig::with_heartbeat_interval(tuning.heartbeat_interval())` at construction.
    /// A coarser value inflates every measured heartbeat age by a bucket and reads as a healthy
    /// fleet going Stale with no error anywhere.
    #[must_use]
    pub fn heartbeat_interval(&self) -> Duration {
        self.liveness.heartbeat_interval()
    }

    #[must_use]
    pub fn poll_interval(&self) -> Duration {
        self.poll_interval
    }

    #[must_use]
    pub fn metric_step(&self) -> Duration {
        self.metric_step
    }

    #[must_use]
    pub fn chart_window(&self) -> Duration {
        self.chart_window
    }

    #[must_use]
    pub fn max_staleness(&self) -> Duration {
        self.max_staleness
    }

    #[must_use]
    pub fn forget_host_after(&self) -> Duration {
        self.forget_host_after
    }

    #[must_use]
    pub fn max_retained_hosts(&self) -> u32 {
        self.max_retained_hosts
    }

    /// `down_threshold() + 2 * heartbeat_interval()` — a Down host must still be listed.
    ///
    /// Derived, not supplied, so it cannot be set shorter than the age at which liveness would
    /// call a host down; that combination makes a silent host vanish from the roster at exactly
    /// the moment an operator needs to see it.
    #[must_use]
    pub fn host_window(&self) -> Duration {
        summed(
            self.liveness.down_threshold(),
            scaled(self.liveness.heartbeat_interval(), 2),
        )
    }

    /// `max_staleness() + 2 * metric_step()` — about seven buckets at defaults.
    #[must_use]
    pub fn overview_window(&self) -> Duration {
        summed(self.max_staleness, scaled(self.metric_step, 2))
    }

    /// `2 * poll_interval() + metric_step()` — two missed polls plus a bucket. Past this the
    /// view is `Degraded`.
    #[must_use]
    pub fn staleness_tolerance(&self) -> Duration {
        summed(scaled(self.poll_interval, 2), self.metric_step)
    }

    /// `down_threshold()` — past the age at which we would call a host down, the whole view is
    /// past the age at which it deserves to be believed. Past this it is `Unusable`.
    #[must_use]
    pub fn freshness_budget(&self) -> Duration {
        self.liveness.down_threshold()
    }

    /// `min(poll_interval * 2^min(n, 5), 5min)`.
    ///
    /// Deterministic and unjittered on purpose: Swift may add jitter, core stays reproducible so
    /// a backoff schedule is exact in a test.
    #[must_use]
    pub fn backoff_after(&self, consecutive_failures: u32) -> Duration {
        let ceiling = Duration::minutes(BACKOFF_CEILING_MINUTES);
        let doublings = consecutive_failures.min(BACKOFF_MAX_DOUBLINGS);
        scaled(self.poll_interval, 2_i32.pow(doublings)).min(ceiling)
    }

    /// Every interlock, in one place so `new` and the wire type cannot drift apart.
    fn check(&self) -> Result<()> {
        // The policy first: everything below is measured against its thresholds, so a broken
        // policy would otherwise surface as a confusing complaint about some other field.
        //
        // `LivenessPolicy::new` already guards the interval and the ordering, and core now routes
        // `Deserialize` through a validating wire type too. These stay because they are *this
        // crate's* invariants rather than a bet on another crate keeping them: the third one is
        // ours alone, and it is the only way to check the documented `stale_after_intervals >= 3`
        // given the multipliers are private.
        if self.liveness.heartbeat_interval() <= Duration::zero() {
            return Err(ClientError::InvalidTuning(format!(
                "heartbeat interval {}s must be positive",
                self.liveness.heartbeat_interval().num_seconds()
            )));
        }
        if self.liveness.stale_threshold() >= self.liveness.down_threshold() {
            return Err(ClientError::InvalidTuning(format!(
                "stale threshold {}s must be below down threshold {}s",
                self.liveness.stale_threshold().num_seconds(),
                self.liveness.down_threshold().num_seconds()
            )));
        }
        // Three beats of headroom, not two: a heartbeat's timestamp comes back quantised to the
        // start of its query bucket, so a healthy host's measured age is already up to a full
        // interval older than its true age.
        let three_beats = scaled(self.liveness.heartbeat_interval(), 3);
        if self.liveness.stale_threshold() < three_beats {
            return Err(ClientError::InvalidTuning(format!(
                "stale threshold {}s must be at least three heartbeat intervals ({}s), or a \
                 healthy host flickers stale on bucket quantisation alone",
                self.liveness.stale_threshold().num_seconds(),
                three_beats.num_seconds()
            )));
        }

        if self.poll_interval <= Duration::zero() {
            return Err(ClientError::InvalidTuning(format!(
                "poll interval {}s must be positive",
                self.poll_interval.num_seconds()
            )));
        }
        // The SigNoz adapter truncates the step with `num_seconds().max(1)`, so anything under a
        // second is silently rounded up and the plan stops describing the query it produces.
        if self.metric_step < Duration::seconds(1) {
            return Err(ClientError::InvalidTuning(format!(
                "metric step {}ms must be at least 1s; the adapter truncates sub-second steps",
                self.metric_step.num_milliseconds()
            )));
        }
        let three_steps = scaled(self.metric_step, 3);
        if self.max_staleness < three_steps {
            return Err(ClientError::InvalidTuning(format!(
                "max staleness {}s must be at least three metric steps ({}s), or `observe` \
                 returns NoData on a successful poll",
                self.max_staleness.num_seconds(),
                three_steps.num_seconds()
            )));
        }
        if self.max_staleness > self.liveness.down_threshold() {
            return Err(ClientError::InvalidTuning(format!(
                "max staleness {}s must not outlive the liveness down threshold ({}s)",
                self.max_staleness.num_seconds(),
                self.liveness.down_threshold().num_seconds()
            )));
        }
        let chart_floor = summed(self.max_staleness, self.metric_step);
        if self.chart_window < chart_floor {
            return Err(ClientError::InvalidTuning(format!(
                "chart window {}s must cover max staleness plus a step ({}s)",
                self.chart_window.num_seconds(),
                chart_floor.num_seconds()
            )));
        }
        if self.forget_host_after < self.liveness.down_threshold() {
            return Err(ClientError::InvalidTuning(format!(
                "forget-host-after {}s must not be shorter than the liveness down threshold \
                 ({}s), or a host is forgotten before it can be called down",
                self.forget_host_after.num_seconds(),
                self.liveness.down_threshold().num_seconds()
            )));
        }
        // The freshness ladder runs Fresh -> Degraded -> Unusable. If the tolerance passed the
        // budget the view would go straight from Fresh to Unusable and never show the Degraded
        // banner that explains itself.
        if self.staleness_tolerance() > self.freshness_budget() {
            return Err(ClientError::InvalidTuning(format!(
                "staleness tolerance {}s must not exceed the freshness budget {}s, or the \
                 freshness ladder inverts",
                self.staleness_tolerance().num_seconds(),
                self.freshness_budget().num_seconds()
            )));
        }
        if self.max_retained_hosts == 0 {
            return Err(ClientError::InvalidTuning(
                "max retained hosts must be at least 1".to_owned(),
            ));
        }
        Ok(())
    }
}

impl Default for PollTuning {
    /// The preset over [`LivenessPolicy::default()`]: a 30-second beat, a 150-second staleness
    /// bound, a 90-second tolerance before the banner appears.
    fn default() -> Self {
        Self::from_liveness(LivenessPolicy::default())
    }
}

/// The only shape [`PollTuning`] deserialises through, so a persisted or synced value cannot
/// bypass [`PollTuning::new`].
///
/// Private to the module and field-identical to `PollTuning`, so serialising through it is a
/// no-op on the wire format while deserialising through it is a full revalidation.
#[derive(Serialize, Deserialize)]
struct PollTuningWire {
    liveness: LivenessPolicy,
    poll_interval: Duration,
    metric_step: Duration,
    chart_window: Duration,
    max_staleness: Duration,
    forget_host_after: Duration,
    max_retained_hosts: u32,
}

impl TryFrom<PollTuningWire> for PollTuning {
    type Error = ClientError;

    fn try_from(wire: PollTuningWire) -> Result<Self> {
        Self::new(
            wire.liveness,
            wire.poll_interval,
            wire.metric_step,
            wire.chart_window,
            wire.max_staleness,
            wire.forget_host_after,
            wire.max_retained_hosts,
        )
    }
}

impl From<PollTuning> for PollTuningWire {
    fn from(tuning: PollTuning) -> Self {
        let PollTuning {
            liveness,
            poll_interval,
            metric_step,
            chart_window,
            max_staleness,
            forget_host_after,
            max_retained_hosts,
        } = tuning;
        Self {
            liveness,
            poll_interval,
            metric_step,
            chart_window,
            max_staleness,
            forget_host_after,
            max_retained_hosts,
        }
    }
}

/// Everything the user configures: which environment, how to poll, what to alert on, what to
/// chart, and which host is focused.
///
/// `PartialEq` but not `Eq`: [`pessimal_core::AlertRule`] carries an `f64` threshold.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "FleetConfigWire", into = "FleetConfigWire")]
pub struct FleetConfig {
    pub environment: String,
    pub tuning: PollTuning,
    pub rules: Vec<AlertRule>,
    pub overview_metrics: Vec<MetricKind>,
    pub detail_metrics: Vec<MetricKind>,
    pub focus: Option<HostId>,
}

impl FleetConfig {
    /// # Errors
    /// [`ClientError::InvalidConfig`] if `environment` is empty or contains `::`, since it is
    /// embedded in every rule's URN. Deserialisation goes through the same check.
    pub fn new(environment: impl Into<String>, tuning: PollTuning) -> Result<Self> {
        let environment = environment.into();
        check_environment(&environment)?;
        Ok(Self {
            environment,
            tuning,
            rules: Vec::new(),
            overview_metrics: DEFAULT_OVERVIEW_METRICS.to_vec(),
            detail_metrics: DEFAULT_DETAIL_METRICS.to_vec(),
            focus: None,
        })
    }

    #[must_use]
    pub fn with_rules(mut self, rules: Vec<AlertRule>) -> Self {
        self.rules = rules;
        self
    }

    #[must_use]
    pub fn with_focus(mut self, host: Option<HostId>) -> Self {
        self.focus = host;
        self
    }

    #[must_use]
    pub fn with_overview_metrics(mut self, metrics: Vec<MetricKind>) -> Self {
        self.overview_metrics = metrics;
        self
    }

    #[must_use]
    pub fn with_detail_metrics(mut self, metrics: Vec<MetricKind>) -> Self {
        self.detail_metrics = metrics;
        self
    }

    #[must_use]
    pub fn enabled_rules(&self) -> Vec<&AlertRule> {
        self.rules.iter().filter(|rule| rule.enabled).collect()
    }

    /// Overview metrics UNION every enabled rule's metric, sorted and deduped. This is why a
    /// rule on a metric nobody charts still gets its data: the planner works from this list, so
    /// an alert cannot silently sit on a metric the poll never fetches.
    #[must_use]
    pub fn planned_metrics(&self) -> Vec<MetricKind> {
        let mut metrics = self.overview_metrics.clone();
        metrics.extend(self.enabled_rules().into_iter().map(|rule| rule.metric));
        metrics.sort_unstable();
        metrics.dedup();
        metrics
    }

    /// Legal-but-wrong configurations, computed when the config changes rather than per poll.
    ///
    /// Audits every rule, disabled ones included. All three warnings describe a rule's
    /// *definition* rather than its live behaviour, and the settings screen that shows them shows
    /// disabled rules too — a warning that appears only when a rule is switched on arrives after
    /// the mistake has already been made.
    ///
    /// Order is rules in config order, and within a rule the variant order below, so two runs
    /// over the same config produce the same list.
    #[must_use]
    pub fn audit(&self) -> Vec<TuningWarning> {
        let mut warnings = Vec::new();
        for rule in &self.rules {
            let rule_id = rule.id().to_string();
            if rule.for_duration() < self.tuning.max_staleness() {
                warnings.push(TuningWarning::SpikeCanFire {
                    rule_id: rule_id.clone(),
                    rule_name: rule.name.clone(),
                });
            }
            if matches!(
                rule.metric,
                MetricKind::NetworkIo | MetricKind::AgentCollectionFailures
            ) {
                warnings.push(TuningWarning::CounterThresholdIsRate {
                    rule_id: rule_id.clone(),
                    metric: rule.metric,
                });
            }
            if matches!(&rule.selector, HostSelector::AnyOf(hosts) if hosts.is_empty()) {
                warnings.push(TuningWarning::SelectorMatchesNothing {
                    rule_id,
                    rule_name: rule.name.clone(),
                });
            }
        }
        warnings
    }
}

/// Rejects an environment that cannot be a URN segment.
///
/// Same rule `pessimal_core::Urn` applies, checked here so the failure names the config field
/// rather than surfacing as an opaque URN error the first time a rule is saved.
fn check_environment(environment: &str) -> Result<()> {
    if environment.is_empty() {
        return Err(ClientError::InvalidConfig(
            "environment must not be empty".to_owned(),
        ));
    }
    if environment.contains(URN_SEPARATOR) {
        return Err(ClientError::InvalidConfig(format!(
            "environment {environment:?} must not contain `{URN_SEPARATOR}`, which separates URN \
             segments"
        )));
    }
    Ok(())
}

/// The only shape [`FleetConfig`] deserialises through, so a persisted config cannot arrive with
/// an environment that would produce unparseable rule URNs.
#[derive(Serialize, Deserialize)]
struct FleetConfigWire {
    environment: String,
    tuning: PollTuning,
    rules: Vec<AlertRule>,
    overview_metrics: Vec<MetricKind>,
    detail_metrics: Vec<MetricKind>,
    focus: Option<HostId>,
}

impl TryFrom<FleetConfigWire> for FleetConfig {
    type Error = ClientError;

    fn try_from(wire: FleetConfigWire) -> Result<Self> {
        check_environment(&wire.environment)?;
        Ok(Self {
            environment: wire.environment,
            tuning: wire.tuning,
            rules: wire.rules,
            overview_metrics: wire.overview_metrics,
            detail_metrics: wire.detail_metrics,
            focus: wire.focus,
        })
    }
}

impl From<FleetConfig> for FleetConfigWire {
    fn from(config: FleetConfig) -> Self {
        let FleetConfig {
            environment,
            tuning,
            rules,
            overview_metrics,
            detail_metrics,
            focus,
        } = config;
        Self {
            environment,
            tuning,
            rules,
            overview_metrics,
            detail_metrics,
            focus,
        }
    }
}

/// A configuration that is legal and almost certainly not what the user meant.
///
/// Warnings, not errors: every one of these describes a rule that will do *something*, just not
/// the something its author expected. Refusing the config would be worse than explaining it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TuningWarning {
    /// `rule.for_duration() < tuning.max_staleness()`: because `observe` sets `since = point.at`
    /// on a fresh breach, one sample that is old-but-still-valid carries the rule straight to
    /// `Firing` with no further evidence.
    SpikeCanFire { rule_id: String, rule_name: String },
    /// A rule on `NetworkIo` / `AgentCollectionFailures`: the threshold is compared against the
    /// normalised rate or delta, not the cumulative total the backend returns.
    CounterThresholdIsRate { rule_id: String, metric: MetricKind },
    /// `HostSelector::AnyOf(vec![])` — a rule that can never match a host.
    SelectorMatchesNothing { rule_id: String, rule_name: String },
}

impl TuningWarning {
    /// A sentence a settings screen can show verbatim. Not localised — the UI layer owns
    /// presentation, this only owns the explanation.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::SpikeCanFire { rule_name, .. } => format!(
                "Rule {rule_name:?} can fire on a single sample: its dwell is shorter than the \
                 staleness bound, so one old-but-still-valid point carries it straight to firing."
            ),
            Self::CounterThresholdIsRate { metric, .. } => format!(
                "{} is a counter. Its threshold is compared against the normalised rate or \
                 per-bucket delta, not the cumulative total the backend returns.",
                metric.display_name()
            ),
            Self::SelectorMatchesNothing { rule_name, .. } => format!(
                "Rule {rule_name:?} selects an empty set of hosts and can never match anything."
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use pessimal_core::Comparator;
    use serde_json::Value;

    use super::*;

    /// `LivenessPolicy::default()`: a 30-second beat, stale at 90s, down at 150s. Every literal
    /// below is arithmetic over those three numbers.
    fn liveness() -> LivenessPolicy {
        LivenessPolicy::default()
    }

    fn hours(count: i64) -> Duration {
        Duration::hours(count)
    }

    fn secs(count: i64) -> Duration {
        Duration::seconds(count)
    }

    /// A rule whose dwell comfortably exceeds the default `max_staleness`, so it contributes no
    /// `SpikeCanFire` noise to an audit that is testing something else.
    fn quiet_rule(name: &str, metric: MetricKind) -> AlertRule {
        AlertRule::new(
            "prod",
            name,
            metric,
            Comparator::GreaterThan,
            0.9,
            secs(300),
        )
        .expect("valid rule")
    }

    fn config() -> FleetConfig {
        FleetConfig::new("prod", PollTuning::default()).expect("valid environment")
    }

    #[test]
    fn the_default_preset_satisfies_every_interlock() {
        let preset = PollTuning::default();
        let revalidated = PollTuning::new(
            preset.liveness(),
            preset.poll_interval(),
            preset.metric_step(),
            preset.chart_window(),
            preset.max_staleness(),
            preset.forget_host_after(),
            preset.max_retained_hosts(),
        )
        .expect("the preset must pass the constructor that guards it");

        assert_eq!(revalidated, preset);
    }

    #[test]
    fn default_detail_metrics_is_every_metric_but_the_heartbeat() {
        let expected: Vec<MetricKind> = MetricKind::ALL
            .into_iter()
            .filter(|kind| *kind != MetricKind::AgentHeartbeat)
            .collect();

        assert_eq!(DEFAULT_DETAIL_METRICS.to_vec(), expected);
    }

    #[test]
    fn tuning_rejects_a_max_staleness_below_three_steps() {
        // Three 30s steps is 90s; 60s would make `observe` return NoData on a successful poll.
        let rejected = PollTuning::new(
            liveness(),
            secs(30),
            secs(30),
            hours(1),
            secs(60),
            hours(24),
            256,
        );

        assert!(matches!(rejected, Err(ClientError::InvalidTuning(_))));
    }

    #[test]
    fn tuning_rejects_a_max_staleness_above_the_down_threshold() {
        // 200s clears three steps but outlives the 150s at which liveness calls a host down.
        let rejected = PollTuning::new(
            liveness(),
            secs(30),
            secs(30),
            hours(1),
            secs(200),
            hours(24),
            256,
        );

        assert!(matches!(rejected, Err(ClientError::InvalidTuning(_))));
    }

    #[test]
    fn tuning_rejects_a_chart_window_shorter_than_staleness_plus_a_step() {
        // 150s of staleness plus a 30s step needs 180s of window; 150s is one bucket short.
        let rejected = PollTuning::new(
            liveness(),
            secs(30),
            secs(30),
            secs(150),
            secs(150),
            hours(24),
            256,
        );

        assert!(matches!(rejected, Err(ClientError::InvalidTuning(_))));
    }

    #[test]
    fn tuning_rejects_a_liveness_policy_with_stale_at_or_past_down() {
        // The direct path is unreachable: core's own constructor refuses the inversion, and its
        // `Deserialize` now routes through a validating wire type. That is why this case is
        // exercised through JSON — the only door such a policy could ever come through.
        assert!(LivenessPolicy::new(secs(30), 5, 5).is_err());

        let mut wire = serde_json::to_value(PollTuning::default()).expect("tuning serialises");
        wire["liveness"]["stale_after_intervals"] = Value::from(5_u32);

        let rejected: std::result::Result<PollTuning, _> = serde_json::from_value(wire);

        assert!(rejected.is_err());
    }

    #[test]
    fn tuning_rejects_a_liveness_policy_with_fewer_than_three_stale_intervals() {
        // Core accepts stale-at-2; this crate does not. A heartbeat's timestamp is quantised to
        // its bucket, so at two intervals a healthy host lands exactly on the stale threshold.
        let two_intervals = LivenessPolicy::new(secs(30), 2, 5).expect("core accepts this policy");

        let rejected = PollTuning::new(
            two_intervals,
            secs(30),
            secs(30),
            hours(1),
            secs(150),
            hours(24),
            256,
        );

        assert!(matches!(rejected, Err(ClientError::InvalidTuning(_))));
    }

    #[test]
    fn tuning_rejects_a_staleness_tolerance_above_the_freshness_budget() {
        // 2 * 120s + 30s = 270s of tolerance against a 150s budget: the view would jump from
        // Fresh straight to Unusable and never show the Degraded banner.
        let rejected = PollTuning::new(
            liveness(),
            secs(120),
            secs(30),
            hours(1),
            secs(150),
            hours(24),
            256,
        );

        assert!(matches!(rejected, Err(ClientError::InvalidTuning(_))));
    }

    #[test]
    fn deserializing_an_invalid_tuning_is_rejected() {
        // Round-tripping through `serde_json::Value` rather than hand-written JSON keeps this
        // free of assumptions about how chrono encodes a Duration, and exercises the `into` half
        // of the wire pair: a field-name drift between the two would fail here.
        let mut wire = serde_json::to_value(PollTuning::default()).expect("tuning serialises");
        wire["max_staleness"] = serde_json::to_value(secs(60)).expect("duration serialises");

        let rejected: std::result::Result<PollTuning, _> = serde_json::from_value(wire);

        let error = rejected.expect_err("serde must not bypass the constructor");
        assert!(
            error.to_string().contains("invalid poll tuning"),
            "expected our own interlock to fire, got: {error}"
        );
    }

    #[test]
    fn deserializing_a_config_with_a_colon_colon_environment_is_rejected() {
        let mut wire = serde_json::to_value(config()).expect("config serialises");
        wire["environment"] = Value::from("prod::eu");

        let rejected: std::result::Result<FleetConfig, _> = serde_json::from_value(wire);

        let error = rejected.expect_err("serde must not bypass the constructor");
        assert!(
            error.to_string().contains("invalid client configuration"),
            "expected our own environment check to fire, got: {error}"
        );
    }

    #[test]
    fn derived_windows_are_not_settable_and_match_their_formulas() {
        // There is no setter for any of these: they are functions of the constructor's inputs,
        // which is what stops an overview window from disagreeing with the step it is fetched at.
        let tuning = PollTuning::default();

        assert_eq!(tuning.host_window(), secs(210)); // down 150 + 2 * 30
        assert_eq!(tuning.overview_window(), secs(210)); // staleness 150 + 2 * 30
        assert_eq!(tuning.staleness_tolerance(), secs(90)); // 2 * 30 + 30
        assert_eq!(tuning.freshness_budget(), secs(150)); // the down threshold itself
        assert_eq!(
            tuning.overview_window().num_seconds() / tuning.metric_step().num_seconds(),
            7,
            "the fleet-wide window is about seven buckets"
        );

        // Move one input and only the windows derived from it move.
        let finer = PollTuning::new(
            liveness(),
            secs(30),
            secs(10),
            hours(1),
            secs(150),
            hours(24),
            256,
        )
        .expect("valid tuning");

        assert_eq!(finer.overview_window(), secs(170)); // staleness 150 + 2 * 10
        assert_eq!(finer.staleness_tolerance(), secs(70)); // 2 * 30 + 10
        assert_eq!(finer.host_window(), secs(210)); // liveness-derived, unchanged
    }

    #[test]
    fn backoff_doubles_then_caps_at_five_minutes() {
        let tuning = PollTuning::default();

        assert_eq!(tuning.backoff_after(0), secs(30));
        assert_eq!(tuning.backoff_after(1), secs(60));
        assert_eq!(tuning.backoff_after(2), secs(120));
        assert_eq!(tuning.backoff_after(3), secs(240));
        assert_eq!(tuning.backoff_after(4), secs(300)); // 480s, clamped
        assert_eq!(tuning.backoff_after(5), secs(300));
        assert_eq!(tuning.backoff_after(u32::MAX), secs(300));
    }

    #[test]
    fn planned_metrics_includes_a_rule_metric_nobody_charts() {
        let uncharted = MetricKind::SystemUptime;
        assert!(!DEFAULT_OVERVIEW_METRICS.contains(&uncharted));

        let config = config().with_rules(vec![quiet_rule("uptime", uncharted)]);
        let planned = config.planned_metrics();

        assert!(planned.contains(&uncharted));
        assert_eq!(planned.len(), DEFAULT_OVERVIEW_METRICS.len() + 1);

        let mut sorted = planned.clone();
        sorted.sort_unstable();
        assert_eq!(
            planned, sorted,
            "planned metrics must be sorted and deduped"
        );
    }

    #[test]
    fn spike_can_fire_warns_when_dwell_is_below_max_staleness() {
        // 60s of dwell under a 150s staleness bound: one old-but-valid sample fires it outright.
        let spiky = AlertRule::new(
            "prod",
            "cpu spike",
            MetricKind::CpuUtilization,
            Comparator::GreaterThan,
            0.9,
            secs(60),
        )
        .expect("valid rule");
        let rule_id = spiky.id().to_string();

        let warnings = config().with_rules(vec![spiky]).audit();

        assert_eq!(
            warnings,
            vec![TuningWarning::SpikeCanFire {
                rule_id,
                rule_name: "cpu spike".to_owned(),
            }]
        );

        // The same rule with a dwell past the staleness bound is quiet.
        let patient = quiet_rule("cpu sustained", MetricKind::CpuUtilization);
        assert!(config().with_rules(vec![patient]).audit().is_empty());
    }

    #[test]
    fn counter_threshold_is_rate_warns_for_network_io() {
        let counter = quiet_rule("chatty", MetricKind::NetworkIo);
        let rule_id = counter.id().to_string();

        let warnings = config().with_rules(vec![counter]).audit();

        assert_eq!(
            warnings,
            vec![TuningWarning::CounterThresholdIsRate {
                rule_id,
                metric: MetricKind::NetworkIo,
            }]
        );
    }

    #[test]
    fn selector_matches_nothing_warns_for_an_empty_any_of() {
        let orphan = quiet_rule("nobody", MetricKind::CpuUtilization)
            .with_selector(HostSelector::AnyOf(vec![]));
        let rule_id = orphan.id().to_string();

        let warnings = config().with_rules(vec![orphan]).audit();

        assert_eq!(
            warnings,
            vec![TuningWarning::SelectorMatchesNothing {
                rule_id,
                rule_name: "nobody".to_owned(),
            }]
        );
    }

    #[test]
    fn a_disabled_rule_is_still_audited_but_never_planned() {
        // Disabled rules are not fetched for, but their definitions are still shown and edited,
        // so a warning that waits for the switch arrives after the mistake.
        let disabled = quiet_rule("chatty", MetricKind::NetworkIo).disabled();
        let config = config().with_rules(vec![disabled]);

        assert_eq!(config.audit().len(), 1);
        assert!(config.enabled_rules().is_empty());
        assert!(!config.planned_metrics().contains(&MetricKind::NetworkIo));
    }
}
