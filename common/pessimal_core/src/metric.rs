//! Metric identity, units, and time series.
//!
//! Metric names follow the OpenTelemetry system semantic conventions so that whatever backend
//! ingests the agent's OTLP — SigNoz, ClickStack, Honeycomb, a bare collector — stores them under
//! names it already understands. [`MetricKind`] is the closed set Pessimal charts and alerts on;
//! anything else in the backend is still exported, just not modelled here.

use std::collections::BTreeMap;
use std::fmt;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::error::CoreError;
use crate::host::HostId;

/// Metric namespace for Pessimal's own instrumentation, as opposed to `system.*` semconv metrics.
pub const PESSIMAL_METRIC_NAMESPACE: &str = "pessimal.agent";

/// Whether a metric's values stand alone or accumulate.
///
/// This decides which OTel instrument the agent registers, and how a backend should be asked to
/// aggregate the metric on read. Getting it wrong is not cosmetic: a cumulative value charted as
/// a gauge is a line that only ever goes up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum InstrumentKind {
    /// A value that is meaningful on its own, e.g. current CPU utilisation.
    Gauge,
    /// A monotonically increasing total since the process or host started, e.g. bytes sent.
    Counter,
}

/// The unit a metric's values carry. Formatting is the UI's business; this only says what a
/// number means.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MetricUnit {
    /// Dimensionless ratio in `0.0..=1.0`. OTLP unit `1`.
    Ratio,
    /// Bytes. OTLP unit `By`.
    Bytes,
    /// Seconds. OTLP unit `s`.
    Seconds,
    /// A plain count. OTLP unit `{count}`.
    Count,
    /// Run-queue load average — dimensionless but not a ratio, so not [`MetricUnit::Ratio`].
    Load,
}

impl MetricUnit {
    #[must_use]
    pub fn otel_unit(self) -> &'static str {
        match self {
            Self::Ratio | Self::Load => "1",
            Self::Bytes => "By",
            Self::Seconds => "s",
            Self::Count => "{count}",
        }
    }
}

/// The metrics Pessimal models end to end: collected by the agent, charted by the apps, and
/// alertable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum MetricKind {
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
    /// Pessimal's own liveness beat. Its most recent timestamp is what [`crate::Liveness`] reads.
    AgentHeartbeat,
    /// Cumulative host-sampling failures. An agent can be beating happily while collecting
    /// nothing — a permissions change, a vanished mount — and this is how that shows up.
    AgentCollectionFailures,
}

impl MetricKind {
    /// Every modelled metric, in display order.
    pub const ALL: [Self; 12] = [
        Self::CpuUtilization,
        Self::MemoryUtilization,
        Self::MemoryUsage,
        Self::FilesystemUtilization,
        Self::FilesystemUsage,
        Self::NetworkIo,
        Self::LoadAverage1m,
        Self::LoadAverage5m,
        Self::LoadAverage15m,
        Self::SystemUptime,
        Self::AgentHeartbeat,
        Self::AgentCollectionFailures,
    ];

