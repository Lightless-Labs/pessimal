//! Sampling the host with `sysinfo`.
//!
//! Every value is turned into the shape [`pessimal_core::MetricKind`] declares: ratios in
//! `0.0..=1.0` (not percentages), bytes (not kibibytes), seconds, degrees Celsius. The backend
//! sees semantic convention names with semantic convention units, so a dashboard written against
//! any other OpenTelemetry host collector keeps working.

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use opentelemetry_semantic_conventions::attribute;
use pessimal_agent_core::collector::{MetricCollector, Observation};
use pessimal_agent_core::config::CollectionConfig;
use pessimal_agent_core::error::Result;
use pessimal_core::MetricKind;
use sysinfo::{Components, Disks, MemoryRefreshKind, Networks, RefreshKind, System};

/// Semconv `state` value for the portion of a resource in use. Both `system.memory.usage` and
/// `system.filesystem.usage` are defined per state, so a bare total is not the same metric a
/// dashboard written against another OpenTelemetry host collector expects.
const STATE_USED: &str = "used";

/// Attribute value for bytes arriving.
const DIRECTION_RECEIVE: &str = "receive";
/// Attribute value for bytes leaving.
const DIRECTION_TRANSMIT: &str = "transmit";

/// Samples the local host.
pub struct HostCollector {
    system: System,
    disks: Disks,
    networks: Networks,
    components: Components,
    filesystems: HashSet<String>,
    per_interface_network: bool,
    temperatures: bool,
    temperature_sensors: Vec<String>,
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
            // Empty rather than refreshed, so a host that does not ask for temperatures never
            // enumerates its sensors. `Components::new_with_refreshed_list` is `new()` followed by
            // `refresh(true)` in sysinfo 0.39.6, and both sampling paths here refresh before
            // reading, so nothing is lost by listing late.
            components: Components::new(),
            filesystems: config.filesystems.iter().cloned().collect(),
            per_interface_network: config.per_interface_network,
            temperatures: config.temperatures,
            temperature_sensors: config.temperature_sensors.clone(),
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

        let mut observations = vec![
            Observation::new(MetricKind::MemoryUsage, round_to_f64(used))
                .with_attribute(attribute::SYSTEM_MEMORY_STATE, STATE_USED),
        ];
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
                    .with_attribute(attribute::SYSTEM_FILESYSTEM_MOUNTPOINT, &mount_point)
                    .with_attribute(attribute::SYSTEM_FILESYSTEM_STATE, STATE_USED),
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

    /// One reading per reporting sensor, or nothing at all when the configuration has not asked
    /// for temperatures.
    ///
    /// `hw.id` and `hw.name` both carry the sensor label as the host spells it. There is no
    /// portable identifier behind it to prefer: on macOS arm `sysinfo` exposes the HID driver's
    /// serial, on Linux a label it synthesises from `hwmon` files.
    fn temperatures(&mut self) -> Vec<Observation> {
        if !self.temperatures {
            return Vec::new();
        }
        self.components.refresh(true);
        let kept: HashSet<String> = self
            .sensor_selection_of_current_list()
            .kept
            .into_iter()
            .collect();

        self.components
            .list()
            .iter()
            .filter(|component| kept.contains(component.label()))
            .filter_map(|component| observation_for(component.label(), component.temperature()))
            .collect()
    }

    /// Which sensors this host offers and which of them the configuration reports.
    ///
    /// Refreshes the sensor list, so it costs what a temperature sample costs. `--sample` uses it
    /// to say whether a host reported no temperatures because it has no sensors or because none
    /// of them matched `temperature_sensors`.
    pub fn sensor_selection(&mut self) -> SensorSelection {
        self.components.refresh(true);
        self.sensor_selection_of_current_list()
    }

    fn sensor_selection_of_current_list(&self) -> SensorSelection {
        let labels: Vec<String> = self
            .components
            .list()
            .iter()
            .map(|component| component.label().to_owned())
            .collect();
        select_sensors(&labels, &self.temperature_sensors)
    }
}

/// What a host's sensors are and which of them a configured list selects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SensorSelection {
    /// Every sensor label the host offered, as the host spells it.
    pub seen: Vec<String>,
    /// The ones to report.
    pub kept: Vec<String>,
}

/// Applies `[collection] temperature_sensors` to the labels a host offers. An empty configured
/// list means every sensor.
///
/// A function over labels rather than over hardware, so the rule is testable on a machine with no
/// sensors — this project's own VM reports none, and a test that went through `sysinfo` there would
/// assert nothing at all.
fn select_sensors(labels: &[String], configured: &[String]) -> SensorSelection {
    let kept = if configured.is_empty() {
        labels.to_vec()
    } else {
        labels
            .iter()
            .filter(|label| configured.contains(*label))
            .cloned()
            .collect()
    };
    SensorSelection {
        seen: labels.to_vec(),
        kept,
    }
}

