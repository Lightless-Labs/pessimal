//! What the backend said, kept strictly apart from what it means.
//!
//! A [`PollObservation`] is the complete record of one poll: the instant it was evaluated at, what
//! `list_hosts` answered, and what every [`QuerySpec`] in the plan answered. It holds no verdicts.
//! Nothing here decides liveness, dwell, availability or freshness — `crate::fold` does all of
//! that, from this value alone — which is why the whole type is `serde`-round-trippable: a
//! production poll recorded here replays in a test as a literal fixture, with no backend, no
//! runtime and no clock.
//!
//! The load-bearing idea is the [`Coverage`] trichotomy. `Ok(vec![])` and `Err` are not the same
//! absence, and collapsing them is the bug this module exists to make unrepresentable.

use chrono::{DateTime, Utc};
use pessimal_core::{Host, HostId, MetricKind, MetricSeries};
use serde::{Deserialize, Serialize};

use crate::error::PollFailure;
use crate::plan::QuerySpec;

/// What this poll learned about one `(host, metric)` pair.
///
/// Three cases, not two, because "the backend holds nothing" and "we could not ask" are different
/// facts with opposite consequences. `Ok(vec![])` is a genuine absence of data: core's
/// `an_empty_series_is_no_data_not_ok` says an empty series legitimately drives an evaluation to
/// `NoData` and clears its dwell. An `Err`, or a pair no request even covered, is not evidence of
/// anything — and calling `AlertEvaluation::observe` with an empty series on a *failed* fetch
/// would return `NoData` too, resetting the dwell on every alert in the fleet after one transient
/// 500. Freezing is the only safe reading, and a `match` on three variants is what makes the
/// distinction structural rather than a remembered `if`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Coverage {
    /// A covering request succeeded and returned at least one series for this host and metric.
    Fresh,
    /// A covering request succeeded and returned nothing: genuine `NoData`.
    Empty,
    /// Every covering request failed, or none was issued: freeze, do not judge.
    Unavailable,
}

/// What `list_hosts` answered.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum RosterOutcome {
    /// `list_hosts` was not attempted. A partial poll is representable.
    NotAttempted,
    Listed(Vec<Host>),
    Failed(PollFailure),
}

/// What one [`QuerySpec`] answered. An explicit enum rather than a `Result` in field position:
/// UniFFI carries `Result` only in return position, and the named variants read better anyway.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SeriesResult {
    Returned(Vec<MetricSeries>),
    Failed(PollFailure),
}

/// One request paired with its answer. The spec travels with the result so [`Coverage`] can be
/// derived from the plan rather than guessed from the shape of what came back.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SeriesOutcome {
    pub spec: QuerySpec,
    pub result: SeriesResult,
}

/// One poll, recorded whole.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PollObservation {
    /// The single instant this whole poll is evaluated at — the query window, liveness, and dwell
    /// all read it. Stamped once, before any request. Nothing else in the crate reads a clock,
    /// which is why there is no `completed_at`.
    pub observed_at: DateTime<Utc>,
    pub hosts: RosterOutcome,
    pub series: Vec<SeriesOutcome>,
}

