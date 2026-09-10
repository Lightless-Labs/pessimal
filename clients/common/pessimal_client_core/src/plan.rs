//! What to ask the backend for, decided from configuration and the clock alone.
//!
//! Planning is the first stage of plan-gather-fold and the only place a query window is chosen.
//! [`plan_poll`] is a function of `(&FleetConfig, now)` and deliberately *not* of accumulated
//! state: nothing here consults the last poll, the current roster, or what failed. That is what
//! makes a plan reproducible — the same config and the same instant produce the same requests,
//! byte for byte — and it is why a test asserts the query plan by typing the instant it wants.
//!
//! The shape is two tiers. Fleet-wide, one request per planned metric with
//! [`HostSelector::All`] over a short window: one request per metric per poll, never one per
//! host per metric, which is the difference between a poll a menu-bar app can afford every
//! thirty seconds and one it cannot. Then, only for the one host the user is looking at, a
//! request per detail metric over the long chart window. The focused host's metrics are fetched
//! twice, once in each tier, and that is intended: the detail specs are emitted last and later
//! outcomes win, so the long window replaces the short one with no merge step to get wrong.

use chrono::{DateTime, Duration, Utc};
use pessimal_core::{HostId, HostSelector, MetricKind, SeriesRequest, TimeRange};
use serde::{Deserialize, Serialize};

use crate::config::FleetConfig;
use crate::error::Result;

/// One query, as a value.
///
/// A serialisable twin of [`pessimal_core::SeriesRequest`], which derives no serde. Field-
/// identical, and it exists only so a whole poll — plan and observation together — can be written
/// to JSON and replayed as a fixture.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuerySpec {
    pub metric: MetricKind,
    pub selector: HostSelector,
    pub range: TimeRange,
    pub step: Duration,
}

impl QuerySpec {
    /// The port-level request this spec stands for.
    #[must_use]
    pub fn to_request(&self) -> SeriesRequest {
        // Destructuring `Self` and building `SeriesRequest` as a struct literal — rather than
        // calling `SeriesRequest::new` — is what makes a field added on *either* side a compile
        // error right here. Neither direction fails loudly on its own: a dropped field is a query
        // that quietly asks for something other than what was planned, and the data comes back
        // looking perfectly plausible.
        let Self {
            metric,
            selector,
            range,
            step,
        } = self;
        SeriesRequest {
            metric: *metric,
            selector: selector.clone(),
            range: *range,
            step: *step,
        }
    }

    /// Whether this spec's results could contain data for `host` and `metric`.
    ///
    /// Metric and selector only; the window and the step deliberately do not enter. "Covering"
    /// means *we asked*, which is what separates a successful request that returned nothing —
    /// real absence of data, and a legitimate reason to clear an alert's dwell — from a pair no
    /// request touched, where the honest answer is that we do not know. Gating on the range as
    /// well would report a pair the plan really did fetch as unfetched, and freeze alerts that
    /// have live evidence sitting right there.
    #[must_use]
    pub fn covers(&self, host: &HostId, metric: MetricKind) -> bool {
        self.metric == metric && self.selector.matches(host)
    }
}

/// Everything one poll will ask for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PollPlan {
    /// The single instant the whole poll is planned and later evaluated at. Supplied by the
    /// caller; nothing in this crate reads a clock.
    pub now: DateTime<Utc>,
    /// The window for `list_hosts`. Wider than the down threshold *plus* the backend's lag
    /// allowance, so a host we are about to call `Down` is still listed rather than silently gone,
    /// and the newest point the backend can answer with sits well inside the window instead of at
    /// its far edge — at the edge it intermittently falls outside, `list_hosts` returns nothing,
    /// and a late fleet reads as an absent one.
    pub host_range: TimeRange,
    /// Fleet-wide overview queries first, focused detail queries last.
    ///
    /// The order is load-bearing, not cosmetic: outcomes are consumed in plan order and later
    /// ones win, which is how the focused host's long detail window replaces its short overview
    /// window without a merge step.
    pub queries: Vec<QuerySpec>,
}

