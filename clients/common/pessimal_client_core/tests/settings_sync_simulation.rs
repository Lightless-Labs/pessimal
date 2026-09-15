//! Seeded convergence simulation of `pessimal_client_core::settings_sync`.
//!
//! Three devices share one last-writer-wins cell. Writes in flight may be dropped or land in any
//! order, notifications may be dropped, devices relaunch with only their persisted state, and
//! device clocks step backwards. Once the actions stop, every replica must equal the cell and the
//! join of every document a Save persisted, and a further round must publish nothing.

use chrono::Duration;
use pessimal_client_core::draft_rule;
use pessimal_client_core::settings_sync::{
    Classified, Document, SettingsSyncStatus, SyncedSettings, classify, join, record_edits, step,
};
use pessimal_core::{AlertRule, Comparator, HostSelector, MetricKind};
use serde_json::Value;

const SEEDS: u64 = 500;
const TICKS: usize = 60;
const DEVICES: usize = 3;
const MAX_QUIESCENCE_ROUNDS: usize = 10;
const ENVIRONMENTS: [&str; 3] = ["prod", "staging", "eu-west"];
const INTERVALS: [i64; 3] = [15, 30, 60];
const DEFAULT_INTERVAL: i64 = 30;
const SECOND_MS: i64 = 1_000;
const HOUR_SECONDS: u64 = 3_600;

/// xorshift64. Not for anything but choosing test actions.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1)
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next_u64() % bound
    }

    fn index(&mut self, len: usize) -> usize {
        let bound = u64::try_from(len).expect("a small length");
        usize::try_from(self.below(bound)).expect("below a usize length")
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }

    /// Whole seconds, so devices often mint equal stamps for different values.
    fn seconds_below(&mut self, bound: u64) -> i64 {
        SECOND_MS * i64::try_from(self.below(bound)).expect("a small duration")
    }
}

/// What the settings screen was seeded with, as the Swift views build it.
fn seeded(applied: &SyncedSettings) -> SyncedSettings {
    SyncedSettings {
        environment: applied.environment.clone(),
        poll_interval_seconds: applied.poll_interval_seconds.or(Some(DEFAULT_INTERVAL)),
        rules_json: applied.rules_json.clone().or_else(|| Some("[]".to_owned())),
    }
}

struct Device {
    // Persisted: survives a relaunch.
    replica: Option<String>,
    replaced_digest: Option<String>,
    applied: SyncedSettings,
    // In memory.
    committed: SyncedSettings,
    clock_ms: i64,
    notified: bool,
}

impl Device {
    fn new(clock_ms: i64) -> Self {
        let applied = SyncedSettings {
            environment: None,
            poll_interval_seconds: None,
            rules_json: None,
        };
        Self {
            replica: None,
            replaced_digest: None,
            committed: seeded(&applied),
            applied,
            clock_ms,
            notified: false,
        }
    }

    fn relaunch(&mut self) {
        self.committed = seeded(&self.applied);
        self.notified = false;
    }
}

struct World {
    rng: Rng,
    template: Value,
    devices: Vec<Device>,
    cell: Option<String>,
    in_flight: Vec<(usize, String)>,
    saved: Vec<Document>,
    next_rule: u64,
}

impl World {
    fn new(seed: u64, template: Value) -> Self {
        let mut rng = Rng::new(seed);
        let devices = (0..DEVICES)
            .map(|_| Device::new(1_789_430_400_000 + rng.seconds_below(3)))
            .collect();
        Self {
            rng,
            template,
            devices,
            cell: None,
            in_flight: Vec::new(),
            saved: Vec::new(),
            next_rule: 0,
        }
    }

    fn new_rule(&mut self, environment: &str) -> AlertRule {
        self.next_rule += 1;
        let mut value = self.template.clone();
        value["id"]["environment"] = Value::from(environment);
        value["id"]["id"] = Value::from(format!("01994f3a-0000-7000-8000-{:012x}", self.next_rule));
        value["name"] = Value::from(format!("rule {}", self.next_rule));
        serde_json::from_value(value).expect("the template is a valid rule")
    }

    fn save(&mut self, device: usize) {
        let base = self.devices[device].committed.clone();
        let mut edited = base.clone();
        let mut rules: Vec<AlertRule> =
            serde_json::from_str(base.rules_json.as_deref().unwrap_or("[]")).expect("rules");
        match self.rng.below(4) {
            0 => {
                edited.environment =
                    Some(ENVIRONMENTS[self.rng.index(ENVIRONMENTS.len())].to_owned());
            }
            1 => edited.poll_interval_seconds = Some(INTERVALS[self.rng.index(INTERVALS.len())]),
            2 => {
                let environment = edited
                    .environment
                    .clone()
                    .unwrap_or_else(|| "prod".to_owned());
                rules.push(self.new_rule(&environment));
            }
            _ => {
                if !rules.is_empty() {
                    let index = self.rng.index(rules.len());
                    rules.remove(index);
                }
            }
        }
        edited.rules_json = Some(serde_json::to_string(&rules).expect("rules serialise"));

        let this = &mut self.devices[device];
        let edit = record_edits(
            this.replica.as_deref(),
            &this.applied,
            &base,
            &edited,
            this.clock_ms,
        )
        .expect("the simulation only makes edits core accepts");
        match classify(Some(&edit.replica_document)) {
            Classified::Valid(document) => self.saved.push(document),
            other => panic!("record_edits persisted an unreadable replica: {other:?}"),
        }
        this.replica = Some(edit.replica_document);
        this.applied = edit.settings;
        this.committed = edited;
    }

