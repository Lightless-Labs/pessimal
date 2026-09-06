//! Metric collection and the snapshot cache.
//!
//! Instruments are observable: the SDK calls back at export time rather than the agent pushing on
//! a loop of its own. That means one clock instead of two, and no window where a recorded value
//! and an exported value disagree.
//!
//! The cost is that each instrument's callback fires separately within a single export cycle, so
//! collection is cached: the first callback of a cycle samples the host, the rest read what it
//! took.

use std::collections::BTreeMap;
use std::sync::Mutex;

use chrono::{DateTime, Duration, Utc};
use pessimal_core::{Clock, MetricKind};

use crate::error::Result;

/// One measurement.
#[derive(Debug, Clone, PartialEq)]
pub struct Observation {
    pub kind: MetricKind,
    pub value: f64,
    /// Labels that distinguish series of the same metric, e.g. `{"device": "en0"}` or
    /// `{"mountpoint": "/data"}`. Empty for host-wide metrics.
    pub attributes: BTreeMap<String, String>,
}

impl Observation {
    #[must_use]
    pub fn new(kind: MetricKind, value: f64) -> Self {
        Self {
            kind,
            value,
            attributes: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn with_attribute(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.attributes.insert(name.into(), value.into());
        self
    }
}

/// Everything one sampling pass produced.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub taken_at: DateTime<Utc>,
    pub observations: Vec<Observation>,
}

impl Snapshot {
    #[must_use]
    pub fn new(taken_at: DateTime<Utc>, observations: Vec<Observation>) -> Self {
        Self {
            taken_at,
            observations,
        }
    }

    #[must_use]
    pub fn empty(taken_at: DateTime<Utc>) -> Self {
        Self::new(taken_at, Vec::new())
    }

    /// Every observation of one metric — several, for per-device or per-mount metrics.
    pub fn observations_of(&self, kind: MetricKind) -> impl Iterator<Item = &Observation> {
        self.observations.iter().filter(move |o| o.kind == kind)
    }
}

/// Samples the host. Implemented per platform-ish concern; the host agent has one.
///
/// Takes `&mut self` because collectors are stateful: rate metrics need the previous sample.
pub trait MetricCollector: Send {
    /// # Errors
    /// Returns [`AgentError::Collection`] if the host could not be sampled.
    fn collect(&mut self, now: DateTime<Utc>) -> Result<Vec<Observation>>;
}

struct CacheState {
    collector: Box<dyn MetricCollector>,
    snapshot: Option<Snapshot>,
    consecutive_failures: u32,
    total_failures: u64,
}

/// A [`MetricCollector`] wrapped so that many callbacks in one export cycle produce one sample.
///
/// A snapshot older than `max_age` triggers a fresh sample; anything newer is reused. Set
/// `max_age` below the export interval and above the time one export cycle's callbacks take —
/// half the interval is the obvious choice.
pub struct CachedCollector<C: Clock> {
    state: Mutex<CacheState>,
    max_age: Duration,
    clock: C,
}

impl<C: Clock> CachedCollector<C> {
    #[must_use]
    pub fn new(collector: Box<dyn MetricCollector>, max_age: Duration, clock: C) -> Self {
        Self {
            state: Mutex::new(CacheState {
                collector,
                snapshot: None,
                consecutive_failures: 0,
                total_failures: 0,
            }),
            max_age,
            clock,
        }
    }

    /// The current snapshot, sampling first if the cached one has aged out.
    ///
    /// A failed sample is not fatal: the previous snapshot is returned unchanged, keeping its
    /// original timestamp so a stale reading is visibly stale rather than silently republished as
    /// fresh. A host that cannot be sampled at all yields an empty snapshot, which the exporter
    /// turns into no datapoints — and no heartbeat, which is exactly the signal the clients need.
    pub fn snapshot(&self) -> Snapshot {
        let now = self.clock.now();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let is_fresh = state
            .snapshot
            .as_ref()
            .is_some_and(|s| now - s.taken_at < self.max_age);
        if is_fresh {
            return state
                .snapshot
                .clone()
                .unwrap_or_else(|| Snapshot::empty(now));
        }

        match state.collector.collect(now) {
            Ok(observations) => {
                state.consecutive_failures = 0;
                let snapshot = Snapshot::new(now, observations);
                state.snapshot = Some(snapshot.clone());
                snapshot
            }
            Err(error) => {
                state.consecutive_failures = state.consecutive_failures.saturating_add(1);
                state.total_failures = state.total_failures.saturating_add(1);
                tracing::warn!(
                    %error,
                    consecutive_failures = state.consecutive_failures,
                    "host sampling failed; reusing the previous snapshot"
                );
                state
                    .snapshot
                    .clone()
                    .unwrap_or_else(|| Snapshot::empty(now))
            }
        }
    }

    /// How many samples have failed in a row. Zero after any success.
    pub fn consecutive_failures(&self) -> u32 {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .consecutive_failures
    }

