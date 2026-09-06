//! Sampling the host with `sysinfo`.
//!
//! Every value is turned into the shape [`pessimal_core::MetricKind`] declares: ratios in
//! `0.0..=1.0` (not percentages), bytes (not kibibytes), seconds. The backend sees semantic
//! convention names with semantic convention units, so a dashboard written against any other
//! OpenTelemetry host collector keeps working.

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use opentelemetry_semantic_conventions::attribute;
use pessimal_agent_core::collector::{MetricCollector, Observation};
use pessimal_agent_core::config::CollectionConfig;
use pessimal_agent_core::error::Result;
use pessimal_core::MetricKind;
use sysinfo::{Disks, MemoryRefreshKind, Networks, RefreshKind, System};

/// Attribute value for bytes arriving.
const DIRECTION_RECEIVE: &str = "receive";
/// Attribute value for bytes leaving.
const DIRECTION_TRANSMIT: &str = "transmit";

/// Samples the local host.
pub struct HostCollector {
    system: System,
    disks: Disks,
    networks: Networks,
    filesystems: HashSet<String>,
    per_interface_network: bool,
}

impl HostCollector {
    /// Builds a collector and takes a priming sample.
    ///
    /// CPU utilisation is a delta between two refreshes, so the first `collect` after this returns
    /// utilisation measured since construction. That is why the agent primes here rather than
    /// letting the first exported datapoint be meaningless.
    #[must_use]
    pub fn new(config: &CollectionConfig) -> Self {
        let mut system = System::new_with_specifics(
            RefreshKind::nothing()
                .with_cpu(sysinfo::CpuRefreshKind::nothing().with_cpu_usage())
                .with_memory(MemoryRefreshKind::nothing().with_ram()),
        );
        system.refresh_cpu_usage();
        system.refresh_memory();

        Self {
            system,
            disks: Disks::new_with_refreshed_list(),
            networks: Networks::new_with_refreshed_list(),
            filesystems: config.filesystems.iter().cloned().collect(),
            per_interface_network: config.per_interface_network,
        }
    }

    /// Whether a mount point should be reported. An empty configured set means all of them.
    fn reports_filesystem(&self, mount_point: &str) -> bool {
        self.filesystems.is_empty() || self.filesystems.contains(mount_point)
    }

    fn cpu(&mut self) -> Vec<Observation> {
        self.system.refresh_cpu_usage();
        // sysinfo reports 0..=100; semconv wants a 0..=1 ratio.
        let ratio = f64::from(self.system.global_cpu_usage()) / 100.0;
        vec![Observation::new(
            MetricKind::CpuUtilization,
            ratio.clamp(0.0, 1.0),
        )]
    }

    fn memory(&mut self) -> Vec<Observation> {
        self.system.refresh_memory();
        let total = self.system.total_memory();
        let used = self.system.used_memory();

        let mut observations = vec![Observation::new(
            MetricKind::MemoryUsage,
            round_to_f64(used),
        )];
        if total > 0 {
            observations.push(Observation::new(
                MetricKind::MemoryUtilization,
                (round_to_f64(used) / round_to_f64(total)).clamp(0.0, 1.0),
            ));
        }
        observations
    }

    fn filesystems(&mut self) -> Vec<Observation> {
        self.disks.refresh(true);
        let mut observations = Vec::new();

        for disk in self.disks.list() {
            let mount_point = disk.mount_point().to_string_lossy().into_owned();
            if !self.reports_filesystem(&mount_point) {
                continue;
            }
            let total = disk.total_space();
            if total == 0 {
                continue;
            }
            let used = total.saturating_sub(disk.available_space());

            observations.push(
                Observation::new(MetricKind::FilesystemUsage, round_to_f64(used))
                    .with_attribute(attribute::SYSTEM_FILESYSTEM_MOUNTPOINT, &mount_point),
            );
            observations.push(
                Observation::new(
                    MetricKind::FilesystemUtilization,
                    (round_to_f64(used) / round_to_f64(total)).clamp(0.0, 1.0),
                )
                .with_attribute(attribute::SYSTEM_FILESYSTEM_MOUNTPOINT, &mount_point),
            );
        }
        observations
    }

