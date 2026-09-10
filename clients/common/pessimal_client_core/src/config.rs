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
//!
//! `max_staleness` is the part of that budget this machine is responsible for. The bound alerts are
//! actually gated at is [`PollTuning::evidence_horizon`], which adds
//! [`PollTuning::backend_lag_allowance`] — the part the *backend* is responsible for — for the same
//! reason liveness is judged as of `now - backend_lag_allowance`. Every interlock here is written
//! against the raw fields, and the `# Errors` list on [`PollTuning::new`] records why each of them
//! still says what it meant once the allowance is in play.

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

/// The ingestion lag the preset budgets for: three minutes.
///
/// Deliberately loose. Measured against SigNoz Cloud, the newest *queryable* heartbeat was 88
/// seconds behind wall clock while the agent was still exporting every five seconds; three minutes
/// is about twice that, and about 1.5 times that plus a whole 30-second bucket. A tight fit around
/// the measurement would turn one slow afternoon at the backend into a fleet-wide false alarm,
/// while the only cost of being generous is that a host which really dies is called `Down` three
/// minutes later than the liveness policy alone would call it — and no allowance can beat the
/// backend to a fact it has not ingested yet.
const PRESET_BACKEND_LAG_ALLOWANCE_MINUTES: i64 = 3;

/// The absolute ceiling every duration in a [`PollTuning`] must satisfy: one year.
///
/// The bound exists to keep `now ± window` from overflowing, not to police UX. Every other
/// interlock on [`PollTuning`] is *relative* — each duration bounded against another duration —
/// and relative bounds are satisfied all the way up: a chart window of ten trillion seconds sits
/// comfortably above `max_staleness + metric_step` and is a perfectly legal `Duration`, because
/// `Duration` spans roughly a thousand times more than `DateTime<Utc>` can represent. The whole
/// band between the two passes every relative check and is fatal the moment
/// [`pessimal_core::TimeRange::ending_at`] subtracts it from `now`. That path now errors rather
/// than panicking, but an error surfaced as a failed poll on every cycle is still a broken app;
/// this is the layer that says no while the value is still a setting rather than a query.
///
/// A year is generous rather than tuned — the preset's longest duration is `forget_host_after` at
/// 24 hours — and it leaves every derived window (at most about four times the largest field, now
/// that all three of the roster, detail and overview windows carry the lag allowance)
/// five orders of magnitude below the overflow threshold.
pub const MAX_TUNING_DURATION: Duration = Duration::days(365);

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
    backend_lag_allowance: Duration,
    forget_host_after: Duration,
    max_retained_hosts: u32,
}

