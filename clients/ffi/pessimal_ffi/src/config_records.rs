//! The settings boundary: everything the user configures, in shapes UniFFI can carry.
//!
//! Mirrors `pessimal_client_core::config` — [`PollTuning`], [`FleetConfig`], [`TuningWarning`] —
//! and the four `pessimal_core` types a rule is made of: [`AlertRule`], [`Comparator`],
//! [`HostSelector`] and [`MetricKind`]. Rules are authored from the settings screen, so all four
//! cross in both directions rather than outward only.
//!
//! Nothing here decides anything. Every constructor delegates to the client core and hands back
//! its message; every invariant this module appears to enforce is one of core's, surfaced. A
//! threshold, an interlock or a dwell bound restated here would be a second copy that drifts the
//! first time core's changes, and the app would then refuse configurations core accepts (or accept
//! ones it rejects) with no test anywhere noticing.
//!
//! Four shape decisions, each with a reason that is not obvious from the types:
//!
//! - **A rule set crosses as its JSON array**, in [`FleetConfigRecord::rules_json`], per §4.2 of
//!   the M3 design. [`AlertRule`]'s `id` and `for_duration` are private with no setters and no
//!   `with_id`, so `Deserialize` is the *only* construction path that preserves an existing id —
//!   and the id is what every stored [`pessimal_core::AlertEvaluation`] is keyed by. Rebuilding a
//!   rule with `AlertRule::new` mints a fresh `UUIDv7`, orphans the evaluation, and presents to a
//!   user as "my alerts reset every launch". JSON is therefore not a convenience here; it is the
//!   id-preserving path, and it routes through `AlertRule`'s own validating wire type, which is the
//!   same gate a rule loaded from disk passes.
//! - **[`AlertRuleRecord`] exists alongside that JSON** so the rule list and rule editor are plain
//!   Swift structs rather than a JSON parser in the UI layer. [`alert_rules_from_json`] and
//!   [`alert_rules_to_json`] are the only two places the two representations meet.
//! - **The liveness policy nests rather than flattens.** [`PollTuning`] holds a
//!   [`LivenessPolicy`], itself a validated value with private fields, so flattening its three
//!   numbers into the tuning record would hide which ones `LivenessPolicy::new` guards and which
//!   ones `PollTuning::new` guards.
//! - **Derived windows are absent from [`PollTuningRecord`].** `host_window`, `overview_window`,
//!   `staleness_tolerance` and `freshness_budget` are functions of the supplied fields precisely so
//!   they cannot be set wrong; carrying them as record fields would invite Swift to set them and
//!   this crate to ignore it. The two the UI needs arrive on the view's freshness inputs instead.
//!
//! Record and enum names share one flat Swift namespace across the whole crate, so everything here
//! carries the `Record` suffix the design uses — including the mirrored *enums*, which the design's
//! own `TuningWarningRecord` establishes.

use chrono::Duration;
use pessimal_client_core::{FleetConfig, PollTuning, TuningWarning, draft_rule, validate_rule};
use pessimal_core::{AlertRule, Comparator, CoreError, HostSelector, LivenessPolicy, MetricKind};

use crate::convert::{
    FfiError, duration_to_seconds, host_id_from_string, host_id_to_string, seconds_to_duration,
    urn_from_string, urn_to_string,
};

/// Every metric the product models, mirrored so the rule editor can offer a picker and the chart
/// list can name what it is drawing.
///
/// Variant-for-variant with [`MetricKind`], in the domain's order, which is display order. Both
/// `From` impls below match exhaustively with no wildcard: a thirteenth metric in core stops this
/// file compiling, which is the only way the Swift `switch` statements over this enum get updated
/// in the same commit rather than at the next iOS build.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, uniffi::Enum)]
pub enum MetricKindRecord {
    CpuUtilization,
    MemoryUtilization,
    MemoryUsage,
    FilesystemUtilization,
    FilesystemUsage,
    NetworkIo,
    LoadAverage1m,
    LoadAverage5m,
    LoadAverage15m,
    SystemUptime,
    /// Pessimal's own liveness beat. Not alertable — a silent host is liveness's business — which
    /// is why [`draft_alert_rule`] refuses it.
    AgentHeartbeat,
    /// Cumulative host-sampling failures: an agent beating happily while collecting nothing.
    AgentCollectionFailures,
}

impl From<MetricKind> for MetricKindRecord {
    fn from(kind: MetricKind) -> Self {
        match kind {
            MetricKind::CpuUtilization => Self::CpuUtilization,
            MetricKind::MemoryUtilization => Self::MemoryUtilization,
            MetricKind::MemoryUsage => Self::MemoryUsage,
            MetricKind::FilesystemUtilization => Self::FilesystemUtilization,
            MetricKind::FilesystemUsage => Self::FilesystemUsage,
            MetricKind::NetworkIo => Self::NetworkIo,
            MetricKind::LoadAverage1m => Self::LoadAverage1m,
            MetricKind::LoadAverage5m => Self::LoadAverage5m,
            MetricKind::LoadAverage15m => Self::LoadAverage15m,
            MetricKind::SystemUptime => Self::SystemUptime,
            MetricKind::AgentHeartbeat => Self::AgentHeartbeat,
            MetricKind::AgentCollectionFailures => Self::AgentCollectionFailures,
        }
    }
}

impl From<MetricKindRecord> for MetricKind {
    fn from(record: MetricKindRecord) -> Self {
        match record {
            MetricKindRecord::CpuUtilization => Self::CpuUtilization,
            MetricKindRecord::MemoryUtilization => Self::MemoryUtilization,
            MetricKindRecord::MemoryUsage => Self::MemoryUsage,
            MetricKindRecord::FilesystemUtilization => Self::FilesystemUtilization,
            MetricKindRecord::FilesystemUsage => Self::FilesystemUsage,
            MetricKindRecord::NetworkIo => Self::NetworkIo,
            MetricKindRecord::LoadAverage1m => Self::LoadAverage1m,
            MetricKindRecord::LoadAverage5m => Self::LoadAverage5m,
            MetricKindRecord::LoadAverage15m => Self::LoadAverage15m,
            MetricKindRecord::SystemUptime => Self::SystemUptime,
            MetricKindRecord::AgentHeartbeat => Self::AgentHeartbeat,
            MetricKindRecord::AgentCollectionFailures => Self::AgentCollectionFailures,
        }
    }
}

/// How a sample is compared against a rule's threshold.
///
/// The symbol a UI shows (`>`, `>=`, …) is [`Comparator::symbol`]'s, not a second table here: a
/// mirrored enum may carry the variants, never the meanings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, uniffi::Enum)]
pub enum ComparatorRecord {
    GreaterThan,
    GreaterThanOrEqual,
    LessThan,
    LessThanOrEqual,
}

impl From<Comparator> for ComparatorRecord {
    fn from(comparator: Comparator) -> Self {
        match comparator {
            Comparator::GreaterThan => Self::GreaterThan,
            Comparator::GreaterThanOrEqual => Self::GreaterThanOrEqual,
            Comparator::LessThan => Self::LessThan,
            Comparator::LessThanOrEqual => Self::LessThanOrEqual,
        }
    }
}