    fn network(&mut self) -> Vec<Observation> {
        self.networks.refresh(true);
        let mut observations = Vec::new();

        if self.per_interface_network {
            for (name, data) in self.networks.list() {
                observations.push(
                    Observation::new(MetricKind::NetworkIo, round_to_f64(data.total_received()))
                        .with_attribute(attribute::NETWORK_INTERFACE_NAME, name)
                        .with_attribute(attribute::NETWORK_IO_DIRECTION, DIRECTION_RECEIVE),
                );
                observations.push(
                    Observation::new(
                        MetricKind::NetworkIo,
                        round_to_f64(data.total_transmitted()),
                    )
                    .with_attribute(attribute::NETWORK_INTERFACE_NAME, name)
                    .with_attribute(attribute::NETWORK_IO_DIRECTION, DIRECTION_TRANSMIT),
                );
            }
        } else {
            let (received, transmitted) = self.networks.list().values().fold(
                (0_u64, 0_u64),
                |(received, transmitted), data| {
                    (
                        received.saturating_add(data.total_received()),
                        transmitted.saturating_add(data.total_transmitted()),
                    )
                },
            );
            observations.push(
                Observation::new(MetricKind::NetworkIo, round_to_f64(received))
                    .with_attribute(attribute::NETWORK_IO_DIRECTION, DIRECTION_RECEIVE),
            );
            observations.push(
                Observation::new(MetricKind::NetworkIo, round_to_f64(transmitted))
                    .with_attribute(attribute::NETWORK_IO_DIRECTION, DIRECTION_TRANSMIT),
            );
        }
        observations
    }

    /// Load average, where the platform has one. Windows does not, and `sysinfo` returns zeroes
    /// there — reporting those as real would put a flat zero line on every Windows host's chart,
    /// so they are omitted instead.
    fn load_average() -> Vec<Observation> {
        if cfg!(windows) {
            return Vec::new();
        }
        let load = System::load_average();
        vec![
            Observation::new(MetricKind::LoadAverage1m, load.one),
            Observation::new(MetricKind::LoadAverage5m, load.five),
            Observation::new(MetricKind::LoadAverage15m, load.fifteen),
        ]
    }

    fn uptime() -> Vec<Observation> {
        vec![Observation::new(
            MetricKind::SystemUptime,
            round_to_f64(System::uptime()),
        )]
    }
}

/// Widens a byte or second count to `f64`.
///
/// Above 2^53 this loses precision, which for a byte counter means petabyte-scale totals resolving
/// to the nearest kilobyte. That is well inside the noise for this use.
#[allow(clippy::cast_precision_loss)]
fn round_to_f64(value: u64) -> f64 {
    value as f64
}