/// Plans one poll from configuration and the clock.
///
/// Fleet-wide: one spec per [`FleetConfig::planned_metrics`] with [`HostSelector::All`] over
/// `tuning.overview_window()`. Focus: when `config.focus` is `Some(host)`, one spec per
/// `config.detail_metrics` with [`HostSelector::Host`] over `tuning.detail_window()`, emitted
/// last. `host_range` is `tuning.host_window()` ending at `now`.
///
/// Every window ends at `now` and is widened at the *start*. All three carry
/// `tuning.backend_lag_allowance()` in their derivation, because a window ending at `now` whose
/// span is only what the fleet logically needs is mostly behind the newest queryable point:
/// directly for the roster window and the focused host's detail window, and through
/// `tuning.evidence_horizon()` for the fleet-wide overview window, which has to hold every sample
/// the alert gate is willing to judge — if it held fewer, the window rather than the gate would be
/// deciding which alerts have evidence, and it would decide differently for the one host the user
/// has open. Shortening a window's *end* instead would age every reading the fold then judges as
/// current.
///
/// **There is exactly one step in this crate**, and both tiers use it. The temptation is a
/// coarser `chart_step` for the long focused window, which would be cheaper and would look
/// identical on a chart. It must not be added casually: a coarse bucket boundary lands one step
/// early, `AlertEvaluation::observe` seeds `breaching_since` from the point's own timestamp, and
/// the rule fires a step before it should. Making that safe needs two caches — coarse series for
/// charts, fine series for evaluation — and an invariant that coarse ones never reach `observe`,
/// not a convention someone can forget.
///
/// # Errors
/// [`crate::error::ClientError::InvalidTimeRange`] if a window is not strictly positive, or if it
/// reaches back past the earliest instant a timestamp can represent. A
/// [`crate::config::PollTuning`] built by its constructor cannot cause either — the interlocks
/// force every duration positive, and [`crate::config::MAX_TUNING_DURATION`] holds every derived
/// window — at most about four times the largest field, now that two of them carry the lag
/// allowance as well — five orders of magnitude inside the representable range. That second half is
/// why the claim is safe to make: relative interlocks alone left a whole band of legal `Duration`
/// values that satisfied every one of them and still overflowed here.
pub fn plan_poll(config: &FleetConfig, now: DateTime<Utc>) -> Result<PollPlan> {
    let tuning = config.tuning;
    let step = tuning.metric_step();

    let host_range = TimeRange::ending_at(now, tuning.host_window())?;
    let overview_range = TimeRange::ending_at(now, tuning.overview_window())?;

    // Tier one: the whole fleet, one request per metric. `planned_metrics` is the overview set
    // unioned with every enabled rule's metric, so a rule on a metric nobody charts still gets
    // its data and cannot sit forever at `NoData`.
    let mut queries: Vec<QuerySpec> = config
        .planned_metrics()
        .into_iter()
        .map(|metric| QuerySpec {
            metric,
            selector: HostSelector::All,
            range: overview_range,
            step,
        })
        .collect();

    // Tier two: the one host the user has open, over the chart window. Emitted last so it wins.
    // The overlap with tier one is not deduplicated — that duplication *is* the mechanism.
    if let Some(host) = config.focus.as_ref() {
        let detail_range = TimeRange::ending_at(now, tuning.detail_window())?;
        queries.extend(config.detail_metrics.iter().map(|&metric| QuerySpec {
            metric,
            selector: HostSelector::Host(host.clone()),
            range: detail_range,
            step,
        }));
    }

    Ok(PollPlan {
        now,
        host_range,
        queries,
    })
}

#[cfg(test)]
mod tests {
    use pessimal_core::{AlertRule, Comparator};

    use super::*;
    use crate::config::PollTuning;