impl PollObservation {
    /// An empty observation: nothing attempted yet. `gather` fills it in as answers arrive.
    #[must_use]
    pub fn new(observed_at: DateTime<Utc>) -> Self {
        Self {
            observed_at,
            hosts: RosterOutcome::NotAttempted,
            series: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_hosts(mut self, hosts: RosterOutcome) -> Self {
        self.hosts = hosts;
        self
    }

    /// Appends one outcome. Order is the plan's order, and it is load-bearing: later outcomes win
    /// in [`Self::series_for`], which is how a focused host's long detail window beats the
    /// fleet-wide overview window with no merge step.
    #[must_use]
    pub fn with_series(mut self, spec: QuerySpec, result: SeriesResult) -> Self {
        self.series.push(SeriesOutcome { spec, result });
        self
    }

    /// The listed roster, or `None` when `list_hosts` failed or was never attempted.
    ///
    /// Both non-listing cases collapse to `None` on purpose: neither is a roster, and a caller
    /// that treats "no hosts" as "the fleet is empty" would wipe the picture. The fold reads
    /// `self.hosts` directly when it needs to tell the two apart.
    #[must_use]
    pub fn roster(&self) -> Option<&[Host]> {
        match &self.hosts {
            RosterOutcome::Listed(hosts) => Some(hosts),
            RosterOutcome::NotAttempted | RosterOutcome::Failed(_) => None,
        }
    }

    /// Classifies one `(host, metric)` pair.
    ///
    /// `Fresh` wins over `Empty` and `Empty` over `Unavailable`, so a metric fetched twice in one
    /// plan — the focused host's overview-plus-detail pair — reads `Empty` rather than
    /// `Unavailable` when one of the two succeeded and held nothing. Evidence that arrived beats
    /// evidence that did not.
    #[must_use]
    pub fn coverage(&self, host: &HostId, metric: MetricKind) -> Coverage {
        let mut any_succeeded = false;
        for outcome in self.covering(host, metric) {
            match &outcome.result {
                SeriesResult::Returned(returned) => {
                    any_succeeded = true;
                    if returned
                        .iter()
                        .any(|series| &series.host == host && series.kind == metric)
                    {
                        return Coverage::Fresh;
                    }
                }
                SeriesResult::Failed(_) => {}
            }
        }
        if any_succeeded {
            Coverage::Empty
        } else {
            Coverage::Unavailable
        }
    }

    /// Series for exactly this host and metric, in plan order, deduped by `attributes` keeping the
    /// last — which is how a focused host's long detail window beats its overview window.
    /// Performs the host/kind filtering `AlertEvaluation::observe` does not do.
    ///
    /// The dedup replaces in place rather than appending, so the surviving order is still the
    /// order the plan first saw each attribute set in.
    ///
    /// This is the *storage* path — it is empty for `Empty` and for `Unavailable` alike, and the
    /// two mean opposite things. Nothing that judges an alert should call it; call
    /// [`Self::observable_series`] instead.
    #[must_use]
    pub fn series_for(&self, host: &HostId, metric: MetricKind) -> Vec<&MetricSeries> {
        let mut kept: Vec<&MetricSeries> = Vec::new();
        for outcome in self.covering(host, metric) {
            let SeriesResult::Returned(returned) = &outcome.result else {
                continue;
            };
            for series in returned {
                if &series.host != host || series.kind != metric {
                    continue;
                }
                if let Some(slot) = kept
                    .iter_mut()
                    .find(|earlier| earlier.attributes == series.attributes)
                {
                    *slot = series;
                } else {
                    kept.push(series);
                }
            }
        }
        kept
    }

    /// The series an alert may be judged against, or `None` when this pair must not be judged
    /// at all.
    ///
    /// This is the trichotomy made structural instead of remembered. `Some(vec![])` is
    /// [`Coverage::Empty`]: a covering request succeeded and the backend genuinely holds nothing,
    /// so `observe` should be called with an empty series and dwell should clear to `NoData`.
    /// `None` is [`Coverage::Unavailable`]: there is no value to hand to `observe`, so a caller
    /// physically cannot reset a fleet's dwell on the strength of one transient 500. The
    /// correspondence is exact and total:
    ///
    /// - `None` iff `coverage(..) == Coverage::Unavailable`
    /// - `Some(v)` with `v.is_empty()` iff `coverage(..) == Coverage::Empty`
    /// - `Some(v)` with `!v.is_empty()` iff `coverage(..) == Coverage::Fresh`
    #[must_use]
    pub fn observable_series(
        &self,
        host: &HostId,
        metric: MetricKind,
    ) -> Option<Vec<&MetricSeries>> {
        match self.coverage(host, metric) {
            Coverage::Unavailable => None,
            Coverage::Fresh | Coverage::Empty => Some(self.series_for(host, metric)),
        }
    }

    /// Every failure this poll recorded, roster first and then series in plan order.
    ///
    /// Owned clones rather than borrows: the fold stores the worst of them in `FleetState`, and
    /// the observation it came from is dropped at the end of the poll.
    #[must_use]
    pub fn failures(&self) -> Vec<PollFailure> {
        let mut failures = Vec::new();
        if let RosterOutcome::Failed(failure) = &self.hosts {
            failures.push(failure.clone());
        }
        failures.extend(
            self.series
                .iter()
                .filter_map(|outcome| match &outcome.result {
                    SeriesResult::Failed(failure) => Some(failure.clone()),
                    SeriesResult::Returned(_) => None,
                }),
        );
        failures
    }

    /// True when the roster was listed and no series outcome failed.
    ///
    /// Deliberately stricter than "the roster is usable": `last_clean_at` is what freshness calls
    /// `Fresh`, and a poll that lost one metric is not one this screen should claim is current.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        matches!(self.hosts, RosterOutcome::Listed(_))
            && self
                .series
                .iter()
                .all(|outcome| matches!(outcome.result, SeriesResult::Returned(_)))
    }

    /// The outcomes whose request could have held data for this pair. "Could have" is the plan's
    /// judgement, not the response's: a pair nothing covered is `Unavailable` because we never
    /// asked, and that is indistinguishable from asking and failing.
    /// `'this` and `'host` are separate on purpose: tying them together would make the borrow of
    /// `host` outlive the call and force `series_for`'s result to borrow from its argument.
    fn covering<'this, 'host>(
        &'this self,
        host: &'host HostId,
        metric: MetricKind,
    ) -> impl Iterator<Item = &'this SeriesOutcome> + use<'this, 'host> {
        self.series
            .iter()
            .filter(move |outcome| outcome.spec.covers(host, metric))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use chrono::{DateTime, Duration, Utc};
    use pessimal_core::{
        Host, HostId, HostSelector, MetricKind, MetricPoint, MetricSeries, OsFamily, TimeRange,
    };

    use super::{Coverage, PollObservation, RosterOutcome, SeriesResult};
    use crate::error::{FailureSource, PollFailure, PollFailureKind};
    use crate::plan::QuerySpec;

    fn at(offset_secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_757_000_000 + offset_secs, 0).expect("a valid fixture instant")
    }