impl From<ComparatorRecord> for Comparator {
    fn from(record: ComparatorRecord) -> Self {
        match record {
            ComparatorRecord::GreaterThan => Self::GreaterThan,
            ComparatorRecord::GreaterThanOrEqual => Self::GreaterThanOrEqual,
            ComparatorRecord::LessThan => Self::LessThan,
            ComparatorRecord::LessThanOrEqual => Self::LessThanOrEqual,
        }
    }
}

/// Which hosts a rule applies to.
///
/// Struct-shaped variants rather than tuple-shaped, so each payload reaches Swift under a name:
/// an unlabelled associated value is the kind of thing a UI layer then re-labels for itself.
/// Host ids are `String` because [`pessimal_core::HostId`] is a newtype around the hostname the
/// backend groups by, and newtypes do not cross.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum HostSelectorRecord {
    /// Every host the backend reports.
    All,
    /// Exactly one host.
    Host { host_id: String },
    /// Any of a set.
    ///
    /// An empty set matches nothing. That is *not* rejected on the way through this module:
    /// `FleetConfig::audit` reports it as a [`TuningWarningRecord::SelectorMatchesNothing`]
    /// warning, and a config already on disk with such a rule must survive a round trip through
    /// the settings screen rather than becoming unloadable. [`draft_alert_rule`] and
    /// [`validate_alert_rule`] are where core refuses it, because that is where a *new* mistake is
    /// being made.
    AnyOf { host_ids: Vec<String> },
}

impl From<HostSelector> for HostSelectorRecord {
    fn from(selector: HostSelector) -> Self {
        match selector {
            HostSelector::All => Self::All,
            HostSelector::Host(id) => Self::Host {
                host_id: host_id_to_string(&id),
            },
            HostSelector::AnyOf(ids) => Self::AnyOf {
                host_ids: ids.iter().map(host_id_to_string).collect(),
            },
        }
    }
}

impl From<HostSelectorRecord> for HostSelector {
    fn from(record: HostSelectorRecord) -> Self {
        match record {
            HostSelectorRecord::All => Self::All,
            HostSelectorRecord::Host { host_id } => Self::Host(host_id_from_string(host_id)),
            HostSelectorRecord::AnyOf { host_ids } => {
                Self::AnyOf(host_ids.into_iter().map(host_id_from_string).collect())
            }
        }
    }
}

/// One configured alert rule, as the rule list and the rule editor see it.
///
/// Flat because [`AlertRule`]'s `id` and `for_duration` are private, so there is nothing to
/// destructure and a record of getters is the closest mirror available. The price is that a new
/// field in core is not a compile error here the way it is for [`FleetConfigRecord`]; the
/// round-trip test over a drafted rule is what catches it instead, because the JSON path in
/// [`TryFrom<AlertRuleRecord>`](AlertRule) then fails on a missing field.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct AlertRuleRecord {
    /// The rule's URN, `pessimal::<environment>::alerts::rule::<uuid>`, as its canonical string.
    ///
    /// Carried and never regenerated. Every stored evaluation is keyed by this value, so a rule
    /// that comes back with a fresh id has silently forfeited its dwell and its alert history.
    pub id: String,
    pub name: String,
    pub metric: MetricKindRecord,
    pub selector: HostSelectorRecord,
    pub comparator: ComparatorRecord,
    pub threshold: f64,
    /// How long the breach must hold before the rule fires; zero fires on the first breaching
    /// sample. Seconds, like every duration at this boundary.
    pub for_duration_seconds: i64,
    pub enabled: bool,
}

impl From<&AlertRule> for AlertRuleRecord {
    fn from(rule: &AlertRule) -> Self {
        Self {
            id: urn_to_string(rule.id()),
            name: rule.name.clone(),
            metric: rule.metric.into(),
            selector: rule.selector.clone().into(),
            comparator: rule.comparator.into(),
            threshold: rule.threshold,
            for_duration_seconds: duration_to_seconds(rule.for_duration()),
            enabled: rule.enabled,
        }
    }
}

impl TryFrom<AlertRuleRecord> for AlertRule {
    type Error = FfiError;

    /// Rebuilds the domain rule *with its id intact*, which only `Deserialize` can do.
    ///
    /// See the module docs: `id` and `for_duration` are private, there is no `with_id`, and
    /// `AlertRule::new` mints a fresh `UUIDv7`. Assembling the field map core's own wire type
    /// expects and handing it to `serde_json::from_value` therefore is not a workaround for
    /// missing setters — it is the single construction path that keeps a rule the same rule, and
    /// it runs exactly the validation a rule loaded from disk runs, written once in core.
    ///
    /// Deliberately *not* followed by `validate_rule`. That adds the two checks which make a rule
    /// pointless rather than malformed — a non-alertable metric, an empty `AnyOf` — and those are
    /// warnings on an existing config, not grounds for refusing to load it. [`validate_alert_rule`]
    /// is where a settings screen asks for them.
    ///
    /// # Errors
    /// [`FfiError::InvalidUrn`] if `id` is not a URN; [`FfiError::Internal`] if a domain value
    /// cannot be rendered as JSON (a bridge fault — none of these types can fail to serialise);
    /// [`FfiError::InvalidRule`] when core's wire type refuses the rule, carrying its message.
    /// A non-finite threshold arrives as that last case: JSON has no `NaN`, so it becomes `null`
    /// and the wire type rejects it — the same classification core gives a non-finite threshold,
    /// by a different route.
    fn try_from(record: AlertRuleRecord) -> Result<Self, Self::Error> {
        let AlertRuleRecord {
            id,
            name,
            metric,
            selector,
            comparator,
            threshold,
            for_duration_seconds,
            enabled,
        } = record;

        // Parsed rather than passed through: `Urn`'s own `FromStr` decides what a URN is, and the
        // five-segment string is what `Serialize` round-trips.
        let id = urn_from_string(&id)?;
        let for_duration = seconds_to_duration(for_duration_seconds)?;

        // Field-for-field with core's `AlertRuleWire`. The names are string literals because
        // serde's are; if core renames one, `from_value` below fails loudly on a missing field and
        // the round-trip test in this module is what reports it.
        let fields = [
            ("id", serde_json::to_value(&id)),
            ("name", serde_json::to_value(&name)),
            ("metric", serde_json::to_value(MetricKind::from(metric))),
            (
                "selector",
                serde_json::to_value(HostSelector::from(selector)),
            ),
            (
                "comparator",
                serde_json::to_value(Comparator::from(comparator)),
            ),
            ("threshold", serde_json::to_value(threshold)),
            ("for_duration", serde_json::to_value(for_duration)),
            ("enabled", serde_json::to_value(enabled)),
        ];

        let mut wire = serde_json::Map::new();
        for (key, value) in fields {
            let value = value.map_err(|error| FfiError::Internal {
                message: format!("alert rule field {key} could not be rendered as JSON: {error}"),
            })?;
            wire.insert(key.to_owned(), value);
        }

        serde_json::from_value(serde_json::Value::Object(wire)).map_err(|error| {
            FfiError::InvalidRule {
                message: error.to_string(),
            }
        })
    }
}