    /// The OpenTelemetry instrument name this metric is exported and queried under.
    #[must_use]
    pub fn otel_name(self) -> &'static str {
        match self {
            Self::CpuUtilization => "system.cpu.utilization",
            Self::MemoryUtilization => "system.memory.utilization",
            Self::MemoryUsage => "system.memory.usage",
            Self::FilesystemUtilization => "system.filesystem.utilization",
            Self::FilesystemUsage => "system.filesystem.usage",
            Self::NetworkIo => "system.network.io",
            Self::LoadAverage1m => "system.cpu.load_average.1m",
            Self::LoadAverage5m => "system.cpu.load_average.5m",
            Self::LoadAverage15m => "system.cpu.load_average.15m",
            Self::SystemUptime => "system.uptime",
            Self::AgentHeartbeat => "pessimal.agent.heartbeat",
            Self::AgentCollectionFailures => "pessimal.agent.collection_failures",
        }
    }

    /// Which OTel instrument this metric is exported as.
    #[must_use]
    pub fn instrument_kind(self) -> InstrumentKind {
        match self {
            Self::NetworkIo | Self::AgentHeartbeat | Self::AgentCollectionFailures => {
                InstrumentKind::Counter
            }
            _ => InstrumentKind::Gauge,
        }
    }

    #[must_use]
    pub fn unit(self) -> MetricUnit {
        match self {
            Self::CpuUtilization | Self::MemoryUtilization | Self::FilesystemUtilization => {
                MetricUnit::Ratio
            }
            Self::MemoryUsage | Self::FilesystemUsage | Self::NetworkIo => MetricUnit::Bytes,
            Self::SystemUptime => MetricUnit::Seconds,
            Self::LoadAverage1m | Self::LoadAverage5m | Self::LoadAverage15m => MetricUnit::Load,
            Self::AgentHeartbeat | Self::AgentCollectionFailures => MetricUnit::Count,
        }
    }

    /// A short human label. Not localised — the UI layer owns presentation.
    #[must_use]
    pub fn display_name(self) -> &'static str {
        match self {
            Self::CpuUtilization => "CPU",
            Self::MemoryUtilization => "Memory",
            Self::MemoryUsage => "Memory used",
            Self::FilesystemUtilization => "Disk",
            Self::FilesystemUsage => "Disk used",
            Self::NetworkIo => "Network I/O",
            Self::LoadAverage1m => "Load (1m)",
            Self::LoadAverage5m => "Load (5m)",
            Self::LoadAverage15m => "Load (15m)",
            Self::SystemUptime => "Uptime",
            Self::AgentHeartbeat => "Heartbeat",
            Self::AgentCollectionFailures => "Collection failures",
        }
    }

    /// Resolves an OTel instrument name back to a modelled metric.
    ///
    /// # Errors
    /// Returns [`CoreError::UnknownMetric`] for a name Pessimal does not model.
    pub fn from_otel_name(name: &str) -> Result<Self, CoreError> {
        Self::ALL
            .into_iter()
            .find(|kind| kind.otel_name() == name)
            .ok_or_else(|| CoreError::UnknownMetric(name.to_owned()))
    }
}

impl fmt::Display for MetricKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.otel_name())
    }
}

/// A half-open query window, `start..end`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeRange {
    start: DateTime<Utc>,
    end: DateTime<Utc>,
}

impl TimeRange {
    /// # Errors
    /// Returns [`CoreError::InvalidTimeRange`] unless `start < end`.
    pub fn new(start: DateTime<Utc>, end: DateTime<Utc>) -> Result<Self, CoreError> {
        if start >= end {
            return Err(CoreError::InvalidTimeRange(format!(
                "start {start} is not before end {end}"
            )));
        }
        Ok(Self { start, end })
    }

    /// The window of length `window` ending at `now`.
    ///
    /// # Errors
    /// Returns [`CoreError::InvalidTimeRange`] if `window` is not strictly positive.
    pub fn ending_at(now: DateTime<Utc>, window: Duration) -> Result<Self, CoreError> {
        if window <= Duration::zero() {
            return Err(CoreError::InvalidTimeRange(format!(
                "window {window} must be positive"
            )));
        }
        Self::new(now - window, now)
    }

    #[must_use]
    pub fn start(&self) -> DateTime<Utc> {
        self.start
    }

    #[must_use]
    pub fn end(&self) -> DateTime<Utc> {
        self.end
    }

    #[must_use]
    pub fn duration(&self) -> Duration {
        self.end - self.start
    }

    #[must_use]
    pub fn contains(&self, at: DateTime<Utc>) -> bool {
        at >= self.start && at < self.end
    }
}

/// One observation.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MetricPoint {
    pub at: DateTime<Utc>,
    pub value: f64,
}

impl MetricPoint {
    #[must_use]
    pub fn new(at: DateTime<Utc>, value: f64) -> Self {
        Self { at, value }
    }
}

/// A metric's values for one host over a window, oldest point first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MetricSeries {
    pub host: HostId,
    pub kind: MetricKind,
    /// Distinguishing labels for a per-device or per-mount series, e.g. `{"device": "en0"}`.
    /// Empty for host-wide metrics.
    pub attributes: BTreeMap<String, String>,
    points: Vec<MetricPoint>,
}