    fn web1() -> HostId {
        HostId::new("web-1")
    }

    fn db1() -> HostId {
        HostId::new("db-1")
    }

    fn spec(metric: MetricKind, selector: HostSelector) -> QuerySpec {
        QuerySpec {
            metric,
            selector,
            range: TimeRange::ending_at(at(0), Duration::seconds(210)).expect("positive window"),
            step: Duration::seconds(30),
        }
    }

    fn series(host: &HostId, kind: MetricKind, points: usize) -> MetricSeries {
        let points = (0..points)
            .map(|index| {
                MetricPoint::new(at(-30 * i64::try_from(index).expect("small fixture")), 0.5)
            })
            .collect();
        MetricSeries::new(host.clone(), kind, points)
    }

    fn mount(series: MetricSeries, path: &str) -> MetricSeries {
        let mut attributes = BTreeMap::new();
        attributes.insert("system.filesystem.mountpoint".to_owned(), path.to_owned());
        series.with_attributes(attributes)
    }

    fn failure(source: FailureSource) -> PollFailure {
        PollFailure {
            kind: PollFailureKind::Backend,
            source,
            message: "telemetry backend error: 500".to_owned(),
            at: at(0),
        }
    }

    #[test]
    fn coverage_is_fresh_when_a_covering_request_returned_a_series() {
        let observation = PollObservation::new(at(0)).with_series(
            spec(MetricKind::CpuUtilization, HostSelector::All),
            SeriesResult::Returned(vec![series(&web1(), MetricKind::CpuUtilization, 3)]),
        );

        assert_eq!(
            observation.coverage(&web1(), MetricKind::CpuUtilization),
            Coverage::Fresh
        );
    }

    #[test]
    fn coverage_is_empty_when_a_covering_request_returned_nothing() {
        let observation = PollObservation::new(at(0)).with_series(
            spec(MetricKind::FilesystemUtilization, HostSelector::All),
            SeriesResult::Returned(vec![]),
        );

        assert_eq!(
            observation.coverage(&web1(), MetricKind::FilesystemUtilization),
            Coverage::Empty,
            "a successful request that held nothing is a real absence, not a blind spot"
        );
    }

    #[test]
    fn coverage_is_unavailable_when_every_covering_request_failed() {
        let observation = PollObservation::new(at(0)).with_series(
            spec(MetricKind::CpuUtilization, HostSelector::All),
            SeriesResult::Failed(failure(FailureSource::Series {
                metric: MetricKind::CpuUtilization,
            })),
        );

        assert_eq!(
            observation.coverage(&web1(), MetricKind::CpuUtilization),
            Coverage::Unavailable
        );
    }

