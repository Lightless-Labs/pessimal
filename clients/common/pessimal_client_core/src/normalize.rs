//! Sample arithmetic: counter to rate or delta, and several attribute-set series to one.
//!
//! This is the layer that makes a cumulative counter mean something. The SigNoz adapter
//! aggregates counters `("max", "max")` (see `naming.rs::aggregation_for`), which on a cumulative
//! sum returns the running total — charted raw it is a line that only ever goes up, and alerted
//! raw a `>` rule latches forever after the first breach. Nothing below this crate differences
//! them, so it happens here or not at all.
//!
//! Two properties are load-bearing and neither is obvious from the outside:
//!
//! 1. **A counter reset never yields a negative value.** An agent restart zeroes
//!    `pessimal.agent.*`, and the host-total `system.network.io` sum falls when an interface
//!    leaves. The differencing functions drop that point rather than emitting a negative rate or
//!    (worse) a positive spike from a wrapped subtraction. [`counter_resets`] reports where it
//!    happened so a chart can mark the gap.
//! 2. **[`reduce`] emits ascending points.** [`pessimal_core::MetricSeries::latest_at`] scans in
//!    reverse and [`pessimal_core::MetricSeries::latest`] reads the last element, so an unsorted
//!    series does not fail — it silently returns the wrong sample, and that sample is what alert
//!    evaluation judges. Core carries the same warning on `MetricSeriesWire` because it had
//!    exactly that bug via unsorted deserialisation. Everything built here goes through
//!    `MetricSeries::new`, which sorts.
//!
//! Everything in this module is a pure function of its arguments. Nothing reads a clock.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use pessimal_core::{Comparator, HostId, MetricKind, MetricPoint, MetricSeries};
use serde::{Deserialize, Serialize};

/// The attribute a per-mount filesystem series is split by, as the agent attaches it and the
/// adapter groups by it (`naming.rs::extra_dimensions`).
const FILESYSTEM_MOUNTPOINT: &str = "system.filesystem.mountpoint";
const NETWORK_INTERFACE_NAME: &str = "network.interface.name";
const NETWORK_IO_DIRECTION: &str = "network.io.direction";
const MEMORY_STATE: &str = "system.memory.state";

/// What a metric's raw samples have to become before anything reads them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Normalization {
    /// Already a level: chart and alert on the value as it arrived.
    None,
    /// Cumulative bytes to bytes per second.
    RatePerSecond,
    /// Cumulative count to the count added in this bucket.
    Delta,
}

/// Which way several attribute-set series collapse into one.
///
/// There is deliberately no `Sum`. A sum needs exact timestamp alignment across label sets and
/// silently under-reports whenever one set is missing a bucket, so a `>` rule quietly fails to
/// fire — the one failure mode an alert must not have. `Max` degrades safely, and "disk above
/// 90 % on any mount" is what a rule means anyway.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Reduction {
    Max,
    Min,
}

/// The arithmetic a metric's samples need before they are charted or judged.
///
/// `AgentHeartbeat` is a counter and still gets [`Normalization::None`]: liveness reads the
/// heartbeat through `list_hosts` bucket timestamps only, never its value, so differencing it
/// would cost work and buy nothing.
#[must_use]
pub fn normalization_for(kind: MetricKind) -> Normalization {
    match kind {
        MetricKind::NetworkIo => Normalization::RatePerSecond,
        MetricKind::AgentCollectionFailures => Normalization::Delta,
        _ => Normalization::None,
    }
}

/// Whether this metric's charted values are a rate rather than a level.
///
/// The apps need it for the axis label and the unit suffix: after [`normalize`], `NetworkIo` is
/// bytes per second, not bytes.
#[must_use]
pub fn is_rate(kind: MetricKind) -> bool {
    !matches!(normalization_for(kind), Normalization::None)
}