impl MetricSeries {
    /// Builds a series, sorting points oldest-first so callers need not.
    #[must_use]
    pub fn new(host: HostId, kind: MetricKind, mut points: Vec<MetricPoint>) -> Self {
        points.sort_by_key(|p| p.at);
        Self {
            host,
            kind,
            attributes: BTreeMap::new(),
            points,
        }
    }

    #[must_use]
    pub fn with_attributes(mut self, attributes: BTreeMap<String, String>) -> Self {
        self.attributes = attributes;
        self
    }

    #[must_use]
    pub fn points(&self) -> &[MetricPoint] {
        &self.points
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }

    #[must_use]
    pub fn unit(&self) -> MetricUnit {
        self.kind.unit()
    }

    /// The most recent point, if any.
    #[must_use]
    pub fn latest(&self) -> Option<MetricPoint> {
        self.points.last().copied()
    }

    /// The most recent point at or before `at` — what an alert evaluated "as of" a past instant
    /// should see.
    #[must_use]
    pub fn latest_at(&self, at: DateTime<Utc>) -> Option<MetricPoint> {
        self.points.iter().rev().find(|p| p.at <= at).copied()
    }

    /// Points falling inside `range`.
    #[must_use]
    pub fn within(&self, range: TimeRange) -> Vec<MetricPoint> {
        self.points
            .iter()
            .copied()
            .filter(|p| range.contains(p.at))
            .collect()
    }

    /// Arithmetic mean of all values, or `None` when empty.
    #[must_use]
    pub fn mean(&self) -> Option<f64> {
        if self.points.is_empty() {
            return None;
        }
        let sum: f64 = self.points.iter().map(|p| p.value).sum();
        #[allow(clippy::cast_precision_loss)]
        Some(sum / self.points.len() as f64)
    }

    #[must_use]
    pub fn max(&self) -> Option<f64> {
        self.points
            .iter()
            .map(|p| p.value)
            .fold(None, |acc: Option<f64>, v| {
                Some(acc.map_or(v, |a| a.max(v)))
            })
    }