/// The rule for turning heartbeat age into a liveness verdict.
///
/// Nested inside [`PollTuningRecord`] rather than flattened into it, because this is a validated
/// value in its own right: `LivenessPolicy::new` owns the interval and the ordering of the two
/// multipliers, while `PollTuning::new` owns the relationships between the policy's *derived*
/// thresholds and every other window. Flattening would blur which constructor rejects what.
///
/// The derived thresholds (`stale_threshold`, `down_threshold`) are not fields here for the same
/// reason the tuning's derived windows are not: they are products, and a product that can be set
/// independently of its factors is a product that will eventually disagree with them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct LivenessPolicyRecord {
    /// How often the agent claims to beat.
    ///
    /// Whole seconds, like every duration at this boundary. A sub-second interval — legal in
    /// [`LivenessPolicy`], and meaningless for a host-telemetry product nobody renders faster than
    /// a second — truncates to `0` here and is then refused by `LivenessPolicy::new` on the way
    /// back. Lossy and loud, in preference to lossy and silent. A *fractional* interval above a
    /// second (1500 ms, say) is the one case that truncates quietly, to 1s: the same truncation
    /// `duration_to_seconds` applies to every other duration that crosses, and core has no
    /// sub-second semantics for anything downstream of it to lose.
    pub heartbeat_interval_seconds: i64,
    /// Intervals of silence before a host is `Stale`. At least three, enforced by
    /// `PollTuning::new`: a heartbeat's timestamp is quantised to its query bucket, so a healthy
    /// host's measured age is already up to one interval older than its true age.
    pub stale_after_intervals: u32,
    /// Intervals of silence before a host is `Down`. Must exceed `stale_after_intervals`.
    pub down_after_intervals: u32,
}

impl From<LivenessPolicy> for LivenessPolicyRecord {
    fn from(policy: LivenessPolicy) -> Self {
        Self {
            heartbeat_interval_seconds: duration_to_seconds(policy.heartbeat_interval()),
            stale_after_intervals: intervals_in(
                policy.stale_threshold(),
                policy.heartbeat_interval(),
            ),
            down_after_intervals: intervals_in(
                policy.down_threshold(),
                policy.heartbeat_interval(),
            ),
        }
    }
}

impl TryFrom<LivenessPolicyRecord> for LivenessPolicy {
    type Error = FfiError;

    /// # Errors
    /// [`FfiError::Internal`] if the interval is not a representable duration;
    /// [`FfiError::InvalidTuning`], carrying core's message, for a non-positive interval, a zero
    /// multiplier, or multipliers that would make a host down before it is stale.
    fn try_from(record: LivenessPolicyRecord) -> Result<Self, Self::Error> {
        let LivenessPolicyRecord {
            heartbeat_interval_seconds,
            stale_after_intervals,
            down_after_intervals,
        } = record;

        Self::new(
            seconds_to_duration(heartbeat_interval_seconds)?,
            stale_after_intervals,
            down_after_intervals,
        )
        .map_err(as_invalid_tuning)
    }
}

/// Recovers a multiplier from the threshold it produced.
///
/// [`LivenessPolicy`] exposes `heartbeat_interval()`, `stale_threshold()` and `down_threshold()` —
/// the interval and the two *products* — but not the multipliers, and `LivenessPolicy::new` takes
/// the multipliers. Division is therefore the only route from the value back to the three numbers
/// that built it. It is exact for every policy core accepts: each threshold is
/// `interval * multiplier` computed with `checked_mul`, so the remainder is zero and the quotient
/// is the original `u32`.
///
/// Zero — which `LivenessPolicy::new` refuses — whenever the arithmetic cannot be trusted: a
/// non-positive interval (impossible through core's constructors, so a sentinel rather than a
/// division by zero), a product that saturated at `Duration::MAX` and no longer divides evenly, or
/// a quotient past `u32`. A policy that lands there is one core would itself reject, so the failure
/// surfaces as core's own validation error the next time the record is converted back, rather than
/// as a panic here or a plausible-looking wrong number in the settings screen.
fn intervals_in(threshold: Duration, interval: Duration) -> u32 {
    let interval_ms = interval.num_milliseconds();
    if interval_ms <= 0 {
        return 0;
    }
    let threshold_ms = threshold.num_milliseconds();
    if threshold_ms % interval_ms != 0 {
        return 0;
    }
    u32::try_from(threshold_ms / interval_ms).unwrap_or(0)
}

/// Reclassifies a [`CoreError`] raised while building a poll tuning.
///
/// `LivenessPolicy::new` reports a bad interval or inverted multipliers as
/// `CoreError::InvalidRule`, whose rendered form opens "invalid alert rule" — the wrong words, and
/// the wrong Swift banner, for a field on the polling screen. `PollTuning::new` reports the
/// identical defect as `ClientError::InvalidTuning`, so without this the same mistake routes two
/// ways depending on which constructor happened to catch it.
///
/// Choosing the classification is not restating the check: the message stays core's, verbatim.
/// Anything other than `InvalidRule` is delegated to the crate's one conversion rather than
/// relabelled, because only the rule variant is reachable from here and a future addition should
/// keep its own meaning.
fn as_invalid_tuning(error: CoreError) -> FfiError {
    match error {
        CoreError::InvalidRule(message) => FfiError::InvalidTuning { message },
        other => FfiError::from(other),
    }
}

/// Every duration the client core measures a poll, a window, or a dwell against.
///
/// [`PollTuning`]'s fields are private and its windows are functions, so this is a flat mirror of
/// its eight inputs plus a throwing constructor, [`poll_tuning_validated`]. A record built in Swift
/// is just eight numbers until that function has accepted it — it is not a `PollTuning` and this
/// crate does not treat it as one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct PollTuningRecord {
    pub liveness: LivenessPolicyRecord,
    pub poll_interval_seconds: i64,
    /// The query bucket width. At least one second: the SigNoz adapter truncates with
    /// `num_seconds().max(1)`, so a finer step would stop describing the query it produces.
    pub metric_step_seconds: i64,
    pub chart_window_seconds: i64,
    /// How old a sample may be and still be evidence for an alert — before
    /// `backend_lag_allowance_seconds` is added to it.
    ///
    /// The two are summed into the bound alerts are actually gated at (core's
    /// `PollTuning::evidence_horizon`), because a sample's measured age includes the backend's
    /// ingestion delay whether or not the agent is healthy. This half of the sum is the part that
    /// is about *us*: bucket quantisation and poll latency. Raise the lag allowance for a slow
    /// backend, not this.
    pub max_staleness_seconds: i64,
    /// How far behind wall clock the backend's newest queryable point lags, in seconds.
    ///
    /// The one field on this screen an operator may genuinely need to raise: it describes their
    /// backend's ingestion delay, not their taste. Too low and every host reads stale at once, or
    /// the roster comes back empty. Non-negative; zero only for a collector on loopback. See
    /// [`PollTuning::backend_lag_allowance`].
    pub backend_lag_allowance_seconds: i64,
    pub forget_host_after_seconds: i64,
    pub max_retained_hosts: u32,
}

