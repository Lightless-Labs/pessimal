//! Mapping Pessimal's metric model onto the names a given SigNoz instance stores.
//!
//! SigNoz normalised metric and attribute names to underscores until v0.88 (`system_cpu_utilization`,
//! `host_name`) and uses dot-based OpenTelemetry semantic-convention names after it
//! (`system.cpu.utilization`, `host.name`). Both are still in the wild, and querying with the wrong
//! one returns an empty result rather than an error — so this is a setting, not an assumption.

use pessimal_core::{InstrumentKind, MetricKind};
use serde::{Deserialize, Serialize};

/// The `host.name` resource attribute, in dotted form. The join key between what an agent exports
/// and what a client queries.
pub const HOST_NAME_ATTRIBUTE: &str = "host.name";
/// The `os.type` resource attribute, in dotted form.
pub const OS_TYPE_ATTRIBUTE: &str = "os.type";
/// The `service.version` resource attribute, in dotted form.
pub const SERVICE_VERSION_ATTRIBUTE: &str = "service.version";

/// Which naming convention an instance uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetricNaming {
    /// SigNoz v0.88 and later, and SigNoz Cloud: names are left as the semantic conventions
    /// define them.
    #[default]
    Dotted,
    /// SigNoz before v0.88: dots are normalised to underscores.
    Underscored,
}

impl MetricNaming {
    /// The instrument name to query this metric by.
    #[must_use]
    pub fn metric_name(self, kind: MetricKind) -> String {
        self.apply(kind.otel_name())
    }

    /// The attribute key to group or filter by.
    #[must_use]
    pub fn attribute(self, dotted: &str) -> String {
        self.apply(dotted)
    }

    fn apply(self, dotted: &str) -> String {
        match self {
            Self::Dotted => dotted.to_owned(),
            Self::Underscored => dotted.replace(['.', '/', '-'], "_"),
        }
    }

    /// The dotted spelling of an attribute key a response came back under.
    ///
    /// SigNoz answers keyed the way it stores things, so a pre-v0.88 instance returns `host_name`
    /// for the `host_name` a query asked for — while everything below this adapter looks an
    /// attribute up by its semantic-convention name. Without this the mount point, the interface
    /// and the sensor reach the client under keys nothing reads, and every series loses its label
    /// with no error anywhere.
    ///
    /// The mapping is a lookup over `queried_attributes`, not a substitution:
    /// `network_io_direction` is as good a reading of `network.io_direction` as of
    /// `network.io.direction`, so a key Pessimal never asked for is left exactly as it arrived.
    #[must_use]
    pub fn to_dotted(self, returned: &str) -> String {
        match self {
            Self::Dotted => returned.to_owned(),
            Self::Underscored => queried_attributes()
                .find(|dotted| self.apply(dotted) == returned)
                .map_or_else(|| returned.to_owned(), str::to_owned),
        }
    }

    /// The attribute keys a query must group by to preserve the distinctions the agent exported.
    ///
    /// Grouping only by host would silently collapse every mount point into one filesystem series
    /// and both directions into one network series — an average of `/` and `/data` that describes
    /// neither.
    #[must_use]
    pub fn group_by(self, kind: MetricKind) -> Vec<String> {
        let mut keys = vec![self.attribute(HOST_NAME_ATTRIBUTE)];
        for extra in extra_dimensions(kind) {
            keys.push(self.attribute(extra));
        }
        keys
    }
}

/// The attributes, beyond the host, that distinguish two series of the same metric. Mirrors what
/// `pessimal_agent_host` attaches when it collects.
#[must_use]
pub fn extra_dimensions(kind: MetricKind) -> &'static [&'static str] {
    match kind {
        MetricKind::FilesystemUsage | MetricKind::FilesystemUtilization => {
            &["system.filesystem.mountpoint"]
        }
        MetricKind::NetworkIo => &["network.io.direction", "network.interface.name"],
        MetricKind::MemoryUsage => &["system.memory.state"],
        // Both, because the agent attaches both: `hw.id` is the identity the conventions require
        // and `hw.name` the display name they recommend.
        MetricKind::Temperature => &["hw.id", "hw.name"],
        _ => &[],
    }
}