/// One sensor's reading, as an observation, or nothing when the sensor did not report one.
///
/// Pure, and separate from [`HostCollector::temperatures`], because the loop it came from runs only
/// on a host that has sensors — and the machine this suite runs on has none, so every assertion
/// about the reading, its attributes and the values that are dropped would pass over an empty
/// iterator. Here it is tested directly.
///
/// A listed sensor that will not say how hot it is reports `None`, or `f32::NAN` on Linux where the
/// read failed; sysinfo 0.39.6 documents both on `Component`. The semantic conventions say nothing
/// about a sensor that does not report, so dropping the point is Pessimal's choice: a gap reads as
/// a gap, where a zero would read as a cold host.
///
/// `hw.id` and `hw.name` both carry the label as the host spells it. There is no portable
/// identifier behind it to prefer: on macOS arm `sysinfo` exposes the HID driver's serial, on Linux
/// a label it synthesises from `hwmon` files. Two components that report the same label therefore
/// become one series, and the later reading wins in aggregation — accepted, because a slug would
/// not tell them apart either.
fn observation_for(label: &str, celsius: Option<f32>) -> Option<Observation> {
    let celsius = celsius.filter(|value| value.is_finite())?;
    Some(
        Observation::new(MetricKind::Temperature, f64::from(celsius))
            .with_attribute(attribute::HW_ID, label)
            .with_attribute(attribute::HW_NAME, label),
    )
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
        observations.extend(self.temperatures());
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
    fn usage_metrics_declare_which_state_they_report() {
        let observations = sample(&CollectionConfig::default());

        for observation in observations
            .iter()
            .filter(|o| o.kind == MetricKind::MemoryUsage)
        {
            assert_eq!(
                observation
                    .attributes
                    .get(attribute::SYSTEM_MEMORY_STATE)
                    .map(String::as_str),
                Some(STATE_USED)
            );
        }
        for observation in observations
            .iter()
            .filter(|o| o.kind == MetricKind::FilesystemUsage)
        {
            assert_eq!(
                observation
                    .attributes
                    .get(attribute::SYSTEM_FILESYSTEM_STATE)
                    .map(String::as_str),
                Some(STATE_USED)
            );
        }
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
            ..CollectionConfig::default()
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

    fn labels(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    #[test]
    fn an_empty_sensor_list_keeps_every_sensor() {
        let seen = labels(&["PMU tdev1", "SOC MTR Temp Sensor0"]);
        let selection = select_sensors(&seen, &[]);
        assert_eq!(selection.seen, seen);
        assert_eq!(selection.kept, seen);
    }

    #[test]
    fn a_named_sensor_list_keeps_only_the_sensors_it_names() {
        let seen = labels(&["PMU tdev1", "SOC MTR Temp Sensor0", "gpu thermal"]);
        let selection = select_sensors(&seen, &labels(&["gpu thermal", "PMU tdev1"]));
        assert_eq!(selection.seen, seen);
        assert_eq!(selection.kept, labels(&["PMU tdev1", "gpu thermal"]));
    }

    #[test]
    fn a_sensor_list_that_names_nothing_present_keeps_nothing() {
        let seen = labels(&["PMU tdev1"]);
        let selection = select_sensors(&seen, &labels(&["no such sensor"]));
        assert_eq!(selection.seen, seen);
        assert!(selection.kept.is_empty());
    }

    #[test]
    fn a_host_with_no_sensors_keeps_nothing_and_reports_nothing_seen() {
        let selection = select_sensors(&[], &labels(&["PMU tdev1"]));
        assert!(selection.seen.is_empty());
        assert!(selection.kept.is_empty());
    }

    #[test]
    fn temperatures_are_absent_unless_collection_asks_for_them() {
        let observations = sample(&CollectionConfig::default());
        assert!(values_of(&observations, MetricKind::Temperature).is_empty());
    }

    #[test]
    fn a_sensor_reading_carries_its_label_twice() {
        // The real per-sensor decision, tested where sensors exist: here. Driving it through
        // `sample()` instead would assert nothing on this project's own VM, which reports none —
        // the loop body never runs and every assertion passes over an empty iterator.
        let observation = observation_for("PMU tdev1", Some(41.5)).expect("a reading is reported");

        assert_eq!(observation.kind, MetricKind::Temperature);
        assert!((observation.value - 41.5).abs() < f64::EPSILON);
        assert_eq!(
            observation.attributes.get("hw.id").map(String::as_str),
            Some("PMU tdev1"),
            "hw.id is Required by the convention and carries the label verbatim"
        );
        assert_eq!(
            observation.attributes.get("hw.name").map(String::as_str),
            Some("PMU tdev1"),
            "hw.name carries it too: there is no portable identifier to prefer"
        );
    }

    #[test]
    fn a_sensor_that_will_not_say_is_dropped_rather_than_reported_cold() {
        // `None` on every platform, and NAN on Linux where the read failed. A zero would render as
        // a cold host, which is a claim the agent cannot make.
        assert!(observation_for("PMU tdev1", None).is_none());
        assert!(observation_for("PMU tdev1", Some(f32::NAN)).is_none());
        assert!(observation_for("PMU tdev1", Some(f32::INFINITY)).is_none());
        // A real reading below zero is not an error: an outdoor probe or a freezer is a host too.
        assert!(observation_for("chiller", Some(-8.0)).is_some());
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
