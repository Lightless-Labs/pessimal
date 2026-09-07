#![cfg(any(test, feature = "testing"))]
//! Test doubles for the two ports, plus the two series builders every test fixture needs.
//!
//! Gated on `cfg(any(test, feature = "testing"))` so the fakes never reach a release build or the
//! published docs, and exported under the `testing` feature so `pessimal_ffi`'s tests reuse these
//! rather than growing a second copy of the same fake that drifts from this one.
//!
//! **There is no clock double here, and there must not be one.** `now` is a parameter on every
//! entry point in this crate — `plan_poll`, `poll_once`, `probe_backend`, `freshness_at` — so a
//! test says "it is now T+90s" by typing T+90s. A `Clock` trait would buy nothing: `set` needs
//! `&mut self` and cannot be advanced from behind an `Arc<dyn Clock>` anyway.
//!
//! Everything here is deliberately dumb. [`ScriptedQuery`] returns exactly what it was handed,
//! unfiltered by the request's selector or window, because the filtering is what
//! [`crate::observation::PollObservation::series_for`] is for — a double that filtered would hide
//! the bug a test is reaching for. The defaults are the boring ones: an unscripted roster is an
//! empty success, and so is an unscripted metric, which is the `Coverage::Empty` path rather than
//! the `Unavailable` one.

use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard, PoisonError};

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use pessimal_core::{
    AlertRule, AlertRuleRepository, CoreError, Host, HostId, MetricKind, MetricPoint, MetricSeries,
    SeriesRequest, TelemetryQuery, TimeRange, Urn,
};

use crate::plan::QuerySpec;

/// Takes a lock without a panic path.
///
/// A poisoned mutex means some *other* test already failed while holding it; turning that into a
/// second panic here cascades one red test into a suite-wide one and buries the original. It also
/// keeps every public method on these doubles free of a `# Panics` section it would have to earn.
fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A [`TelemetryQuery`] whose every answer is scripted up front.
///
/// One roster result, and one result per [`MetricKind`], so a test can say "the roster succeeds
/// but the CPU query fails" — the partial-failure case the whole freezing design exists for —
/// without a mock framework or a runtime. Every series request is recorded, so a test asserts the
/// query plan the gather stage actually issued rather than inferring it from the data that came
/// back.
#[derive(Debug)]
pub struct ScriptedQuery {
    backend_name: String,
    hosts: Result<Vec<Host>, CoreError>,
    series: BTreeMap<MetricKind, Result<Vec<MetricSeries>, CoreError>>,
    calls: Mutex<Vec<QuerySpec>>,
}

impl ScriptedQuery {
    /// A backend that answers every request with an empty success.
    #[must_use]
    pub fn new(backend_name: impl Into<String>) -> Self {
        Self {
            backend_name: backend_name.into(),
            // An empty roster rather than an error: the unscripted default has to be the
            // uninteresting case, so that a test which scripts only one metric is not silently
            // also testing a roster failure.
            hosts: Ok(Vec::new()),
            series: BTreeMap::new(),
            calls: Mutex::new(Vec::new()),
        }
    }

    /// Scripts `list_hosts`, and with it `check_connection`.
    ///
    /// The two share one script on purpose. The port gives `check_connection` no separate failure
    /// to carry, and the failure a probe has to distinguish — a rejected key from a reachable
    /// backend holding no Pessimal data — is expressed by the pair: `Err(Unauthorized)` here fails
    /// both calls, while `Ok(vec![])` is connected-but-empty.
    #[must_use]
    pub fn with_hosts(mut self, hosts: Result<Vec<Host>, CoreError>) -> Self {
        self.hosts = hosts;
        self
    }

    /// Scripts every `query_series` request for `metric`. Later calls replace earlier ones.
    ///
    /// Per metric rather than per request: the same script answers the fleet-wide overview query
    /// and the focused detail query for that metric, which is what lets one line set up a
    /// two-tier plan.
    #[must_use]
    pub fn with_series(
        mut self,
        metric: MetricKind,
        result: Result<Vec<MetricSeries>, CoreError>,
    ) -> Self {
        self.series.insert(metric, result);
        self
    }