    #[must_use]
    pub fn min(&self) -> Option<f64> {
        self.points
            .iter()
            .map(|p| p.value)
            .fold(None, |acc: Option<f64>, v| {
                Some(acc.map_or(v, |a| a.min(v)))
            })
    }
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "fixture values are exact; no arithmetic involved"
)]
mod tests {
    use super::*;
    use crate::host::HostId;

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_760_000_000 + secs, 0).expect("valid timestamp")
    }

    fn host() -> HostId {
        HostId::new("web-1")
    }

    #[test]
    fn every_metric_name_resolves_back_to_its_kind() {
        for kind in MetricKind::ALL {
            assert_eq!(
                MetricKind::from_otel_name(kind.otel_name()).expect("modelled"),
                kind
            );
        }
    }

    #[test]
    fn metric_names_are_unique() {
        let mut names: Vec<&str> = MetricKind::ALL.iter().map(|k| k.otel_name()).collect();
        names.sort_unstable();
        let count = names.len();
        names.dedup();
        assert_eq!(names.len(), count, "duplicate `OTel` metric name");
    }

    #[test]
    fn system_metrics_use_semconv_names_and_pessimal_metrics_are_namespaced() {
        assert_eq!(
            MetricKind::CpuUtilization.otel_name(),
            "system.cpu.utilization"
        );
        assert!(
            MetricKind::AgentHeartbeat
                .otel_name()
                .starts_with(PESSIMAL_METRIC_NAMESPACE)
        );
    }

    #[test]
    fn rejects_an_unmodelled_metric_name() {
        assert!(matches!(
            MetricKind::from_otel_name("system.paging.faults"),
            Err(CoreError::UnknownMetric(_))
        ));
    }

    #[test]
    fn cumulative_metrics_are_counters_and_the_rest_are_gauges() {
        assert_eq!(
            MetricKind::NetworkIo.instrument_kind(),
            InstrumentKind::Counter,
            "network I/O is cumulative bytes, not a level"
        );
        assert_eq!(
            MetricKind::AgentHeartbeat.instrument_kind(),
            InstrumentKind::Counter
        );
        assert_eq!(
            MetricKind::CpuUtilization.instrument_kind(),
            InstrumentKind::Gauge
        );
        assert_eq!(
            MetricKind::MemoryUsage.instrument_kind(),
            InstrumentKind::Gauge,
            "bytes in use is a level, despite sharing a unit with network I/O"
        );
    }

    #[test]
    fn utilisation_metrics_are_ratios_and_usage_metrics_are_bytes() {
        assert_eq!(MetricKind::CpuUtilization.unit(), MetricUnit::Ratio);
        assert_eq!(MetricKind::MemoryUsage.unit(), MetricUnit::Bytes);
        assert_eq!(MetricUnit::Ratio.otel_unit(), "1");
        assert_eq!(MetricUnit::Bytes.otel_unit(), "By");
    }

    #[test]
    fn time_range_requires_start_before_end() {
        assert!(TimeRange::new(at(0), at(60)).is_ok());
        assert!(TimeRange::new(at(60), at(0)).is_err());
        assert!(TimeRange::new(at(0), at(0)).is_err(), "empty range");
    }

    #[test]
    fn time_range_is_half_open() {
        let range = TimeRange::new(at(0), at(60)).expect("valid");
        assert!(range.contains(at(0)));
        assert!(range.contains(at(59)));
        assert!(!range.contains(at(60)), "end is exclusive");
    }

    #[test]
    fn ending_at_builds_a_window_of_the_requested_length() {
        let range = TimeRange::ending_at(at(600), Duration::minutes(5)).expect("valid");
        assert_eq!(range.end(), at(600));
        assert_eq!(range.duration(), Duration::minutes(5));
        assert!(TimeRange::ending_at(at(600), Duration::zero()).is_err());
    }

    #[test]
    fn series_sorts_points_oldest_first() {
        let series = MetricSeries::new(
            host(),
            MetricKind::CpuUtilization,
            vec![
                MetricPoint::new(at(120), 0.3),
                MetricPoint::new(at(0), 0.1),
                MetricPoint::new(at(60), 0.2),
            ],
        );
        let times: Vec<_> = series.points().iter().map(|p| p.at).collect();
        assert_eq!(times, vec![at(0), at(60), at(120)]);
        assert_eq!(series.latest().expect("non-empty").value, 0.3);
    }

    #[test]
    fn latest_at_ignores_points_after_the_instant() {
        let series = MetricSeries::new(
            host(),
            MetricKind::CpuUtilization,
            vec![
                MetricPoint::new(at(0), 0.1),
                MetricPoint::new(at(60), 0.2),
                MetricPoint::new(at(120), 0.3),
            ],
        );
        assert_eq!(series.latest_at(at(90)).expect("has point").value, 0.2);
        assert_eq!(series.latest_at(at(60)).expect("has point").value, 0.2);
        assert!(series.latest_at(at(-1)).is_none());
    }

    #[test]
    fn aggregates_are_none_for_an_empty_series() {
        let series = MetricSeries::new(host(), MetricKind::CpuUtilization, vec![]);
        assert!(series.is_empty());
        assert!(series.latest().is_none());
        assert!(series.mean().is_none());
        assert!(series.min().is_none());
        assert!(series.max().is_none());
    }

    #[test]
    fn aggregates_summarise_the_points() {
        let series = MetricSeries::new(
            host(),
            MetricKind::CpuUtilization,
            vec![
                MetricPoint::new(at(0), 0.2),
                MetricPoint::new(at(60), 0.4),
                MetricPoint::new(at(120), 0.6),
            ],
        );
        assert!((series.mean().expect("non-empty") - 0.4).abs() < f64::EPSILON);
        assert!((series.min().expect("non-empty") - 0.2).abs() < f64::EPSILON);
        assert!((series.max().expect("non-empty") - 0.6).abs() < f64::EPSILON);
    }

    #[test]
    fn within_filters_to_the_range() {
        let series = MetricSeries::new(
            host(),
            MetricKind::CpuUtilization,
            vec![
                MetricPoint::new(at(0), 0.1),
                MetricPoint::new(at(60), 0.2),
                MetricPoint::new(at(120), 0.3),
            ],
        );
        let range = TimeRange::new(at(60), at(120)).expect("valid");
        let values: Vec<f64> = series.within(range).iter().map(|p| p.value).collect();
        assert_eq!(values, vec![0.2]);
    }
}
