# M3 — `pessimal_client_core`

**Created:** 2026-09-07
**Status:** Design, not yet implemented
**Completes:** the outstanding half of [`2026-09-06-m3-signoz-query-adapter.md`](2026-09-06-m3-signoz-query-adapter.md)

## 1. Goal

`pessimal_client_core` turns a telemetry backend into a fleet picture the macOS and iOS apps can
render without deciding anything. It is a three-stage pipeline — **plan** (pure) decides every query
from `FleetConfig` and `now`; **gather** (the only `async` function in the crate) issues them and
turns every failure into data; **fold** (pure) consumes `(FleetConfig, FleetState, PollObservation)`
and returns a new `FleetState`, the `FleetView` the apps draw, the `AlertTransition`s worth a
notification, and the `PollAdvice` that says when to come back. Every decision the product makes —
liveness, alert dwell, counter normalisation, per-mount reduction, evaluation lifecycle, freshness,
backoff — is a synchronous pure function over three plain values, so a test constructs its input
literally: no mock port, no runtime, no `Clock`, no `Arc<Mutex<_>>`. The crate depends on
`pessimal_core` and its two ports and nothing else: no HTTP, no `reqwest`, no `tokio`, no
`pessimal_query_signoz`, no `uniffi`. Its one load-bearing promise is that **failure freezes; it
never fabricates** — a backend we cannot reach must never be rendered as a fleet that has gone down.

## 2. Decisions

| Decision | Rationale |
|---|---|
| **Swift/the platform drives the clock.** No timer, no task, no runtime, no `Clock` trait in this crate. `now` is an explicit parameter on `plan_poll`, `poll_once`, `probe_backend`, `freshness_at`. | iOS suspends a Rust-owned `tokio::time::interval` and resumes it at an instant nobody chose; only Swift sees `scenePhase`, popover-open, and sleep/wake, and only Swift knows the cadence those imply. A `Clock` trait buys nothing here — `FixedClock::set` takes `&mut self` and cannot be advanced behind an `Arc<dyn Clock>` anyway. Passing `now` is what makes a test say "it is now T+90s" by typing T+90s. |
| **Core owns the *policy*, Swift owns the *timer*.** Every fold returns `PollAdvice`: `Poll { after }`, `Retry { after, consecutive_failures }`, or `Stop { reason, message }`. | Backoff and the auth cliff are decisions, not timers. Swift obeys the advice and may add jitter; core stays deterministic and unjittered so backoff schedules are exact in tests. A user gesture (pull-to-refresh) may ignore the advice — that is the one sanctioned override. |
| **A failed poll never mutates the picture, only the metadata.** The roster, every `Liveness` verdict, every `liveness_at`, and every stored series are left exactly as they were; only `as_of`, `consecutive_failures`, `last_failure` and `advised_next_poll` move. | Re-running `LivenessPolicy::evaluate(last_heartbeat, now)` against an advanced clock walks a healthy fleet Alive→Stale→Down on the strength of a dead Wi-Fi connection. "I cannot see" and "your fleet is down" must not share a code path. |
| **The last good view is never blanked, and never claims to be current.** `FleetView` carries `FreshnessInputs`, not a `Freshness`; `freshness_at(&inputs, now)` is a function of `now`. | A `Freshness` computed at fold time is frozen between polls, so a hung `execute_plan` or a Swift timer that stopped would read `Fresh` forever while the data rots. Making it a function of `now` is the only shape that greys the screen when nothing is calling us at all. |
| **Alert state lives in `FleetState.evaluations`, one `AlertEvaluation` per `(host, rule)`, keyed `BTreeMap<HostId, BTreeMap<String /* Urn::to_string() */, StoredEvaluation>>`.** `FleetState` is opaque, held by `pessimal_ffi::FleetSession` behind a `Mutex`, persisted as JSON. | Dwell is stateful — `observe` keeps `breaching_since` across polls — and `pessimal_core` deliberately owns none of the lifecycle. `Urn` is `Hash + Eq` but not `Ord`, so the key is its string form; `BTreeMap` everywhere makes iteration order deterministic and whole-state `assert_eq!` viable. |
| **Coverage is a trichotomy per `(host, metric)`: `Fresh` / `Empty` / `Unavailable`.** `observe` is called on `Fresh` and `Empty`, never on `Unavailable`. | `Ok(vec![])` is a real absence of data and legitimately clears dwell (core's `an_empty_series_is_no_data_not_ok`). `Err` and never-asked are not evidence of anything, and calling `observe` with an empty series on a failed fetch would return `NoData` and reset the dwell on every alert in the fleet after one transient 500. This is structural, not a remembered `if`. |
| **The blind-gap reset is keyed to the poll we asked for, not to the poll cadence.** `FleetState` stores `advised_next_poll = as_of + advice.delay()`; the fold replaces every evaluation with a fresh `NoData` iff `observed_at > advised_next_poll + max_staleness`. | Dwell in core is wall-clock `now - since` and survives any unobserved gap, so an hour-suspended app plus one breaching sample fires instantly. Comparing against the *advised* instant means a deliberate 5-minute backoff does not trip its own freeze, while an unattended gap does. "We are more than `max_staleness` late for the poll we asked for" is exact and self-documenting. |
| **History is bounded four ways:** the fetch window (`overview_window()` fleet-wide ≈ 7 buckets, `chart_window()` for the one focused host ≈ 120 buckets), replace-not-merge on every successful outcome, `forget_host_after` (24 h), and a hard `max_retained_hosts` cap (256) evicting the oldest `last_seen_in_roster` first. | Replacing outright kills timestamp alignment, duplicate points and drifting windows at the cost of bandwidth, affordable only because the fleet-wide window is short. The host cap exists because a backend churning hostnames (containers, CI runners) otherwise accumulates 24 h of ghosts, each carrying an evaluation per rule. |
| `PollTuning` and `FleetConfig` deserialize through `#[serde(try_from = "…Wire")]`. | Both derive `Deserialize` over fields their constructors validate — the same hole `LivenessPolicy` has, where a persisted policy can restore `stale_after_intervals >= down_after_intervals` and invert the whole liveness model. A validated constructor that `serde` walks around is a doc comment, not an invariant. `PollTuning::new` therefore also re-checks the embedded `LivenessPolicy`. |
| Default `max_staleness = liveness.down_threshold()` (150 s), interlocked as `3 * metric_step <= max_staleness <= down_threshold()`. | *Judges disagreed:* D1 defaulted it to `stale_threshold()` (90 s), D3 argued that is too tight. D3 wins on arithmetic — at a 30 s step the newest complete SigNoz bucket is already 30–60 s old before poll latency, so 90 s makes `observe` return `NoData` on a *successful* poll. At `down_threshold` the bound also coincides with the age at which we already call a host down. |
| Multi-series metrics reduce to one synthetic series by comparator direction: `Max` for `>`/`>=`, `Min` for `<`/`<=`. There is no `Sum`. | *Judges disagreed:* D2 wanted `SumAcrossSets` for usage and network. Rejected: a sum needs exact timestamp alignment across label sets and silently under-reports when a bucket is missing, so a `>` rule quietly fails to fire; `Max` degrades safely. "Disk above 90 % on any mount" is what a rule means. |
| One evaluation per `(rule, host)`, not per `(rule, host, attribute-set)`. | *Judges disagreed:* D3 keyed dwell per attribute set. Rejected: `MetricSeries.attributes` is an open set the backend controls, so a rotating label splits one logical series into many keys that each restart dwell from zero — unbounded by construction and the rule that should fire never does. |
| `Severity` ships; formatting, sliders and pick-lists do not. | *Judges disagreed:* D2's `format_bytes`/`FleetHeadline.title`/`metric_catalog` thresholds were praised by the FFI judge and called fatal by the YAGNI judge. Split it: a severity *verdict* two apps must not disagree about belongs in core; English strings and slider bounds for an editor nobody has designed do not. `describe_metric` survives because `alertable` and `is_rate` are domain facts, not UI. |
| No `tokio::time` and no timeout in this crate. `pessimal_ffi` builds the `reqwest::Client` with timeouts and passes it to `SignozQuery::with_client`. | *Judges disagreed:* D3 made `tokio::time::timeout` the crate's hang defence. Rejected: it panics outside a runtime context, its behaviour under Swift's executor is unverified in this codebase family, and it fails *silently* — while `SignozQuery::new` configures no `reqwest` timeout, which is the actual bug, one layer down. `freshness_at(now)` is the belt: a hung poll greys the screen even though nothing errored. |
| No suppression of alerts on `Liveness::Down`. | *Judges disagreed:* D3 forced `NoData` on Down hosts. Rejected: with `max_staleness == down_threshold()` by default, a Down host's newest sample is stale by the same clock and `observe` already returns `NoData`; explicit suppression adds only a dwell reset that makes a flapping host slower to alert. |
| `testing.rs` is behind `[features] testing = []`. | *Judges disagreed:* D3 shipped it unconditionally. Rejected — test doubles in the shipped library and in the public docs. `pessimal_ffi` opts in via `pessimal_client_core = { workspace = true, features = ["testing"] }` under `[dev-dependencies]`. |
| An edited rule drops its dwell automatically, via a `rule_fingerprint` stored beside each evaluation. | D2 exposed `invalidate_rule` for the app to call. A fingerprint over `(metric, selector, comparator, threshold.to_bits(), for_duration)` is the same fix with nothing to forget: shortening `for_duration` on a `Pending` rule would otherwise put `fires_at = since + new_duration` in the past and fire instantly off an old `since`. |
| A rule that fails `validate_rule` is shown, flagged, and not evaluated — `AlertView.invalid_reason` on the fold's critical path. | `AlertRule`'s invariants hold only in `new()` and its fields are public, so a NaN threshold arrives intact through `Deserialize`, makes every comparator return `false`, and the rule silently never fires. A config-time `audit()` catches it only if someone wires the audit up. |
| `FleetConfig::audit()` keeps three warnings: `SpikeCanFire`, `CounterThresholdIsRate`, `SelectorMatchesNothing`. | Trimmed from six. `NonFiniteThreshold`/`EmptyRuleName` moved to `validate_rule` where they are visible; `RuleMetricNotCharted` is moot because `planned_metrics()` fetches every enabled rule's metric; `DwellExceedsChartWindow` is cosmetic. |
| `ClientError` is only ever a return type; `PollFailure` is the only failure type in field position. | A type cannot be both a `uniffi::Error` and a record field enum in one crate. Keeping the two vocabularies apart is what lets `PollFailure` sit inside `FleetView` and inside a serialisable `PollObservation`, and `From<CoreError> for ClientError` is an exhaustive variant match — never `.to_string()` of a whole error, which would strip `Unauthorized` before the settings screen could branch on it. |
| Concurrency control lives in `pessimal_ffi::FleetSession`, not here. | `poll_once` is a free function over `&FleetState`; a menu-bar timer tick and a pull-to-refresh will collide and last-write-wins would silently discard one poll's HTTP. The session holds an async mutex, `try_lock`s it, and returns `Skipped` — with an RAII guard, because UniFFI has no cancellation and Swift dropping its `Task` mid-poll must not wedge the poller. |
| Counter and multi-series policy lives here because nothing below it does it. | The SigNoz adapter aggregates counters `("max","max")` (verified in `naming.rs::aggregation_for`), which on a cumulative sum returns the running total. Charted raw it is a line that only goes up; alerted raw a `>` rule latches forever. `NetworkIo` → bytes/s, `AgentCollectionFailures` → per-bucket delta. |
| Dependencies: `pessimal_core`, `chrono`, `uuid`, `serde`, `thiserror`, `async-trait`, `futures`. Dev: `tokio` (`rt`, `macros`), `serde_json`. | `pessimal_core` re-exports none of `chrono`/`uuid`, so the signatures do not name-resolve without the direct deps. `pessimal_query_signoz` appears nowhere, not even in dev-dependencies — the hexagon boundary is enforced by the manifest. |

## 3. Module layout

All paths under `clients/common/pessimal_client_core/src/`.

- `lib.rs` — module declarations, a curated root re-export set (never a glob of `pessimal_core`, whose `Result<T>` would shadow), and the crate doc stating the Plan-Gather-Fold contract and the "failure freezes, never fabricates" rule.
- `error.rs` — `ClientError` (flat, return-position only, exhaustive `From<CoreError>`) and the serialisable `PollFailure` / `PollFailureKind` / `FailureSource` with `worst_failure`'s deterministic ranking.
- `config.rs` — `PollTuning` (private fields, one fallible constructor, derived windows) and `FleetConfig`, both deserialised through validating wire types, plus `TuningWarning` and `FleetConfig::audit`.
- `plan.rs` — `QuerySpec`, `PollPlan`, and `plan_poll(&FleetConfig, now)`: pure two-tier query planning, a function of config and clock only, never of state.
- `observation.rs` — `PollObservation` and the `Coverage { Fresh, Empty, Unavailable }` trichotomy: what the backend said, kept strictly apart from what it means, and serialisable so a production poll replays as a fixture.
- `normalize.rs` — pure sample arithmetic nothing below this crate performs: counter→rate/delta with reset dropping, multi-attribute reduction by comparator direction, `dominant_at`, `series_label`, `series_id`.
- `fold.rs` — `FleetState`, the seven-step `apply`, `FleetUpdate`, `AlertTransition`, `PollAdvice`, and JSON persistence with `RestoreReport`.
- `view.rs` — the render model: `FleetView` and its parts, `Severity`, `MetricAvailability`, `CollectionHealth`, `AlertPhase`, and `freshness_at` as a function of `now`.
- `gather.rs` — the crate's only `async` code: `execute_plan` (never returns `Err`), the ten-line `poll_once` composition, and `probe_backend`.
- `rules.rs` — a thin wrapper over `AlertRuleRepository`, plus `validate_rule` and `rule_fingerprint`, which the fold calls without ever touching a port.
- `testing.rs` — `#[cfg(any(test, feature = "testing"))]`: `ScriptedQuery`, `InMemoryRuleRepository`, and series builders.

## 4. Public API

Every block is marked **Exported** — `pessimal_ffi` mirrors it as a `uniffi::Record`/`uniffi::Enum`
with `DateTime<Utc>` → `i64` epoch millis, `chrono::Duration` → `i64` seconds, `Urn` → `String`,
`BTreeMap<String, String>` → a sorted `Vec<AttributeRecord>`, and `usize` → `u32` — or **Internal**,
meaning it never crosses the boundary and `pessimal_ffi` wraps it as described in §4.11.

### 4.1 `error.rs`

**Exported.** `ClientError` is the crate's `uniffi::Error`. It appears in return position only;
`PollFailure` is the only failure type that ever sits in a struct field.

```rust
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ClientError {
    #[error("invalid poll tuning: {0}")] InvalidTuning(String),
    #[error("invalid client configuration: {0}")] InvalidConfig(String),
    #[error("invalid alert rule: {0}")] InvalidRule(String),
    #[error("invalid URN: {0}")] InvalidUrn(String),
    #[error("invalid time range: {0}")] InvalidTimeRange(String),
    #[error("unknown metric: {0}")] UnknownMetric(String),
    #[error("no such alert rule: {0}")] RuleNotFound(String),
    #[error("unknown host: {0}")] UnknownHost(String),
    #[error("telemetry backend error: {0}")] Backend(String),
    #[error("not authorised for the telemetry backend")] Unauthorized,
    #[error("telemetry backend is unreachable: {0}")] Unreachable(String),
}

pub type Result<T> = std::result::Result<T, ClientError>;

/// Exhaustive `match`, never `.to_string()` on the whole error: the variant is what drives
/// backoff, the auth cliff, and whether the banner offers an "Open Settings" button.
impl From<pessimal_core::CoreError> for ClientError { /* one arm per variant, no `_` */ }
```

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum PollFailureKind { Unauthorized, Unreachable, Backend, Malformed }

impl PollFailureKind {
    /// `Unauthorized -> Unauthorized`, `Unreachable -> Unreachable`, `Backend -> Backend`,
    /// every other `CoreError` -> `Malformed`. The single classification point.
    #[must_use] pub fn from_core(error: &pessimal_core::CoreError) -> Self;
    /// True only for `Unauthorized`: retrying cannot help, the user must change something.
    #[must_use] pub fn is_actionable(self) -> bool;
    #[must_use] pub fn is_transient(self) -> bool;
    /// Severity rank for `worst_failure`: Unauthorized 3, Unreachable 2, Backend 1, Malformed 0.
    #[must_use] pub fn rank(self) -> u8;
}

/// Which request failed. Carried so the UI can name the metric, and so `worst_failure` can
/// tie-break without depending on `join_all` completion order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum FailureSource { Roster, Series { metric: pessimal_core::MetricKind } }

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PollFailure {
    pub kind: PollFailureKind,
    pub source: FailureSource,
    pub message: String,
    pub at: chrono::DateTime<chrono::Utc>,
}

impl PollFailure {
    #[must_use] pub fn from_core(
        error: &pessimal_core::CoreError,
        source: FailureSource,
        at: chrono::DateTime<chrono::Utc>,
    ) -> Self;
}