impl PollTuning {
    /// # Errors
    /// [`ClientError::InvalidTuning`] unless every interlock holds:
    /// `poll_interval > 0`; `metric_step >= 1s` (the adapter truncates with
    /// `num_seconds().max(1)`); `3 * metric_step <= max_staleness` (the freshest complete SigNoz
    /// bucket is already `[step, 2*step)` old before poll latency, and a zero lag allowance —
    /// correct for a loopback collector — leaves this the whole of
    /// [`PollTuning::evidence_horizon`]);
    /// `max_staleness <= liveness.down_threshold()` (an alert must not outlive liveness; both
    /// sides gain the lag allowance in use, so the comparison holds on the effective bounds too);
    /// `chart_window >= max_staleness + metric_step` (which makes `detail_window` outreach the
    /// evidence horizon by a step, since both of those carry the allowance as well);
    /// `backend_lag_allowance >= 0` (it is a delay the backend imposes, never a head start);
    /// `forget_host_after >= liveness.down_threshold()`;
    /// `staleness_tolerance() <= freshness_budget()` (so the freshness ladder cannot invert);
    /// `max_retained_hosts >= 1`; every duration, the policy's derived thresholds included, at or
    /// below [`MAX_TUNING_DURATION`]; and, on the embedded policy itself,
    /// `liveness.heartbeat_interval() > 0`,
    /// `liveness.stale_threshold() < liveness.down_threshold()`, and
    /// `liveness.stale_threshold() >= 3 * liveness.heartbeat_interval()`, which is the only way
    /// to check the documented `stale_after_intervals >= 3` given the multipliers are private.
    #[expect(
        clippy::too_many_arguments,
        reason = "eight interlocked durations, and the interlocks are the point: a builder with \
                  per-field setters would let a caller hold a half-valid tuning, which is exactly \
                  what the one fallible constructor exists to prevent"
    )]
    pub fn new(
        liveness: LivenessPolicy,
        poll_interval: Duration,
        metric_step: Duration,
        chart_window: Duration,
        max_staleness: Duration,
        backend_lag_allowance: Duration,
        forget_host_after: Duration,
        max_retained_hosts: u32,
    ) -> Result<Self> {
        let tuning = Self {
            liveness,
            poll_interval,
            metric_step,
            chart_window,
            max_staleness,
            backend_lag_allowance,
            forget_host_after,
            max_retained_hosts,
        };
        tuning.check()?;
        Ok(tuning)
    }

    /// The preset that satisfies every interlock: `poll_interval` and `metric_step` =
    /// `heartbeat_interval()`, `max_staleness` = `down_threshold()`, `chart_window` = 1 hour,
    /// `backend_lag_allowance` = 3 minutes, `forget_host_after` = 24 hours,
    /// `max_retained_hosts` = 256.
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
            backend_lag_allowance: Duration::minutes(PRESET_BACKEND_LAG_ALLOWANCE_MINUTES),
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

    /// How far behind wall clock the backend's newest *queryable* point is assumed to be.
    ///
    /// A measured property of the read path, not a preference. With an agent exporting every five
    /// seconds and still running, the newest queryable heartbeat on SigNoz Cloud was 88 seconds
    /// behind wall clock: part of that is bucket quantisation — a bucket is stamped at its start —
    /// and most of it is the backend's own ingestion-to-queryable delay. Every columnar metrics
    /// backend batches on write, so every one of them has some version of this number. `now` is
    /// simply not an instant a poll can observe, and the two places that used to assume it was
    /// both reported a healthy fleet as a broken one.
    ///
    /// **This is the one tuning value an operator may genuinely need to raise**, because it
    /// describes their backend rather than their preference. Too low shows up two ways, both of
    /// them a lie about the fleet: hosts that are beating perfectly read `Stale` and drift toward
    /// `Down` (their newest heartbeat is older than the policy's threshold purely because of the
    /// lag), or the roster comes back empty and the fleet reads as *absent* rather than as late
    /// (`list_hosts` asks for a window the newest data has not landed in yet). Either symptom,
    /// fleet-wide and all at once rather than host by host, means this value is below the lag the
    /// backend actually has.
    ///
    /// Raising it costs detection latency and nothing else: a host that really dies is called
    /// `Down` this much later than [`LivenessPolicy`] alone would call it. Zero is correct only
    /// for a collector on loopback, which has effectively no lag — and which is precisely why
    /// neither failure was visible before the first poll of a real backend.
    #[must_use]
    pub fn backend_lag_allowance(&self) -> Duration {
        self.backend_lag_allowance
    }

    #[must_use]
    pub fn forget_host_after(&self) -> Duration {
        self.forget_host_after
    }

    #[must_use]
    pub fn max_retained_hosts(&self) -> u32 {
        self.max_retained_hosts
    }

    /// `down_threshold() + 2 * heartbeat_interval() + backend_lag_allowance()` — a Down host must
    /// still be listed, and the newest data the backend holds must sit comfortably inside the
    /// window rather than at its edge.
    ///
    /// Derived, not supplied, so it cannot be set shorter than the age at which liveness would
    /// call a host down; that combination makes a silent host vanish from the roster at exactly
    /// the moment an operator needs to see it.
    ///
    /// The lag term answers a second, sharper failure. The window ends at `now`, so without it the
    /// whole span is *behind* the newest queryable bucket by most of the backend's ingestion lag,
    /// and the newest heartbeat sits at the far edge — intermittently outside. `list_hosts` then
    /// returns nothing at all and the fleet reads as absent rather than as late, which is the one
    /// reading worse than a stale one. Observed directly against SigNoz Cloud: a 30-minute window
    /// found the host, the derived window found nothing, over identical data.
    #[must_use]
    pub fn host_window(&self) -> Duration {
        summed(
            summed(
                self.liveness.down_threshold(),
                scaled(self.liveness.heartbeat_interval(), 2),
            ),
            self.backend_lag_allowance,
        )
    }

    /// `chart_window() + backend_lag_allowance()` — the span a focused host's detail queries ask
    /// for.
    ///
    /// [`PollTuning::chart_window`] is how much history the chart should *show*; this is what has
    /// to be requested to get it. The request ends at `now`, and the most recent
    /// `backend_lag_allowance` of that span is still somewhere inside the backend, so asking for
    /// exactly the charted span yields a chart that is short by the lag at the live end — the end
    /// an operator is actually looking at.
    #[must_use]
    pub fn detail_window(&self) -> Duration {
        summed(self.chart_window, self.backend_lag_allowance)
    }

    /// `max_staleness() + backend_lag_allowance()` — how old the newest sample may be before an
    /// alert stops counting it as evidence and reports
    /// [`pessimal_core::AlertState::NoData`].
    ///
    /// Identically: `max_staleness` measured from `now - backend_lag_allowance()`, the same
    /// shifted instant liveness is judged as of. `now - point.at <= max_staleness + lag` and
    /// `(now - lag) - point.at <= max_staleness` are the same inequality, and the second is the
    /// one that explains it. The gate's job is to reject samples *nobody is refreshing*, and
    /// "nobody is refreshing it" can only be measured from the latest instant the backend could
    /// have answered for — which is not `now`. Measured against SigNoz Cloud the newest queryable
    /// sample was 118 seconds old (88s of ingestion lag plus a 30-second bucket stamped at its
    /// start) on a host exporting every five seconds, so a gate at `max_staleness` alone left
    /// thirty seconds of margin and a backend 160 seconds behind put every alert on a perfectly
    /// healthy fleet at `NoData` while [`PollTuning::backend_lag_allowance`] kept liveness reading
    /// `Alive`. A fleet that looks healthy and has silently stopped alerting is worse than one that
    /// looks stale.
    ///
    /// This is expressed as a *horizon* rather than by handing `AlertEvaluation::observe` the
    /// shifted instant, because that one argument does three jobs: it bounds the gate, it selects
    /// the sample (`MetricSeries::latest_at` discards everything newer than it), and it anchors
    /// dwell (`now - since >= for_duration`). Shifting it would throw away the freshest sample —
    /// the very one this exists to admit — judge on one a whole allowance older, and delay every
    /// fire by the allowance; on a loopback collector, where the lag is genuinely zero and the
    /// allowance is not, it would read `NoData` off perfect data. Only the gate wants the shift.
    ///
    /// Widening it costs detection latency on a dead agent and nothing else, exactly as the
    /// allowance costs liveness a late `Down`: an alert on a host that has gone silent holds its
    /// last verdict for this long instead of `max_staleness`. The two now expire together — see
    /// the `max_staleness <= down_threshold` interlock, which is unchanged because both of its
    /// sides gain the same allowance.
    #[must_use]
    pub fn evidence_horizon(&self) -> Duration {
        summed(self.max_staleness, self.backend_lag_allowance)
    }

    /// `evidence_horizon() + 2 * metric_step()` — about thirteen buckets at defaults.
    ///
    /// The horizon rather than `max_staleness` alone, so the *gate* is what bounds how old a
    /// judged sample may be and the window is merely wide enough to contain it. A window narrower
    /// than the horizon silently becomes the real bound, and it is a bound that differs per tier:
    /// the focused host's [`PollTuning::detail_window`] is an hour wide, so two hosts with
    /// identical data would reach different verdicts depending on which one the user happened to
    /// have open. It also has to outreach the lag for the same reason
    /// [`PollTuning::host_window`] does — a window ending at `now` whose span is only what the
    /// fleet logically needs sits mostly *behind* the newest queryable bucket, and at the edge
    /// that bucket intermittently falls outside it.
    ///
    /// The extra history this fetches is exactly what the gate then refuses, which is the point:
    /// `observe` seeds `breaching_since` from a point's own timestamp, so a window that reaches
    /// past the horizon would otherwise let a sample nobody is refreshing seed dwell and fire a
    /// breach that had already ended. The gate binds first, in both tiers.
    #[must_use]
    pub fn overview_window(&self) -> Duration {
        summed(self.evidence_horizon(), scaled(self.metric_step, 2))
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

    /// The absolute bound on every duration, checked before anything relative.
    fn check_ceilings(&self) -> Result<()> {
        // The absolute ceiling before anything else, because every interlock below derives a sum,
        // a multiple, or a threshold from these fields, and none of those mean anything once an
        // operand has run away. `Duration::MAX` — what `scaled` and `summed` settle on — is
        // roughly 1100 times what `DateTime<Utc>` can hold, and a `LivenessPolicy` threshold can
        // report a product larger still, so a value arriving at the relative checks already
        // enormous would sail through them and take the panic downstream instead. The relative
        // interlocks are all satisfied *upwards*: not one of them can notice a duration that is
        // merely vast. The policy's derived thresholds are listed explicitly for that reason —
        // the interval alone does not bound them, since the multipliers are private `u32`s.
        for (field, value) in [
            ("heartbeat interval", self.liveness.heartbeat_interval()),
            ("liveness stale threshold", self.liveness.stale_threshold()),
            ("liveness down threshold", self.liveness.down_threshold()),
            ("poll interval", self.poll_interval),
            ("metric step", self.metric_step),
            ("chart window", self.chart_window),
            ("max staleness", self.max_staleness),
            ("backend lag allowance", self.backend_lag_allowance),
            ("forget-host-after", self.forget_host_after),
        ] {
            if value > MAX_TUNING_DURATION {
                return Err(ClientError::InvalidTuning(format!(
                    "{field} {}s exceeds the maximum of {}s, past which a window subtracted from \
                     the present overflows the representable range of a timestamp",
                    value.num_seconds(),
                    MAX_TUNING_DURATION.num_seconds()
                )));
            }
        }
        Ok(())
    }

    /// The embedded policy, checked before the fields measured against its thresholds.
    fn check_liveness(&self) -> Result<()> {
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
        Ok(())
    }

    /// Every interlock, in one place so `new` and the wire type cannot drift apart.
    ///
    /// Three functions rather than one only because the list no longer fits on a screen; `check`
    /// stays the single entry point, and the order is load-bearing — the ceilings before anything
    /// derived from a runaway operand, the embedded policy before the fields measured against its
    /// thresholds.
    fn check(&self) -> Result<()> {
        self.check_ceilings()?;
        self.check_liveness()?;

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
        // Both of the next two bound `max_staleness`, which is the *non-lag* part of the evidence
        // budget: what the fold adds to it is `backend_lag_allowance`, and both of these interlocks
        // survive that addition for a different reason.
        //
        // The floor stays necessary. `backend_lag_allowance` may legitimately be zero — that is the
        // right value for a collector on loopback — and then `evidence_horizon()` is exactly
        // `max_staleness`, so this is the whole of what keeps a successful poll of a healthy host
        // from reading NoData on bucket quantisation alone.
        let three_steps = scaled(self.metric_step, 3);
        if self.max_staleness < three_steps {
            return Err(ClientError::InvalidTuning(format!(
                "max staleness {}s must be at least three metric steps ({}s), or `observe` \
                 returns NoData on a successful poll",
                self.max_staleness.num_seconds(),
                three_steps.num_seconds()
            )));
        }
        // The ceiling stays *correct*, unchanged, because the allowance lands on both sides of it
        // and cancels. What it protects is the ordering of two effective bounds: alert evidence
        // expires at `max_staleness + allowance` after a host's last sample, and liveness calls
        // that host Down at `down_threshold + allowance` after its last heartbeat. Comparing the
        // raw fields compares those two, and an alert that outlived liveness would keep firing on a
        // host the app had already given up on. At the preset the two are equal, so evidence runs
        // out on the same second the verdict turns Down.
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
        if self.backend_lag_allowance < Duration::zero() {
            return Err(ClientError::InvalidTuning(format!(
                "backend lag allowance {}s must not be negative; it is how far behind wall clock \
                 the backend's newest queryable point lags, and a negative one would judge \
                 liveness against an instant not even the clock has reached while narrowing the \
                 very windows it exists to widen",
                self.backend_lag_allowance.num_seconds()
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
    backend_lag_allowance: Duration,
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
            wire.backend_lag_allowance,
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
            backend_lag_allowance,
            forget_host_after,
            max_retained_hosts,
        } = tuning;
        Self {
            liveness,
            poll_interval,
            metric_step,
            chart_window,
            max_staleness,
            backend_lag_allowance,
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
            // The evidence horizon, not `max_staleness`: "old but still valid" means "admitted by
            // the gate", and the gate budgets for the backend's ingestion lag. A dwell between the
            // two is every bit as spike-fireable as one below `max_staleness` — one sample that old
            // seeds `since` that far back and arrives already past the dwell — so a warning
            // thresholded on the narrower bound would go quiet over exactly the band the lag
            // allowance opened up.
            if rule.for_duration() < self.tuning.evidence_horizon() {
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
    /// `rule.for_duration() < tuning.evidence_horizon()`: because `observe` sets `since = point.at`
    /// on a fresh breach, one sample that is old-but-still-valid carries the rule straight to
    /// `Firing` with no further evidence. The horizon is the oldest a sample can be and still be
    /// judged, so it is the longest dwell one sample can satisfy by itself.
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

    /// A rule whose dwell comfortably exceeds the default evidence horizon (330s), so it
    /// contributes no `SpikeCanFire` noise to an audit that is testing something else.
    fn quiet_rule(name: &str, metric: MetricKind) -> AlertRule {
        AlertRule::new(
            "prod",
            name,
            metric,
            Comparator::GreaterThan,
            0.9,
            secs(600),
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
            preset.backend_lag_allowance(),
            preset.forget_host_after(),
            preset.max_retained_hosts(),
        )
        .expect("the preset must pass the constructor that guards it");

        assert_eq!(revalidated, preset);
        assert_eq!(
            preset.backend_lag_allowance(),
            secs(180),
            "the preset budgets three minutes of ingestion lag: about twice the 88 seconds \
             measured against SigNoz Cloud, because a tight fit around the measurement turns one \
             slow afternoon at the backend into a fleet-wide false alarm"
        );
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
            secs(180),
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
            secs(180),
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
            secs(180),
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
            secs(180),
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
            secs(180),
            hours(24),
            256,
        );

        assert!(matches!(rejected, Err(ClientError::InvalidTuning(_))));
    }

    #[test]
    fn tuning_rejects_a_duration_beyond_what_a_timestamp_can_represent() {
        // Ten trillion seconds is a legal `Duration` and satisfies every *relative* interlock —
        // it is far above `max_staleness + metric_step` — but `DateTime<Utc>` reaches back only
        // about 8.3e12 seconds, so `TimeRange::ending_at` used to panic on it inside `plan_poll`.
        let beyond = secs(10_000_000_000_000);
        let rejected = PollTuning::new(
            liveness(),
            secs(30),
            secs(30),
            beyond,
            secs(150),
            secs(180),
            hours(24),
            256,
        );
        assert!(matches!(rejected, Err(ClientError::InvalidTuning(_))));

        // The ceiling is a guard, not a preference: the boundary itself is accepted, and so is
        // every duration the preset uses.
        let at_the_ceiling = PollTuning::new(
            liveness(),
            secs(30),
            secs(30),
            MAX_TUNING_DURATION,
            secs(150),
            MAX_TUNING_DURATION,
            MAX_TUNING_DURATION,
            256,
        );
        assert!(at_the_ceiling.is_ok());
    }

    #[test]
    fn tuning_rejects_a_liveness_policy_whose_thresholds_saturate() {
        // `LivenessPolicy::new` checks only positivity and ordering, so this policy is legal in
        // core; its `down_threshold()` saturates to `Duration::MAX`, which is ~1100x what a
        // timestamp can hold. The ceiling has to look at the derived thresholds, not just at the
        // interval it can see.
        let absurd = LivenessPolicy::new(hours(1), 3, u32::MAX).expect("core accepts this policy");

        let rejected = PollTuning::new(
            absurd,
            secs(30),
            secs(30),
            hours(1),
            secs(150),
            secs(180),
            hours(24),
            256,
        );

        assert!(matches!(rejected, Err(ClientError::InvalidTuning(_))));
    }

    #[test]
    fn deserializing_an_overlong_duration_is_rejected() {
        // The documented threat model: chrono's `TimeDelta` serde impl is a `[secs, nanos]` pair
        // bounded only by `TimeDelta`'s own range, so a synced or hand-edited cache can carry a
        // window no timestamp can subtract.
        let mut wire = serde_json::to_value(PollTuning::default()).expect("tuning serialises");
        wire["chart_window"] =
            serde_json::to_value(secs(10_000_000_000_000)).expect("duration serialises");

        let rejected: std::result::Result<PollTuning, _> = serde_json::from_value(wire);

        let error = rejected.expect_err("serde must not bypass the constructor");
        assert!(
            error.to_string().contains("invalid poll tuning"),
            "expected our own ceiling to fire, got: {error}"
        );
    }

    #[test]
    fn deserializing_a_negative_backend_lag_allowance_is_rejected() {
        // chrono encodes a `Duration` as a signed `[secs, nanos]` pair, so a synced or hand-edited
        // cache can carry a *negative* allowance: one that would judge liveness against an instant
        // in the future and shorten every window it widens. The wire type is the only door into
        // `PollTuning`, and it revalidates rather than trusting what it was handed.
        let mut wire = serde_json::to_value(PollTuning::default()).expect("tuning serialises");
        wire["backend_lag_allowance"] =
            serde_json::to_value(secs(-1)).expect("duration serialises");

        let rejected: std::result::Result<PollTuning, _> = serde_json::from_value(wire);

        let error = rejected.expect_err("serde must not bypass the constructor");
        assert!(
            error.to_string().contains("invalid poll tuning"),
            "expected our own interlock to fire, got: {error}"
        );
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

        assert_eq!(tuning.host_window(), secs(390)); // down 150 + 2 * 30 + lag 180
        assert_eq!(tuning.detail_window(), secs(3780)); // chart 3600 + lag 180
        assert_eq!(tuning.evidence_horizon(), secs(330)); // staleness 150 + lag 180
        assert_eq!(tuning.overview_window(), secs(390)); // horizon 330 + 2 * 30
        assert_eq!(tuning.staleness_tolerance(), secs(90)); // 2 * 30 + 30
        assert_eq!(tuning.freshness_budget(), secs(150)); // the down threshold itself
        assert_eq!(
            tuning.overview_window().num_seconds() / tuning.metric_step().num_seconds(),
            13,
            "the fleet-wide window is about thirteen buckets"
        );

        // Move one input and only the windows derived from it move.
        let finer = PollTuning::new(
            liveness(),
            secs(30),
            secs(10),
            hours(1),
            secs(150),
            secs(180),
            hours(24),
            256,
        )
        .expect("valid tuning");

        assert_eq!(finer.overview_window(), secs(350)); // horizon 330 + 2 * 10
        assert_eq!(finer.staleness_tolerance(), secs(70)); // 2 * 30 + 10
        assert_eq!(finer.host_window(), secs(390)); // liveness- and lag-derived, unchanged

        // The lag allowance moves every window that has to outreach it — the roster's, the focused
        // host's, and the fleet-wide one the evidence gate is measured inside — and nothing else: a
        // local collector has no lag, and a tuning that says so plans the narrow windows the
        // earlier analysis assumed.
        let local = PollTuning::new(
            liveness(),
            secs(30),
            secs(30),
            hours(1),
            secs(150),
            Duration::zero(),
            hours(24),
            256,
        )
        .expect("zero lag is a valid allowance, and the right one for a loopback collector");

        assert_eq!(local.host_window(), secs(210)); // down 150 + 2 * 30, no lag term
        assert_eq!(local.detail_window(), hours(1)); // exactly the charted span
        assert_eq!(local.overview_window(), secs(210)); // staleness 150 + 2 * 30, no lag term
        assert_eq!(
            local.evidence_horizon(),
            local.max_staleness(),
            "with no lag to budget for, the gate collapses to `max_staleness` itself — which is \
             why `3 * metric_step <= max_staleness` is still the floor that keeps a successful \
             poll from reading NoData"
        );
    }

    /// The gate, not the window, has to be what bounds a judged sample's age.
    ///
    /// If the fleet-wide window were narrower than the horizon it would silently become the real
    /// bound, and a per-tier one: the focused host's `detail_window` is an hour, so two hosts with
    /// identical data would reach different alert verdicts depending on which one the user had
    /// open. Derivation rather than a `check` rule, so it holds for every tuning that exists.
    #[test]
    fn both_query_tiers_reach_past_the_evidence_horizon_so_the_gate_is_what_binds() {
        let hostile = PollTuning::new(
            LivenessPolicy::new(secs(1), 3, 4).expect("valid policy"),
            secs(1),
            secs(1),
            secs(5),
            secs(3),
            hours(12),
            hours(24),
            1,
        )
        .expect("an allowance thousands of times the staleness bound is still a valid tuning");

        for tuning in [PollTuning::default(), hostile] {
            assert!(
                tuning.overview_window() > tuning.evidence_horizon(),
                "the fleet-wide window must outreach the horizon, or the gate never binds"
            );
            assert!(
                tuning.detail_window() > tuning.evidence_horizon(),
                "and so must the focused host's, or its verdicts differ from everyone else's"
            );
        }
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
    fn spike_can_fire_warns_when_dwell_is_below_the_evidence_horizon() {
        // 60s of dwell under a 330s evidence horizon: one old-but-valid sample fires it outright.
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

        // The same rule with a dwell past the horizon is quiet.
        let patient = quiet_rule("cpu sustained", MetricKind::CpuUtilization);
        assert!(config().with_rules(vec![patient]).audit().is_empty());

        // The band the lag allowance opened: a 300-second dwell clears `max_staleness` and is
        // still spike-fireable, because the gate now admits a sample up to 330 seconds old.
        let between = AlertRule::new(
            "prod",
            "cpu patient-ish",
            MetricKind::CpuUtilization,
            Comparator::GreaterThan,
            0.9,
            secs(300),
        )
        .expect("valid rule");
        assert!(
            config().tuning.max_staleness() < between.for_duration()
                && between.for_duration() < config().tuning.evidence_horizon(),
            "the fixture has to sit between the two bounds for this to mean anything"
        );
        assert!(
            matches!(
                config().with_rules(vec![between]).audit().as_slice(),
                [TuningWarning::SpikeCanFire { .. }]
            ),
            "a dwell the gate can satisfy from one sample must be warned about, whatever part of \
             the horizon that sample's age came from"
        );
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