impl From<PollTuning> for PollTuningRecord {
    /// Built from the getters, because the fields are private. A new field in [`PollTuning`] is
    /// still a compile error here — it changes the arity of `PollTuning::new`, which
    /// [`TryFrom<PollTuningRecord>`](PollTuning) calls — so the pair of impls keeps the property
    /// exhaustive destructuring gives elsewhere.
    fn from(tuning: PollTuning) -> Self {
        Self {
            liveness: tuning.liveness().into(),
            poll_interval_seconds: duration_to_seconds(tuning.poll_interval()),
            metric_step_seconds: duration_to_seconds(tuning.metric_step()),
            chart_window_seconds: duration_to_seconds(tuning.chart_window()),
            max_staleness_seconds: duration_to_seconds(tuning.max_staleness()),
            backend_lag_allowance_seconds: duration_to_seconds(tuning.backend_lag_allowance()),
            forget_host_after_seconds: duration_to_seconds(tuning.forget_host_after()),
            max_retained_hosts: tuning.max_retained_hosts(),
        }
    }
}

impl TryFrom<PollTuningRecord> for PollTuning {
    type Error = FfiError;

    /// # Errors
    /// [`FfiError::Internal`] if a field is not a representable duration;
    /// [`FfiError::InvalidTuning`] when any of core's interlocks fails, carrying core's own
    /// message — which names the field and the bound it missed, so the settings screen can show it
    /// verbatim.
    fn try_from(record: PollTuningRecord) -> Result<Self, Self::Error> {
        let PollTuningRecord {
            liveness,
            poll_interval_seconds,
            metric_step_seconds,
            chart_window_seconds,
            max_staleness_seconds,
            backend_lag_allowance_seconds,
            forget_host_after_seconds,
            max_retained_hosts,
        } = record;

        Self::new(
            LivenessPolicy::try_from(liveness)?,
            seconds_to_duration(poll_interval_seconds)?,
            seconds_to_duration(metric_step_seconds)?,
            seconds_to_duration(chart_window_seconds)?,
            seconds_to_duration(max_staleness_seconds)?,
            seconds_to_duration(backend_lag_allowance_seconds)?,
            seconds_to_duration(forget_host_after_seconds)?,
            max_retained_hosts,
        )
        .map_err(FfiError::from)
    }
}

/// Everything the user configures: which environment, how to poll, what to alert on, what to
/// chart, and which host is focused.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FleetConfigRecord {
    /// The URN segment every rule id embeds. Must be non-empty and free of `::`.
    pub environment: String,
    pub tuning: PollTuningRecord,
    /// The rule set as a JSON array — the id-preserving carriage explained in the module docs.
    ///
    /// Opaque to Swift: obtain it from [`alert_rules_to_json`] or from a config this crate
    /// produced, never by assembling JSON in the app. `"[]"` is the empty rule set, and
    /// [`fleet_config_defaults`] is the way to get one without knowing that.
    pub rules_json: String,
    /// Charted for every host, and the base of the planned metric set.
    pub overview_metrics: Vec<MetricKindRecord>,
    /// Charted for the focused host.
    pub detail_metrics: Vec<MetricKindRecord>,
    /// The focused host's id, if any. `None` is the fleet overview, and is not the same as a host
    /// whose id happens to be empty.
    pub focus: Option<String>,
}

impl TryFrom<FleetConfig> for FleetConfigRecord {
    type Error = FfiError;

    /// # Errors
    /// [`FfiError::Internal`] if the rule set cannot be rendered as JSON. A bridge fault rather
    /// than a user-visible one: every rule in a `FleetConfig` arrived through a constructor that
    /// rejects the one value JSON cannot hold (a non-finite threshold).
    fn try_from(config: FleetConfig) -> Result<Self, Self::Error> {
        // Exhaustively destructured, no `..`: a new field in `FleetConfig` must be a compile error
        // here rather than a value that silently stops crossing.
        let FleetConfig {
            environment,
            tuning,
            rules,
            overview_metrics,
            detail_metrics,
            focus,
        } = config;

        Ok(Self {
            environment,
            tuning: tuning.into(),
            rules_json: encode_rules(&rules)?,
            overview_metrics: overview_metrics
                .into_iter()
                .map(MetricKindRecord::from)
                .collect(),
            detail_metrics: detail_metrics
                .into_iter()
                .map(MetricKindRecord::from)
                .collect(),
            focus: focus.map(|host| host_id_to_string(&host)),
        })
    }
}

impl TryFrom<FleetConfigRecord> for FleetConfig {
    type Error = FfiError;

    /// # Errors
    /// [`FfiError::InvalidConfig`] if the environment is empty or contains `::`;
    /// [`FfiError::InvalidTuning`] if the tuning fails an interlock; [`FfiError::InvalidRule`] if
    /// `rules_json` is not a JSON array of rules core accepts. All three messages are core's.
    fn try_from(record: FleetConfigRecord) -> Result<Self, Self::Error> {
        let FleetConfigRecord {
            environment,
            tuning,
            rules_json,
            overview_metrics,
            detail_metrics,
            focus,
        } = record;

        // `FleetConfig::new` is the environment check; the builders cannot fail and so carry no
        // contract of their own.
        Ok(Self::new(environment, PollTuning::try_from(tuning)?)?
            .with_rules(decode_rules(&rules_json)?)
            .with_overview_metrics(overview_metrics.into_iter().map(MetricKind::from).collect())
            .with_detail_metrics(detail_metrics.into_iter().map(MetricKind::from).collect())
            .with_focus(focus.map(host_id_from_string)))
    }
}

/// A configuration that is legal and almost certainly not what its author meant.
///
/// Warnings, not errors: each describes a rule that will do *something*, just not the something it
/// looks like it does. The sentence to show comes from [`tuning_warning_message`], which is core's
/// `TuningWarning::message`; mirroring the variants without mirroring the prose is the point.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum TuningWarningRecord {
    /// The rule's dwell is shorter than the bound alerts are gated at — `max_staleness` plus the
    /// backend lag allowance — so one old-but-still-valid sample carries it straight to firing.
    SpikeCanFire { rule_id: String, rule_name: String },
    /// A rule on a counter: the threshold is compared against the normalised rate or per-bucket
    /// delta, not the cumulative total the backend returns.
    CounterThresholdIsRate {
        rule_id: String,
        metric: MetricKindRecord,
    },
    /// `AnyOf` with an empty set — a rule that can never match a host.
    SelectorMatchesNothing { rule_id: String, rule_name: String },
}