/// The failure a user should be shown when several requests failed at once. Ranked by
/// `kind.rank()` descending, tie-broken `Roster` before `Series`, then by `MetricKind` order.
/// Deterministic: the banner text must not depend on which future finished first.
#[must_use] pub fn worst_failure(failures: &[PollFailure]) -> Option<&PollFailure>;
```

### 4.2 `config.rs`

**Exported.** `PollTuning` has private fields, so `pessimal_ffi` mirrors it as a flat record plus a
throwing constructor; `FleetConfig` mirrors field-for-field with `Vec<AlertRule>` carried as its
JSON string (see §4.11).

```rust
/// CPU, Memory, Disk, Load (1m), Collection failures. Five fleet-wide requests per poll, not
/// twelve. Collection failures is in the default set because it is the only signal that catches
/// an agent beating happily while its sampling fails every cycle.
pub const DEFAULT_OVERVIEW_METRICS: [pessimal_core::MetricKind; 5];
/// Every modelled metric except `AgentHeartbeat`, whose value nothing reads.
pub const DEFAULT_DETAIL_METRICS: [pessimal_core::MetricKind; 11];

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "PollTuningWire", into = "PollTuningWire")]
pub struct PollTuning { /* all fields private */ }

/// The only shape `PollTuning` deserialises through, so a persisted or synced value cannot
/// bypass `PollTuning::new`. Private to the module; `TryFrom<PollTuningWire>` calls `new`.
```

```rust
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
    /// `max_retained_hosts >= 1`; and — because `LivenessPolicy` derives `Deserialize` over
    /// private fields and so arrives with `LivenessPolicy::new` bypassed —
    /// `liveness.heartbeat_interval() > 0`,
    /// `liveness.stale_threshold() < liveness.down_threshold()`, and
    /// `liveness.stale_threshold() >= 3 * liveness.heartbeat_interval()`, which is the only way
    /// to check the documented `stale_after_intervals >= 3` given the multipliers are private.
    pub fn new(
        liveness: pessimal_core::LivenessPolicy,
        poll_interval: chrono::Duration,
        metric_step: chrono::Duration,
        chart_window: chrono::Duration,
        max_staleness: chrono::Duration,
        forget_host_after: chrono::Duration,
        max_retained_hosts: u32,
    ) -> Result<Self>;

    /// The preset that satisfies every interlock: `poll_interval` and `metric_step` =
    /// `heartbeat_interval()`, `max_staleness` = `down_threshold()`, `chart_window` = 1 hour,
    /// `forget_host_after` = 24 hours, `max_retained_hosts` = 256.
    #[must_use] pub fn from_liveness(liveness: pessimal_core::LivenessPolicy) -> Self;

    #[must_use] pub fn liveness(&self) -> pessimal_core::LivenessPolicy;
    /// The step the adapter must be built with for `list_hosts`. Nothing here can enforce it —
    /// wire `SignozConfig::with_heartbeat_interval(tuning.heartbeat_interval())` at construction.
    #[must_use] pub fn heartbeat_interval(&self) -> chrono::Duration;
    #[must_use] pub fn poll_interval(&self) -> chrono::Duration;
    #[must_use] pub fn metric_step(&self) -> chrono::Duration;
    #[must_use] pub fn chart_window(&self) -> chrono::Duration;
    #[must_use] pub fn max_staleness(&self) -> chrono::Duration;
    #[must_use] pub fn forget_host_after(&self) -> chrono::Duration;
    #[must_use] pub fn max_retained_hosts(&self) -> u32;

    /// Derived, not supplied, so they cannot be set wrong.
    /// `down_threshold() + 2 * heartbeat_interval()` — a Down host must still be listed.
    #[must_use] pub fn host_window(&self) -> chrono::Duration;
    /// `max_staleness() + 2 * metric_step()` — about seven buckets at defaults.
    #[must_use] pub fn overview_window(&self) -> chrono::Duration;
    /// `2 * poll_interval() + metric_step()` — two missed polls plus a bucket. Past this the
    /// view is `Degraded`.
    #[must_use] pub fn staleness_tolerance(&self) -> chrono::Duration;
    /// `down_threshold()` — past the age at which we would call a host down, the whole view is
    /// past the age at which it deserves to be believed. Past this it is `Unusable`.
    #[must_use] pub fn freshness_budget(&self) -> chrono::Duration;
    /// `min(poll_interval * 2^min(n, 5), 5min)`. Deterministic and unjittered; Swift may add
    /// jitter, core stays reproducible.
    #[must_use] pub fn backoff_after(&self, consecutive_failures: u32) -> chrono::Duration;
}