/// Differences consecutive points and divides by the elapsed seconds between them, stamping each
/// result at the later of the two timestamps.
///
/// A negative difference is a counter reset and its point is **dropped**, never emitted as a
/// negative or as a spike. N points in, at most N-1 out; a one-point series yields zero points,
/// which `AlertEvaluation::observe` reads as `NoData` — the honest answer, since one cumulative
/// sample carries no rate at all.
///
/// The input's `attributes` are carried through: they are the mount or interface identity that
/// [`reduce`], [`dominant_at`], [`series_label`] and `PollObservation::series_for`'s dedup all
/// key on, and dropping them here would silently merge every mount into one.
#[must_use]
pub fn rate_per_second(series: &MetricSeries) -> MetricSeries {
    differenced(series, true)
}

/// Differences consecutive points, stamping each result at the later of the two timestamps.
///
/// Per bucket, not per second: "three collection failures since the last sample" is what the
/// number means. Reset handling is identical to [`rate_per_second`]'s: a fall is dropped here too.
#[must_use]
pub fn delta(series: &MetricSeries) -> MetricSeries {
    differenced(series, false)
}

/// Applies [`normalization_for`] to a series' own kind. Gauges pass through untouched.
#[must_use]
pub fn normalize(series: &MetricSeries) -> MetricSeries {
    match normalization_for(series.kind) {
        Normalization::None => series.clone(),
        Normalization::RatePerSecond => rate_per_second(series),
        Normalization::Delta => delta(series),
    }
}

/// The timestamps at which a counter reset was detected and a point dropped, so a chart can mark
/// the gap rather than drawing a straight line across an agent restart.
///
/// Call this on the **raw** series, before [`normalize`]. Called on an already-normalised series
/// it would report every ordinary decrease in rate as a restart.
///
/// Empty for every metric whose [`normalization_for`] is [`Normalization::None`], including
/// `AgentHeartbeat`: a gauge going down is a gauge going down.
#[must_use]
pub fn counter_resets(series: &MetricSeries) -> Vec<DateTime<Utc>> {
    if matches!(normalization_for(series.kind), Normalization::None) {
        return Vec::new();
    }
    series
        .points()
        .windows(2)
        .filter_map(|pair| {
            let [previous, current] = pair else {
                return None;
            };
            (current.value < previous.value).then_some(current.at)
        })
        .collect()
}

/// Which way a rule's comparator wants several series collapsed.
///
/// `Max` for `>`/`>=` and `Min` for `<`/`<=` is the direction in which a single breaching label
/// set still breaches the reduced series — the whole point of reducing rather than picking one.
#[must_use]
pub fn reduction_for(comparator: Comparator) -> Reduction {
    match comparator {
        Comparator::GreaterThan | Comparator::GreaterThanOrEqual => Reduction::Max,
        Comparator::LessThan | Comparator::LessThanOrEqual => Reduction::Min,
    }
}

/// Collapses several attribute-set series (mounts, interfaces, memory states) into one synthetic
/// series carrying empty `attributes`.
///
/// For each timestamp present in any input, the value is the reduction across exactly those
/// series holding a point at that same timestamp. SigNoz buckets align to the step, so
/// timestamps coincide; a series missing a bucket simply does not vote in it, which is what makes
/// this safe where a sum would not be.
///
/// `None` for an empty slice — "nobody asked" — as distinct from `Some(empty series)`, which is a
/// real absence of data and legitimately reads as `NoData`.
///
/// Normalise each input **before** reducing. Reducing raw cumulative counters and differencing
/// afterwards fabricates a reset (or a spike) every time the dominant interface changes.
#[must_use]
pub fn reduce(
    kind: MetricKind,
    host: &HostId,
    series: &[MetricSeries],
    reduction: Reduction,
) -> Option<MetricSeries> {
    if series.is_empty() {
        return None;
    }
    // A `BTreeMap` keyed by instant gives ascending order for free, and `MetricSeries::new` sorts
    // again on the way out. Belt and braces on purpose: `latest_at` scans in reverse, so an
    // out-of-order point here is not a crash but a wrong alert.
    let mut reduced: BTreeMap<DateTime<Utc>, f64> = BTreeMap::new();
    for one in series {
        for point in one.points() {
            reduced
                .entry(point.at)
                .and_modify(|value| *value = combine(reduction, *value, point.value))
                .or_insert(point.value);
        }
    }
    let points = reduced
        .into_iter()
        .map(|(at, value)| MetricPoint::new(at, value))
        .collect();
    Some(MetricSeries::new(host.clone(), kind, points))
}