impl From<TuningWarning> for TuningWarningRecord {
    fn from(warning: TuningWarning) -> Self {
        match warning {
            TuningWarning::SpikeCanFire { rule_id, rule_name } => {
                Self::SpikeCanFire { rule_id, rule_name }
            }
            TuningWarning::CounterThresholdIsRate { rule_id, metric } => {
                Self::CounterThresholdIsRate {
                    rule_id,
                    metric: metric.into(),
                }
            }
            TuningWarning::SelectorMatchesNothing { rule_id, rule_name } => {
                Self::SelectorMatchesNothing { rule_id, rule_name }
            }
        }
    }
}

impl From<TuningWarningRecord> for TuningWarning {
    fn from(record: TuningWarningRecord) -> Self {
        match record {
            TuningWarningRecord::SpikeCanFire { rule_id, rule_name } => {
                Self::SpikeCanFire { rule_id, rule_name }
            }
            TuningWarningRecord::CounterThresholdIsRate { rule_id, metric } => {
                Self::CounterThresholdIsRate {
                    rule_id,
                    metric: metric.into(),
                }
            }
            TuningWarningRecord::SelectorMatchesNothing { rule_id, rule_name } => {
                Self::SelectorMatchesNothing { rule_id, rule_name }
            }
        }
    }
}

/// The rule set as the JSON array [`FleetConfigRecord::rules_json`] carries.
///
/// `to_string` rather than `to_string_pretty`: nothing reads this but the decoder on the way back,
/// and a compact string is what gets persisted and compared.
///
/// # Errors
/// [`FfiError::Internal`] — see [`TryFrom<FleetConfig>`](FleetConfigRecord).
fn encode_rules(rules: &[AlertRule]) -> Result<String, FfiError> {
    serde_json::to_string(rules).map_err(|error| FfiError::Internal {
        message: format!("alert rules could not be rendered as JSON: {error}"),
    })
}

/// The reverse, which is also the gate every rule passes: `Vec<AlertRule>`'s element type
/// deserialises through core's validating wire type.
///
/// # Errors
/// [`FfiError::InvalidRule`] if the text is not a JSON array of rules core accepts, carrying
/// serde's message — which wraps core's own for a rule that parsed but broke an invariant.
fn decode_rules(rules_json: &str) -> Result<Vec<AlertRule>, FfiError> {
    serde_json::from_str(rules_json).map_err(|error| FfiError::InvalidRule {
        message: error.to_string(),
    })
}

/// The preset every interlock holds for, over the default liveness policy: a 30-second beat, stale
/// at 90s, down at 150s, a one-hour chart window.
///
/// Infallible, and deliberately so: this is the value a client falls back to when nothing is
/// stored, and a monitoring app that cannot produce its own defaults cannot start.
#[uniffi::export]
#[must_use]
pub fn poll_tuning_defaults() -> PollTuningRecord {
    PollTuning::default().into()
}

/// The preset derived from a liveness policy the user chose: `poll_interval` and `metric_step` at
/// the heartbeat interval, `max_staleness` at the down threshold, a one-hour chart window, hosts
/// forgotten after 24 hours, 256 retained.
///
/// The formulas are `PollTuning::from_liveness`'s. Note what core documents about it: the preset is
/// *derived from* the policy rather than checked against it, so a pathological policy yields a
/// tuning [`poll_tuning_validated`] would reject. Validate before use if the policy came from a
/// text field.
///
/// # Errors
/// [`FfiError::InvalidTuning`] if the policy itself is not valid; [`FfiError::Internal`] if the
/// interval is not a representable duration.
#[uniffi::export]
pub fn poll_tuning_from_liveness(
    liveness: LivenessPolicyRecord,
) -> Result<PollTuningRecord, FfiError> {
    Ok(PollTuning::from_liveness(LivenessPolicy::try_from(liveness)?).into())
}

/// The throwing constructor for [`PollTuningRecord`]: core's ten interlocks, or the message that
/// says which one failed.
///
/// Takes and returns the record because UniFFI records are plain Swift structs that the app can
/// build field by field — sliders, steppers, a restored preference — with nothing asking core's
/// opinion. This is that question, and the returned record is the one to keep: it has been through
/// `PollTuning::new`, so it is a value the rest of the boundary will accept.
///
/// # Errors
/// [`FfiError::InvalidTuning`] for any failed interlock, carrying core's message;
/// [`FfiError::Internal`] for a field that is not a representable duration.
#[uniffi::export]
pub fn poll_tuning_validated(tuning: PollTuningRecord) -> Result<PollTuningRecord, FfiError> {
    Ok(PollTuning::try_from(tuning)?.into())
}

/// A fresh configuration for an environment: default tuning, default overview and detail metrics,
/// no rules, no focus.
///
/// The metric defaults are core's two constants, read through `FleetConfig::new` rather than
/// re-listed here — a second copy of "which five metrics does the overview poll" is a second thing
/// to keep in step.
///
/// # Errors
/// [`FfiError::InvalidConfig`] if `environment` is empty or contains `::`, which would make every
/// rule URN built from it unparseable.
#[uniffi::export]
pub fn fleet_config_defaults(environment: String) -> Result<FleetConfigRecord, FfiError> {
    FleetConfigRecord::try_from(FleetConfig::new(environment, PollTuning::default())?)
}

/// The legal-but-wrong configurations in a config, in core's order: rules in config order, and
/// within a rule the variant order of [`TuningWarningRecord`].
///
/// Exists alongside the session's own audit because a settings screen shows these while the user is
/// still editing — before any config has been committed — and because it is pure: no session, no
/// clock, no poll. Disabled rules are audited too; a warning that appears only once a rule is
/// switched on arrives after the mistake has been made.
///
/// # Errors
/// Whatever [`TryFrom<FleetConfigRecord>`](FleetConfig) raises: the config must be valid before it
/// can be audited, since an audit of a config core would refuse is not advice anyone can act on.
#[uniffi::export]
pub fn fleet_config_audit(config: FleetConfigRecord) -> Result<Vec<TuningWarningRecord>, FfiError> {
    Ok(FleetConfig::try_from(config)?
        .audit()
        .into_iter()
        .map(TuningWarningRecord::from)
        .collect())
}

/// The sentence to show for a warning, from core's `TuningWarning::message`.
///
/// Not localised, and not composed here. The UI owns presentation; core owns the explanation, and
/// an explanation assembled in the bridge is one the two apps would each have to reimplement.
#[uniffi::export]
#[must_use]
pub fn tuning_warning_message(warning: TuningWarningRecord) -> String {
    TuningWarning::from(warning).message()
}