impl Default for PollTuning { /* from_liveness(LivenessPolicy::default()) */ }
```

```rust
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "FleetConfigWire", into = "FleetConfigWire")]
pub struct FleetConfig {
    pub environment: String,
    pub tuning: PollTuning,
    pub rules: Vec<pessimal_core::AlertRule>,
    pub overview_metrics: Vec<pessimal_core::MetricKind>,
    pub detail_metrics: Vec<pessimal_core::MetricKind>,
    pub focus: Option<pessimal_core::HostId>,
}

impl FleetConfig {
    /// # Errors
    /// [`ClientError::InvalidConfig`] if `environment` is empty or contains `::`, since it is
    /// embedded in every rule's URN. Deserialisation goes through the same check.
    pub fn new(environment: impl Into<String>, tuning: PollTuning) -> Result<Self>;

    #[must_use] pub fn with_rules(self, rules: Vec<pessimal_core::AlertRule>) -> Self;
    #[must_use] pub fn with_focus(self, host: Option<pessimal_core::HostId>) -> Self;
    #[must_use] pub fn with_overview_metrics(self, metrics: Vec<pessimal_core::MetricKind>) -> Self;
    #[must_use] pub fn with_detail_metrics(self, metrics: Vec<pessimal_core::MetricKind>) -> Self;

    #[must_use] pub fn enabled_rules(&self) -> Vec<&pessimal_core::AlertRule>;
    /// Overview metrics UNION every enabled rule's metric, sorted and deduped. This is why a
    /// rule on a metric nobody charts still gets its data.
    #[must_use] pub fn planned_metrics(&self) -> Vec<pessimal_core::MetricKind>;
    /// Legal-but-wrong configurations, computed when the config changes rather than per poll.
    #[must_use] pub fn audit(&self) -> Vec<TuningWarning>;
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TuningWarning {
    /// `rule.for_duration() < tuning.max_staleness()`: because `observe` sets
    /// `since = point.at` on a fresh breach, one sample that is old-but-still-valid carries the
    /// rule straight to `Firing` with no further evidence.
    SpikeCanFire { rule_id: String, rule_name: String },
    /// A rule on `NetworkIo` / `AgentCollectionFailures`: the threshold is compared against the
    /// normalised rate or delta, not the cumulative total the backend returns.
    CounterThresholdIsRate { rule_id: String, metric: pessimal_core::MetricKind },
    /// `HostSelector::AnyOf(vec![])` — a rule that can never match a host.
    SelectorMatchesNothing { rule_id: String, rule_name: String },
}

impl TuningWarning { #[must_use] pub fn message(&self) -> String; }
```

### 4.3 `plan.rs`

**Internal.** Swift never plans; `FleetSession::poll` calls `poll_once`, which plans for it.

```rust
/// A serialisable twin of [`pessimal_core::SeriesRequest`], which derives no serde. Field-
/// identical; `to_request` destructures exhaustively so a new field on `SeriesRequest` is a
/// compile error here rather than a silently dropped one.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct QuerySpec {
    pub metric: pessimal_core::MetricKind,
    pub selector: pessimal_core::HostSelector,
    pub range: pessimal_core::TimeRange,
    pub step: chrono::Duration,
}

impl QuerySpec {
    #[must_use] pub fn to_request(&self) -> pessimal_core::SeriesRequest;
    /// Whether this spec's results could contain data for `host` and `metric`.
    #[must_use] pub fn covers(
        &self,
        host: &pessimal_core::HostId,
        metric: pessimal_core::MetricKind,
    ) -> bool;
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PollPlan {
    pub now: chrono::DateTime<chrono::Utc>,
    pub host_range: pessimal_core::TimeRange,
    /// Fleet-wide overview queries first, focused detail queries last. Later outcomes win, which
    /// is how the focused host gets the long window with no merge step.
    pub queries: Vec<QuerySpec>,
}

/// Two tiers. Fleet-wide: one spec per `config.planned_metrics()` with `HostSelector::All` over
/// `tuning.overview_window()` at `tuning.metric_step()` — N_metrics requests, not
/// N_hosts x N_metrics. Focus: when `config.focus` is `Some(host)`, one spec per `detail_metrics`
/// with `HostSelector::Host(host)` over `tuning.chart_window()` — at `tuning.metric_step()`,
/// the same step — emitted last. `host_range = TimeRange::ending_at(now, tuning.host_window())`.
///
/// **There is exactly one step in this crate.** If a coarser `chart_step` is ever added, coarse
/// series must never reach `observe`: a bucket boundary one step early seeds `since` a step early
/// and the rule fires a step early. That would need two caches and an enforced invariant, not a
/// convention.
///
/// # Errors
/// [`ClientError::InvalidTimeRange`] if a window is not strictly positive. A `PollTuning` built
/// by its constructor cannot cause this.
pub fn plan_poll(
    config: &FleetConfig,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<PollPlan>;
```

### 4.4 `observation.rs`

**Internal.** Explicit outcome enums rather than `Result` in field position — UniFFI accepts
`Result` only in return position, and the enums read better anyway.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Coverage {
    /// A covering request succeeded and returned at least one series for this host and metric.
    Fresh,
    /// A covering request succeeded and returned nothing: genuine `NoData`.
    Empty,
    /// Every covering request failed, or none was issued: freeze, do not judge.
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum RosterOutcome {
    /// `list_hosts` was not attempted. A partial poll is representable.
    NotAttempted,
    Listed(Vec<pessimal_core::Host>),
    Failed(PollFailure),
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum SeriesResult { Returned(Vec<pessimal_core::MetricSeries>), Failed(PollFailure) }

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SeriesOutcome { pub spec: QuerySpec, pub result: SeriesResult }

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PollObservation {
    /// The single instant this whole poll is evaluated at — the query window, liveness, and
    /// dwell all read it. Stamped once, before any request. Nothing else in the crate reads a
    /// clock, which is why there is no `completed_at`.
    pub observed_at: chrono::DateTime<chrono::Utc>,
    pub hosts: RosterOutcome,
    pub series: Vec<SeriesOutcome>,
}

impl PollObservation {
    #[must_use] pub fn new(observed_at: chrono::DateTime<chrono::Utc>) -> Self;
    #[must_use] pub fn with_hosts(self, hosts: RosterOutcome) -> Self;
    #[must_use] pub fn with_series(self, spec: QuerySpec, result: SeriesResult) -> Self;

    #[must_use] pub fn roster(&self) -> Option<&[pessimal_core::Host]>;
    #[must_use] pub fn coverage(
        &self,
        host: &pessimal_core::HostId,
        metric: pessimal_core::MetricKind,
    ) -> Coverage;
    /// Series for exactly this host and metric, in plan order, deduped by `attributes` keeping
    /// the last — which is how a focused host's long detail window beats its overview window.
    /// Performs the host/kind filtering `AlertEvaluation::observe` does not do.
    #[must_use] pub fn series_for(
        &self,
        host: &pessimal_core::HostId,
        metric: pessimal_core::MetricKind,
    ) -> Vec<&pessimal_core::MetricSeries>;
    #[must_use] pub fn failures(&self) -> Vec<PollFailure>;
    /// True when the roster was listed and no series outcome failed.
    #[must_use] pub fn is_clean(&self) -> bool;
}
```

### 4.5 `normalize.rs`

**Internal**, except `series_id` and `series_label`, whose outputs are carried on the view.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Normalization { None, RatePerSecond, Delta }

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Reduction { Max, Min }

/// `NetworkIo -> RatePerSecond`, `AgentCollectionFailures -> Delta`,
/// `AgentHeartbeat -> None` (liveness reads heartbeat via `list_hosts` bucket timestamps only,
/// never its value), every gauge `-> None`.
#[must_use] pub fn normalization_for(kind: pessimal_core::MetricKind) -> Normalization;
#[must_use] pub fn is_rate(kind: pessimal_core::MetricKind) -> bool;

/// Differences consecutive points and divides by elapsed seconds, stamping each result at the
/// later of the two timestamps. A negative delta is a counter reset — an agent restart zeroing
/// `pessimal.agent.*`, or the host-total `system.network.io` sum falling when an interface
/// leaves — and its point is DROPPED, never emitted as a negative or a spike. N points in, at
/// most N-1 out; a one-point series yields zero points, which `observe` reads as `NoData`.
#[must_use] pub fn rate_per_second(s: &pessimal_core::MetricSeries) -> pessimal_core::MetricSeries;
#[must_use] pub fn delta(s: &pessimal_core::MetricSeries) -> pessimal_core::MetricSeries;
/// Applies `normalization_for(series.kind)`; gauges pass through untouched.
#[must_use] pub fn normalize(s: &pessimal_core::MetricSeries) -> pessimal_core::MetricSeries;
/// Timestamps at which a reset was detected and a point dropped, so a chart can mark them.
#[must_use] pub fn counter_resets(
    s: &pessimal_core::MetricSeries,
) -> Vec<chrono::DateTime<chrono::Utc>>;

/// `Max` for `>`/`>=`, `Min` for `<`/`<=` — what "disk above 90 % on any mount" means.
#[must_use] pub fn reduction_for(comparator: pessimal_core::Comparator) -> Reduction;

/// Collapses several attribute-set series (mounts, interfaces, memory states) into one synthetic
/// series carrying empty `attributes`: for each timestamp present in any input, the reduced value
/// across the series holding a point at that exact timestamp. SigNoz buckets align to the step,
/// so timestamps coincide. Built through `MetricSeries::new`, which sorts — `latest_at` scans in
/// reverse and is only correct on ascending points. `None` for an empty slice.
#[must_use] pub fn reduce(
    kind: pessimal_core::MetricKind,
    host: &pessimal_core::HostId,
    series: &[pessimal_core::MetricSeries],
    reduction: Reduction,
) -> Option<pessimal_core::MetricSeries>;