    #[test]
    fn coverage_is_unavailable_when_no_request_covered_the_pair() {
        // The plan asked for CPU fleet-wide and for memory on `db-1` only. Nothing covers memory
        // on `web-1`, and never having asked is not evidence that there is nothing there.
        let observation = PollObservation::new(at(0))
            .with_series(
                spec(MetricKind::CpuUtilization, HostSelector::All),
                SeriesResult::Returned(vec![series(&web1(), MetricKind::CpuUtilization, 3)]),
            )
            .with_series(
                spec(MetricKind::MemoryUtilization, HostSelector::Host(db1())),
                SeriesResult::Returned(vec![series(&db1(), MetricKind::MemoryUtilization, 3)]),
            );

        assert_eq!(
            observation.coverage(&web1(), MetricKind::MemoryUtilization),
            Coverage::Unavailable
        );
    }

    #[test]
    fn a_failed_query_beside_a_successful_one_is_empty_not_unavailable() {
        // The focused host is queried twice for the same metric: once fleet-wide, once for its
        // long detail window. Evidence that arrived beats evidence that did not.
        let observation = PollObservation::new(at(0))
            .with_series(
                spec(MetricKind::CpuUtilization, HostSelector::All),
                SeriesResult::Returned(vec![]),
            )
            .with_series(
                spec(MetricKind::CpuUtilization, HostSelector::Host(web1())),
                SeriesResult::Failed(failure(FailureSource::Series {
                    metric: MetricKind::CpuUtilization,
                })),
            );

        assert_eq!(
            observation.coverage(&web1(), MetricKind::CpuUtilization),
            Coverage::Empty
        );
    }