/// Authors a new rule: the client core's `draft_rule`, which mints the `UUIDv7` URN and applies the
/// selector.
///
/// The one construction path for a rule the user is creating. It checks more than
/// `AlertRule::new` does — a non-alertable metric (`AgentHeartbeat`: a silent host is liveness's
/// business) and an empty `AnyOf` selector, both of which produce a rule that can never fire.
///
/// # Errors
/// [`FfiError::InvalidRule`] for an empty name, a non-finite threshold, a negative dwell, a dwell
/// past core's one-year maximum, a non-alertable metric, or an empty `AnyOf`;
/// [`FfiError::InvalidUrn`] if `environment` is not a usable URN segment; [`FfiError::Internal`] if
/// the dwell is not a representable duration.
#[uniffi::export]
#[allow(
    clippy::needless_pass_by_value,
    reason = "UniFFI lifts a foreign string as an owned `String`; `&str` cannot cross the boundary"
)]
pub fn draft_alert_rule(
    environment: String,
    name: String,
    metric: MetricKindRecord,
    comparator: ComparatorRecord,
    threshold: f64,
    for_duration_seconds: i64,
    selector: HostSelectorRecord,
) -> Result<AlertRuleRecord, FfiError> {
    let rule = draft_rule(
        &environment,
        &name,
        metric.into(),
        comparator.into(),
        threshold,
        seconds_to_duration(for_duration_seconds)?,
        selector.into(),
    )?;
    Ok(AlertRuleRecord::from(&rule))
}

/// Re-checks an edited rule against the same predicate [`draft_alert_rule`] uses, so a settings
/// screen can refuse a save at the moment of editing.
///
/// Stricter than the decode in [`alert_rules_from_json`], on purpose: a stored rule with an empty
/// `AnyOf` must still load (the fold surfaces it, the audit warns about it), while a user typing
/// one should be told now. The split is core's — `draft_rule`/`validate_rule` refuse; `Deserialize`
/// and `audit` report — and this function is the refusing half.
///
/// # Errors
/// [`FfiError::InvalidRule`] for any defect core's `validate_rule` names; the errors of
/// [`TryFrom<AlertRuleRecord>`](AlertRule) if the record cannot be made into a rule at all.
#[uniffi::export]
pub fn validate_alert_rule(rule: AlertRuleRecord) -> Result<(), FfiError> {
    validate_rule(&AlertRule::try_from(rule)?).map_err(FfiError::from)
}

/// The rules inside a [`FleetConfigRecord::rules_json`] as records the rule list can render.
///
/// # Errors
/// [`FfiError::InvalidRule`] if the text is not a JSON array of rules core accepts.
#[uniffi::export]
#[allow(
    clippy::needless_pass_by_value,
    reason = "UniFFI lifts a foreign string as an owned `String`; `&str` cannot cross the boundary"
)]
pub fn alert_rules_from_json(rules_json: String) -> Result<Vec<AlertRuleRecord>, FfiError> {
    Ok(decode_rules(&rules_json)?
        .iter()
        .map(AlertRuleRecord::from)
        .collect())
}

/// Edited records back into the JSON a [`FleetConfigRecord`] carries, ids preserved.
///
/// Every record goes through core's validating wire type on the way, so a rule set that reaches
/// `set_config` has already been judged by the same gate a rule set loaded from disk passes.
///
/// # Errors
/// The errors of [`TryFrom<AlertRuleRecord>`](AlertRule), for the first record core refuses;
/// [`FfiError::Internal`] if the result cannot be rendered as JSON.
#[uniffi::export]
pub fn alert_rules_to_json(rules: Vec<AlertRuleRecord>) -> Result<String, FfiError> {
    let rules = rules
        .into_iter()
        .map(AlertRule::try_from)
        .collect::<Result<Vec<AlertRule>, FfiError>>()?;
    encode_rules(&rules)
}

#[cfg(test)]
mod tests {
    use chrono::Duration;
    use pessimal_client_core::{FleetConfig, PollTuning, TuningWarning, draft_rule};
    use pessimal_core::{AlertRule, Comparator, HostId, HostSelector, LivenessPolicy, MetricKind};

    use crate::convert::FfiError;

    use super::{
        AlertRuleRecord, ComparatorRecord, FleetConfigRecord, HostSelectorRecord,
        LivenessPolicyRecord, MetricKindRecord, PollTuningRecord, TuningWarningRecord,
        alert_rules_from_json, alert_rules_to_json, draft_alert_rule, fleet_config_audit,
        fleet_config_defaults, poll_tuning_defaults, poll_tuning_from_liveness,
        poll_tuning_validated, tuning_warning_message, validate_alert_rule,
    };

    /// A rule whose dwell comfortably exceeds the default evidence horizon (330s: a 150s staleness
    /// bound plus the three-minute lag allowance), so it contributes no `SpikeCanFire` noise to a
    /// test that is measuring something else.
    fn quiet_rule(name: &str, metric: MetricKind) -> AlertRule {
        draft_rule(
            "prod",
            name,
            metric,
            Comparator::GreaterThan,
            0.9,
            Duration::seconds(600),
            HostSelector::All,
        )
        .expect("a valid rule")
    }

    fn config() -> FleetConfig {
        FleetConfig::new("prod", PollTuning::default()).expect("a valid environment")
    }

    /// The default tuning as a record, for tests that then break one field.
    fn tuning_record() -> PollTuningRecord {
        poll_tuning_defaults()
    }

    #[test]
    fn every_metric_kind_round_trips_through_its_record() {
        for kind in MetricKind::ALL {
            let record = MetricKindRecord::from(kind);
            assert_eq!(MetricKind::from(record), kind, "{kind:?} lost its identity");
        }
    }

    #[test]
    fn every_comparator_round_trips_through_its_record() {
        for comparator in [
            Comparator::GreaterThan,
            Comparator::GreaterThanOrEqual,
            Comparator::LessThan,
            Comparator::LessThanOrEqual,
        ] {
            let record = ComparatorRecord::from(comparator);
            assert_eq!(Comparator::from(record), comparator);
        }
    }

    #[test]
    fn every_host_selector_shape_round_trips_through_its_record() {
        for selector in [
            HostSelector::All,
            HostSelector::Host(HostId::new("web-1")),
            HostSelector::AnyOf(vec![HostId::new("web-1"), HostId::new("db-1")]),
            // The shape `audit` warns about: it must survive the boundary, not be rejected by it.
            HostSelector::AnyOf(vec![]),
        ] {
            let record = HostSelectorRecord::from(selector.clone());
            assert_eq!(HostSelector::from(record), selector);
        }
    }

    #[test]
    fn every_tuning_warning_round_trips_through_its_record() {
        for warning in [
            TuningWarning::SpikeCanFire {
                rule_id: "urn-1".to_owned(),
                rule_name: "spiky".to_owned(),
            },
            TuningWarning::CounterThresholdIsRate {
                rule_id: "urn-2".to_owned(),
                metric: MetricKind::NetworkIo,
            },
            TuningWarning::SelectorMatchesNothing {
                rule_id: "urn-3".to_owned(),
                rule_name: "nobody".to_owned(),
            },
        ] {
            let record = TuningWarningRecord::from(warning.clone());
            assert_eq!(TuningWarning::from(record), warning);
        }
    }