/// Which input series held the reduced value at `at` — the mount that is actually full, so the
/// alert can say "/ (95 %)" rather than "95 %".
#[must_use] pub fn dominant_at<'a>(
    series: &'a [pessimal_core::MetricSeries],
    at: chrono::DateTime<chrono::Utc>,
    reduction: Reduction,
) -> Option<&'a pessimal_core::MetricSeries>;

/// A short human label from a series' attributes: `system.filesystem.mountpoint`, then
/// `network.interface.name` + `network.io.direction`, then `system.memory.state`. `None` for a
/// host-wide series.
#[must_use] pub fn series_label(s: &pessimal_core::MetricSeries) -> Option<String>;
/// Stable `ForEach` identity: `"{host}|{otel_name}|{k=v,…}"` over the sorted attribute map.
#[must_use] pub fn series_id(
    host: &pessimal_core::HostId,
    kind: pessimal_core::MetricKind,
    attributes: &std::collections::BTreeMap<String, String>,
) -> String;
```

### 4.6 `fold.rs`

**Internal.** `FleetState` is an opaque handle: private fields over nested `BTreeMap`s, not a
Record under any mapping. `pessimal_ffi` holds it inside `FleetSession` and moves it across the
boundary only as a JSON string.

```rust
pub const FLEET_STATE_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FleetState {
    // All private. `PartialEq` but not `Eq`: `MetricSeries` and `AlertRule` carry `f64`.
    //
    //   schema_version: u32,
    //   backend_name: String,
    //   as_of: Option<DateTime<Utc>>,          // None until the first fold
    //   advised_next_poll: Option<DateTime<Utc>>,
    //   last_clean_at: Option<DateTime<Utc>>,  // last poll with a listed roster and no failure
    //   last_roster_at: Option<DateTime<Utc>>,
    //   consecutive_failures: u32,
    //   last_failure: Option<PollFailure>,     // worst_failure of the last failing poll
    //   #[serde(skip)] polled_this_session: bool,
    //   hosts: BTreeMap<HostId, HostRecord>,           // host, liveness, liveness_at,
    //                                                  // last_seen_in_roster, in_current_roster
    //   series: BTreeMap<HostId, BTreeMap<MetricKind, StoredMetric>>,
    //   evaluations: BTreeMap<HostId, BTreeMap<String /* Urn */, StoredEvaluation>>,
}

/// Private. `StoredMetric { series: Vec<MetricSeries>, availability, fetched_at, resets }`.
/// Private. `StoredEvaluation { evaluation: AlertEvaluation, fingerprint: u64,
///           frozen_since: Option<DateTime<Utc>>, latest_value: Option<f64>,
///           dominant_label: Option<String> }`.
```

```rust
impl FleetState {
    #[must_use] pub fn new(backend_name: impl Into<String>) -> Self;

    /// The pure fold. Consumes one poll and returns an entirely new state plus everything the
    /// app needs. Never mutates, never blocks, never reads a clock: `observation.observed_at` is
    /// the only instant used. Steps, each its own test:
    ///
    /// 1. **Blind gap.** If `self.advised_next_poll` is `Some(t)` and
    ///    `observed_at > t + tuning.max_staleness()`, every `AlertEvaluation` is replaced with a
    ///    fresh `NoData` one BEFORE anything is observed. We are more than a staleness bound late
    ///    for the poll we ourselves asked for, so we did not watch the interval and must not
    ///    judge it. Skipped on the first fold, when there is no advised instant. This is also
    ///    what makes persisting `FleetState` across a relaunch safe.
    /// 2. **Roster.** On `RosterOutcome::Listed`, merge each `Host` FIELD BY FIELD —
    ///    `last_heartbeat` only when strictly greater, `agent_version` never overwritten with
    ///    `None`, and `os` never overwritten with `OsFamily::Other` when the stored value is not
    ///    `Other` (a label set missing `os.type` otherwise degrades a Windows host and turns its
    ///    load tiles from `Unsupported` into a gap) — then set `last_seen_in_roster`,
    ///    mark `in_current_roster`, and recompute `Liveness` for EVERY retained host, including
    ///    ones absent from this roster, which keep their old `last_heartbeat` and therefore read
    ///    `Down` naturally. `Unknown` is left to mean "in the roster, never beat". On
    ///    `Failed`/`NotAttempted`, no liveness verdict and no `liveness_at` moves at all.
    /// 3. **Retention.** Forget hosts whose `last_seen_in_roster` is older than
    ///    `forget_host_after`, then, if still over `max_retained_hosts`, evict oldest
    ///    `last_seen_in_roster` first. Series and evaluations go with them.
    /// 4. **Evaluation reconciliation.** The set is exactly
    ///    `{(host, rule) : rule.enabled && rule.selector.matches(host) && host retained &&
    ///    validate_rule(rule).is_ok()}`. Missing ones are created at `NoData`; others dropped;
    ///    any whose stored `fingerprint != rule_fingerprint(rule)` is replaced with a fresh one,
    ///    so an edited threshold or dwell cannot inherit an old `since`. This is also why core's
    ///    quirk — `observe` returns `Ok`, not `NoData`, for a disabled or out-of-scope rule, so
    ///    an unjudged host would read healthy — never reaches the view.
    /// 5. **Series.** Per retained host and planned metric, switch on `observation.coverage`:
    ///    `Fresh` replaces the stored series with `normalize`d ones and records `resets`,
    ///    availability `Present`, `fetched_at = observed_at`; `Empty` stores nothing, availability
    ///    `NotReported` or `Unsupported` per `expected_on(kind, host.os)`; `Unavailable` leaves
    ///    the stored series and `fetched_at` untouched and sets availability `Unavailable`.
    /// 6. **Alerts.** Per reconciled `(host, rule)`: on `Unavailable`, `observe` is NOT called,
    ///    `frozen_since` is set if unset, and the evidence is `Frozen`. Otherwise
    ///    `reduce(normalize(...), reduction_for(rule.comparator))` — or an empty `MetricSeries`
    ///    for `Empty` — is passed to `eval.observe(rule, &candidate, observed_at,
    ///    tuning.max_staleness())` on the SAME evaluation value as last poll, so dwell
    ///    accumulates. Every `from != to` emits an `AlertTransition`.
    /// 7. **Bookkeeping.** `last_roster_at` advances on a listed roster; `last_clean_at` only on
    ///    `observation.is_clean()`; `consecutive_failures` counts consecutive observations
    ///    carrying any failure; `last_failure = worst_failure(...)`; `polled_this_session` is set
    ///    by the FIRST FOLD, whatever its outcome — a relaunch whose first poll 401s must
    ///    read `Unusable { failure: Unauthorized }` with an Open Settings button, not `Idle`
    ///    hiding it. `advice` falls out, and
    ///    `advised_next_poll = Some(observed_at + advice.delay().unwrap_or(Duration::zero()))` —
    ///    `Stop` carries no delay, and leaving `advised_next_poll` unset would disable the
    ///    blind-gap reset exactly when it is most needed: a key fixed three hours later would
    ///    fire every frozen `Pending` off a three-hour-old `since` on the first manual poll.
    #[must_use] pub fn apply(&self, config: &FleetConfig, observation: &PollObservation)
        -> FleetUpdate;

    /// A pure projection. `FleetUpdate` carries one already built, so the pair cannot disagree.
    #[must_use] pub fn view(&self, config: &FleetConfig) -> FleetView;
    #[must_use] pub fn advice(&self, config: &FleetConfig) -> PollAdvice;

    /// Drops a host with its series and evaluations — an operator decommissioning a machine.
    #[must_use] pub fn forget_host(&self, host: &pessimal_core::HostId) -> Self;

    #[must_use] pub fn backend_name(&self) -> &str;
    #[must_use] pub fn as_of(&self) -> Option<chrono::DateTime<chrono::Utc>>;
    #[must_use] pub fn last_clean_at(&self) -> Option<chrono::DateTime<chrono::Utc>>;
    #[must_use] pub fn consecutive_failures(&self) -> u32;
    #[must_use] pub fn known_hosts(&self) -> Vec<pessimal_core::HostId>;
    #[must_use] pub fn evaluation(
        &self,
        rule_id: &pessimal_core::Urn,
        host: &pessimal_core::HostId,
    ) -> Option<pessimal_core::AlertState>;

    /// # Errors
    /// [`ClientError::Backend`] if serialisation fails.
    pub fn to_json(&self) -> Result<String>;

    /// Cold start from a cache. NEVER returns `Err` for a bad cache — a monitoring app must not
    /// refuse to launch because its state is a schema behind; what could not be salvaged is
    /// reported. `polled_this_session` is `#[serde(skip)]` and so comes back `false`, which is
    /// what makes the restored view read `Idle` rather than `Fresh`. Stale dwell is NOT handled
    /// here: `advised_next_poll` survives the round trip, so the blind-gap rule in `apply`
    /// step 1 covers relaunch with the same mechanism and the same test. `Idle` is cleared by
    /// the first fold whatever its outcome, so a relaunch whose first poll fails shows the
    /// failure rather than hiding it behind a cache age.
    #[must_use] pub fn restore(
        backend_name: &str,
        json: &str,
    ) -> (Self, RestoreReport);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct RestoreReport {
    /// True when the cache's `schema_version` did not match and everything was discarded.
    pub discarded_incompatible: bool,
    /// True when the cache was not valid JSON.
    pub discarded_unreadable: bool,
    pub hosts_restored: u32,
    pub evaluations_restored: u32,
    pub saved_as_of: Option<chrono::DateTime<chrono::Utc>>,
}
```

```rust
/// **Internal.** `pessimal_ffi` destructures it: the state goes back into the session's mutex,
/// the rest crosses the boundary as `PollResult`.
#[derive(Debug, Clone, PartialEq)]
pub struct FleetUpdate {
    pub state: FleetState,
    pub view: FleetView,
    /// Only where `from != to`, sorted by `(host, rule_id)`.
    pub transitions: Vec<AlertTransition>,
    pub advice: PollAdvice,
}

/// **Exported.**
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AlertTransition {
    pub rule_id: pessimal_core::Urn,
    pub rule_name: String,
    pub host: pessimal_core::HostId,
    /// `AlertPhase`, not `pessimal_core::AlertState`: the payload `DateTime` travels beside
    /// it, for the same reason `AlertView` carries a phase.
    pub from: AlertPhase,
    pub to: AlertPhase,
    pub breaching_since: Option<chrono::DateTime<chrono::Utc>>,
    pub at: chrono::DateTime<chrono::Utc>,
}