    /// How many samples have failed since the process started. Monotonic, so it can back a
    /// counter instrument.
    pub fn total_failures(&self) -> u64 {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .total_failures
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::AgentError;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A clock the test advances by hand.
    #[derive(Clone)]
    struct TestClock(Arc<Mutex<DateTime<Utc>>>);

    impl TestClock {
        fn new(at: DateTime<Utc>) -> Self {
            Self(Arc::new(Mutex::new(at)))
        }

        fn advance(&self, by: Duration) {
            let mut now = self.0.lock().expect("not poisoned");
            *now += by;
        }
    }

    impl Clock for TestClock {
        fn now(&self) -> DateTime<Utc> {
            *self.0.lock().expect("not poisoned")
        }
    }

    /// Counts calls, and can be told to fail.
    struct CountingCollector {
        calls: Arc<AtomicU32>,
        fail_after: Option<u32>,
    }

    impl MetricCollector for CountingCollector {
        fn collect(&mut self, _now: DateTime<Utc>) -> Result<Vec<Observation>> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            if self.fail_after.is_some_and(|threshold| call > threshold) {
                return Err(AgentError::Collection("no".to_owned()));
            }
            #[allow(clippy::cast_lossless)]
            Ok(vec![Observation::new(
                MetricKind::CpuUtilization,
                f64::from(call),
            )])
        }
    }

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_760_000_000 + secs, 0).expect("valid timestamp")
    }

    fn cached(fail_after: Option<u32>) -> (CachedCollector<TestClock>, TestClock, Arc<AtomicU32>) {
        let clock = TestClock::new(at(0));
        let calls = Arc::new(AtomicU32::new(0));
        let collector = CachedCollector::new(
            Box::new(CountingCollector {
                calls: Arc::clone(&calls),
                fail_after,
            }),
            Duration::seconds(15),
            clock.clone(),
        );
        (collector, clock, calls)
    }

    #[test]
    fn the_first_snapshot_samples_the_host() {
        let (collector, _clock, calls) = cached(None);
        let snapshot = collector.snapshot();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(snapshot.taken_at, at(0));
        assert_eq!(snapshot.observations.len(), 1);
    }

    #[test]
    fn callbacks_within_one_cycle_share_a_single_sample() {
        let (collector, _clock, calls) = cached(None);
        let first = collector.snapshot();
        let second = collector.snapshot();
        let third = collector.snapshot();

        assert_eq!(calls.load(Ordering::SeqCst), 1, "one sample per cycle");
        assert_eq!(first, second);
        assert_eq!(second, third);
    }

    #[test]
    fn a_snapshot_older_than_max_age_is_resampled() {
        let (collector, clock, calls) = cached(None);
        collector.snapshot();
        clock.advance(Duration::seconds(15));
        collector.snapshot();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn max_age_is_exclusive_at_the_boundary() {
        let (collector, clock, calls) = cached(None);
        collector.snapshot();
        clock.advance(Duration::seconds(14));
        collector.snapshot();
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "still fresh just under max_age"
        );
    }

    #[test]
    fn a_failed_sample_reuses_the_previous_snapshot_with_its_original_timestamp() {
        let (collector, clock, _calls) = cached(Some(1));
        let first = collector.snapshot();
        clock.advance(Duration::seconds(30));

        let second = collector.snapshot();
        assert_eq!(second, first, "stale data must not be restamped as fresh");
        assert_eq!(second.taken_at, at(0));
        assert_eq!(collector.consecutive_failures(), 1);
    }

    #[test]
    fn a_host_that_never_samples_yields_an_empty_snapshot() {
        let (collector, _clock, _calls) = cached(Some(0));
        let snapshot = collector.snapshot();
        assert!(
            snapshot.observations.is_empty(),
            "no observations means no heartbeat, which is the signal clients need"
        );
        assert_eq!(collector.consecutive_failures(), 1);
    }

    #[test]
    fn the_failure_count_resets_after_a_success() {
        /// Fails on the first call, succeeds thereafter.
        struct FlakyOnce {
            calls: Arc<AtomicU32>,
        }

        impl MetricCollector for FlakyOnce {
            fn collect(&mut self, _now: DateTime<Utc>) -> Result<Vec<Observation>> {
                if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    return Err(AgentError::Collection("first attempt".to_owned()));
                }
                Ok(vec![Observation::new(MetricKind::CpuUtilization, 0.5)])
            }
        }

        let clock = TestClock::new(at(0));
        let calls = Arc::new(AtomicU32::new(0));
        let collector = CachedCollector::new(
            Box::new(FlakyOnce {
                calls: Arc::clone(&calls),
            }),
            Duration::seconds(15),
            clock.clone(),
        );

        collector.snapshot();
        assert_eq!(collector.consecutive_failures(), 1);

        clock.advance(Duration::seconds(30));
        collector.snapshot();
        assert_eq!(collector.consecutive_failures(), 0);
        assert_eq!(
            collector.total_failures(),
            1,
            "the running total survives a recovery"
        );
    }

    #[test]
    fn observations_of_selects_by_metric() {
        let snapshot = Snapshot::new(
            at(0),
            vec![
                Observation::new(MetricKind::NetworkIo, 1.0).with_attribute("device", "en0"),
                Observation::new(MetricKind::NetworkIo, 2.0).with_attribute("device", "en1"),
                Observation::new(MetricKind::CpuUtilization, 0.5),
            ],
        );
        assert_eq!(snapshot.observations_of(MetricKind::NetworkIo).count(), 2);
        assert_eq!(
            snapshot.observations_of(MetricKind::CpuUtilization).count(),
            1
        );
        assert_eq!(
            snapshot.observations_of(MetricKind::SystemUptime).count(),
            0
        );
    }
}