    /// The guard on the eight field names the JSON construction path hardcodes, and on the id
    /// surviving it. A renamed or added field in `AlertRule` fails here rather than in the app.
    #[test]
    fn an_alert_rule_round_trips_through_its_record_with_its_id_intact() {
        let rule = draft_rule(
            "prod",
            "cpu hot",
            MetricKind::CpuUtilization,
            Comparator::GreaterThanOrEqual,
            0.85,
            Duration::seconds(180),
            HostSelector::AnyOf(vec![HostId::new("web-1"), HostId::new("web-2")]),
        )
        .expect("a valid rule")
        .disabled();

        let record = AlertRuleRecord::from(&rule);
        let restored = AlertRule::try_from(record).expect("the record must rebuild its rule");

        assert_eq!(restored.id(), rule.id(), "the id must not be re-minted");
        assert_eq!(restored, rule);
    }

    #[test]
    fn a_rule_with_a_zero_dwell_and_the_default_selector_round_trips() {
        let rule = draft_rule(
            "prod",
            "load spike",
            MetricKind::LoadAverage1m,
            Comparator::LessThan,
            0.0,
            Duration::zero(),
            HostSelector::All,
        )
        .expect("a valid rule");

        let restored = AlertRule::try_from(AlertRuleRecord::from(&rule)).expect("rebuilt");

        assert_eq!(restored, rule);
    }

    #[test]
    fn the_default_poll_tuning_round_trips_through_its_record() {
        let tuning = PollTuning::default();

        let restored = PollTuning::try_from(PollTuningRecord::from(tuning)).expect("rebuilt");

        assert_eq!(restored, tuning);
    }

    /// Non-default multipliers, because the record recovers them by dividing the thresholds core
    /// exposes; a policy whose multipliers are the default ones could pass by accident.
    #[test]
    fn a_custom_liveness_policy_round_trips_through_its_record() {
        let policy = LivenessPolicy::new(Duration::seconds(45), 4, 9).expect("a valid policy");

        let record = LivenessPolicyRecord::from(policy);

        assert_eq!(record.heartbeat_interval_seconds, 45);
        assert_eq!(record.stale_after_intervals, 4);
        assert_eq!(record.down_after_intervals, 9);
        assert_eq!(LivenessPolicy::try_from(record).expect("rebuilt"), policy);
    }

    #[test]
    fn a_tuning_over_a_custom_policy_round_trips_through_the_validating_export() {
        let policy = LivenessPolicy::new(Duration::seconds(60), 3, 6).expect("a valid policy");
        let tuning = PollTuning::new(
            policy,
            Duration::seconds(60),
            Duration::seconds(60),
            Duration::hours(2),
            Duration::seconds(300),
            Duration::minutes(3),
            Duration::hours(12),
            64,
        )
        .expect("a valid tuning");

        let record = poll_tuning_validated(PollTuningRecord::from(tuning)).expect("accepted");

        assert_eq!(PollTuning::try_from(record).expect("rebuilt"), tuning);
    }

    #[test]
    fn a_fleet_config_round_trips_through_its_record() {
        let config = config()
            .with_rules(vec![
                quiet_rule("cpu", MetricKind::CpuUtilization),
                quiet_rule("memory", MetricKind::MemoryUtilization),
            ])
            .with_overview_metrics(vec![MetricKind::CpuUtilization])
            .with_detail_metrics(vec![MetricKind::MemoryUsage, MetricKind::SystemUptime])
            .with_focus(Some(HostId::new("web-1")));

        let record = FleetConfigRecord::try_from(config.clone()).expect("a carryable config");
        let restored = FleetConfig::try_from(record).expect("rebuilt");

        assert_eq!(restored, config);
    }

    #[test]
    fn the_default_config_carries_cores_metric_defaults_and_no_rules() {
        let record = fleet_config_defaults("prod".to_owned()).expect("a valid environment");

        let restored = FleetConfig::try_from(record.clone()).expect("rebuilt");
        assert_eq!(restored, config());
        assert!(
            alert_rules_from_json(record.rules_json)
                .expect("an empty rule set")
                .is_empty()
        );
        assert_eq!(record.focus, None);
    }

    #[test]
    fn an_environment_with_urn_separators_is_an_invalid_config() {
        let error = fleet_config_defaults("prod::eu".to_owned()).expect_err("core must refuse it");

        assert!(
            matches!(error, FfiError::InvalidConfig { .. }),
            "expected InvalidConfig, got {error:?}"
        );
    }

    /// The interlock core documents most loudly: under three metric steps of staleness, `observe`
    /// returns `NoData` on a perfectly successful poll.
    #[test]
    fn a_max_staleness_below_three_metric_steps_is_invalid_tuning() {
        let record = PollTuningRecord {
            max_staleness_seconds: 60,
            ..tuning_record()
        };

        let error = poll_tuning_validated(record).expect_err("core must refuse it");

        assert!(
            matches!(error, FfiError::InvalidTuning { .. }),
            "expected InvalidTuning, got {error:?}"
        );
    }

    /// A liveness defect on the polling screen must reach Swift as a tuning failure, not as the
    /// alert-rule banner `CoreError::InvalidRule` would otherwise produce.
    #[test]
    fn inverted_liveness_multipliers_are_invalid_tuning() {
        let record = PollTuningRecord {
            liveness: LivenessPolicyRecord {
                heartbeat_interval_seconds: 30,
                stale_after_intervals: 9,
                down_after_intervals: 2,
            },
            ..tuning_record()
        };

        let error = poll_tuning_validated(record).expect_err("core must refuse it");

        assert!(
            matches!(error, FfiError::InvalidTuning { .. }),
            "expected InvalidTuning, got {error:?}"
        );
    }

    #[test]
    fn a_zero_heartbeat_interval_is_invalid_tuning() {
        let record = LivenessPolicyRecord {
            heartbeat_interval_seconds: 0,
            stale_after_intervals: 3,
            down_after_intervals: 5,
        };

        let error = poll_tuning_from_liveness(record).expect_err("core must refuse it");

        assert!(
            matches!(error, FfiError::InvalidTuning { .. }),
            "expected InvalidTuning, got {error:?}"
        );
    }

    #[test]
    fn the_liveness_preset_matches_cores_own() {
        let policy = LivenessPolicy::new(Duration::seconds(20), 3, 7).expect("a valid policy");

        let record = poll_tuning_from_liveness(LivenessPolicyRecord::from(policy))
            .expect("the preset must be valid");

        assert_eq!(
            PollTuning::try_from(record).expect("rebuilt"),
            PollTuning::from_liveness(policy)
        );
    }

    #[test]
    fn malformed_rules_json_is_an_invalid_rule() {
        let record = FleetConfigRecord {
            rules_json: "not json".to_owned(),
            ..fleet_config_defaults("prod".to_owned()).expect("a valid environment")
        };

        let error = FleetConfig::try_from(record).expect_err("the decode must refuse it");

        assert!(
            matches!(error, FfiError::InvalidRule { .. }),
            "expected InvalidRule, got {error:?}"
        );
    }