impl AlertTransition {
    /// `to` is `Firing` and `from` is not. The edge worth a notification.
    #[must_use] pub fn is_fire(&self) -> bool;
    /// `from` was `Firing` and `to == AlertState::Ok` — nothing looser. A blind-gap reset drives
    /// a `Firing` evaluation to `NoData` and then to `Pending`; if `is_resolve` matched anything
    /// but `Ok`, every app resume would emit a spurious resolve for every still-breaching alert
    /// and re-fire one dwell later.
    #[must_use] pub fn is_resolve(&self) -> bool;
}

/// **Exported.**
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PollAdvice {
    /// A clean poll, or a partial failure (roster listed, some series failed) — a broken single
    /// metric must never slow liveness to a five-minute cadence, so only a failed roster backs
    /// off. Any state that has not folded this session, restored or brand new, advises
    /// `after = zero` so neither a relaunch nor a cold start sleeps an interval before showing
    /// data.
    Poll { after: chrono::Duration },
    /// The roster failed. `after = tuning.backoff_after(consecutive_failures)`.
    Retry { after: chrono::Duration, consecutive_failures: u32 },
    /// Retrying cannot help. Emitted whenever ANY failure this poll — roster or a single series
    /// query — carried `PollFailureKind::Unauthorized`: one expired key does not fail
    /// selectively, and burning battery on a problem only the user can fix is wrong.
    Stop { reason: PollFailureKind, message: String },
}

impl PollAdvice {
    #[must_use] pub fn delay(&self) -> Option<chrono::Duration>;
    #[must_use] pub fn should_poll(&self) -> bool;
}
```

### 4.7 `view.rs`

**Exported**, all of it. `pessimal_ffi` is a mechanical `From` mapping onto Records and nothing
else: every type is concrete (no generics — UniFFI cannot carry them), every collection is a sorted
`Vec`, every field is owned, every list element carries a stable `String` id.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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

/// False only for `LoadAverage1m`/`5m`/`15m` on [`pessimal_core::OsFamily::Windows`].
#[must_use] pub fn expected_on(
    kind: pessimal_core::MetricKind,
    os: pessimal_core::OsFamily,
) -> bool;
/// False for `AgentHeartbeat`: a silent host is `Liveness`'s business, never an alert's.
#[must_use] pub fn alertable(kind: pessimal_core::MetricKind) -> bool;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MetricFacts {
    pub kind: pessimal_core::MetricKind,
    pub display_name: String,
    pub unit: pessimal_core::MetricUnit,
    /// True when values are a derived rate or delta rather than a raw reading. Label the axis
    /// from this and from `kind` — never from the unit string: `MetricUnit::Ratio` and
    /// `MetricUnit::Load` both report OTLP unit `"1"`.
    pub is_rate: bool,
    pub alertable: bool,
}

#[must_use] pub fn describe_metric(kind: pessimal_core::MetricKind) -> MetricFacts;
#[must_use] pub fn metric_facts() -> Vec<MetricFacts>;
```

```rust
/// The single thing the UI colours by, computed once here so two apps cannot disagree.
/// Liveness maps `Alive -> Ok`, `Stale -> Warning`, `Down -> Critical`, `Unknown -> Unknown`;
/// alerts map `Ok -> Ok`, `Pending -> Warning`, `Firing -> Critical`, `NoData -> Unknown`;
/// a host is the max of its liveness severity and its worst alert severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub enum Severity { Ok, Unknown, Warning, Critical }

/// Everything `freshness_at` needs, carried on the view so the UI can recompute as time passes.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FreshnessInputs {
    pub as_of: Option<chrono::DateTime<chrono::Utc>>,
    /// Last poll with a listed roster and no failure at all.
    pub last_clean_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Last poll with a listed roster, whatever the series did. `Unusable` keys off this, not
    /// off `last_clean_at`: one permanently broken metric — a naming mismatch on exactly one
    /// name — must not walk the whole banner to `Unusable` and leave it there.
    pub last_roster_at: Option<chrono::DateTime<chrono::Utc>>,
    pub consecutive_failures: u32,
    pub last_failure: Option<PollFailure>,
    /// False for a state restored from disk that has not yet folded a successful poll.
    pub polled_this_session: bool,
    pub tolerance: chrono::Duration,
    pub budget: chrono::Duration,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Freshness {
    Fresh { at: chrono::DateTime<chrono::Utc> },
    /// Restored from disk, not yet polled this session. A six-hour-old cache must never claim a
    /// live connection this process has not made.
    Idle { last_success: chrono::DateTime<chrono::Utc> },
    /// Real data, visibly older than it should be. The banner is mandatory; the values stay.
    Degraded {
        last_success: chrono::DateTime<chrono::Utc>,
        consecutive_failures: u32,
        failure: Option<PollFailure>,
    },
    Unusable {
        last_success: Option<chrono::DateTime<chrono::Utc>>,
        consecutive_failures: u32,
        failure: Option<PollFailure>,
    },
}

impl Freshness {
    #[must_use] pub fn is_fresh(&self) -> bool;
    #[must_use] pub fn last_success(&self) -> Option<chrono::DateTime<chrono::Utc>>;
    #[must_use] pub fn severity(&self) -> Severity;
}

/// Freshness is a FUNCTION OF `now`, never a field frozen at fold time — that is the only shape
/// that greys the screen when a poll hangs or Swift's timer stops. In order:
/// no `last_roster_at` -> `Unusable`; `!polled_this_session` -> `Idle`;
/// `now - last_roster_at > budget` -> `Unusable` (past the age at which we would call a *host*
/// down, the view is past the age at which it deserves to be believed);
/// `consecutive_failures == 0 && now - last_clean_at <= tolerance` -> `Fresh`;
/// otherwise -> `Degraded`.
#[must_use] pub fn freshness_at(
    inputs: &FreshnessInputs,
    now: chrono::DateTime<chrono::Utc>,
) -> Freshness;
```

```rust
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MetricView {
    /// `series_id(host, kind, attributes)`. Stable `ForEach` identity across mounts appearing
    /// and vanishing.
    pub id: String,
    pub kind: pessimal_core::MetricKind,
    pub unit: pessimal_core::MetricUnit,
    pub display_name: String,
    /// From `series_label`: `"/"`, `"en0 receive"`. `None` for host-wide metrics.
    pub label: Option<String>,
    pub attributes: std::collections::BTreeMap<String, String>,
    pub latest: Option<pessimal_core::MetricPoint>,
    pub points: Vec<pessimal_core::MetricPoint>,
    pub availability: MetricAvailability,
    pub is_rate: bool,
    /// `tuning.metric_step()`. A gap wider than this must be drawn as a break — never
    /// interpolated, never carried to zero.
    pub expected_step: chrono::Duration,
    /// Timestamps where a counter reset was detected and the point dropped. Draw a marker, not
    /// a spike.
    pub resets: Vec<chrono::DateTime<chrono::Utc>>,
    pub fetched_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Whether the agent is collecting, as opposed to merely beating. Derived from the newest delta
/// of `AgentCollectionFailures`: a failed collection re-exports the previous snapshot with fresh
/// timestamps, so the data itself shows a flat line rather than a gap and this is the only tell.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum CollectionHealth {
    Healthy,
    Failing { added: f64, over: chrono::Duration },
    /// Fewer than two samples, or the metric was not fetched. Unjudged, not a problem.
    Unknown,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct HostView {
    pub id: pessimal_core::HostId,
    pub os: pessimal_core::OsFamily,
    pub agent_version: Option<String>,
    pub last_heartbeat: Option<chrono::DateTime<chrono::Utc>>,
    pub liveness: pessimal_core::Liveness,
    /// When this verdict was computed. Frozen while the roster query is failing — the UI renders
    /// age relative to this, and never re-judges liveness itself.
    pub liveness_at: chrono::DateTime<chrono::Utc>,
    pub severity: Severity,
    pub collection: CollectionHealth,
    /// Sorted by `(kind, id)`, one entry per attribute set.
    pub metrics: Vec<MetricView>,
    pub firing_alerts: u32,
    pub pending_alerts: u32,
    /// False for a retained host absent from the last listed roster.
    pub in_current_roster: bool,
    /// True when the id is the agent's `"unknown-host"` fallback, which several unrelated
    /// machines can collide into. Flag it; nothing else can detect it.
    pub id_is_placeholder: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct FleetCounts {
    pub hosts: u32, pub alive: u32, pub stale: u32, pub down: u32, pub unknown: u32,
    pub firing_alerts: u32, pub pending_alerts: u32,
}

/// `AlertState` without its `DateTime` payload, which travels as sibling fields — UniFFI cannot
/// carry a `chrono` payload inside an enum variant without a custom type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum AlertPhase { Ok, Pending, Firing, NoData }

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum AlertEvidence {
    Fresh,
    /// The query for this metric failed; the state is held, not re-judged.
    Frozen { since: chrono::DateTime<chrono::Utc> },
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AlertView {
    /// `"{rule_urn}|{host}"`. Stable identity for a list that mixes rules and hosts.
    pub id: String,
    pub rule_id: pessimal_core::Urn,
    pub rule_name: String,
    /// `AlertRule::describe()`. Do not parse it.
    pub description: String,
    pub host: pessimal_core::HostId,
    pub metric: pessimal_core::MetricKind,
    pub comparator: pessimal_core::Comparator,
    pub threshold: f64,
    pub phase: AlertPhase,
    pub severity: Severity,
    /// When the BREACH started, not when the alert fired.
    pub breaching_since: Option<chrono::DateTime<chrono::Utc>>,
    /// `breaching_since + rule.for_duration()`; already in the past once `Firing`.
    pub fires_at: Option<chrono::DateTime<chrono::Utc>>,
    /// The reduced value the phase was judged on.
    pub latest_value: Option<f64>,
    /// Which mount or interface held it, from `dominant_at`.
    pub series_label: Option<String>,
    pub evidence: AlertEvidence,
    pub is_rate: bool,
    /// Set when the stored rule failed `validate_rule` — a NaN threshold makes every comparator
    /// return false and the rule silently never fires. Such a rule is SHOWN and NOT evaluated.
    pub invalid_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FleetView {
    pub backend_name: String,
    /// `None` until the first fold.
    pub as_of: Option<chrono::DateTime<chrono::Utc>>,
    /// Absolute instants only. Ages are presentation's business and freezing one at fold time
    /// would make it lie between polls.
    pub freshness: FreshnessInputs,
    pub severity: Severity,
    /// Sorted by `HostId`.
    pub hosts: Vec<HostView>,
    /// Sorted Firing, Pending, NoData, Ok, then by host, then by rule id string — `Urn` is not
    /// `Ord`. Invalid rules sort with `NoData`.
    pub alerts: Vec<AlertView>,
    pub counts: FleetCounts,
    /// From `FleetConfig::audit()`, carried so a settings screen cannot forget to ask.
    pub warnings: Vec<TuningWarning>,
}

impl FleetView {
    #[must_use] pub fn host(&self, id: &pessimal_core::HostId) -> Option<&HostView>;
    #[must_use] pub fn firing(&self) -> Vec<&AlertView>;
    #[must_use] pub fn degraded_hosts(&self) -> Vec<&HostView>;
    #[must_use] pub fn freshness_at(&self, now: chrono::DateTime<chrono::Utc>) -> Freshness;
}
```