impl MetricCollector for HostCollector {
    fn collect(&mut self, _now: DateTime<Utc>) -> Result<Vec<Observation>> {
        let mut observations = Vec::new();
        observations.extend(self.cpu());
        observations.extend(self.memory());
        observations.extend(self.filesystems());
        observations.extend(self.network());
        observations.extend(Self::load_average());
        observations.extend(Self::uptime());
        Ok(observations)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(config: &CollectionConfig) -> Vec<Observation> {
        let mut collector = HostCollector::new(config);
        collector
            .collect(Utc::now())
            .expect("sampling the local host")
    }

    fn values_of(observations: &[Observation], kind: MetricKind) -> Vec<f64> {
        observations
            .iter()
            .filter(|o| o.kind == kind)
            .map(|o| o.value)
            .collect()
    }

    #[test]
    fn samples_the_local_host() {
        let observations = sample(&CollectionConfig::default());
        assert!(!observations.is_empty());
    }

    #[test]
    fn reports_cpu_and_memory_as_ratios_in_range() {
        let observations = sample(&CollectionConfig::default());

        for kind in [
            MetricKind::CpuUtilization,
            MetricKind::MemoryUtilization,
            MetricKind::FilesystemUtilization,
        ] {
            for value in values_of(&observations, kind) {
                assert!(
                    (0.0..=1.0).contains(&value),
                    "{kind} reported {value}, which is not a ratio"
                );
            }
        }
    }

    #[test]
    fn reports_memory_in_bytes_not_kibibytes() {
        let used = values_of(
            &sample(&CollectionConfig::default()),
            MetricKind::MemoryUsage,
        );
        let used = used.first().copied().expect("memory is always reported");
        assert!(
            used > 1_000_000.0,
            "{used} looks like KiB, not bytes; every host running this has more than a megabyte in use"
        );
    }

    #[test]
    fn reports_uptime_in_seconds() {
        let uptime = values_of(
            &sample(&CollectionConfig::default()),
            MetricKind::SystemUptime,
        );
        assert_eq!(uptime.len(), 1);
        assert!(uptime[0] > 0.0);
    }

    #[test]
    fn reports_network_io_split_by_direction() {
        let observations = sample(&CollectionConfig::default());
        let network: Vec<&Observation> = observations
            .iter()
            .filter(|o| o.kind == MetricKind::NetworkIo)
            .collect();

        assert_eq!(network.len(), 2, "one total per direction");
        let directions: HashSet<&str> = network
            .iter()
            .filter_map(|o| o.attributes.get(attribute::NETWORK_IO_DIRECTION))
            .map(String::as_str)
            .collect();
        assert_eq!(
            directions,
            HashSet::from([DIRECTION_RECEIVE, DIRECTION_TRANSMIT])
        );
    }

    #[test]
    fn per_interface_network_labels_each_interface() {
        let config = CollectionConfig {
            per_interface_network: true,
            ..CollectionConfig::default()
        };
        let observations = sample(&config);
        for observation in observations
            .iter()
            .filter(|o| o.kind == MetricKind::NetworkIo)
        {
            assert!(
                observation
                    .attributes
                    .contains_key(attribute::NETWORK_INTERFACE_NAME),
                "per-interface mode must name the interface"
            );
        }
    }

    #[test]
    fn filesystem_observations_name_their_mount_point() {
        let observations = sample(&CollectionConfig::default());
        for observation in observations.iter().filter(|o| {
            matches!(
                o.kind,
                MetricKind::FilesystemUsage | MetricKind::FilesystemUtilization
            )
        }) {
            assert!(
                observation
                    .attributes
                    .contains_key(attribute::SYSTEM_FILESYSTEM_MOUNTPOINT),
                "a filesystem reading is meaningless without its mount point"
            );
        }
    }

    #[test]
    fn an_unmatched_filesystem_filter_reports_no_filesystems() {
        let config = CollectionConfig {
            filesystems: vec!["/no/such/mount".to_owned()],
            per_interface_network: false,
        };
        let observations = sample(&config);
        assert!(values_of(&observations, MetricKind::FilesystemUsage).is_empty());
        assert!(
            !values_of(&observations, MetricKind::CpuUtilization).is_empty(),
            "filtering filesystems must not suppress everything else"
        );
    }

    #[test]
    fn reports_load_average_on_platforms_that_have_one() {
        let observations = sample(&CollectionConfig::default());
        let has_load = !values_of(&observations, MetricKind::LoadAverage1m).is_empty();
        assert_eq!(
            has_load,
            !cfg!(windows),
            "load average should be present exactly where the platform provides it"
        );
    }

    #[test]
    fn every_observation_uses_a_metric_the_core_models() {
        for observation in sample(&CollectionConfig::default()) {
            assert!(
                MetricKind::ALL.contains(&observation.kind),
                "{:?} is not a modelled metric",
                observation.kind
            );
        }
    }
}