/// Which input series held the reduced value at `at` — the mount that is actually full, so an
/// alert can say "/ (95 %)" rather than "95 %".
///
/// Matches on the exact instant, the same rule [`reduce`] uses, so the label can never name a
/// series that did not produce the value. Pass the `at` of the reduced point the alert judged,
/// not `now`. Ties go to the first series in slice order, which is the plan order
/// `PollObservation::series_for` returns and therefore stable across polls.
#[must_use]
pub fn dominant_at(
    series: &[MetricSeries],
    at: DateTime<Utc>,
    reduction: Reduction,
) -> Option<&MetricSeries> {
    let mut best: Option<(&MetricSeries, f64)> = None;
    for one in series {
        let Some(point) = one.points().iter().find(|point| point.at == at) else {
            continue;
        };
        let wins = match best {
            None => true,
            Some((_, incumbent)) => match reduction {
                Reduction::Max => point.value > incumbent,
                Reduction::Min => point.value < incumbent,
            },
        };
        if wins {
            best = Some((one, point.value));
        }
    }
    best.map(|(winner, _)| winner)
}

/// A short human label from a series' attributes, or `None` for a host-wide series.
///
/// Only the dimensions the agent actually splits on (`naming.rs::extra_dimensions`) are
/// recognised, in the order a reader would name them: the mount, then the interface and
/// direction, then the memory state.
#[must_use]
pub fn series_label(series: &MetricSeries) -> Option<String> {
    let attributes = &series.attributes;
    if let Some(mountpoint) = attributes.get(FILESYSTEM_MOUNTPOINT) {
        return Some(mountpoint.clone());
    }
    match (
        attributes.get(NETWORK_INTERFACE_NAME),
        attributes.get(NETWORK_IO_DIRECTION),
    ) {
        (Some(interface), Some(direction)) => return Some(format!("{interface} {direction}")),
        (Some(only), None) | (None, Some(only)) => return Some(only.clone()),
        (None, None) => {}
    }
    attributes.get(MEMORY_STATE).cloned()
}

/// A stable `ForEach` identity: `"{host}|{otel_name}|{k=v,…}"` over the sorted attribute map.
///
/// Stable across polls and unique per attribute set, which is what keeps a `SwiftUI` list from
/// re-animating every row when a mount appears or an interface goes away. `BTreeMap` iteration is
/// sorted, so two maps with the same pairs produce the same id whatever order they were built in.
#[must_use]
pub fn series_id(host: &HostId, kind: MetricKind, attributes: &BTreeMap<String, String>) -> String {
    let pairs: Vec<String> = attributes
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect();
    format!("{host}|{}|{}", kind.otel_name(), pairs.join(","))
}

/// The shared body of [`rate_per_second`] and [`delta`]: the reset rule and the attribute
/// carry-through must not drift apart between them.
fn differenced(series: &MetricSeries, per_second: bool) -> MetricSeries {
    let mut points = Vec::with_capacity(series.points().len().saturating_sub(1));
    for pair in series.points().windows(2) {
        let [previous, current] = pair else { continue };
        // A fall is a restart, not negative traffic. Note the previous point still advances to
        // `current`, so the first bucket after the restart differences against the new baseline
        // and the series resumes on the very next sample.
        if current.value < previous.value {
            continue;
        }
        let added = current.value - previous.value;
        let value = if per_second {
            // Duplicate timestamps would divide by zero and poison every downstream comparison
            // with an infinity. Points are sorted, so this is the only way elapsed is not
            // positive, and it is not a reset — it is two samples of the same instant.
            let Ok(elapsed) = (current.at - previous.at).to_std() else {
                continue;
            };
            let seconds = elapsed.as_secs_f64();
            if seconds <= 0.0 {
                continue;
            }
            added / seconds
        } else {
            added
        };
        points.push(MetricPoint::new(current.at, value));
    }
    MetricSeries::new(series.host.clone(), series.kind, points)
        .with_attributes(series.attributes.clone())
}