    /// Every request received, in arrival order, so a test asserts the query plan directly.
    #[must_use]
    pub fn calls(&self) -> Vec<QuerySpec> {
        locked(&self.calls).clone()
    }

    /// Records one request as the serialisable twin the plan is written in.
    ///
    /// Destructured exhaustively, for the same reason [`QuerySpec::to_request`] is: a field added
    /// to `SeriesRequest` must be a compile error here, not a recorded call that quietly differs
    /// from the one issued and makes a plan assertion pass for the wrong reason.
    fn record(&self, request: &SeriesRequest) {
        let SeriesRequest {
            metric,
            selector,
            range,
            step,
        } = request;
        locked(&self.calls).push(QuerySpec {
            metric: *metric,
            selector: selector.clone(),
            range: *range,
            step: *step,
        });
    }
}

#[async_trait]
impl TelemetryQuery for ScriptedQuery {
    async fn list_hosts(&self, _range: TimeRange) -> pessimal_core::Result<Vec<Host>> {
        // The window is ignored rather than used to filter: a test that wants a host excluded
        // says so by not scripting it, which is legible in the fixture instead of hidden in here.
        self.hosts.clone()
    }

    async fn query_series(
        &self,
        request: &SeriesRequest,
    ) -> pessimal_core::Result<Vec<MetricSeries>> {
        self.record(request);
        self.series
            .get(&request.metric)
            .cloned()
            .unwrap_or_else(|| Ok(Vec::new()))
    }

    async fn check_connection(&self) -> pessimal_core::Result<()> {
        self.hosts.as_ref().map(|_| ()).map_err(Clone::clone)
    }

    fn backend_name(&self) -> &str {
        &self.backend_name
    }
}

/// An [`AlertRuleRepository`] holding rules in memory, keyed by `Urn::to_string()`.
///
/// The key is the string form because [`Urn`] is `Hash + Eq` but not `Ord`, and a `BTreeMap` is
/// what makes `list` deterministic — a repository whose order depended on a hash seed would make
/// every whole-`Vec` assertion flaky.
#[derive(Debug, Default)]
pub struct InMemoryRuleRepository {
    rules: Mutex<BTreeMap<String, AlertRule>>,
    next_error: Mutex<Option<CoreError>>,
}

impl InMemoryRuleRepository {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A repository already holding `rules`, as a relaunch against a populated store would.
    #[must_use]
    pub fn seeded(rules: Vec<AlertRule>) -> Self {
        let repo = Self::new();
        {
            let mut held = locked(&repo.rules);
            for rule in rules {
                held.insert(rule.id().to_string(), rule);
            }
        }
        repo
    }

    /// Arms one failure, returned by whichever port method runs next and then cleared.
    ///
    /// One call rather than a permanent mode: the cases worth testing are "this save failed, the
    /// reload after it did not", so a latch that had to be disarmed would be a second thing to
    /// forget.
    pub fn fail_next(&self, error: CoreError) {
        *locked(&self.next_error) = Some(error);
    }

    /// Round-trips every held rule through serde, proving persistence preserves the [`Urn`].
    ///
    /// # Panics
    /// If a held rule cannot survive its own serialisation — a NaN threshold reached by assigning
    /// to the public field, say. That is the assertion this method exists to make, so failing it
    /// loudly is the point; the port methods, which must not panic, report it as a failure value
    /// instead.
    #[must_use]
    pub fn round_tripped(&self) -> Vec<AlertRule> {
        locked(&self.rules)
            .values()
            .map(|rule| round_trip(rule).expect("a held rule must survive a serde round trip"))
            .collect()
    }

    fn take_error(&self) -> Option<CoreError> {
        locked(&self.next_error).take()
    }
}