    /// The decode is core's wire type, not a parser here: a rule that is shaped correctly but
    /// breaks a domain invariant must still be refused.
    #[test]
    fn a_record_with_an_empty_name_is_refused_by_cores_wire_type() {
        let rule = quiet_rule("cpu", MetricKind::CpuUtilization);
        let record = AlertRuleRecord {
            name: String::new(),
            ..AlertRuleRecord::from(&rule)
        };

        let error = AlertRule::try_from(record).expect_err("core must refuse it");

        assert!(
            matches!(error, FfiError::InvalidRule { .. }),
            "expected InvalidRule, got {error:?}"
        );
    }

    /// The route a non-finite threshold takes, which the conversion's docs promise: JSON has no
    /// `NaN`, so `to_value` renders it `null` and core's wire type refuses the missing number. The
    /// same classification core gives a non-finite threshold directly, by a different road — and
    /// the only reason this module does not have to check `is_finite` itself.
    #[test]
    fn a_non_finite_threshold_is_refused_as_an_invalid_rule() {
        let rule = quiet_rule("cpu", MetricKind::CpuUtilization);

        for threshold in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let record = AlertRuleRecord {
                threshold,
                ..AlertRuleRecord::from(&rule)
            };

            let error = AlertRule::try_from(record).expect_err("core must refuse it");

            assert!(
                matches!(error, FfiError::InvalidRule { .. }),
                "expected InvalidRule for {threshold}, got {error:?}"
            );
        }
    }

    #[test]
    fn a_record_with_an_unparseable_id_is_an_invalid_urn() {
        let rule = quiet_rule("cpu", MetricKind::CpuUtilization);
        let record = AlertRuleRecord {
            id: "not-a-urn".to_owned(),
            ..AlertRuleRecord::from(&rule)
        };

        let error = AlertRule::try_from(record).expect_err("the URN parse must refuse it");

        assert!(
            matches!(error, FfiError::InvalidUrn { .. }),
            "expected InvalidUrn, got {error:?}"
        );
    }

    #[test]
    fn drafting_refuses_a_non_alertable_metric() {
        let error = draft_alert_rule(
            "prod".to_owned(),
            "heartbeat".to_owned(),
            MetricKindRecord::AgentHeartbeat,
            ComparatorRecord::GreaterThan,
            1.0,
            60,
            HostSelectorRecord::All,
        )
        .expect_err("a silent host is liveness's business");

        assert!(
            matches!(error, FfiError::InvalidRule { .. }),
            "expected InvalidRule, got {error:?}"
        );
    }

    #[test]
    fn drafting_produces_a_record_that_rebuilds_to_the_same_rule() {
        let record = draft_alert_rule(
            "prod".to_owned(),
            "disk full".to_owned(),
            MetricKindRecord::FilesystemUtilization,
            ComparatorRecord::GreaterThan,
            0.95,
            600,
            HostSelectorRecord::Host {
                host_id: "db-1".to_owned(),
            },
        )
        .expect("a valid rule");

        let rule = AlertRule::try_from(record.clone()).expect("rebuilt");

        assert_eq!(AlertRuleRecord::from(&rule), record);
        assert_eq!(rule.name, "disk full");
        assert_eq!(rule.for_duration(), Duration::seconds(600));
        assert_eq!(rule.selector, HostSelector::Host(HostId::new("db-1")));
    }

    /// The asymmetry the module documents: the strict half refuses an empty `AnyOf`, the carriage
    /// half preserves it, because a config already on disk must still load.
    #[test]
    fn an_empty_any_of_is_refused_by_validation_and_preserved_by_the_carriage() {
        let rule = quiet_rule("nobody", MetricKind::CpuUtilization)
            .with_selector(HostSelector::AnyOf(vec![]));
        let record = AlertRuleRecord::from(&rule);

        let error = validate_alert_rule(record.clone()).expect_err("validation must refuse it");
        assert!(
            matches!(error, FfiError::InvalidRule { .. }),
            "expected InvalidRule, got {error:?}"
        );

        let json = alert_rules_to_json(vec![record.clone()]).expect("the carriage must accept it");
        assert_eq!(alert_rules_from_json(json).expect("decoded"), vec![record]);
    }

    #[test]
    fn validation_accepts_a_rule_the_client_core_drafted() {
        let record = AlertRuleRecord::from(&quiet_rule("cpu", MetricKind::CpuUtilization));

        validate_alert_rule(record).expect("a drafted rule must survive its own validator");
    }

    #[test]
    fn a_rule_set_round_trips_through_json_in_order_with_its_ids() {
        let rules = [
            quiet_rule("cpu", MetricKind::CpuUtilization),
            quiet_rule("memory", MetricKind::MemoryUtilization),
            quiet_rule("disk", MetricKind::FilesystemUtilization),
        ];
        let records: Vec<AlertRuleRecord> = rules.iter().map(AlertRuleRecord::from).collect();

        let json = alert_rules_to_json(records.clone()).expect("encoded");

        assert_eq!(alert_rules_from_json(json).expect("decoded"), records);
    }

    #[test]
    fn the_audit_is_the_client_cores_in_cores_order() {
        let config = config().with_rules(vec![
            // Dwell under the default 330s evidence horizon: one old sample fires it.
            draft_rule(
                "prod",
                "spiky",
                MetricKind::CpuUtilization,
                Comparator::GreaterThan,
                0.9,
                Duration::seconds(30),
                HostSelector::All,
            )
            .expect("a valid rule"),
            // A counter, whose threshold is compared against a rate.
            quiet_rule("net", MetricKind::NetworkIo),
        ]);
        let expected: Vec<TuningWarningRecord> = config
            .audit()
            .into_iter()
            .map(TuningWarningRecord::from)
            .collect();

        let record = FleetConfigRecord::try_from(config).expect("a carryable config");
        let warnings = fleet_config_audit(record).expect("an auditable config");

        assert_eq!(warnings, expected);
        assert!(matches!(
            warnings.first(),
            Some(TuningWarningRecord::SpikeCanFire { .. })
        ));
        assert!(matches!(
            warnings.get(1),
            Some(TuningWarningRecord::CounterThresholdIsRate {
                metric: MetricKindRecord::NetworkIo,
                ..
            })
        ));
    }

    #[test]
    fn a_warnings_message_is_the_client_cores_sentence() {
        let warning = TuningWarningRecord::SpikeCanFire {
            rule_id: "urn-1".to_owned(),
            rule_name: "spiky".to_owned(),
        };

        let message = tuning_warning_message(warning.clone());

        assert_eq!(message, TuningWarning::from(warning).message());
        assert!(message.contains("spiky"), "{message}");
    }

    /// Advice about a configuration core would refuse is advice nobody can act on, so the audit
    /// reports the refusal instead.
    #[test]
    fn an_audit_of_an_invalid_config_fails_rather_than_advising() {
        let record = FleetConfigRecord {
            tuning: PollTuningRecord {
                max_retained_hosts: 0,
                ..tuning_record()
            },
            ..fleet_config_defaults("prod".to_owned()).expect("a valid environment")
        };

        let error = fleet_config_audit(record).expect_err("core must refuse it");

        assert!(
            matches!(error, FfiError::InvalidTuning { .. }),
            "expected InvalidTuning, got {error:?}"
        );
    }
}
