//! The only `async` code in the crate: issue the plan, record what came back.
//!
//! Gather is the middle stage of plan-gather-fold and deliberately the thinnest. It makes no
//! decisions at all — it does not know what a `Liveness` is, what dwell means, or which failure a
//! user should be shown — which is why everything else in this crate is a synchronous pure
//! function over literal values. Confining `async` to one module is what keeps the rest of the
//! codebase testable without a runtime, a mock port, or a clock double.
//!
//! [`execute_plan`] **never returns `Err`**. A failed request is a [`PollFailure`] inside the
//! returned [`PollObservation`], because that is the only shape in which the fold can tell "we
//! could not see" apart from "your fleet is down". An `Err` here would collapse the two: the
//! caller would have nothing to fold, so either the last good picture is discarded or the failure
//! is invisible. Both are the bug this crate exists to prevent, so the type system forbids the
//! shape that causes it.
//!
//! Nothing here reads a clock. `plan.now` is the single instant every failure is stamped at and
//! the instant the fold evaluates the whole poll at, so an observation is a self-contained
//! fixture: record one in production, replay it in a test as a literal.

use chrono::{DateTime, Duration, Utc};
use futures::future::{join, join_all};
use pessimal_core::{HostId, TelemetryQuery, TimeRange};
use serde::{Deserialize, Serialize};

use crate::config::{FleetConfig, PollTuning};
use crate::error::{FailureSource, PollFailure, Result};
use crate::fold::{FleetState, FleetUpdate};
use crate::observation::{PollObservation, RosterOutcome, SeriesResult};
use crate::plan::{PollPlan, plan_poll};

/// How many host ids [`probe_backend`] carries back for display. Enough to recognise a fleet,
/// short enough that a thousand-host backend does not hand a settings screen a thousand rows.
const SAMPLE_HOST_LIMIT: usize = 5;

/// Issues every request in `plan` concurrently and records what each one answered.
///
/// `list_hosts(plan.host_range)` and every [`crate::plan::QuerySpec`] go out together through
/// `futures::future::join_all`, which resolves in *argument* order rather than completion order.
/// That is load-bearing twice over: the observation is byte-identical for a given plan no matter
/// which request was slowest, and series outcomes stay in plan order, which is what lets
/// `PollObservation::series_for` take the last matching attribute set and so lets the focused
/// host's long detail window replace its short overview window with no merge step.
///
/// Every `CoreError` becomes a [`PollFailure`] stamped at `plan.now` — never at a second clock
/// read, which would put a failure in the future relative to the poll it belongs to.
///
/// This function never returns `Err`, and that is a contract rather than an accident of the
/// current implementation. See the module docs.
pub async fn execute_plan(query: &dyn TelemetryQuery, plan: &PollPlan) -> PollObservation {
    let roster = async {
        match query.list_hosts(plan.host_range).await {
            Ok(hosts) => RosterOutcome::Listed(hosts),
            Err(error) => RosterOutcome::Failed(PollFailure::from_core(
                &error,
                FailureSource::Roster,
                plan.now,
            )),
        }
    };

    let series = join_all(plan.queries.iter().map(|spec| async move {
        let request = spec.to_request();
        match query.query_series(&request).await {
            Ok(returned) => SeriesResult::Returned(returned),
            Err(error) => SeriesResult::Failed(PollFailure::from_core(
                &error,
                FailureSource::Series {
                    metric: spec.metric,
                },
                plan.now,
            )),
        }
    }));

    let (hosts, results) = join(roster, series).await;

    let mut observation = PollObservation::new(plan.now).with_hosts(hosts);
    for (spec, result) in plan.queries.iter().zip(results) {
        observation = observation.with_series(spec.clone(), result);
    }
    observation
}