/// Sends one rule through serde the way a real store would.
///
/// `AlertRule`'s fields are public and its invariants hold only in `new()`, so a rule can be
/// mutated into a shape no persistent store could hold — `threshold = f64::NAN` is the case the
/// fold is designed to show and flag. A store would report that as a failure, so this returns one
/// rather than panicking inside an `async` trait method, where a panic crosses the FFI as a crash.
fn round_trip(rule: &AlertRule) -> Result<AlertRule, CoreError> {
    let json = serde_json::to_string(rule)
        .map_err(|error| CoreError::Backend(format!("cannot serialise rule: {error}")))?;
    serde_json::from_str(&json)
        .map_err(|error| CoreError::Backend(format!("cannot deserialise rule: {error}")))
}

#[async_trait]
impl AlertRuleRepository for InMemoryRuleRepository {
    async fn list(&self) -> pessimal_core::Result<Vec<AlertRule>> {
        if let Some(error) = self.take_error() {
            return Err(error);
        }
        // Through serde rather than cloned, so that `load_rules` in a test exercises the same
        // path a relaunch does: `AlertRuleWire` carries the stored id instead of minting a fresh
        // one, and a regression there would orphan every evaluation keyed to a rule.
        locked(&self.rules).values().map(round_trip).collect()
    }

    async fn save(&self, rule: &AlertRule) -> pessimal_core::Result<()> {
        if let Some(error) = self.take_error() {
            return Err(error);
        }
        locked(&self.rules).insert(rule.id().to_string(), rule.clone());
        Ok(())
    }

    async fn delete(&self, id: &Urn) -> pessimal_core::Result<()> {
        if let Some(error) = self.take_error() {
            return Err(error);
        }
        // A missing id is `RuleNotFound`, never a silent `Ok`: the port documents it, the client's
        // `delete_rule` maps it to `ClientError::RuleNotFound`, and a lax delete here would make
        // that mapping untestable.
        if locked(&self.rules).remove(&id.to_string()).is_none() {
            return Err(CoreError::RuleNotFound(id.to_string()));
        }
        Ok(())
    }

    async fn get(&self, id: &Urn) -> pessimal_core::Result<Option<AlertRule>> {
        if let Some(error) = self.take_error() {
            return Err(error);
        }
        Ok(locked(&self.rules).get(&id.to_string()).cloned())
    }
}

/// A host-wide series from literal `(instant, value)` samples.
///
/// Attributes are empty; a per-mount or per-interface series is this plus
/// [`MetricSeries::with_attributes`], which keeps the common case to one line.
#[must_use]
pub fn series(host: &str, kind: MetricKind, samples: &[(DateTime<Utc>, f64)]) -> MetricSeries {
    MetricSeries::new(
        HostId::new(host),
        kind,
        samples
            .iter()
            .map(|&(at, value)| MetricPoint::new(at, value))
            .collect(),
    )
}

/// `count` points ending at `end`, spaced `step` apart, all `value` — "five 30-second buckets of
/// 95 % CPU ending now" in one line.
///
/// Built backwards from `end` so the newest point lands exactly on the instant a test cares about
/// — `observe` and `latest_at` both read from the newest end — and so no index arithmetic has to
/// be cast between `usize` and a `Duration` multiplier. `MetricSeries::new` sorts the result
/// oldest-first. `count == 0` yields an empty series, which is a legitimate fixture: the
/// successful-but-empty response that clears dwell to `NoData`.
#[must_use]
pub fn steady(
    host: &str,
    kind: MetricKind,
    end: DateTime<Utc>,
    step: Duration,
    count: usize,
    value: f64,
) -> MetricSeries {
    let mut points = Vec::with_capacity(count);
    let mut at = end;
    for _ in 0..count {
        points.push(MetricPoint::new(at, value));
        at -= step;
    }
    MetricSeries::new(HostId::new(host), kind, points)
}