### 4.8 `gather.rs`

**Internal** (`execute_plan`, `poll_once`); `BackendProbe` is **Exported**.

```rust
/// Issues `list_hosts(plan.host_range)` and every `QuerySpec`, concurrently via
/// `futures::future::join_all` — order-preserving, so the observation is deterministic and the
/// detail-after-overview ordering survives. Converts each `CoreError` to a `PollFailure` stamped
/// at `plan.now`. NEVER returns `Err`: a failure is a value, not a control-flow event. Contains
/// no decisions, which is why it is the only `async fn` and has almost nothing to test.
pub async fn execute_plan(
    query: &dyn pessimal_core::TelemetryQuery,
    plan: &PollPlan,
) -> PollObservation;

/// `plan_poll` -> `execute_plan` -> `FleetState::apply`. The whole poll, one clock read, supplied
/// by the caller.
///
/// # Errors
/// Only if planning failed; backend failures arrive inside the returned [`FleetUpdate`].
pub async fn poll_once(
    query: &dyn pessimal_core::TelemetryQuery,
    config: &FleetConfig,
    state: &FleetState,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<FleetUpdate>;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BackendProbe {
    pub backend_name: String,
    pub connected: bool,
    pub failure: Option<PollFailure>,
    pub hosts_seen: u32,
    /// Up to five ids, so the settings screen can show what it actually found.
    pub sample_hosts: Vec<pessimal_core::HostId>,
    pub window: pessimal_core::TimeRange,
}

impl BackendProbe {
    /// Connected AND at least one host seen.
    #[must_use] pub fn is_usable(&self) -> bool;
    #[must_use] pub fn message(&self) -> String;
}

/// Runs `check_connection`, then a short `list_hosts`. `check_connection` alone returns `Ok(())`
/// for any 2xx that parses — including a backend holding no Pessimal data at all — so it cannot
/// detect a metric-naming mismatch, the single most likely misconfiguration. "Connected, 0 hosts
/// seen in the last 3 minutes" is the only signal a settings screen can put in front of an
/// operator. Infallible: the outcome is the return value.
pub async fn probe_backend(
    query: &dyn pessimal_core::TelemetryQuery,
    tuning: &PollTuning,
    now: chrono::DateTime<chrono::Utc>,
) -> BackendProbe;
```

### 4.9 `rules.rs`

**Exported**, except `rule_fingerprint`. Rules reach the fold as a plain `Vec<AlertRule>` snapshot
inside `FleetConfig`, so the reducer never touches a port.