/// One whole poll: plan it, issue it, fold it.
///
/// `plan_poll` -> [`execute_plan`] -> [`FleetState::apply`], with exactly one clock read, supplied
/// by the caller. Concurrency control is not here: this is a free function over `&FleetState`, and
/// the session that owns the state is what serialises a timer tick against a pull-to-refresh.
///
/// # Errors
/// [`crate::error::ClientError::InvalidTimeRange`] if planning failed, which is the only error
/// path in the entire poll — a [`PollTuning`] built through its constructor cannot trigger it.
/// Every backend failure arrives as data inside the returned [`FleetUpdate`], so a caller that
/// handles this `Result` by showing an error banner has mistaken a programming mistake for an
/// outage.
pub async fn poll_once(
    query: &dyn TelemetryQuery,
    config: &FleetConfig,
    state: &FleetState,
    now: DateTime<Utc>,
) -> Result<FleetUpdate> {
    let plan = plan_poll(config, now)?;
    let observation = execute_plan(query, &plan).await;
    Ok(state.apply(config, &observation))
}

/// What a "test connection" actually learned.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BackendProbe {
    pub backend_name: String,
    /// `check_connection`'s verdict alone: the endpoint answered and the credentials worked.
    ///
    /// Kept separate from `failure` rather than derived from it, because `connected: true` beside
    /// `failure: Some(..)` is the reachable state this two-step probe exists to expose — the URL
    /// and key are fine and the heartbeat query itself errored, which points at a different
    /// repair from "cannot reach" or "bad key".
    pub connected: bool,
    pub failure: Option<PollFailure>,
    pub hosts_seen: u32,
    /// Up to five ids, so the settings screen can show what it actually found.
    ///
    /// Sorted rather than left in backend order: `list_hosts` promises no order, and an operator
    /// re-running the test should not watch five names shuffle.
    pub sample_hosts: Vec<HostId>,
    pub window: TimeRange,
}

impl BackendProbe {
    /// Connected AND at least one host seen.
    ///
    /// Both halves, because `check_connection` returning `Ok(())` proves only that something
    /// answered — see [`probe_backend`].
    #[must_use]
    pub fn is_usable(&self) -> bool {
        self.connected && self.hosts_seen > 0
    }

    /// One line for a settings screen.
    ///
    /// A failure's message is rendered as-is: it is already `CoreError`'s `Display` string and
    /// carries its own prefix ("not authorised for the telemetry backend"), so re-prefixing it
    /// would stutter.
    #[must_use]
    pub fn message(&self) -> String {
        let window = window_phrase(self.window.duration());
        let hosts = if self.hosts_seen == 1 {
            "host"
        } else {
            "hosts"
        };
        match (&self.failure, self.connected) {
            (Some(failure), false) => failure.message.clone(),
            // The state worth spelling out: the handshake passed and the listing did not.
            (Some(failure), true) => {
                format!(
                    "connected to {}, but listing hosts failed: {}",
                    self.backend_name, failure.message
                )
            }
            (None, _) if self.hosts_seen == 0 => format!(
                "connected to {}, but no hosts reported in the last {window}",
                self.backend_name
            ),
            (None, _) => format!(
                "connected to {}; {} {hosts} seen in the last {window}",
                self.backend_name, self.hosts_seen
            ),
        }
    }
}