    #[test]
    fn series_for_drops_other_hosts_from_an_all_selector_response() {
        let observation = PollObservation::new(at(0)).with_series(
            spec(MetricKind::CpuUtilization, HostSelector::All),
            SeriesResult::Returned(vec![
                series(&web1(), MetricKind::CpuUtilization, 3),
                series(&db1(), MetricKind::CpuUtilization, 3),
            ]),
        );

        let kept = observation.series_for(&web1(), MetricKind::CpuUtilization);

        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].host, web1());
    }

    #[test]
    fn series_for_drops_a_series_whose_kind_disagrees_with_the_request() {
        // A backend that answers a CPU query with a memory series is not a reason to chart memory
        // as CPU. `AlertEvaluation::observe` does no such filtering, so it happens here.
        let observation = PollObservation::new(at(0)).with_series(
            spec(MetricKind::CpuUtilization, HostSelector::All),
            SeriesResult::Returned(vec![
                series(&web1(), MetricKind::CpuUtilization, 3),
                series(&web1(), MetricKind::MemoryUtilization, 3),
            ]),
        );

        let kept = observation.series_for(&web1(), MetricKind::CpuUtilization);

        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].kind, MetricKind::CpuUtilization);
    }

    #[test]
    fn series_for_prefers_the_last_matching_attribute_set() {
        // Overview first, detail last: the focused host's long window replaces the short one for
        // the mount they share, and the mount only the overview saw survives untouched.
        let observation = PollObservation::new(at(0))
            .with_series(
                spec(MetricKind::FilesystemUtilization, HostSelector::All),
                SeriesResult::Returned(vec![
                    mount(series(&web1(), MetricKind::FilesystemUtilization, 1), "/"),
                    mount(
                        series(&web1(), MetricKind::FilesystemUtilization, 1),
                        "/data",
                    ),
                ]),
            )
            .with_series(
                spec(
                    MetricKind::FilesystemUtilization,
                    HostSelector::Host(web1()),
                ),
                SeriesResult::Returned(vec![mount(
                    series(&web1(), MetricKind::FilesystemUtilization, 5),
                    "/",
                )]),
            );

        let kept = observation.series_for(&web1(), MetricKind::FilesystemUtilization);

        assert_eq!(kept.len(), 2, "the detail window replaces, it does not add");
        assert_eq!(
            kept[0].attributes.get("system.filesystem.mountpoint"),
            Some(&"/".to_owned()),
            "dedup keeps the plan's first-seen position"
        );
        assert_eq!(kept[0].points().len(), 5, "the last matching series wins");
        assert_eq!(kept[1].points().len(), 1);
    }

    #[test]
    fn an_unavailable_pair_yields_no_series_to_observe() {
        // The structural half of the trichotomy: `Empty` hands `observe` an empty list, which is
        // a judgement; `Unavailable` hands it nothing at all, so it cannot be judged by mistake.
        let observation = PollObservation::new(at(0))
            .with_series(
                spec(MetricKind::CpuUtilization, HostSelector::All),
                SeriesResult::Failed(failure(FailureSource::Series {
                    metric: MetricKind::CpuUtilization,
                })),
            )
            .with_series(
                spec(MetricKind::MemoryUtilization, HostSelector::All),
                SeriesResult::Returned(vec![]),
            )
            .with_series(
                spec(MetricKind::LoadAverage1m, HostSelector::All),
                SeriesResult::Returned(vec![series(&web1(), MetricKind::LoadAverage1m, 2)]),
            );

        assert_eq!(
            observation.observable_series(&web1(), MetricKind::CpuUtilization),
            None
        );
        assert_eq!(
            observation
                .observable_series(&web1(), MetricKind::MemoryUtilization)
                .map(|series| series.len()),
            Some(0)
        );
        assert_eq!(
            observation
                .observable_series(&web1(), MetricKind::LoadAverage1m)
                .map(|series| series.len()),
            Some(1)
        );
        assert_eq!(
            observation.observable_series(&web1(), MetricKind::SystemUptime),
            None,
            "a pair nothing covered is as unjudgeable as one that failed"
        );
    }

    #[test]
    fn a_partial_poll_is_not_clean_and_reports_its_failures() {
        let clean = PollObservation::new(at(0))
            .with_hosts(RosterOutcome::Listed(vec![Host::new(
                web1(),
                OsFamily::Linux,
            )]))
            .with_series(
                spec(MetricKind::CpuUtilization, HostSelector::All),
                SeriesResult::Returned(vec![series(&web1(), MetricKind::CpuUtilization, 3)]),
            );
        assert!(clean.is_clean());
        assert!(clean.failures().is_empty());
        assert_eq!(clean.roster().map(<[Host]>::len), Some(1));

        let partial = clean.clone().with_series(
            spec(MetricKind::NetworkIo, HostSelector::All),
            SeriesResult::Failed(failure(FailureSource::Series {
                metric: MetricKind::NetworkIo,
            })),
        );
        assert!(!partial.is_clean());
        assert_eq!(partial.failures().len(), 1);

        // A failed roster is not an empty fleet: `roster` refuses to answer at all.
        let blind = partial.with_hosts(RosterOutcome::Failed(failure(FailureSource::Roster)));
        assert_eq!(blind.roster(), None);
        assert_eq!(
            blind.failures().first().map(|failure| failure.source),
            Some(FailureSource::Roster),
            "the roster failure is reported first"
        );
    }

    #[test]
    fn an_observation_round_trips_through_json() {
        // Record and replay is the fixture strategy: whatever a production poll saw must survive
        // a trip through JSON exactly, or a captured incident cannot be replayed as a test.
        let observation = PollObservation::new(at(0))
            .with_hosts(RosterOutcome::Listed(vec![
                Host::new(web1(), OsFamily::Linux)
                    .with_last_heartbeat(at(-15))
                    .with_agent_version("0.1.0"),
                Host::new(db1(), OsFamily::Windows),
            ]))
            .with_series(
                spec(MetricKind::FilesystemUtilization, HostSelector::All),
                SeriesResult::Returned(vec![mount(
                    series(&web1(), MetricKind::FilesystemUtilization, 3),
                    "/",
                )]),
            )
            .with_series(
                spec(MetricKind::NetworkIo, HostSelector::AnyOf(vec![web1()])),
                SeriesResult::Failed(failure(FailureSource::Series {
                    metric: MetricKind::NetworkIo,
                })),
            );

        let json = serde_json::to_string(&observation).expect("an observation serialises");
        let restored: PollObservation =
            serde_json::from_str(&json).expect("an observation deserialises");

        assert_eq!(restored, observation);
        assert_eq!(
            restored.coverage(&web1(), MetricKind::FilesystemUtilization),
            Coverage::Fresh,
            "the replayed fixture classifies exactly as the live poll did"
        );
        assert_eq!(
            restored.coverage(&web1(), MetricKind::NetworkIo),
            Coverage::Unavailable
        );
    }
}