    fn at(offset_secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_757_000_000 + offset_secs, 0).expect("a valid fixture instant")
    }

    fn config() -> FleetConfig {
        FleetConfig::new("prod", PollTuning::default()).expect("a valid environment")
    }

    fn web1() -> HostId {
        HostId::new("web-1")
    }

    /// The lag measured against SigNoz Cloud: with the agent exporting every five seconds and
    /// still running, the newest *queryable* heartbeat was 88 seconds behind wall clock. Part of
    /// that is bucket quantisation, most of it the backend's ingestion-to-queryable delay.
    const MEASURED_BACKEND_LAG_SECONDS: i64 = 88;

    /// A rule on a metric the default overview set does not chart, so `planned_metrics()` is
    /// observably wider than `overview_metrics` rather than accidentally equal to it.
    fn uptime_rule() -> AlertRule {
        AlertRule::new(
            "prod",
            "uptime floor",
            MetricKind::SystemUptime,
            Comparator::LessThan,
            300.0,
            Duration::seconds(300),
        )
        .expect("a valid rule")
    }

    #[test]
    fn an_unfocused_plan_is_one_query_per_planned_metric() {
        let config = config().with_rules(vec![uptime_rule()]);
        let planned = config.planned_metrics();
        assert!(
            planned.contains(&MetricKind::SystemUptime),
            "a rule's metric is planned even though no overview tile charts it"
        );

        let plan = plan_poll(&config, at(0)).expect("the default tuning has positive windows");

        assert_eq!(
            plan.queries.iter().map(|q| q.metric).collect::<Vec<_>>(),
            planned,
            "one query per planned metric, in that order"
        );
        for query in &plan.queries {
            assert_eq!(
                query.selector,
                HostSelector::All,
                "the overview tier is one request per metric, never one per host"
            );
            assert_eq!(query.range.duration(), config.tuning.overview_window());
            assert_eq!(query.step, config.tuning.metric_step());
        }
    }

    #[test]
    fn focus_adds_detail_queries_and_orders_them_last() {
        let config = config().with_focus(Some(web1()));
        let overview_count = config.planned_metrics().len();

        let plan = plan_poll(&config, at(0)).expect("the default tuning has positive windows");

        assert_eq!(
            plan.queries.len(),
            overview_count + config.detail_metrics.len()
        );

        let (overview, detail) = plan.queries.split_at(overview_count);
        assert!(
            overview.iter().all(|q| q.selector == HostSelector::All),
            "the fleet-wide tier comes first"
        );
        assert_eq!(
            detail.iter().map(|q| q.metric).collect::<Vec<_>>(),
            config.detail_metrics,
            "the detail tier is the configured detail set, in order, emitted last"
        );
        for query in detail {
            assert_eq!(query.selector, HostSelector::Host(web1()));
            assert_eq!(
                query.range.duration(),
                config.tuning.detail_window(),
                "the long window is what the focused host earns, and it is why later outcomes \
                 win — widened by the lag allowance, because the window ends at `now` and the \
                 freshest part of it is still inside the backend"
            );
            assert_eq!(
                query.step,
                config.tuning.metric_step(),
                "one step in the crate: a coarser detail step would seed dwell a bucket early"
            );
        }
    }

    #[test]
    fn the_range_end_is_exactly_now() {
        let now = at(0);
        let config = config().with_focus(Some(web1()));

        let plan = plan_poll(&config, now).expect("the default tuning has positive windows");

        assert_eq!(plan.now, now);
        assert_eq!(plan.host_range.end(), now);
        for query in &plan.queries {
            assert_eq!(
                query.range.end(),
                now,
                "every window ends at the poll instant; a lag margin here would silently age \
                 every reading the fold then judges as current"
            );
        }
    }

    #[test]
    fn the_host_window_outlives_the_down_threshold() {
        let config = config();

        let plan = plan_poll(&config, at(0)).expect("the default tuning has positive windows");

        assert!(
            plan.host_range.duration()
                > config.tuning.liveness().down_threshold() + config.tuning.backend_lag_allowance(),
            "a host silent long enough to be called Down must still be listed, or it vanishes \
             from the roster at exactly the moment an operator needs to see it — and liveness is \
             judged as of `now - backend_lag_allowance`, so the window has to reach a whole down \
             threshold back from *that* instant rather than from `now`"
        );
    }

    /// Measured against SigNoz Cloud, not reasoned about: a 30-minute window found the host and
    /// the derived window found nothing, over identical data. A window ending at `now` and only as
    /// wide as the down threshold is mostly *behind* the newest queryable bucket, which then sits
    /// at its far edge and intermittently outside it — `list_hosts` returns nothing and the fleet
    /// reads as absent rather than as late.
    #[test]
    fn the_newest_data_the_backend_can_answer_with_sits_well_inside_every_window() {
        let now = at(0);
        let config = config().with_focus(Some(web1()));
        let newest_queryable = now - Duration::seconds(MEASURED_BACKEND_LAG_SECONDS);

        let plan = plan_poll(&config, now).expect("the default tuning has positive windows");

        assert!(
            plan.host_range.contains(newest_queryable),
            "the freshest heartbeat the backend can return must be inside the roster window"
        );
        assert!(
            newest_queryable - plan.host_range.start() > config.tuning.liveness().down_threshold(),
            "inside is not enough: a full down threshold of history has to sit behind the newest \
             queryable point, or a host about to be called Down has already dropped out of the \
             roster"
        );

        let overview = plan.queries.first().expect("a plan has a fleet-wide tier");
        assert_eq!(overview.range.duration(), config.tuning.overview_window());
        assert!(
            overview.range.contains(newest_queryable),
            "and inside the fleet-wide window too, or every alert on a healthy host reads NoData \
             for want of a sample the backend was holding the whole time"
        );
        assert!(
            overview.range.duration() > config.tuning.evidence_horizon(),
            "the window has to hold every sample the evidence gate would admit. Narrower, and the \
             window rather than the gate decides which alerts have evidence — and it decides \
             differently for the focused host, whose detail window is an hour wide"
        );

        let detail = plan
            .queries
            .last()
            .expect("a focused plan ends with a detail query");
        assert_eq!(detail.range.duration(), config.tuning.detail_window());
        assert!(
            detail.range.duration()
                >= config.tuning.chart_window() + Duration::seconds(MEASURED_BACKEND_LAG_SECONDS),
            "a chart asked for exactly its own span is short by the lag at the live end — the end \
             an operator is looking at"
        );
    }

    #[test]
    fn a_query_spec_round_trips_to_a_series_request() {
        let range = TimeRange::ending_at(at(0), Duration::seconds(210)).expect("positive window");
        let spec = QuerySpec {
            metric: MetricKind::NetworkIo,
            selector: HostSelector::Host(web1()),
            range,
            step: Duration::seconds(30),
        };

        assert_eq!(
            spec.to_request(),
            SeriesRequest::new(
                MetricKind::NetworkIo,
                HostSelector::Host(web1()),
                range,
                Duration::seconds(30),
            )
        );
    }

    #[test]
    fn covers_asks_only_about_the_metric_and_the_selector() {
        let long = TimeRange::ending_at(at(0), Duration::hours(1)).expect("positive window");
        let short = TimeRange::ending_at(at(0), Duration::seconds(210)).expect("positive window");
        let spec = |range| QuerySpec {
            metric: MetricKind::CpuUtilization,
            selector: HostSelector::Host(web1()),
            range,
            step: Duration::seconds(30),
        };

        assert!(spec(long).covers(&web1(), MetricKind::CpuUtilization));
        assert!(
            spec(short).covers(&web1(), MetricKind::CpuUtilization),
            "the window is not part of coverage: a spec covers what it asked for, whatever its \
             span, or the observation would report a fetched pair as unavailable"
        );
        assert!(!spec(long).covers(&web1(), MetricKind::MemoryUtilization));
        assert!(!spec(long).covers(&HostId::new("db-1"), MetricKind::CpuUtilization));
    }
}