/// Not `Reduction::apply`: the direction rule is this module's business and nothing outside it
/// should be folding values by hand.
fn combine(reduction: Reduction, left: f64, right: f64) -> f64 {
    match reduction {
        Reduction::Max => left.max(right),
        Reduction::Min => left.min(right),
    }
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "fixtures divide exactly in binary floating point; no accumulated error"
)]
mod tests {
    use super::*;

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_760_000_000 + secs, 0).expect("valid timestamp")
    }

    fn host() -> HostId {
        HostId::new("web-1")
    }

    fn series(kind: MetricKind, points: Vec<(i64, f64)>) -> MetricSeries {
        MetricSeries::new(
            host(),
            kind,
            points
                .into_iter()
                .map(|(secs, value)| MetricPoint::new(at(secs), value))
                .collect(),
        )
    }

    fn mount(path: &str) -> BTreeMap<String, String> {
        let mut attributes = BTreeMap::new();
        attributes.insert(FILESYSTEM_MOUNTPOINT.to_owned(), path.to_owned());
        attributes
    }

    fn values(series: &MetricSeries) -> Vec<(DateTime<Utc>, f64)> {
        series.points().iter().map(|p| (p.at, p.value)).collect()
    }

    #[test]
    fn a_counter_reset_yields_no_negative_rate() {
        // An agent restart at t=120 drops the cumulative total from 1120 back to 100.
        let raw = series(
            MetricKind::NetworkIo,
            vec![(0, 1000.0), (60, 1120.0), (120, 100.0), (180, 220.0)],
        );
        let rates = normalize(&raw);
        assert_eq!(
            values(&rates),
            vec![(at(60), 2.0), (at(180), 2.0)],
            "the reset bucket is dropped and the next one differences against the new baseline"
        );
        assert!(
            rates.points().iter().all(|p| p.value >= 0.0),
            "a restart must never render as negative traffic"
        );
    }

    #[test]
    fn a_counter_reset_is_recorded_for_the_chart() {
        let raw = series(
            MetricKind::NetworkIo,
            vec![(0, 1000.0), (60, 1120.0), (120, 100.0), (180, 220.0)],
        );
        assert_eq!(counter_resets(&raw), vec![at(120)]);
        assert_eq!(
            counter_resets(&series(
                MetricKind::CpuUtilization,
                vec![(0, 0.9), (60, 0.1)]
            )),
            Vec::<DateTime<Utc>>::new(),
            "a gauge going down is a gauge going down"
        );
        assert_eq!(
            counter_resets(&series(
                MetricKind::AgentHeartbeat,
                vec![(0, 9.0), (60, 1.0)]
            )),
            Vec::<DateTime<Utc>>::new(),
            "the heartbeat's value is never read, so it is never reset-tracked"
        );
    }

    #[test]
    fn a_one_point_counter_series_normalizes_to_no_points() {
        let normalized = normalize(&series(MetricKind::NetworkIo, vec![(0, 1000.0)]));
        assert!(
            normalized.is_empty(),
            "one cumulative sample carries no rate; `observe` should read `NoData`"
        );
    }

    #[test]
    fn delta_is_per_bucket_and_rate_is_per_second() {
        let raw = series(
            MetricKind::AgentCollectionFailures,
            vec![(0, 4.0), (60, 7.0)],
        );
        assert_eq!(values(&normalize(&raw)), vec![(at(60), 3.0)]);
        assert_eq!(values(&rate_per_second(&raw)), vec![(at(60), 0.05)]);
    }

    #[test]
    fn a_gauge_normalizes_to_itself() {
        let gauge = series(MetricKind::CpuUtilization, vec![(0, 0.4), (60, 0.2)]);
        assert_eq!(normalize(&gauge), gauge);
    }

    #[test]
    fn normalization_keeps_the_attribute_set() {
        let raw = series(MetricKind::NetworkIo, vec![(0, 100.0), (60, 220.0)])
            .with_attributes(mount("/data"));
        assert_eq!(
            normalize(&raw).attributes,
            mount("/data"),
            "the label identity is what `reduce` and `series_for`'s dedup key on"
        );
    }

    #[test]
    fn network_io_is_a_rate_and_the_heartbeat_is_not() {
        assert!(is_rate(MetricKind::NetworkIo));
        assert!(is_rate(MetricKind::AgentCollectionFailures));
        assert!(!is_rate(MetricKind::CpuUtilization));
        assert!(
            !is_rate(MetricKind::AgentHeartbeat),
            "a counter whose value nothing reads"
        );
    }

    #[test]
    fn reduction_follows_the_comparator_direction() {
        assert_eq!(reduction_for(Comparator::GreaterThan), Reduction::Max);
        assert_eq!(
            reduction_for(Comparator::GreaterThanOrEqual),
            Reduction::Max
        );
        assert_eq!(reduction_for(Comparator::LessThan), Reduction::Min);
        assert_eq!(reduction_for(Comparator::LessThanOrEqual), Reduction::Min);
    }

    #[test]
    fn two_mounts_reduce_to_the_higher_under_max_and_the_lower_under_min() {
        let mounts = vec![
            series(MetricKind::FilesystemUtilization, vec![(0, 0.5)]).with_attributes(mount("/")),
            series(MetricKind::FilesystemUtilization, vec![(0, 0.9)])
                .with_attributes(mount("/data")),
        ];
        let highest = reduce(
            MetricKind::FilesystemUtilization,
            &host(),
            &mounts,
            Reduction::Max,
        )
        .expect("non-empty slice");
        assert_eq!(
            values(&highest),
            vec![(at(0), 0.9)],
            "disk full on any mount"
        );
        assert!(
            highest.attributes.is_empty(),
            "the synthetic series belongs to no mount; the label comes from `dominant_at`"
        );

        let lowest = reduce(
            MetricKind::FilesystemUtilization,
            &host(),
            &mounts,
            Reduction::Min,
        )
        .expect("non-empty slice");
        assert_eq!(values(&lowest), vec![(at(0), 0.5)]);
    }

    #[test]
    fn reduce_emits_ascending_points() {
        // Interleaved buckets: a naive concatenation would read t0, t120, t60, t180.
        let inputs = vec![
            series(
                MetricKind::FilesystemUtilization,
                vec![(0, 0.1), (120, 0.3)],
            )
            .with_attributes(mount("/")),
            series(
                MetricKind::FilesystemUtilization,
                vec![(60, 0.2), (180, 0.4)],
            )
            .with_attributes(mount("/data")),
        ];
        let reduced = reduce(
            MetricKind::FilesystemUtilization,
            &host(),
            &inputs,
            Reduction::Max,
        )
        .expect("non-empty slice");

        let times: Vec<DateTime<Utc>> = reduced.points().iter().map(|p| p.at).collect();
        assert_eq!(times, vec![at(0), at(60), at(120), at(180)]);
        assert_eq!(
            reduced
                .latest_at(at(150))
                .expect("a point at or before")
                .value,
            0.3,
            "`latest_at` scans in reverse: unsorted points do not fail, they judge the wrong sample"
        );
    }

    #[test]
    fn reduce_of_an_empty_slice_is_none() {
        assert!(
            reduce(
                MetricKind::FilesystemUtilization,
                &host(),
                &[],
                Reduction::Max
            )
            .is_none(),
            "nobody asked is not the same as nothing came back"
        );
    }

    #[test]
    fn dominant_at_names_the_winning_mount() {
        let mounts = vec![
            series(MetricKind::FilesystemUtilization, vec![(0, 0.5)]).with_attributes(mount("/")),
            series(MetricKind::FilesystemUtilization, vec![(0, 0.9)])
                .with_attributes(mount("/data")),
        ];
        let fullest = dominant_at(&mounts, at(0), Reduction::Max).expect("a point at that instant");
        assert_eq!(series_label(fullest).as_deref(), Some("/data"));

        let emptiest =
            dominant_at(&mounts, at(0), Reduction::Min).expect("a point at that instant");
        assert_eq!(series_label(emptiest).as_deref(), Some("/"));

        assert!(
            dominant_at(&mounts, at(60), Reduction::Max).is_none(),
            "no series holds a point at that instant"
        );
    }

    #[test]
    fn series_label_prefers_the_mountpoint_then_the_interface() {
        let mut attributes = mount("/data");
        attributes.insert(NETWORK_INTERFACE_NAME.to_owned(), "en0".to_owned());
        let both = series(MetricKind::FilesystemUtilization, vec![]).with_attributes(attributes);
        assert_eq!(series_label(&both).as_deref(), Some("/data"));

        let mut network = BTreeMap::new();
        network.insert(NETWORK_INTERFACE_NAME.to_owned(), "en0".to_owned());
        network.insert(NETWORK_IO_DIRECTION.to_owned(), "receive".to_owned());
        let interface = series(MetricKind::NetworkIo, vec![]).with_attributes(network);
        assert_eq!(series_label(&interface).as_deref(), Some("en0 receive"));

        let mut memory = BTreeMap::new();
        memory.insert(MEMORY_STATE.to_owned(), "used".to_owned());
        let state = series(MetricKind::MemoryUsage, vec![]).with_attributes(memory);
        assert_eq!(series_label(&state).as_deref(), Some("used"));

        assert_eq!(
            series_label(&series(MetricKind::CpuUtilization, vec![])),
            None,
            "a host-wide series has nothing to disambiguate"
        );
    }

    #[test]
    fn series_id_is_stable_across_polls_and_unique_per_attribute_set() {
        let mut first = BTreeMap::new();
        first.insert(NETWORK_INTERFACE_NAME.to_owned(), "en0".to_owned());
        first.insert(NETWORK_IO_DIRECTION.to_owned(), "receive".to_owned());
        // The same pairs, inserted the other way round: sorted iteration is what makes the id
        // stable, not the order the backend happened to hand them over in.
        let mut second = BTreeMap::new();
        second.insert(NETWORK_IO_DIRECTION.to_owned(), "receive".to_owned());
        second.insert(NETWORK_INTERFACE_NAME.to_owned(), "en0".to_owned());

        assert_eq!(
            series_id(&host(), MetricKind::NetworkIo, &first),
            series_id(&host(), MetricKind::NetworkIo, &second)
        );
        assert_eq!(
            series_id(&host(), MetricKind::NetworkIo, &first),
            "web-1|system.network.io|network.interface.name=en0,network.io.direction=receive"
        );

        let mut other = first.clone();
        other.insert(NETWORK_IO_DIRECTION.to_owned(), "transmit".to_owned());
        assert_ne!(
            series_id(&host(), MetricKind::NetworkIo, &first),
            series_id(&host(), MetricKind::NetworkIo, &other),
            "two directions on one interface are two rows"
        );
        assert_ne!(
            series_id(&host(), MetricKind::NetworkIo, &first),
            series_id(&host(), MetricKind::MemoryUsage, &first),
            "the metric is part of the identity"
        );
        assert_eq!(
            series_id(&host(), MetricKind::CpuUtilization, &BTreeMap::new()),
            "web-1|system.cpu.utilization|"
        );
    }
}