/// Tests the backend the way a poll uses it, not the way a health check would.
///
/// `check_connection` alone returns `Ok(())` for any 2xx that parses — including a backend holding
/// no Pessimal data whatsoever — so on its own it cannot detect a metric-naming mismatch, which is
/// the single most likely misconfiguration and the one that otherwise presents as a permanently
/// empty fleet with no error anywhere. Following it with a real `list_hosts` over the real roster
/// window is what turns that into a sentence an operator can act on: "connected, but no hosts
/// reported in the last 210s".
///
/// The window is [`PollTuning::host_window`], the same one a poll's roster uses, so "0 hosts here"
/// means exactly what "an empty roster" would mean in a poll rather than something subtly
/// narrower.
///
/// Infallible: the outcome *is* the return value. A settings screen's job is to report a
/// misconfiguration, never to raise one.
pub async fn probe_backend(
    query: &dyn TelemetryQuery,
    tuning: &PollTuning,
    now: DateTime<Utc>,
) -> BackendProbe {
    let backend_name = query.backend_name().to_owned();
    let window = probe_window(now, tuning);

    // `FailureSource::Roster` for both calls. Neither is a series query, and the roster is what
    // this probe is ultimately asking about.
    if let Err(error) = query.check_connection().await {
        return BackendProbe {
            backend_name,
            connected: false,
            failure: Some(PollFailure::from_core(&error, FailureSource::Roster, now)),
            hosts_seen: 0,
            sample_hosts: Vec::new(),
            window,
        };
    }

    match query.list_hosts(window).await {
        Ok(hosts) => {
            let hosts_seen = u32::try_from(hosts.len()).unwrap_or(u32::MAX);
            let mut sample_hosts: Vec<HostId> = hosts.into_iter().map(|host| host.id).collect();
            sample_hosts.sort();
            sample_hosts.truncate(SAMPLE_HOST_LIMIT);
            BackendProbe {
                backend_name,
                connected: true,
                failure: None,
                hosts_seen,
                sample_hosts,
                window,
            }
        }
        Err(error) => BackendProbe {
            backend_name,
            connected: true,
            failure: Some(PollFailure::from_core(&error, FailureSource::Roster, now)),
            hosts_seen: 0,
            sample_hosts: Vec::new(),
            window,
        },
    }
}

/// The probe's `list_hosts` window.
///
/// Private, and it clamps instead of propagating, because [`probe_backend`] is infallible by
/// design. A [`PollTuning`] built through its constructor already forces every derived window
/// strictly positive, so the clamp is a guard against a future field rather than a case any
/// caller meets — and with the clamp in place `TimeRange::ending_at` cannot fail.
fn probe_window(now: DateTime<Utc>, tuning: &PollTuning) -> TimeRange {
    let length = tuning.host_window().max(Duration::seconds(1));
    TimeRange::ending_at(now, length).expect("a strictly positive window is a valid range")
}