    /// One round of the loop on one device. Returns whether it handed a write to the cell.
    fn run(&mut self, device: usize, may_publish: bool) -> bool {
        let this = &mut self.devices[device];
        let result = step(
            this.replica.as_deref(),
            self.cell.as_deref(),
            &this.applied,
            this.replaced_digest.as_deref(),
        );
        assert!(
            matches!(result.status, SettingsSyncStatus::UpToDate),
            "unexpected status {:?}",
            result.status
        );
        assert!(result.rejected.is_empty(), "{:?}", result.rejected);
        assert_eq!(result.hidden_rule_count, 0);
        if let Some(replica) = result.replica_document {
            this.replica = Some(replica);
        }
        let changed = this.applied != result.settings;
        this.applied = result.settings;
        if changed && self.rng.chance(50) {
            let this = &mut self.devices[device];
            this.committed = seeded(&this.applied);
        }
        match result.publish_document {
            Some(text) if may_publish => {
                if let Some(digest) = result.replaced_digest {
                    self.devices[device].replaced_digest = Some(digest);
                }
                self.in_flight.push((device, text));
                true
            }
            _ => false,
        }
    }

    fn land(&mut self, index: usize) {
        let (writer, text) = self.in_flight.remove(index);
        self.cell = Some(text);
        for (other, device) in self.devices.iter_mut().enumerate() {
            if other != writer {
                device.notified = true;
            }
        }
    }

    fn act(&mut self) {
        for device in &mut self.devices {
            device.clock_ms += self.rng.seconds_below(3);
        }
        let device = self.rng.index(DEVICES);
        match self.rng.below(10) {
            0 | 1 => {
                self.save(device);
                if self.rng.chance(70) {
                    self.run(device, true);
                }
            }
            2 => {
                self.run(device, true);
            }
            3 if !self.in_flight.is_empty() => {
                let index = self.rng.index(self.in_flight.len());
                self.land(index);
            }
            4 if !self.in_flight.is_empty() => {
                let index = self.rng.index(self.in_flight.len());
                self.in_flight.remove(index);
            }
            5 if self.devices[device].notified => {
                self.devices[device].notified = false;
                // An initial-sync notification runs without publishing.
                let may_publish = self.rng.chance(80);
                self.run(device, may_publish);
            }
            6 => self.devices[device].notified = false,
            7 => {
                self.devices[device].relaunch();
                self.run(device, true);
            }
            8 => {
                let back = self.rng.seconds_below(HOUR_SECONDS + 1);
                self.devices[device].clock_ms -= back;
            }
            _ => {
                if !self.in_flight.is_empty() {
                    let index = self.rng.index(self.in_flight.len());
                    self.land(index);
                }
            }
        }
    }

    /// Lands every write still in flight, in random order, then runs full rounds with no drops
    /// until two consecutive rounds publish nothing.
    fn quiesce(&mut self, seed: u64) {
        while !self.in_flight.is_empty() {
            let index = self.rng.index(self.in_flight.len());
            self.land(index);
        }
        let mut quiet_rounds = 0;
        for _ in 0..MAX_QUIESCENCE_ROUNDS {
            if self.round() == 0 {
                quiet_rounds += 1;
                if quiet_rounds == 2 {
                    return;
                }
            } else {
                quiet_rounds = 0;
            }
        }
        panic!("seed {seed}: still publishing after {MAX_QUIESCENCE_ROUNDS} rounds");
    }

    /// Every device runs once, and each write lands at once. Returns the number of writes.
    fn round(&mut self) -> usize {
        let mut writes = 0;
        for device in 0..DEVICES {
            if self.run(device, true) {
                writes += 1;
                let last = self.in_flight.len() - 1;
                self.land(last);
            }
        }
        writes
    }
}

fn decoded(text: Option<&str>) -> Document {
    match classify(text) {
        Classified::Valid(document) => document,
        other => panic!("expected a valid document, got {other:?}"),
    }
}

fn check_converged(world: &mut World, seed: u64) {
    let cell = decoded(world.cell.as_deref());
    let everything_saved = world
        .saved
        .iter()
        .fold(Document::default(), |acc, document| join(&acc, document));
    assert_eq!(cell, everything_saved, "seed {seed}: the cell");

    for (index, device) in world.devices.iter().enumerate() {
        assert_eq!(
            decoded(device.replica.as_deref()),
            cell,
            "seed {seed}: device {index}"
        );
        let first = &world.devices[0].applied;
        assert_eq!(device.applied.environment, first.environment, "seed {seed}");
        assert_eq!(
            device.applied.poll_interval_seconds, first.poll_interval_seconds,
            "seed {seed}"
        );
        if !cell.rules.is_empty() {
            assert_eq!(device.applied.rules_json, first.rules_json, "seed {seed}");
        }
    }

    assert_eq!(world.round(), 0, "seed {seed}: a further round published");
}

#[test]
fn three_devices_converge_for_every_seed() {
    let drafted = draft_rule(
        "prod",
        "template",
        MetricKind::CpuUtilization,
        Comparator::GreaterThan,
        0.9,
        Duration::seconds(300),
        HostSelector::All,
    )
    .expect("a valid rule");
    let template = serde_json::to_value(&drafted).expect("a rule serialises");

    let mut total_saves = 0;
    for seed in 0..SEEDS {
        let mut world = World::new(seed, template.clone());
        for _ in 0..TICKS {
            world.act();
        }
        // Make sure every seed has something to converge on.
        world.save(0);
        world.quiesce(seed);
        check_converged(&mut world, seed);
        total_saves += world.saved.len();
    }
    assert!(total_saves > usize::try_from(SEEDS).expect("small") * 5);
}
