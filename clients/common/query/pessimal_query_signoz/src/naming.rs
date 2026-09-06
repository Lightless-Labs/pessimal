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
        _ => &[],
    }
}

/// How to aggregate a metric over time and across series.
///
/// A gauge is averaged: it is a level, and the mean over a step is the honest summary. A counter
/// is read at its latest value, because it is a running total and averaging one is meaningless.
#[must_use]
pub fn aggregation_for(kind: MetricKind) -> (&'static str, &'static str) {
    match kind.instrument_kind() {
        InstrumentKind::Gauge => ("avg", "avg"),
        InstrumentKind::Counter => ("latest", "max"),
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
    fn counters_are_read_at_their_latest_value_and_gauges_are_averaged() {
        assert_eq!(aggregation_for(MetricKind::CpuUtilization), ("avg", "avg"));
        assert_eq!(aggregation_for(MetricKind::NetworkIo), ("latest", "max"));
        assert_eq!(
            aggregation_for(MetricKind::AgentHeartbeat),
            ("latest", "max")
        );
    }
}