```rust
/// The only construction path the apps use. Wraps `AlertRule::new` (which mints a UUIDv7 URN as
/// `pessimal::<environment>::alerts::rule::<uuid>`) and applies the selector.
///
/// # Errors
/// [`ClientError::InvalidRule`] for an empty name, a non-finite threshold, a negative dwell, a
/// non-alertable metric (`AgentHeartbeat`), or `HostSelector::AnyOf(vec![])`, which matches
/// nothing and would silently never fire. [`ClientError::InvalidUrn`] if `environment` is empty
/// or contains `::`.
pub fn draft_rule(
    environment: &str,
    name: &str,
    metric: pessimal_core::MetricKind,
    comparator: pessimal_core::Comparator,
    threshold: f64,
    for_duration: chrono::Duration,
    selector: pessimal_core::HostSelector,
) -> Result<pessimal_core::AlertRule>;

/// Re-checks what `AlertRule::new` enforces, on a rule that came back from storage. The fields
/// are public and it derives `Deserialize`, so its invariants hold only at construction.
///
/// # Errors
/// [`ClientError::InvalidRule`] for an empty name, a non-finite threshold, a negative dwell, a
/// non-alertable metric, or an empty `AnyOf` selector.
pub fn validate_rule(rule: &pessimal_core::AlertRule) -> Result<()>;

/// **Internal.** A stable hash of `(metric, selector, comparator, threshold.to_bits(),
/// for_duration)`. Stored beside each evaluation; a change drops the dwell, so an edited rule
/// cannot fire instantly off an old `since`.
#[must_use] pub fn rule_fingerprint(rule: &pessimal_core::AlertRule) -> u64;

/// # Errors
/// Propagates the repository's error.
pub async fn load_rules(
    repo: &dyn pessimal_core::AlertRuleRepository,
) -> Result<Vec<pessimal_core::AlertRule>>;

/// Persists through the rule's own `Serialize`; NEVER reconstructs it with `AlertRule::new`,
/// which would mint a fresh `Uuid::now_v7()` and orphan every stored evaluation and every
/// `delete`/`get` key — a bug that presents as "my alerts reset every launch".
///
/// # Errors
/// Propagates the repository's error.
pub async fn save_rule(
    repo: &dyn pessimal_core::AlertRuleRepository,
    rule: &pessimal_core::AlertRule,
) -> Result<()>;

/// # Errors
/// [`ClientError::RuleNotFound`] if no rule has that id.
pub async fn delete_rule(
    repo: &dyn pessimal_core::AlertRuleRepository,
    id: &pessimal_core::Urn,
) -> Result<()>;

/// # Errors
/// [`ClientError::InvalidUrn`] unless `value` parses as a URN. `Urn` cannot cross FFI.
pub fn parse_rule_id(value: &str) -> Result<pessimal_core::Urn>;
```

`AlertRule::for_duration` and `id` are private with no setters and there is no `AlertRule::with_id`,
so **editing a rule's dwell is delete-then-create today**, which changes the URN and drops the
evaluation — safe, and consistent with the fingerprint rule. A `pessimal_core` follow-up adding
`AlertRule::with_for_duration` (returning `Result`, preserving the id) is the fix; it is a domain
change and is out of scope for M3.

### 4.10 `testing.rs` — `#[cfg(any(test, feature = "testing"))]`

**Internal.** Exported from the crate so `pessimal_ffi`'s tests reuse it rather than writing a
second copy of the same fake. There is no `FixedClock` and no clock double: `now` is a parameter.

```rust
pub struct ScriptedQuery { /* backend_name, scripted per-metric results, Mutex<Vec<QuerySpec>> */ }

impl ScriptedQuery {
    #[must_use] pub fn new(backend_name: impl Into<String>) -> Self;
    #[must_use] pub fn with_hosts(
        self, hosts: std::result::Result<Vec<pessimal_core::Host>, pessimal_core::CoreError>,
    ) -> Self;
    #[must_use] pub fn with_series(
        self,
        metric: pessimal_core::MetricKind,
        result: std::result::Result<Vec<pessimal_core::MetricSeries>, pessimal_core::CoreError>,
    ) -> Self;
    /// Every request received, in arrival order, so a test asserts the query plan directly.
    #[must_use] pub fn calls(&self) -> Vec<QuerySpec>;
}

#[async_trait::async_trait]
impl pessimal_core::TelemetryQuery for ScriptedQuery { /* four methods */ }

#[derive(Debug, Default)]
pub struct InMemoryRuleRepository { /* Mutex<BTreeMap<String, AlertRule>> */ }

impl InMemoryRuleRepository {
    #[must_use] pub fn new() -> Self;
    #[must_use] pub fn seeded(rules: Vec<pessimal_core::AlertRule>) -> Self;
    pub fn fail_next(&self, error: pessimal_core::CoreError);
    /// Round-trips every held rule through serde, proving persistence preserves the `Urn`.
    #[must_use] pub fn round_tripped(&self) -> Vec<pessimal_core::AlertRule>;
}

#[async_trait::async_trait]
impl pessimal_core::AlertRuleRepository for InMemoryRuleRepository { /* four methods */ }

#[must_use] pub fn series(
    host: &str,
    kind: pessimal_core::MetricKind,
    samples: &[(chrono::DateTime<chrono::Utc>, f64)],
) -> pessimal_core::MetricSeries;

/// `count` points ending at `end`, spaced `step` apart, all `value` — "five 30-second buckets of
/// 95 % CPU ending now" in one line.
#[must_use] pub fn steady(
    host: &str,
    kind: pessimal_core::MetricKind,
    end: chrono::DateTime<chrono::Utc>,
    step: chrono::Duration,
    count: usize,
    value: f64,
) -> pessimal_core::MetricSeries;
```

### 4.11 What `pessimal_ffi` must wrap (M4, specified here so the boundary is not invented later)

```rust
#[derive(uniffi::Object)]
pub struct FleetSession {
    state: std::sync::Mutex<FleetState>,
    config: std::sync::Mutex<FleetConfig>,
    query: std::sync::Arc<dyn TelemetryQuery>,
    in_flight: tokio::sync::Mutex<()>,
}

#[uniffi::export(async_runtime = "tokio")]
impl FleetSession {
    #[uniffi::constructor] fn new(...) -> Result<Arc<Self>, FfiError>;
    /// `try_lock` on `in_flight` -> `PollResult { skipped: true, view, .. }` when a poll is
    /// already running: a menu-bar timer tick and a pull-to-refresh WILL collide, and two folds
    /// onto the same base state would silently discard one poll's HTTP. The guard is RAII —
    /// UniFFI has no cancellation, so Swift dropping its `Task` mid-poll drops the future, and a
    /// plain `AtomicBool` would wedge the poller forever.
    async fn poll(&self, now_millis: i64) -> Result<PollResult, FfiError>;
    fn view(&self) -> FleetViewRecord;
    fn freshness(&self, now_millis: i64) -> FreshnessRecord;   // recomputed per TimelineView tick
    fn set_config(&self, config: FleetConfigRecord) -> Result<Vec<TuningWarningRecord>, FfiError>;
    fn export_state(&self) -> Result<String, FfiError>;        // FleetState::to_json
    /// The constructor's `(FleetState, RestoreReport)` tuple cannot cross UniFFI; the session
    /// keeps the report and hands it over on request.
    fn restore_report(&self) -> Option<RestoreReportRecord>;
    fn forget_host(&self, host_id: String) -> Result<FleetViewRecord, FfiError>;
    async fn probe(&self, now_millis: i64) -> BackendProbeRecord;
}

pub struct PollResult { view: FleetViewRecord, transitions: Vec<AlertTransitionRecord>,
                        advice: PollAdviceRecord, skipped: bool }
```

Rules the FFI crate is held to:

- **The `std::sync::Mutex<FleetState>` is never held across the `poll_once` await.** Lock,
  clone the state and config, unlock, await, relock, store. `clippy::await_holding_lock` is under
  `all = deny`, so this is enforced, but the *store* half is not: `forget_host` and `set_config`
  must take the `in_flight` async mutex with `.lock().await` rather than `try_lock`, or a
  mid-poll forget is clobbered when the fold's result is written back.
- **`async_runtime = "tokio"` is required because `reqwest` needs a tokio reactor**, not because
  anything in `pessimal_client_core` does — this crate has no `tokio` dependency and its futures
  are executor-agnostic. Verify it on day one of M4 with an exported async fn that performs one
  real HTTP call driven from Swift; the sibling projects' `runtime.rs` exists because that path
  fails silently rather than erroring.
- **Every exported `async fn` returns `Result`.** A non-`Result` async export generates Swift
  `try!`, which crashes the app on a Rust panic. `FfiError::Internal { message }` is the only
  variant reachable from `poll`; real backend problems arrive inside the view.
- **Every `From` impl destructures the source exhaustively** — `let FleetView { backend_name,
  as_of, .. } = v;` with **no** `..` — so a new field is a compile error in the FFI crate rather
  than a silently dropped value.
- **`ClientError` is the only `uniffi::Error`; `PollFailure` is the only failure type in field
  position.** A type cannot be both in one crate, and mirroring the same enum twice is how the
  mirrors rot.
- **`SignozConfig::with_heartbeat_interval(tuning.heartbeat_interval())` and a
  `reqwest::Client` with timeouts via `SignozQuery::with_client` are wired in one constructor**,
  under a comment saying why: nothing fails if either line is deleted.
- **Record names share one flat Swift namespace.** Grep `clients/ffi/pessimal_ffi/src/` before
  naming anything `*Record`; a collision is a bindgen-time failure that looks like an unrelated
  Swift redeclaration error. Adding a variant to any mirrored enum breaks every Swift `switch` in
  the same commit — add a round-trip test per enum so that fails Rust CI, not the iOS build.

## 5. Behaviour under failure

The last good view is never discarded, never blanked, and never quietly re-judged. Failures reach
the fold as data, not as an error return.

| Failure mode | What the core does | What the UI shows |
|---|---|---|
| `list_hosts` fails (any kind) | Roster, every `Liveness`, every `liveness_at` and every stored series untouched. `consecutive_failures += 1`, `last_failure = worst_failure(..)`. `PollAdvice::Retry { after: backoff_after(n) }`. | Same host list, same verdicts, each with its frozen `liveness_at`. Banner from `freshness_at(now)`: `Degraded` past `staleness_tolerance()` (90 s), `Unusable` past `freshness_budget()` (150 s) — "Last updated 4 min ago — SigNoz unreachable". |
| One metric query fails, roster fine | `Coverage::Unavailable` for that `(host, metric)`. Stored series and `fetched_at` untouched, availability `Unavailable`. `observe` **not called**; `frozen_since` set; evidence `Frozen`. **`PollAdvice::Poll { after: poll_interval }`** — a broken single metric must not slow liveness to a five-minute cadence. | Exactly that tile greyed with its older timestamp; every other tile live. That alert row badged "held since 12:04". Liveness fresh. |
| One metric fails for hours (a naming mismatch on one name) | `last_roster_at` keeps advancing even though `last_clean_at` does not, and `Unusable` keys off the roster. | A permanent `Degraded` banner naming the broken metric — never a whole screen dimmed to `Unusable` because one of five requests is misconfigured. |
| A metric returns `Ok(vec![])` | `Coverage::Empty`. Nothing stored; availability `NotReported`, or `Unsupported` when `!expected_on(kind, os)`. `observe` **is** called with an empty series and legitimately clears dwell to `NoData`. | A gap, never a zero. Windows load average reads "not supported on this platform"; a Windows agent's filtered-out filesystem reads "not reported". |
| `Unauthorized` from **any** request, roster or a single series | `PollAdvice::Stop { reason: Unauthorized, message }`. One expired key does not fail selectively. Everything else behaves as the ordinary failure rows above. | Banner with an **Open Settings** button (routed by `PollFailureKind::is_actionable`), and Swift stops its timer. A manual refresh still polls, so a re-entered key recovers without a relaunch. |
| Several requests fail differently in one poll | `worst_failure` ranks `Unauthorized > Unreachable > Backend > Malformed`, tie-broken roster-before-series then by `MetricKind` order. | One stable message. Not whichever future finished first. |
| The poll hangs (no `reqwest` timeout wired) | Nothing folds, so nothing in the state moves. `freshness_at(now)` keeps advancing because it is a function of `now`. | The screen greys on schedule and then reads `Unusable`, even though no error ever arrived. The real fix is one layer down: `SignozQuery::with_client` with a `reqwest` timeout, wired by the FFI composition root. |
| App suspended, then resumed after a long gap | `observed_at > advised_next_poll + max_staleness` → every evaluation replaced with a fresh `NoData` **before** anything is observed. Transitions are emitted, but a still-breaching alert goes `Firing → Pending`, which is neither a fire nor a resolve. | No false page, no spurious resolve notification. Alerts re-accumulate dwell from now. |
| App killed, relaunched against a persisted state | `restore` returns the state with `polled_this_session == false`; `advised_next_poll` survives so the blind-gap rule handles the dwell. Advice is `Poll { after: 0 }`. | `Freshness::Idle { last_success }` — "as of 6 hours ago", never `Fresh`. The fleet draws instantly from cache; the first fold clears `Idle`. |
| Relaunch whose very first poll fails | `polled_this_session` is set by the first fold whatever its outcome, so `Idle` is cleared and the real failure surfaces. | `Unusable { failure: Unauthorized }` with an Open Settings button — not `Idle` hiding a dead key behind a cache age. |
| Persisted state is a schema behind, or corrupt | `restore` discards it and returns `FleetState::new` plus a `RestoreReport` saying so. Never `Err`. | An empty fleet and one cold start. The app does not refuse to launch. |
| A host goes silent past `host_window` and drops out of `list_hosts` | Retained with its old `last_heartbeat`; `LivenessPolicy::evaluate` reads `Down` because `host_window > down_threshold` by construction. Forgotten after `forget_host_after`, or when the `max_retained_hosts` cap evicts it, or on `forget_host`. | A positive **Down** row flagged "not in the last roster" — not a host that silently vanished at the moment the operator needed it. |
| A backend churns hostnames | The cap evicts the oldest `last_seen_in_roster` first, dropping series and evaluations with them. | A bounded list. No 24 h of ghosts each carrying an evaluation per rule. |
| A stored rule has a NaN threshold or an empty name | `validate_rule` fails on the fold's critical path: no evaluation is created and `AlertView.invalid_reason` is set. | The rule is shown, badged broken. Not a row that silently never fires. |
| A rule is edited (threshold, dwell, selector, metric) | `rule_fingerprint` differs, the evaluation is replaced at `NoData`. | Dwell restarts. A shortened `for_duration` cannot fire instantly off an old `since`. |
| A counter resets (agent restart, interface leaves the host-total sum) | The negative delta's point is dropped, never emitted as a negative or a spike; the timestamp is recorded in `MetricView.resets`. | A marked break in the line. A `NetworkIo` alert may miss a burst coinciding with a VPN drop — documented, not hidden. |
| An agent beats while its collection fails every cycle | The `AgentCollectionFailures` delta is positive → `CollectionHealth::Failing { added, over }`. | An `Alive` host badged "collector failing" — the only signal, since a failed collection re-exports the previous snapshot with fresh timestamps and looks like a flat line, not a gap. |
| `SignozConfig.heartbeat_interval` is not wired from `tuning.heartbeat_interval()` | **Nothing here can detect it.** `TelemetryQuery::list_hosts` takes no step; the adapter uses its own copy, seeded from `LivenessPolicy::default()` at construction. A coarse value inflates every measured heartbeat age by a bucket. | A healthy fleet reading `Stale` or `Down` with no error anywhere. Mitigation is a single FFI constructor that takes the interval once and feeds both, under a comment saying why. |
| The roster contains the agent's `"unknown-host"` fallback | Retained like any host; `HostView.id_is_placeholder = true`. Several machines can collide into one row and `HostSelector::matches` will apply a rule to all of them. | A warning badge on that row. Nothing else can detect it. |

## 6. Test plan

Roughly forty synchronous pure-function tests over literal values, plus four `#[tokio::test]`s in
`gather.rs` against `ScriptedQuery`. No network, no sleeping, no clock double.

**Config and interlocks (`config.rs`)**
- `tuning_rejects_a_max_staleness_below_three_steps`
- `tuning_rejects_a_max_staleness_above_the_down_threshold`
- `tuning_rejects_a_chart_window_shorter_than_staleness_plus_a_step`
- `tuning_rejects_a_liveness_policy_with_stale_at_or_past_down`
- `tuning_rejects_a_liveness_policy_with_fewer_than_three_stale_intervals`
- `tuning_rejects_a_staleness_tolerance_above_the_freshness_budget`
- `deserializing_an_invalid_tuning_is_rejected` — the `try_from` wire path, not just `new`
- `deserializing_a_config_with_a_colon_colon_environment_is_rejected`
- `derived_windows_are_not_settable_and_match_their_formulas`
- `backoff_doubles_then_caps_at_five_minutes`
- `planned_metrics_includes_a_rule_metric_nobody_charts`
- `spike_can_fire_warns_when_dwell_is_below_max_staleness`
- `counter_threshold_is_rate_warns_for_network_io`
- `selector_matches_nothing_warns_for_an_empty_any_of`

**Planning (`plan.rs`)**
- `an_unfocused_plan_is_one_query_per_planned_metric`
- `focus_adds_detail_queries_and_orders_them_last`
- `the_range_end_is_exactly_now`
- `the_host_window_outlives_the_down_threshold`
- `a_query_spec_round_trips_to_a_series_request`

**Observation (`observation.rs`)**
- `coverage_is_fresh_when_a_covering_request_returned_a_series`
- `coverage_is_empty_when_a_covering_request_returned_nothing`
- `coverage_is_unavailable_when_every_covering_request_failed`
- `coverage_is_unavailable_when_no_request_covered_the_pair`
- `series_for_drops_other_hosts_from_an_all_selector_response`
- `series_for_drops_a_series_whose_kind_disagrees_with_the_request`
- `series_for_prefers_the_last_matching_attribute_set` — detail beats overview
- `an_observation_round_trips_through_json` — record-and-replay is the fixture strategy

**Normalisation (`normalize.rs`)**
- `a_counter_reset_yields_no_negative_rate`
- `a_counter_reset_is_recorded_for_the_chart`
- `a_one_point_counter_series_normalizes_to_no_points`
- `two_mounts_reduce_to_the_higher_under_max_and_the_lower_under_min`
- `reduce_emits_ascending_points` — `latest_at` scans in reverse; load-bearing
- `reduce_of_an_empty_slice_is_none`
- `dominant_at_names_the_winning_mount`
- `series_id_is_stable_across_polls_and_unique_per_attribute_set`

**The fold (`fold.rs`)**
- `a_failed_roster_does_not_age_hosts_toward_down`
- `a_failed_series_query_does_not_clear_dwell`
- `an_empty_but_successful_series_clears_dwell_to_nodata`
- `dwell_accumulates_across_three_polls_to_firing`
- `a_failure_streak_under_backoff_does_not_reset_dwell`
- `an_unattended_gap_past_the_advised_poll_resets_dwell`
- `a_stop_followed_by_a_late_manual_poll_resets_dwell` — `Stop` carries no delay; the advised
  instant must still be armed
- `the_first_fold_never_triggers_the_blind_gap_reset`
- `blind_gap_reset_emits_no_resolve_transition`
- `is_resolve_requires_to_ok`
- `a_host_absent_from_the_roster_reads_down_not_unknown`
- `roster_merge_never_regresses_last_heartbeat`
- `roster_merge_does_not_clobber_os_to_other`
- `deleting_a_rule_drops_its_evaluations`
- `disabling_a_rule_drops_its_evaluation_rather_than_resetting_it_to_ok`
- `editing_a_rule_drops_its_dwell`
- `an_invalid_rule_shows_invalid_reason_and_is_not_evaluated`
- `forgetting_a_host_drops_its_series_and_evaluations`
- `retained_hosts_are_capped_evicting_oldest_roster_sighting`
- `a_partial_failure_keeps_the_roster_cadence`
- `unauthorized_advises_stop_not_retry`
- `unauthorized_on_a_series_query_alone_advises_stop`
- `worst_failure_is_independent_of_outcome_order`
- `a_restored_state_advises_polling_immediately`
- `restoring_an_incompatible_state_starts_fresh_and_reports_it`
- `applying_a_clean_observation_twice_is_idempotent` — only the clean case is: a failing
  observation increments `consecutive_failures`

**The view (`view.rs`)**
- `freshness_degrades_as_now_advances_without_a_fold`
- `a_permanently_broken_metric_reads_degraded_not_unusable`
- `a_relaunch_whose_first_poll_fails_reads_unusable_not_idle`
- `a_restored_state_reads_idle_not_fresh`
- `first_successful_fold_clears_idle`
- `windows_load_average_is_unsupported_not_a_gap`
- `windows_filesystem_is_not_reported_not_unsupported`
- `a_frozen_metric_keeps_its_older_fetched_at`
- `collection_health_is_failing_when_the_delta_is_positive`
- `host_severity_is_the_worse_of_liveness_and_alerts`
- `the_alert_sort_is_firing_first_and_stable`
- `views_carry_stable_ids` — every `MetricView.id` and `AlertView.id` unique and unchanged across
  two folds of the same data
- `fleet_counts_match_the_host_list`
- `a_counter_metric_view_is_flagged_is_rate`
- `unknown_host_in_roster_raises_a_warning`

**Rules (`rules.rs`)**
- `a_saved_rule_reloads_with_an_identical_id`
- `draft_rule_rejects_an_empty_name_a_nan_threshold_and_a_negative_dwell`
- `draft_rule_rejects_an_empty_any_of`
- `draft_rule_rejects_a_heartbeat_rule`
- `draft_rule_rejects_an_environment_containing_colon_colon`
- `delete_rule_on_an_unknown_id_is_rule_not_found`
- `rule_fingerprint_changes_with_every_judged_field`

**Errors (`error.rs`)**
- `every_core_error_variant_classifies` — exhaustive, so a new `CoreError` fails to compile
- `only_unauthorized_is_actionable`
- `a_poll_failure_round_trips_through_json`

**Gather (`gather.rs`, the only `#[tokio::test]`s)**
- `a_scripted_401_lands_as_a_poll_failure_not_a_returned_error`
- `a_five_query_plan_produces_five_outcomes_in_plan_order`
- `a_mixed_poll_produces_a_partial_observation`
- `probe_reports_usable_but_empty_distinctly_from_unauthorized`

## 7. Rejected alternatives

**Design 2 — "One Snapshot, Rust Owns the Schedule."** One `FleetMonitor` object; every call
(`tick`, `refresh`, `save_rule`, …) returns a complete, pre-formatted `FleetSnapshot`; Rust owns the
poll schedule and Swift sleeps until `next_poll_at_millis`; a `generation` counter drives
`@Observable`; rules persist through a two-method `RuleStore` blob port; ~26 view types are mirrored
by hand in `pessimal_ffi`.

It has the best app-facing contract of the three and the weakest correctness core. `FleetTracker::apply`
re-observes cached series at the advanced `report.at` on a partial failure, so within two or three
consecutive failures of one metric `now - point.at > max_staleness` and dwell clears on every rule for
that metric — while `freshness` still reads `Fresh` and the failure surfaces only as a `Notice`. It has
no state for "we could not fetch this", so a carried-over tile is indistinguishable from a fresh one at
the type level. `ConnectionStatus::Failed { kind: Unauthorized }` still goes through `BackoffPolicy`, so
a rejected API key is retried every five minutes forever. Its `generation` contract, documented as never
bumped by the passage of time, freezes the very fields its banner binds to. And it puts a presentation
layer in the domain crate — `format_bytes`, English `FleetHeadline.title`, slider bounds — for screens
that do not exist at M3.

**Taken from it:** the RAII in-flight guard (now in `FleetSession`), automatic dwell invalidation on a
rule edit (as a fingerprint, so there is nothing to call), the empty-`AnyOf` rejection, merging
`last_heartbeat` only when greater, the exhaustive-destructure rule for every FFI `From` impl,
`alertable: false` on `AgentHeartbeat`, "never key formatting on the unit string", and `Severity`.

**Design 3 — "Frozen Frame."** `RwLock<Arc<FleetView>>` swapped whole; `poll()` returns
`PollOutcome::{Complete, Partial, Failed, Skipped}` and never `Err`; readings for failed metrics are
carried over and flagged; one `AlertEvaluation` per `(rule, host, attribute-set)`; every request wrapped
in `tokio::time::timeout`; `PersistedState` embeds the whole view.

Its failure vocabulary is the sharpest of the three, and most of this design's failure handling is
its idea. But it carries the bug it exists to prevent twice over: `AlertEngine::observe_host` has no
notion of "this metric's query failed", so on a partial poll the missing series is indistinguishable
from genuine absence and dwell clears on the *first* failed poll; and `retain()` prunes by a live set
built from the attribute sets this poll returned, so a transient filesystem 500 deletes every
filesystem evaluation. Its evaluation key is an open label set the backend controls — a rotating
container id splits one logical series into many keys that each restart dwell. It has no blind-gap
guard on the suspend-and-resume path, only on process death. And it stakes its only hang defence on
`tokio::time::timeout`, which needs `async_runtime = "tokio"`, is unverified under Swift's executor in
this codebase family, and fails silently — for a problem that is a `reqwest::Client` timeout one layer
down. At the boundary, `restore -> Result<(Self, RestoreReport)>` returns a tuple and
`view() -> Arc<FleetView>` returns an `Arc` of a data struct; UniFFI can export neither.

**Taken from it:** staleness as a function of `now`, `Freshness::Idle` for a restored cache,
`worst_failure`'s deterministic ranking, "`Unauthorized` from any request promotes the whole poll",
`invalid_reason` on the alert row rather than in an optional audit, field-by-field host merging,
`CollectionHealth`, chart `resets` + `expected_step`, `RestoreReport`, and the default
`max_staleness = down_threshold()`.

## 8. Known limitations, stated rather than hidden

- **A rule cannot target a specific mount or interface.** `AlertRule` has no attribute selector, so
  a host's series reduce to one by comparator direction. "Disk above 90 % on any mount" works;
  "the root volume, ignore the Time Machine drive" is not expressible. Fixing it means adding an
  attribute filter to `pessimal_core::AlertRule` — a domain change, not a client one.
- **A threshold on a counter is compared against a rate and the rule text cannot say so.**
  `NetworkIo > 1_000_000` means a million bytes per second. `TuningWarning::CounterThresholdIsRate`
  and `MetricFacts.is_rate` are mitigations; the UI must label the field.
- **`SpikeCanFire` is a warning, not a fix.** `observe` sets `since = point.at` on a fresh breach, so
  any rule whose `for_duration` is under `max_staleness` can fire on one sample. Closing it properly
  means core taking a "breach must be seen in N samples" bound; the client cannot do it without
  reimplementing dwell.
- **Every poll refetches its whole window.** Affordable only because the fleet-wide window is about
  seven buckets and the full `chart_window` is spent on exactly one focused host. The upgrade path
  is an incremental fetch with a two-step overlap, deliberately later and with its own tests.
- **`apply(&self) -> FleetUpdate` clones the state every poll.** Kilobytes and microseconds at
  fleet scale, and it is what makes `assert_eq!(expected, update.state)` a legitimate test. Do not
  pre-emptively add an `apply_in_place`; a second write path is how the two diverge.
- **Liveness rests on an unverified assumption about SigNoz's gap behaviour.** `list_hosts` derives
  `last_heartbeat` from the newest non-partial bucket timestamp. If a SigNoz version gap-fills empty
  buckets or carries a cumulative sum forward, nothing is ever `Stale` or `Down` and the feature
  silently does nothing while looking healthy. Validate by stopping one agent and watching it go
  Down; the adapter has still never run against a live instance.
- **Frozen data misleads if the UI ignores freshness.** Showing a host as `Alive` while the backend
  has been unreachable for four minutes is defensible only because a banner says so. That coupling
  belongs in the shared SwiftUI component, not in convention.

<!-- Written to: /Users/thomas/Projects/Banade-a-Bonnot/pessimal/docs/plans/2026-09-07-m3-client-core.md -->