/// Whole minutes only when the window divides evenly, otherwise seconds.
///
/// The default roster window is 210 seconds. Rendering that as "3 minutes" would understate the
/// window an operator is being told about by thirty seconds, in the one message whose entire
/// purpose is to be believed.
fn window_phrase(window: Duration) -> String {
    let seconds = window.num_seconds();
    if seconds >= 60 && seconds % 60 == 0 {
        format!("{} min", seconds / 60)
    } else {
        format!("{seconds}s")
    }
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, Duration, Utc};
    use pessimal_core::{CoreError, Host, HostId, MetricKind, MetricPoint, MetricSeries, OsFamily};

    use super::{BackendProbe, execute_plan, poll_once, probe_backend};
    use crate::config::{FleetConfig, PollTuning};
    use crate::error::{FailureSource, PollFailureKind, worst_failure};
    use crate::fold::{FleetState, PollAdvice};
    use crate::observation::{Coverage, RosterOutcome, SeriesResult};
    use crate::plan::plan_poll;
    use crate::testing::ScriptedQuery;

    fn at(offset_secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_757_000_000 + offset_secs, 0).expect("a valid fixture instant")
    }

    fn web1() -> HostId {
        HostId::new("web-1")
    }

    fn config() -> FleetConfig {
        FleetConfig::new("prod", PollTuning::default()).expect("a valid environment")
    }

    /// One point, so a returned series is distinguishable from an empty one *and* carries the
    /// metric it belongs to — which is what makes a mis-paired `join_all` detectable.
    fn sample(kind: MetricKind, value: f64) -> MetricSeries {
        MetricSeries::new(web1(), kind, vec![MetricPoint::new(at(0), value)])
    }

    /// Scripts every metric the default overview plan asks for, so no test depends on what
    /// `ScriptedQuery` does with a metric nobody scripted.
    fn scripted_everything(query: ScriptedQuery) -> ScriptedQuery {
        config()
            .planned_metrics()
            .into_iter()
            .fold(query, |query, metric| {
                query.with_series(metric, Ok(vec![sample(metric, 0.5)]))
            })
    }

    #[tokio::test]
    async fn a_scripted_401_lands_as_a_poll_failure_not_a_returned_error() {
        let config = config();
        let plan = plan_poll(&config, at(0)).expect("the default tuning has positive windows");
        let query = config.planned_metrics().into_iter().fold(
            ScriptedQuery::new("SigNoz").with_hosts(Err(CoreError::Unauthorized)),
            |query, metric| query.with_series(metric, Err(CoreError::Unauthorized)),
        );

        // The signature is the proof: there is no `Err` to unwrap here.
        let observation = execute_plan(&query, &plan).await;

        let RosterOutcome::Failed(roster) = &observation.hosts else {
            panic!("a scripted 401 must land as a recorded roster failure");
        };
        assert_eq!(roster.kind, PollFailureKind::Unauthorized);
        assert_eq!(roster.source, FailureSource::Roster);
        assert_eq!(
            roster.at, plan.now,
            "every failure is stamped at the poll's single instant, not at a second clock read"
        );

        let failures = observation.failures();
        assert_eq!(
            failures.len(),
            plan.queries.len() + 1,
            "one failure per query plus the roster"
        );
        assert!(
            failures
                .iter()
                .all(|failure| failure.kind == PollFailureKind::Unauthorized)
        );
        assert_eq!(
            worst_failure(&failures).map(|failure| failure.source),
            Some(FailureSource::Roster),
            "worst_failure ranks the roster ahead of the series it tied with"
        );

        // And the composition passes it on as data rather than as an error return.
        let update = poll_once(&query, &config, &FleetState::new("SigNoz"), at(0))
            .await
            .expect("planning is the only error path in a poll");
        assert!(
            matches!(
                update.advice,
                PollAdvice::Stop {
                    reason: PollFailureKind::Unauthorized,
                    ..
                }
            ),
            "an unauthorized poll advises Stop, not Retry: {:?}",
            update.advice
        );
    }

    #[tokio::test]
    async fn a_five_query_plan_produces_five_outcomes_in_plan_order() {
        let config = config();
        let plan = plan_poll(&config, at(0)).expect("the default tuning has positive windows");
        assert_eq!(
            plan.queries.len(),
            5,
            "the default overview set is five fleet-wide requests"
        );

        let query = scripted_everything(ScriptedQuery::new("SigNoz").with_hosts(Ok(vec![])));
        let observation = execute_plan(&query, &plan).await;

        assert_eq!(observation.series.len(), plan.queries.len());
        assert_eq!(
            observation
                .series
                .iter()
                .map(|outcome| outcome.spec.clone())
                .collect::<Vec<_>>(),
            plan.queries,
            "outcomes come back in plan order, not completion order"
        );
        // The pairing, not just the ordering: every outcome must carry the answer to its OWN
        // request. `join_all` resolves in argument order, and this is what notices if it ever
        // stops doing so.
        for outcome in &observation.series {
            let SeriesResult::Returned(returned) = &outcome.result else {
                panic!("every scripted metric returned data");
            };
            assert_eq!(
                returned.first().map(|series| series.kind),
                Some(outcome.spec.metric),
                "outcome for {:?} carries another metric's series",
                outcome.spec.metric
            );
        }
        assert_eq!(
            query.calls().len(),
            plan.queries.len(),
            "one request per spec, no retries and no extras"
        );
    }

    #[tokio::test]
    async fn a_mixed_poll_produces_a_partial_observation() {
        let config = config();
        let plan = plan_poll(&config, at(0)).expect("the default tuning has positive windows");
        let host = Host::new(web1(), OsFamily::Linux).with_last_heartbeat(at(0));

        // CPU answers, memory 500s, everything else answers emptily: one poll holding all three
        // coverage cases at once.
        let query = config
            .planned_metrics()
            .into_iter()
            .filter(|&metric| {
                metric != MetricKind::CpuUtilization && metric != MetricKind::MemoryUtilization
            })
            .fold(
                ScriptedQuery::new("SigNoz")
                    .with_hosts(Ok(vec![host.clone()]))
                    .with_series(
                        MetricKind::CpuUtilization,
                        Ok(vec![sample(MetricKind::CpuUtilization, 0.9)]),
                    )
                    .with_series(
                        MetricKind::MemoryUtilization,
                        Err(CoreError::Backend("query rejected".to_owned())),
                    ),
                |query, metric| query.with_series(metric, Ok(vec![])),
            );

        let observation = execute_plan(&query, &plan).await;

        assert!(
            !observation.is_clean(),
            "one failed series query makes the whole poll partial"
        );
        assert_eq!(
            observation.roster(),
            Some(&[host][..]),
            "a failed series query does not cost us the roster"
        );
        assert_eq!(
            observation.coverage(&web1(), MetricKind::CpuUtilization),
            Coverage::Fresh
        );
        assert_eq!(
            observation.coverage(&web1(), MetricKind::MemoryUtilization),
            Coverage::Unavailable,
            "a failed query is never mistaken for an absence of data"
        );
        assert_eq!(
            observation.coverage(&web1(), MetricKind::LoadAverage1m),
            Coverage::Empty
        );

        let failures = observation.failures();
        assert_eq!(failures.len(), 1, "exactly the one query that failed");
        assert_eq!(failures[0].kind, PollFailureKind::Backend);
        assert_eq!(
            failures[0].source,
            FailureSource::Series {
                metric: MetricKind::MemoryUtilization
            },
            "the source names the metric, so a banner can too"
        );
    }

    #[tokio::test]
    async fn probe_reports_usable_but_empty_distinctly_from_unauthorized() {
        let tuning = PollTuning::default();
        let now = at(0);

        let empty = probe_backend(
            &ScriptedQuery::new("SigNoz").with_hosts(Ok(vec![])),
            &tuning,
            now,
        )
        .await;
        let rejected = probe_backend(
            &ScriptedQuery::new("SigNoz").with_hosts(Err(CoreError::Unauthorized)),
            &tuning,
            now,
        )
        .await;
        let populated = probe_backend(
            &ScriptedQuery::new("SigNoz").with_hosts(Ok(vec![
                Host::new(HostId::new("web-2"), OsFamily::Linux),
                Host::new(web1(), OsFamily::Darwin),
            ])),
            &tuning,
            now,
        )
        .await;

        // Neither is usable, and that is precisely why they must not read the same: one needs a
        // credential, the other needs the metric names checked.
        assert!(!empty.is_usable());
        assert!(!rejected.is_usable());

        assert!(
            empty.connected,
            "a 2xx that parses is a connection, even over an empty backend"
        );
        assert_eq!(empty.failure, None);
        assert_eq!(empty.hosts_seen, 0);

        assert_eq!(
            rejected.failure.as_ref().map(|failure| failure.kind),
            Some(PollFailureKind::Unauthorized),
            "the kind is what routes an Open Settings button"
        );
        assert_ne!(
            empty.message(),
            rejected.message(),
            "the two unusable outcomes must not share one sentence"
        );

        assert!(populated.is_usable());
        assert_eq!(populated.hosts_seen, 2);
        assert_eq!(
            populated.sample_hosts,
            vec![web1(), HostId::new("web-2")],
            "the sample is sorted, so re-running the test does not shuffle it"
        );

        for probe in [&empty, &rejected, &populated] {
            assert_eq!(probe.backend_name, "SigNoz");
            assert_eq!(
                probe.window.end(),
                now,
                "the window ends at the supplied now"
            );
            assert_eq!(
                probe.window.duration(),
                tuning.host_window(),
                "the probe asks over the same window a poll's roster uses"
            );
        }
    }

    #[test]
    fn a_probe_round_trips_through_json() {
        let probe = BackendProbe {
            backend_name: "SigNoz".to_owned(),
            connected: true,
            failure: None,
            hosts_seen: 1,
            sample_hosts: vec![web1()],
            window: pessimal_core::TimeRange::ending_at(at(0), Duration::seconds(210))
                .expect("a positive window"),
        };

        let json = serde_json::to_string(&probe).expect("a probe serialises");
        assert_eq!(
            serde_json::from_str::<BackendProbe>(&json).expect("and deserialises"),
            probe
        );
    }
}