/// Every dotted attribute key a query names: the resource attributes `list_hosts` groups by, and
/// every dimension a metric is split on. [`MetricNaming::to_dotted`] inverts a response against
/// this set, so a dimension added to `extra_dimensions` is invertible without a second list to
/// keep in step.
fn queried_attributes() -> impl Iterator<Item = &'static str> {
    [
        HOST_NAME_ATTRIBUTE,
        OS_TYPE_ATTRIBUTE,
        SERVICE_VERSION_ATTRIBUTE,
    ]
    .into_iter()
    .chain(
        MetricKind::ALL
            .into_iter()
            .flat_map(|kind| extra_dimensions(kind).iter().copied()),
    )
}

/// How to aggregate a metric over time and across series.
///
/// A gauge is averaged: it is a level, and the mean over a step is the honest summary.
///
/// A counter takes `max`, which for a monotonically increasing total is the same as its latest
/// value in the bucket — and `max` is accepted for every metric type, where `latest` on a
/// cumulative sum is not something we have confirmed SigNoz allows. That matters more than it
/// looks: `check_connection` queries the heartbeat, so a rejected aggregation would fail the "test
/// connection" button on a perfectly good connection.
/// The temporality a metric must be queried with.
///
/// This is not cosmetic and not optional. A cumulative sum queried as `Unspecified` returns
/// `aggregations: null` — no error, no warning, just nothing — so the whole fleet reads as absent.
/// Verified against a live SigNoz Cloud instance: every gauge returns data only under
/// `Unspecified`, and every counter only under `Cumulative`.
#[must_use]
pub fn temporality_for(kind: MetricKind) -> &'static str {
    match kind.instrument_kind() {
        // A gauge has no temporality; SigNoz stores it as unspecified and matches it literally.
        InstrumentKind::Gauge => "Unspecified",
        // Every counter the agent exports is a cumulative OTLP Sum.
        InstrumentKind::Counter => "Cumulative",
    }
}