#[cfg(test)]
mod tests {
    use futures::executor::block_on;
    use pessimal_core::{Comparator, HostSelector, OsFamily};

    use super::*;

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_760_000_000 + secs, 0).expect("valid timestamp")
    }

    fn request(metric: MetricKind, selector: HostSelector) -> SeriesRequest {
        SeriesRequest::new(
            metric,
            selector,
            TimeRange::ending_at(at(0), Duration::seconds(210)).expect("positive window"),
            Duration::seconds(30),
        )
    }

    fn rule(name: &str) -> AlertRule {
        AlertRule::new(
            "prod",
            name,
            MetricKind::CpuUtilization,
            Comparator::GreaterThan,
            0.9,
            Duration::seconds(120),
        )
        .expect("valid rule")
    }

    #[test]
    fn an_unscripted_backend_answers_every_request_emptily() {
        let query = ScriptedQuery::new("SigNoz");

        assert_eq!(query.backend_name(), "SigNoz");
        assert_eq!(
            block_on(query.list_hosts(
                TimeRange::ending_at(at(0), Duration::seconds(210)).expect("positive window")
            )),
            Ok(Vec::new())
        );
        assert_eq!(
            block_on(query.query_series(&request(MetricKind::CpuUtilization, HostSelector::All))),
            Ok(Vec::new()),
            "an unscripted metric is the successful-but-empty case, not the unavailable one"
        );
    }

    #[test]
    fn the_roster_succeeds_while_one_metric_fails() {
        let query = ScriptedQuery::new("SigNoz")
            .with_hosts(Ok(vec![Host::new(HostId::new("web-1"), OsFamily::Linux)]))
            .with_series(
                MetricKind::CpuUtilization,
                Err(CoreError::Backend("query timed out".to_owned())),
            )
            .with_series(
                MetricKind::MemoryUtilization,
                Ok(vec![series(
                    "web-1",
                    MetricKind::MemoryUtilization,
                    &[(at(-30), 0.4)],
                )]),
            );

        let hosts = block_on(query.list_hosts(
            TimeRange::ending_at(at(0), Duration::seconds(210)).expect("positive window"),
        ))
        .expect("roster scripted as a success");
        assert_eq!(hosts.len(), 1);
        assert_eq!(
            block_on(query.query_series(&request(MetricKind::CpuUtilization, HostSelector::All))),
            Err(CoreError::Backend("query timed out".to_owned()))
        );
        assert_eq!(
            block_on(
                query.query_series(&request(MetricKind::MemoryUtilization, HostSelector::All))
            )
            .expect("memory scripted as a success")
            .len(),
            1,
            "one broken metric must not take the rest of the poll with it"
        );
    }

    #[test]
    fn calls_record_every_request_in_arrival_order() {
        let query = ScriptedQuery::new("SigNoz");
        let overview = request(MetricKind::CpuUtilization, HostSelector::All);
        let detail = request(
            MetricKind::CpuUtilization,
            HostSelector::Host(HostId::new("web-1")),
        );

        drop(block_on(query.query_series(&overview)));
        drop(block_on(query.query_series(&detail)));

        let calls = query.calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(
            calls[0].to_request(),
            overview,
            "a recorded call must be the request that was issued, field for field"
        );
        assert_eq!(
            calls[1].to_request(),
            detail,
            "arrival order is how a test asserts detail-after-overview"
        );
    }

    #[test]
    fn check_connection_fails_exactly_as_the_roster_does() {
        let rejected = ScriptedQuery::new("SigNoz").with_hosts(Err(CoreError::Unauthorized));
        assert_eq!(
            block_on(rejected.check_connection()),
            Err(CoreError::Unauthorized)
        );

        let empty = ScriptedQuery::new("SigNoz").with_hosts(Ok(Vec::new()));
        assert_eq!(
            block_on(empty.check_connection()),
            Ok(()),
            "connected-but-empty is a probe's most likely misconfiguration and must be reachable"
        );
    }

    #[test]
    fn a_saved_rule_is_listed_and_fetched_with_its_stored_id() {
        let repo = InMemoryRuleRepository::new();
        let saved = rule("CPU hot");

        block_on(repo.save(&saved)).expect("save");

        let listed = block_on(repo.list()).expect("list");
        assert_eq!(listed, vec![saved.clone()]);
        assert_eq!(
            block_on(repo.get(saved.id())).expect("get").as_ref(),
            Some(&saved),
            "a round trip that minted a fresh id would orphan every evaluation keyed to the rule"
        );
    }

    #[test]
    fn seeded_rules_round_trip_with_their_ids() {
        let first = rule("CPU hot");
        let second = rule("Memory hot");
        let repo = InMemoryRuleRepository::seeded(vec![first.clone(), second.clone()]);

        let ids: Vec<String> = repo
            .round_tripped()
            .iter()
            .map(|rule| rule.id().to_string())
            .collect();

        assert!(ids.contains(&first.id().to_string()));
        assert!(ids.contains(&second.id().to_string()));
    }

    #[test]
    fn deleting_an_unknown_id_is_rule_not_found() {
        let repo = InMemoryRuleRepository::new();
        let absent = rule("never saved");

        assert!(matches!(
            block_on(repo.delete(absent.id())),
            Err(CoreError::RuleNotFound(_))
        ));
    }

    #[test]
    fn fail_next_fails_exactly_one_call() {
        let repo = InMemoryRuleRepository::new();
        repo.fail_next(CoreError::Unreachable("no route to host".to_owned()));

        assert!(matches!(
            block_on(repo.list()),
            Err(CoreError::Unreachable(_))
        ));
        assert_eq!(
            block_on(repo.list()).expect("the armed failure is spent"),
            Vec::new(),
            "a latch that stayed armed would be a second thing every test had to disarm"
        );
    }

    #[test]
    fn a_rule_that_cannot_be_persisted_fails_the_list_rather_than_panicking() {
        let repo = InMemoryRuleRepository::new();
        let mut broken = rule("CPU hot");
        // `threshold` is a public field, so this is exactly how an invalid rule reaches the fold.
        broken.threshold = f64::NAN;
        block_on(repo.save(&broken)).expect("save");

        assert!(
            matches!(block_on(repo.list()), Err(CoreError::Backend(_))),
            "a panic inside a port method crosses the FFI as a crash"
        );
    }

    #[test]
    #[allow(
        clippy::float_cmp,
        reason = "the builder copies the literal value; no arithmetic involved"
    )]
    fn steady_builds_count_buckets_ending_at_end() {
        let built = steady(
            "web-1",
            MetricKind::CpuUtilization,
            at(0),
            Duration::seconds(30),
            5,
            0.95,
        );

        let times: Vec<DateTime<Utc>> = built.points().iter().map(|point| point.at).collect();
        assert_eq!(times, vec![at(-120), at(-90), at(-60), at(-30), at(0)]);
        assert_eq!(
            built.latest().expect("non-empty").value,
            0.95,
            "the newest point must land on `end`, which is the instant an alert judges"
        );
        assert!(
            steady(
                "web-1",
                MetricKind::CpuUtilization,
                at(0),
                Duration::seconds(30),
                0,
                0.95,
            )
            .is_empty(),
            "zero buckets is the successful-but-empty fixture, not an error"
        );
    }

    #[test]
    fn series_sorts_its_samples_oldest_first() {
        let built = series(
            "web-1",
            MetricKind::CpuUtilization,
            &[(at(60), 0.3), (at(0), 0.1), (at(30), 0.2)],
        );

        let times: Vec<DateTime<Utc>> = built.points().iter().map(|point| point.at).collect();
        assert_eq!(
            times,
            vec![at(0), at(30), at(60)],
            "`latest_at` scans backwards, so an unsorted fixture judges the wrong sample"
        );
        assert!(built.attributes.is_empty());
    }
}