#[must_use]
pub fn aggregation_for(kind: MetricKind) -> (&'static str, &'static str) {
    match kind.instrument_kind() {
        InstrumentKind::Gauge => ("avg", "avg"),
        InstrumentKind::Counter => ("max", "max"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dotted_naming_passes_semconv_names_through() {
        assert_eq!(
            MetricNaming::Dotted.metric_name(MetricKind::CpuUtilization),
            "system.cpu.utilization"
        );
        assert_eq!(
            MetricNaming::Dotted.attribute(HOST_NAME_ATTRIBUTE),
            "host.name"
        );
    }

    #[test]
    fn underscored_naming_normalises_every_separator() {
        assert_eq!(
            MetricNaming::Underscored.metric_name(MetricKind::CpuUtilization),
            "system_cpu_utilization"
        );
        assert_eq!(
            MetricNaming::Underscored.attribute(HOST_NAME_ATTRIBUTE),
            "host_name"
        );
        assert_eq!(
            MetricNaming::Underscored.metric_name(MetricKind::AgentCollectionFailures),
            "pessimal_agent_collection_failures"
        );
    }

    #[test]
    fn dotted_is_the_default_because_it_is_what_current_signoz_uses() {
        assert_eq!(MetricNaming::default(), MetricNaming::Dotted);
    }

    #[test]
    fn every_metric_groups_by_host() {
        for kind in MetricKind::ALL {
            assert_eq!(
                MetricNaming::Dotted
                    .group_by(kind)
                    .first()
                    .map(String::as_str),
                Some("host.name"),
                "{kind} must be grouped by host"
            );
        }
    }

    #[test]
    fn filesystem_and_network_keep_the_dimensions_the_agent_exported() {
        assert!(
            MetricNaming::Dotted
                .group_by(MetricKind::FilesystemUsage)
                .contains(&"system.filesystem.mountpoint".to_owned()),
            "collapsing mount points would average / and /data into a number describing neither"
        );
        assert!(
            MetricNaming::Dotted
                .group_by(MetricKind::NetworkIo)
                .contains(&"network.io.direction".to_owned())
        );
    }

    #[test]
    fn group_by_keys_are_normalised_along_with_the_metric_name() {
        let keys = MetricNaming::Underscored.group_by(MetricKind::FilesystemUsage);
        assert!(keys.contains(&"system_filesystem_mountpoint".to_owned()));
        assert!(!keys.iter().any(|k| k.contains('.')));
    }

    #[test]
    fn temperature_keeps_the_sensor_the_agent_exported() {
        let keys = MetricNaming::Dotted.group_by(MetricKind::Temperature);
        assert!(
            keys.contains(&"hw.id".to_owned()),
            "collapsing sensors would average the CPU die and the battery into one number"
        );
        assert!(keys.contains(&"hw.name".to_owned()));
    }

    #[test]
    fn an_underscored_key_comes_back_under_the_name_the_rest_of_pessimal_uses() {
        let underscored = MetricNaming::Underscored;
        assert_eq!(underscored.to_dotted("host_name"), "host.name");
        assert_eq!(
            underscored.to_dotted("system_filesystem_mountpoint"),
            "system.filesystem.mountpoint"
        );
        assert_eq!(underscored.to_dotted("hw_name"), "hw.name");
    }

    #[test]
    fn a_key_pessimal_never_asked_for_is_left_exactly_as_it_arrived() {
        // Underscores are not invertible in general: `foo_bar` may be a name with an underscore
        // in it. Only the keys a query grouped by are re-spelled.
        assert_eq!(
            MetricNaming::Underscored.to_dotted("deployment_environment"),
            "deployment_environment"
        );
        assert_eq!(MetricNaming::Dotted.to_dotted("host_name"), "host_name");
    }

    #[test]
    fn no_two_queried_attributes_share_an_underscored_spelling() {
        // The inverse above is a lookup over this set, so two attributes sharing a spelling would
        // re-spell one of them as the other rather than leave it alone. The set repeats keys —
        // both filesystem metrics carry the mount point — so distinct keys are what is counted.
        let mut dotted: Vec<&str> = queried_attributes().collect();
        dotted.sort_unstable();
        dotted.dedup();
        let mut spellings: Vec<String> = dotted
            .iter()
            .map(|key| MetricNaming::Underscored.attribute(key))
            .collect();
        spellings.sort_unstable();
        spellings.dedup();
        assert_eq!(spellings.len(), dotted.len(), "{dotted:?} -> {spellings:?}");
    }

    #[test]
    fn gauges_are_queried_as_unspecified_and_counters_as_cumulative() {
        // Verified against a live instance: the wrong one returns no series at all, silently.
        assert_eq!(temporality_for(MetricKind::CpuUtilization), "Unspecified");
        assert_eq!(temporality_for(MetricKind::MemoryUsage), "Unspecified");
        assert_eq!(temporality_for(MetricKind::NetworkIo), "Cumulative");
        assert_eq!(temporality_for(MetricKind::AgentHeartbeat), "Cumulative");
        assert_eq!(
            temporality_for(MetricKind::AgentCollectionFailures),
            "Cumulative"
        );
    }

    #[test]
    fn every_metric_has_a_temporality_matching_its_instrument_kind() {
        for kind in MetricKind::ALL {
            let expected = match kind.instrument_kind() {
                InstrumentKind::Gauge => "Unspecified",
                InstrumentKind::Counter => "Cumulative",
            };
            assert_eq!(temporality_for(kind), expected, "{kind}");
        }
    }

    #[test]
    fn counters_take_their_maximum_and_gauges_are_averaged() {
        assert_eq!(aggregation_for(MetricKind::CpuUtilization), ("avg", "avg"));
        assert_eq!(aggregation_for(MetricKind::NetworkIo), ("max", "max"));
        assert_eq!(aggregation_for(MetricKind::AgentHeartbeat), ("max", "max"));
    }

    #[test]
    fn no_metric_uses_an_aggregation_signoz_might_reject_on_a_cumulative_sum() {
        for kind in MetricKind::ALL {
            let (time, space) = aggregation_for(kind);
            assert!(
                ["avg", "max", "min", "sum", "count"].contains(&time),
                "{kind} uses time aggregation {time}"
            );
            assert!(
                ["avg", "max", "min", "sum", "count"].contains(&space),
                "{kind} uses space aggregation {space}"
            );
        }
    }
}
