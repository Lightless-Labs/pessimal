//! Laws and behaviour of `pessimal_client_core::settings_sync`.
//!
//! The law tests are exhaustive over small generated domains. The domains are kept tiny so that
//! two entries with the same stamp and different bytes occur in the checked cases.

use std::collections::BTreeMap;

use chrono::Duration;
use pessimal_client_core::settings_sync::{
    Classified, Document, ENVIRONMENT, MAX_MS, POLL_INTERVAL_SECONDS, Register, RuleEntry,
    SETTINGS_SYNC_BUDGET_BYTES, SettingsSyncError, SettingsSyncStatus, Stamp, SyncedSettings,
    bootstrap, classify, fnv1a64_hex, join, project, record_edits, step, tick,
};
use pessimal_client_core::{ClientError, draft_rule};
use pessimal_core::{AlertRule, Comparator, HostId, HostSelector, MetricKind};
use serde_json::Value;

const UUID_1: &str = "01994f3a-6c1e-7d3b-9a2f-4b8c1d2e3f40";
const UUID_2: &str = "01994f3b-0a4d-7e21-8c55-6f7a8b9c0d1e";
const UUID_3: &str = "01994f3c-1b2c-7a00-8000-000000000003";

// Fixtures

fn stamp(ms: u64, seq: u32) -> Stamp {
    Stamp::new(ms, seq).expect("a stamp inside the range")
}

fn register(ms: u64, seq: u32, value: Option<&str>) -> Register {
    Register {
        at: stamp(ms, seq),
        value: value.map(str::to_owned),
    }
}

fn rule_entry(ms: u64, seq: u32, rule: Option<&str>) -> RuleEntry {
    RuleEntry {
        at: stamp(ms, seq),
        rule: rule.map(str::to_owned),
    }
}

fn synced(
    environment: Option<&str>,
    poll_interval_seconds: Option<i64>,
    rules_json: Option<&str>,
) -> SyncedSettings {
    SyncedSettings {
        environment: environment.map(str::to_owned),
        poll_interval_seconds,
        rules_json: rules_json.map(str::to_owned),
    }
}

/// A rule core accepts, with a fixed id, as `(urn, compact JSON body)`.
fn rule_with(
    environment: &str,
    uuid: &str,
    name: &str,
    selector: HostSelector,
) -> (String, String) {
    let drafted = draft_rule(
        environment,
        name,
        MetricKind::CpuUtilization,
        Comparator::GreaterThan,
        0.9,
        Duration::seconds(300),
        selector,
    )
    .expect("a valid rule");
    let mut value = serde_json::to_value(&drafted).expect("a rule serialises");
    value["id"]["id"] = Value::from(uuid);
    let rule: AlertRule = serde_json::from_value(value).expect("the rule decodes");
    (
        rule.id().to_string(),
        serde_json::to_string(&rule).expect("a rule serialises"),
    )
}

fn rule(environment: &str, uuid: &str) -> (String, String) {
    rule_with(environment, uuid, "CPU high", HostSelector::All)
}

fn rules_json(bodies: &[&str]) -> String {
    format!("[{}]", bodies.join(","))
}

fn valid(text: &str) -> Document {
    match classify(Some(text)) {
        Classified::Valid(document) => document,
        other => panic!("expected a valid document, got {other:?} for {text}"),
    }
}

/// Pointwise order: every entry of `lower` is at or below the entry `upper` holds for its key.
fn below_or_equal(lower: &Document, upper: &Document) -> bool {
    lower
        .settings
        .iter()
        .all(|(name, entry)| upper.settings.get(name).is_some_and(|other| entry <= other))
        && lower
            .rules
            .iter()
            .all(|(urn, entry)| upper.rules.get(urn).is_some_and(|other| entry <= other))
}

// Generated domains

const SMALL_STAMPS: [(u64, u32); 6] = [(0, 0), (0, 1), (1, 0), (1, 1), (2, 0), (2, 1)];
const LIFT_STAMPS: [(u64, u32); 2] = [(0, 0), (1, 0)];

fn register_states(stamps: &[(u64, u32)]) -> Vec<Option<Register>> {
    let mut states = vec![None];
    for &(ms, seq) in stamps {
        for value in [None, Some("a"), Some("b")] {
            states.push(Some(register(ms, seq, value)));
        }
    }
    states
}

fn rule_states(stamps: &[(u64, u32)]) -> Vec<Option<RuleEntry>> {
    let mut states = vec![None];
    for &(ms, seq) in stamps {
        for body in [Some("a"), Some("b"), None] {
            states.push(Some(rule_entry(ms, seq, body)));
        }
    }
    states
}

/// Every document whose settings are drawn from `states` at each of `names`.
fn settings_documents(names: &[&str], states: &[Option<Register>]) -> Vec<Document> {
    let mut documents = vec![Document::default()];
    for name in names {
        let mut next = Vec::new();
        for document in &documents {
            for state in states {
                let mut extended = document.clone();
                if let Some(entry) = state {
                    extended.settings.insert((*name).to_owned(), entry.clone());
                }
                next.push(extended);
            }
        }
        documents = next;
    }
    documents
}

/// Every document whose rules are drawn from `states` at each of `urns`.
fn rule_documents(urns: &[&str], states: &[Option<RuleEntry>]) -> Vec<Document> {
    let mut documents = vec![Document::default()];
    for urn in urns {
        let mut next = Vec::new();
        for document in &documents {
            for state in states {
                let mut extended = document.clone();
                if let Some(entry) = state {
                    extended.rules.insert((*urn).to_owned(), entry.clone());
                }
                next.push(extended);
            }
        }
        documents = next;
    }
    documents
}

/// Commutativity (structurally and byte for byte), idempotence, absorption and inflation over
/// every pair, and associativity over every triple.
fn check_join_laws(domain: &[Document]) {
    let mut pairs = Vec::with_capacity(domain.len());
    for first in domain {
        assert_eq!(&join(first, first), first, "idempotence for {first:?}");
        let mut row = Vec::with_capacity(domain.len());
        for second in domain {
            let forward = join(first, second);
            let backward = join(second, first);
            assert_eq!(
                forward, backward,
                "commutativity for {first:?} and {second:?}"
            );
            assert_eq!(
                forward.encode(),
                backward.encode(),
                "byte identity for {first:?} and {second:?}"
            );
            assert_eq!(
                join(first, &forward),
                forward,
                "absorption for {first:?} and {second:?}"
            );
            assert!(
                below_or_equal(first, &forward) && below_or_equal(second, &forward),
                "inflation for {first:?} and {second:?}"
            );
            row.push(forward);
        }
        pairs.push(row);
    }
    // `pairs[i][j]` is `join(domain[i], domain[j])`.
    for (first, first_joined) in domain.iter().zip(&pairs) {
        for (first_second, second_joined) in first_joined.iter().zip(&pairs) {
            for (third, second_third) in domain.iter().zip(second_joined) {
                let left = join(first_second, third);
                let right = join(first, second_third);
                assert_eq!(
                    left, right,
                    "associativity for {first:?}, {second_third:?}, {third:?}"
                );
                assert_eq!(left.encode(), right.encode(), "associativity bytes");
            }
        }
    }
}

// Laws

#[test]
fn register_join_laws_hold_for_every_triple() {
    let states = register_states(&SMALL_STAMPS);
    assert_eq!(states.len(), 19);
    let domain = settings_documents(&["environment"], &states);
    check_join_laws(&domain);

    // A single key: the join is one of the two inputs.
    for first in &domain {
        for second in &domain {
            let joined = join(first, second);
            assert!(&joined == first || &joined == second);
        }
    }
}

#[test]
fn rule_entry_join_laws_hold_for_every_triple() {
    let states = rule_states(&SMALL_STAMPS);
    assert_eq!(states.len(), 19);
    let domain = rule_documents(&["urn-1"], &states);
    check_join_laws(&domain);
}

#[test]
fn a_tombstone_at_the_lowest_stamp_beats_a_live_entry_at_the_highest() {
    let tombstone = rule_entry(0, 0, None);
    let live = rule_entry(MAX_MS, u32::MAX, Some("a"));
    assert!(tombstone > live);

    let mut dead = Document::default();
    dead.rules.insert("urn-1".to_owned(), tombstone.clone());
    let mut alive = Document::default();
    alive.rules.insert("urn-1".to_owned(), live);
    assert_eq!(join(&dead, &alive).rules["urn-1"], tombstone);
    assert_eq!(join(&alive, &dead).rules["urn-1"], tombstone);
}

#[test]
fn equal_stamps_break_ties_by_bytes_the_same_way_in_both_orders() {
    assert!(register(1, 0, Some("b")) > register(1, 0, Some("a")));
    assert!(register(1, 0, Some("a")) > register(1, 0, None));
    assert!(register(1, 1, None) > register(1, 0, Some("b")));
    assert!(rule_entry(1, 0, Some("b")) > rule_entry(1, 0, Some("a")));
}

#[test]
fn document_join_laws_hold_for_every_triple_of_two_key_documents() {
    let settings = settings_documents(&["environment", "theme"], &register_states(&LIFT_STAMPS));
    assert_eq!(settings.len(), 49);
    check_join_laws(&settings);

    let rules = rule_documents(&["urn-1", "urn-2"], &rule_states(&LIFT_STAMPS));
    assert_eq!(rules.len(), 49);
    check_join_laws(&rules);
}

#[test]
fn every_arrival_order_of_up_to_four_documents_encodes_to_the_same_bytes() {
    // 16 documents, one register and one rule each over 4 states, so that sequences of 4 stay
    // cheap to enumerate. The 19-state domains above would give 130,321 sequences per map.
    let register_choices = [
        None,
        Some(register(1, 0, Some("a"))),
        Some(register(1, 0, Some("b"))),
        Some(register(2, 0, None)),
    ];
    let rule_choices = [
        None,
        Some(rule_entry(1, 0, Some("a"))),
        Some(rule_entry(2, 0, Some("b"))),
        Some(rule_entry(0, 0, None)),
    ];
    let mut domain = Vec::new();
    for setting in &register_choices {
        for entry in &rule_choices {
            let mut document = Document::default();
            if let Some(setting) = setting {
                document
                    .settings
                    .insert("environment".to_owned(), setting.clone());
            }
            if let Some(entry) = entry {
                document.rules.insert("urn-1".to_owned(), entry.clone());
            }
            domain.push(document);
        }
    }

    let fold = |indices: &[usize]| {
        indices
            .iter()
            .fold(Document::default(), |acc, &index| {
                join(&acc, &domain[index])
            })
            .encode()
    };

    // Every permutation of a sequence is itself one of the generated sequences, so comparing each
    // sequence with its sorted form covers every permutation of every sequence.
    let mut sequences: Vec<Vec<usize>> = vec![Vec::new()];
    for _ in 0..4 {
        let mut longer = Vec::new();
        for sequence in &sequences {
            for index in 0..domain.len() {
                let mut extended = sequence.clone();
                extended.push(index);
                longer.push(extended);
            }
        }
        for sequence in &longer {
            let mut sorted = sequence.clone();
            sorted.sort_unstable();
            assert_eq!(fold(sequence), fold(&sorted), "arrival order {sequence:?}");
        }
        sequences = longer;
    }
}

// Tick

#[test]
fn tick_is_strictly_increasing_while_now_goes_backwards_stays_equal_and_jumps_forward() {
    let mut document = Document::default();
    let mut previous = document.top_stamp();
    for now in [1_000, 1_000, 500, 0, -5, 2_000, 1_999, 2_000, 10_000] {
        let minted = tick(&document, now).expect("stamps are left");
        assert!(
            minted > previous,
            "{minted:?} after {previous:?} at now {now}"
        );
        assert!(minted > document.top_stamp());
        let mut edit = Document::default();
        edit.settings.insert(
            ENVIRONMENT.to_owned(),
            Register {
                at: minted,
                value: Some("\"prod\"".to_owned()),
            },
        );
        document = join(&document, &edit);
        previous = minted;
    }
}

#[test]
fn tick_beats_a_document_from_a_clock_two_hours_ahead() {
    let now: i64 = 1_789_430_400_000;
    let ahead = u64::try_from(now + 7_200_000).expect("positive");
    let mut remote = Document::default();
    remote
        .settings
        .insert(ENVIRONMENT.to_owned(), register(ahead, 3, Some("\"fast\"")));
    let local = join(&Document::default(), &remote);

    let minted = tick(&local, now).expect("stamps are left");
    assert!(minted > stamp(ahead, 3));
}

#[test]
fn tick_carries_into_the_next_millisecond_at_the_largest_counter() {
    let mut document = Document::default();
    document
        .settings
        .insert(ENVIRONMENT.to_owned(), register(100, u32::MAX, None));
    assert_eq!(tick(&document, 50), Ok(stamp(101, 0)));
}

#[test]
fn tick_reports_exhaustion_at_the_last_stamp() {
    let mut document = Document::default();
    document
        .rules
        .insert("urn-1".to_owned(), rule_entry(MAX_MS, u32::MAX, None));
    assert_eq!(
        tick(&document, i64::MAX),
        Err(SettingsSyncError::StampSpaceExhausted)
    );
}

#[test]
fn tick_clamps_now_into_the_stamp_range() {
    let empty = Document::default();
    assert_eq!(tick(&empty, i64::MAX), Ok(stamp(MAX_MS, 0)));
    assert_eq!(tick(&empty, -1), Ok(stamp(0, 1)));
    assert_eq!(tick(&empty, 7), Ok(stamp(7, 0)));
    assert!(Stamp::new(MAX_MS + 1, 0).is_none());
}

// Bootstrap

#[test]
fn bootstrap_stamps_every_entry_at_zero() {
    let (urn_1, body_1) = rule("prod", UUID_1);
    let (urn_2, body_2) = rule("prod", UUID_2);
    let applied = synced(
        Some("prod"),
        Some(45),
        Some(&rules_json(&[&body_1, &body_2])),
    );

    let document = bootstrap(&applied).expect("the rules decode");

    assert_eq!(document.settings.len(), 2);
    assert_eq!(document.rules.len(), 2);
    assert!(
        document
            .settings
            .values()
            .all(|entry| entry.at == Stamp::ZERO)
    );
    assert!(document.rules.values().all(|entry| entry.at == Stamp::ZERO));
    assert_eq!(
        document.settings[ENVIRONMENT].value.as_deref(),
        Some("\"prod\"")
    );
    assert_eq!(
        document.settings[POLL_INTERVAL_SECONDS].value.as_deref(),
        Some("45")
    );
    assert_eq!(
        document.rules[&urn_1].rule.as_deref(),
        Some(body_1.as_str())
    );
    assert_eq!(
        document.rules[&urn_2].rule.as_deref(),
        Some(body_2.as_str())
    );
    assert!(
        bootstrap(&synced(None, None, None))
            .expect("empty")
            .is_empty()
    );
}

#[test]
fn any_recorded_edit_beats_a_bootstrap_entry_at_the_same_key() {
    let applied = synced(Some("zzz-bootstrap"), Some(30), Some("[]"));
    let base = applied.clone();
    let edited = synced(Some("aaa-edit"), Some(30), Some("[]"));
    let edit = record_edits(None, &applied, &base, &edited, 0).expect("accepted");

    let other_device = bootstrap(&applied).expect("no rules");
    let merged = join(&valid(&edit.replica_document), &other_device);
    assert_eq!(
        merged.settings[ENVIRONMENT].value.as_deref(),
        Some("\"aaa-edit\"")
    );
    assert_eq!(edit.settings.environment.as_deref(), Some("aaa-edit"));
}

#[test]
fn two_bootstraps_with_different_environments_pick_the_larger_bytes_in_both_orders() {
    let alpha = bootstrap(&synced(Some("alpha"), None, None)).expect("no rules");
    let beta = bootstrap(&synced(Some("beta"), None, None)).expect("no rules");
    for merged in [join(&alpha, &beta), join(&beta, &alpha)] {
        assert_eq!(
            merged.settings[ENVIRONMENT].value.as_deref(),
            Some("\"beta\"")
        );
    }
}

#[test]
fn undecodable_applied_rules_defer_the_bootstrap() {
    let applied = synced(Some("prod"), Some(30), Some("[{\"not\":\"a rule\"}]"));
    assert!(matches!(
        bootstrap(&applied),
        Err(ClientError::InvalidRule(_))
    ));

    let result = step(None, None, &applied, None);
    assert!(matches!(
        result.status,
        SettingsSyncStatus::BootstrapDeferred { .. }
    ));
    assert_eq!(result.replica_document, None);
    assert_eq!(result.publish_document, None);
    assert_eq!(result.settings, applied);
}

// Record edits: the three-way base

#[test]
fn a_save_from_a_stale_window_keeps_a_remote_change_it_did_not_touch() {
    let applied = synced(Some("v0"), Some(30), Some("[]"));
    let first = record_edits(
        None,
        &applied,
        &applied,
        &synced(Some("v1"), Some(30), Some("[]")),
        1_000,
    )
    .expect("accepted");
    let committed = synced(Some("v1"), Some(30), Some("[]"));

    let mut remote = valid(&first.replica_document);
    remote
        .settings
        .insert(ENVIRONMENT.to_owned(), register(5_000, 0, Some("\"v3\"")));
    let arrived = step(
        Some(&first.replica_document),
        Some(&remote.encode()),
        &first.settings,
        None,
    );
    assert_eq!(arrived.settings.environment.as_deref(), Some("v3"));
    let replica = arrived.replica_document.expect("a replica");

    let second = record_edits(
        Some(&replica),
        &arrived.settings,
        &committed,
        &synced(Some("v1"), Some(45), Some("[]")),
        2_000,
    )
    .expect("accepted");

    let before = valid(&replica);
    let after = valid(&second.replica_document);
    assert_eq!(
        after.settings[ENVIRONMENT],
        register(5_000, 0, Some("\"v3\""))
    );
    assert_eq!(after.rules, before.rules);
    let interval = &after.settings[POLL_INTERVAL_SECONDS];
    assert_eq!(interval.value.as_deref(), Some("45"));
    assert!(interval.at > before.top_stamp());
    assert_eq!(second.settings.environment.as_deref(), Some("v3"));
    assert_eq!(second.settings.poll_interval_seconds, Some(45));
}

#[test]
fn a_first_run_save_before_the_download_stamps_only_the_environment() {
    let (urn_1, body_1) = rule("prod", UUID_1);
    let (urn_2, body_2) = rule("prod", UUID_2);
    let mut remote = Document::default();
    remote
        .rules
        .insert(urn_1, rule_entry(900, 0, Some(&body_1)));
    remote
        .rules
        .insert(urn_2, rule_entry(901, 0, Some(&body_2)));
    let nothing = synced(None, None, None);
    let arrived = step(None, Some(&remote.encode()), &nothing, None);
    let replica = arrived.replica_document.expect("a replica");

    let base = synced(None, Some(30), Some("[]"));
    let edited = synced(Some("typed"), Some(30), Some("[]"));
    let edit =
        record_edits(Some(&replica), &arrived.settings, &base, &edited, 1_000).expect("accepted");

    let after = valid(&edit.replica_document);
    assert_eq!(
        after.rules, remote.rules,
        "no rule restamped and no tombstone"
    );
    assert_eq!(after.settings.len(), 1);
    assert_eq!(
        after.settings[ENVIRONMENT].value.as_deref(),
        Some("\"typed\"")
    );
    assert_eq!(
        edit.settings.rules_json.as_deref(),
        Some(rules_json(&[&body_1, &body_2]).as_str())
    );
    assert_eq!(edit.settings.poll_interval_seconds, None);
}

#[test]
fn an_unknown_base_rule_set_emits_no_tombstones() {
    let (_, body_1) = rule("prod", UUID_1);
    let applied = synced(Some("prod"), Some(30), Some(&rules_json(&[&body_1])));
    let replica = record_edits(None, &applied, &applied, &applied, 1_000)
        .expect("accepted")
        .replica_document;

    let base = synced(Some("prod"), Some(30), None);
    let edited = synced(Some("prod"), Some(30), Some("[]"));
    let edit = record_edits(Some(&replica), &applied, &base, &edited, 2_000).expect("accepted");

    let after = valid(&edit.replica_document);
    assert!(after.rules.values().all(|entry| entry.rule.is_some()));
}

#[test]
fn removing_one_base_rule_emits_exactly_one_tombstone() {
    let (urn_1, body_1) = rule("prod", UUID_1);
    let applied = synced(Some("prod"), Some(30), Some(&rules_json(&[&body_1])));
    let edit = record_edits(
        None,
        &applied,
        &applied,
        &synced(Some("prod"), Some(30), Some("[]")),
        1_000,
    )
    .expect("accepted");

    let after = valid(&edit.replica_document);
    let tombstones: Vec<&String> = after
        .rules
        .iter()
        .filter(|(_, entry)| entry.rule.is_none())
        .map(|(urn, _)| urn)
        .collect();
    assert_eq!(tombstones, vec![&urn_1]);
    assert_eq!(edit.settings.rules_json.as_deref(), Some("[]"));
}

#[test]
fn an_unchanged_save_leaves_the_document_byte_identical() {
    let (_, body_1) = rule("prod", UUID_1);
    let applied = synced(Some("prod"), Some(45), Some(&rules_json(&[&body_1])));
    let replica = record_edits(None, &applied, &applied, &applied, 1_000)
        .expect("accepted")
        .replica_document;

    let again =
        record_edits(Some(&replica), &applied, &applied, &applied, 9_000).expect("accepted");
    assert_eq!(again.replica_document, replica);
}

#[test]
fn a_save_that_leaves_a_rule_unchanged_keeps_a_newer_remote_edit_of_it() {
    let (urn, body_x) = rule("prod", UUID_1);
    let (_, body_y) = rule_with("prod", UUID_1, "CPU higher", HostSelector::All);
    let applied = synced(Some("prod"), Some(30), Some(&rules_json(&[&body_x])));
    let first = record_edits(None, &applied, &applied, &applied, 1_000).expect("accepted");

    let mut remote = valid(&first.replica_document);
    remote
        .rules
        .insert(urn.clone(), rule_entry(5_000, 0, Some(&body_y)));
    let arrived = step(
        Some(&first.replica_document),
        Some(&remote.encode()),
        &first.settings,
        None,
    );
    let replica = arrived.replica_document.expect("a replica");

    // The screen still shows the rule as it was before the remote edit, and only the environment
    // changes.
    let edited = synced(Some("staging"), Some(30), Some(&rules_json(&[&body_x])));
    let edit = record_edits(Some(&replica), &arrived.settings, &applied, &edited, 2_000)
        .expect("accepted");

    let after = valid(&edit.replica_document);
    assert_eq!(after.rules[&urn], rule_entry(5_000, 0, Some(&body_y)));
    assert_eq!(
        after.settings[ENVIRONMENT].value.as_deref(),
        Some("\"staging\"")
    );
    assert_eq!(
        edit.settings.rules_json.as_deref(),
        Some(rules_json(&[&body_y]).as_str())
    );
}

#[test]
fn a_save_with_an_unknown_base_does_not_restamp_a_rule_the_replica_holds() {
    let (_, body_1) = rule("prod", UUID_1);
    let applied = synced(Some("prod"), Some(30), Some(&rules_json(&[&body_1])));
    let replica = record_edits(None, &applied, &applied, &applied, 1_000)
        .expect("accepted")
        .replica_document;

    let base = synced(Some("prod"), Some(30), None);
    let edit = record_edits(Some(&replica), &applied, &base, &applied, 2_000).expect("accepted");

    assert_eq!(edit.replica_document, replica);
}

#[test]
fn discarding_unreadable_rules_and_saving_starts_sync() {
    let applied = synced(Some("prod"), Some(30), Some("[{\"not\":\"a rule\"}]"));
    let base = synced(Some("prod"), Some(30), None);
    let edited = synced(Some("prod"), Some(30), Some("[]"));

    let edit = record_edits(None, &applied, &base, &edited, 1_000).expect("accepted");

    assert_eq!(edit.settings.rules_json.as_deref(), Some("[]"));
    let after = valid(&edit.replica_document);
    assert!(after.rules.is_empty());
    let next = step(Some(&edit.replica_document), None, &edit.settings, None);
    assert_eq!(next.status, SettingsSyncStatus::UpToDate);
}

#[test]
fn record_edits_refuses_values_core_refuses() {
    let applied = synced(Some("prod"), Some(30), Some("[]"));

    let environment = record_edits(
        None,
        &applied,
        &applied,
        &synced(Some("pr::od"), Some(30), Some("[]")),
        1_000,
    );
    assert!(matches!(
        environment,
        Err(SettingsSyncError::Refused(ClientError::InvalidConfig(_)))
    ));

    let interval = record_edits(
        None,
        &applied,
        &applied,
        &synced(Some("prod"), Some(0), Some("[]")),
        1_000,
    );
    assert!(matches!(
        interval,
        Err(SettingsSyncError::Refused(ClientError::InvalidTuning(_)))
    ));

    let rules = record_edits(
        None,
        &applied,
        &applied,
        &synced(Some("prod"), Some(30), Some("not json")),
        1_000,
    );
    assert!(matches!(
        rules,
        Err(SettingsSyncError::Refused(ClientError::InvalidRule(_)))
    ));
}

// Opaque payloads

#[test]
fn content_this_build_cannot_use_is_carried_byte_for_byte_and_never_applied() {
    let (future_urn, body) = rule("prod", UUID_2);
    let mut future: Value = serde_json::from_str(&body).expect("json");
    future["metric"] = Value::from("GpuUtilization");
    // Key order and spacing differ from what this build would write, on purpose.
    let future_body = format!(" {future} ");
    assert!(serde_json::from_str::<AlertRule>(&future_body).is_err());

    let mut remote = Document::default();
    remote
        .settings
        .insert("theme".to_owned(), register(10, 0, Some("{\"dark\":true}")));
    remote
        .rules
        .insert(future_urn.clone(), rule_entry(10, 0, Some(&future_body)));
    let remote_text = remote.encode();

    let applied = synced(Some("prod"), Some(30), Some("[]"));
    let result = step(None, Some(&remote_text), &applied, None);
    let replica = valid(result.replica_document.as_deref().expect("a replica"));
    let published = valid(result.publish_document.as_deref().expect("a publish"));

    for document in [&replica, &published, &join(&replica, &remote)] {
        assert_eq!(document.settings["theme"], remote.settings["theme"]);
        assert_eq!(document.rules[&future_urn], remote.rules[&future_urn]);
        assert_eq!(valid(&document.encode()), *document);
    }
    assert_eq!(result.hidden_rule_count, 1);
    assert_eq!(result.settings.rules_json.as_deref(), Some("[]"));
    assert_eq!(result.settings, applied);
}

// Two-stage decode

#[test]
fn a_newer_format_is_never_merged_or_published() {
    let applied = synced(Some("prod"), Some(30), Some("[]"));
    let local = record_edits(None, &applied, &applied, &applied, 1_000)
        .expect("accepted")
        .replica_document;

    let result = step(
        Some(&local),
        Some("{\"format\":2,\"surprise\":true}"),
        &applied,
        None,
    );

    assert_eq!(
        result.status,
        SettingsSyncStatus::PausedNewerFormat { format: 2 }
    );
    assert_eq!(result.publish_document, None);
    assert_eq!(result.replica_document.as_deref(), Some(local.as_str()));
    assert_eq!(result.settings, applied);
}

#[test]
fn text_that_is_not_a_settings_document_is_foreign() {
    for text in [
        "not json",
        "",
        "{}",
        "[1,{},{}]",
        "{\"format\":\"1\"}",
        "{\"format\":-1}",
        "{\"format\":1.5}",
    ] {
        assert!(
            matches!(classify(Some(text)), Classified::Foreign),
            "{text}"
        );
    }
    assert!(matches!(classify(None), Classified::Absent));
}

#[test]
fn a_format_one_document_with_a_bad_known_key_is_invalid() {
    let beyond = MAX_MS + 1;
    for text in [
        format!(
            "{{\"format\":1,\"settings\":{{\"environment\":{{\"at\":[{beyond},0],\"value\":null}}}},\"rules\":{{}}}}"
        ),
        "{\"format\":1,\"settings\":{\"environment\":{\"at\":[1,2,3],\"value\":null}},\"rules\":{}}"
            .to_owned(),
        "{\"format\":1,\"settings\":{\"environment\":{\"at\":[1,-2],\"value\":null}},\"rules\":{}}"
            .to_owned(),
        "{\"format\":1,\"settings\":{\"environment\":{\"at\":[1,0]}},\"rules\":{}}".to_owned(),
        "{\"format\":1,\"settings\":{\"environment\":{\"at\":[1,0],\"value\":60}},\"rules\":{}}"
            .to_owned(),
        "{\"format\":1,\"settings\":{},\"rules\":{\"u\":{\"at\":[1,0],\"rule\":{}}}}".to_owned(),
        "{\"format\":1,\"settings\":{}}".to_owned(),
        "{\"format\":0,\"settings\":{},\"rules\":{}}".to_owned(),
        // Entries written as arrays rather than objects.
        "{\"format\":1,\"settings\":{\"environment\":[[1,0],null]},\"rules\":{}}".to_owned(),
        "{\"format\":1,\"settings\":{},\"rules\":{\"u\":[[1,0],null]}}".to_owned(),
        "{\"format\":1,\"settings\":[],\"rules\":{}}".to_owned(),
    ] {
        assert!(
            matches!(classify(Some(&text)), Classified::Invalid),
            "{text}"
        );
    }
    let at_the_bound = format!(
        "{{\"format\":1,\"settings\":{{\"environment\":{{\"at\":[{MAX_MS},{}],\"value\":null}}}},\"rules\":{{}}}}",
        u32::MAX
    );
    assert!(matches!(
        classify(Some(&at_the_bound)),
        Classified::Valid(_)
    ));
}

#[test]
fn unknown_keys_in_a_format_one_document_are_ignored() {
    let text = "{\"format\":1,\"surprise\":[1,2],\"settings\":{\"environment\":{\"at\":[3,0],\"value\":\"\\\"prod\\\"\",\"origin\":\"x\"}},\"rules\":{}}";
    let document = valid(text);
    assert_eq!(
        document.encode(),
        "{\"format\":1,\"settings\":{\"environment\":{\"at\":[3,0],\"value\":\"\\\"prod\\\"\"}},\"rules\":{}}"
    );
}

// Replace once

#[test]
fn unreadable_remote_content_is_replaced_once_per_distinct_content() {
    let applied = synced(Some("prod"), Some(30), Some("[]"));
    let foreign = "{\"hello\":\"world\"}";

    let first = step(None, Some(foreign), &applied, None);
    assert_eq!(first.status, SettingsSyncStatus::RemoteReplaced);
    assert!(first.publish_document.is_some());
    assert_eq!(first.replaced_digest, Some(fnv1a64_hex(foreign)));

    let digest = first.replaced_digest.clone().expect("a digest");
    let replica = first.replica_document.expect("a replica");
    let second = step(Some(&replica), Some(foreign), &applied, Some(&digest));
    assert_eq!(second.status, SettingsSyncStatus::RemoteUnreadable);
    assert_eq!(second.publish_document, None);
    assert_eq!(second.replaced_digest, None);

    let other = "{\"hello\":\"again\"}";
    let third = step(Some(&replica), Some(other), &applied, Some(&digest));
    assert_eq!(third.status, SettingsSyncStatus::RemoteReplaced);
    assert!(third.publish_document.is_some());
    assert_eq!(third.replaced_digest, Some(fnv1a64_hex(other)));
}

#[test]
fn the_digest_is_lowercase_sixteen_digit_fnv1a64() {
    assert_eq!(fnv1a64_hex(""), "cbf29ce484222325");
    assert_eq!(fnv1a64_hex("a"), "af63dc4c8601ec8c");
    assert_eq!(fnv1a64_hex("foobar"), "85944171f73967e8");
}

// Version skew

/// A newer build that keeps one extra key, `"origin"`, on every register it has seen.
struct NewerWriter {
    replica: Option<String>,
    origins: BTreeMap<String, String>,
}

impl NewerWriter {
    /// The cell as this build sees it: the format-1 document plus its own extra keys.
    fn with_origins(&self, text: &str) -> Value {
        let mut value: Value = serde_json::from_str(text).expect("json");
        for (name, origin) in &self.origins {
            if let Some(entry) = value["settings"].get_mut(name) {
                entry["origin"] = Value::from(origin.as_str());
            }
        }
        value
    }

    fn round(&mut self, cell: &mut Option<String>, applied: &SyncedSettings) -> bool {
        let result = step(self.replica.as_deref(), cell.as_deref(), applied, None);
        self.replica.clone_from(&result.replica_document);
        let replica = result.replica_document.expect("a replica");
        let mine = self.with_origins(&replica);
        let theirs = cell
            .as_deref()
            .map(|text| serde_json::from_str::<Value>(text).expect("json"));
        if theirs.as_ref() == Some(&mine) {
            return false;
        }
        *cell = Some(mine.to_string());
        true
    }
}

#[test]
fn an_old_reader_and_a_new_writer_stop_publishing_within_three_rounds() {
    let applied = synced(Some("prod"), Some(30), Some("[]"));
    let mut newer = NewerWriter {
        replica: None,
        origins: BTreeMap::from([(ENVIRONMENT.to_owned(), "phone".to_owned())]),
    };
    let mut older_replica: Option<String> = None;
    let mut cell: Option<String> = None;

    let mut publishes = Vec::new();
    for _ in 0..3 {
        let newer_published = newer.round(&mut cell, &applied);

        let older = step(older_replica.as_deref(), cell.as_deref(), &applied, None);
        older_replica.clone_from(&older.replica_document);
        let older_published = older.publish_document.is_some();
        if let Some(text) = older.publish_document {
            cell = Some(text);
        }
        publishes.push((newer_published, older_published));
    }

    assert_eq!(publishes.last(), Some(&(false, false)), "{publishes:?}");
}

// Golden shape

#[test]
fn the_encoded_document_has_the_pinned_shape_and_bytes() {
    let mut document = Document::default();
    document
        .settings
        .insert(POLL_INTERVAL_SECONDS.to_owned(), register(0, 0, Some("60")));
    document.settings.insert(
        ENVIRONMENT.to_owned(),
        register(1_789_430_400_123, 0, Some("\"production\"")),
    );
    document.rules.insert(
        format!("pessimal::production::alerts::rule::{UUID_2}"),
        rule_entry(1_789_430_500_000, 1, None),
    );

    let expected = concat!(
        "{\"format\":1,\"settings\":{",
        "\"environment\":{\"at\":[1789430400123,0],\"value\":\"\\\"production\\\"\"},",
        "\"poll_interval_seconds\":{\"at\":[0,0],\"value\":\"60\"}},",
        "\"rules\":{\"pessimal::production::alerts::rule::01994f3b-0a4d-7e21-8c55-6f7a8b9c0d1e\":",
        "{\"at\":[1789430500000,1],\"rule\":null}}}"
    );
    assert_eq!(document.encode(), expected);
    assert_eq!(valid(expected), document);
}

#[test]
fn serde_json_is_built_without_preserve_order() {
    // The decoder reads through `serde_json::Value`. With `preserve_order` unified into the graph,
    // its maps keep insertion order and this assertion fails.
    let value: Value = serde_json::from_str("{\"b\":1,\"a\":2}").expect("json");
    assert_eq!(value.to_string(), "{\"a\":2,\"b\":1}");
}

// Budget

fn realistic_rule(index: usize) -> (String, String) {
    let uuid = format!("01994f3a-6c1e-7d3b-9a2f-{index:012x}");
    let hosts = (0..5)
        .map(|host| HostId::new(format!("web-{index:03}-{host}.eu-west-1.internal")))
        .collect();
    let name = format!("CPU above ninety percent on web tier {index:03}");
    assert_eq!(name.len(), 40);
    rule_with("production", &uuid, &name, HostSelector::AnyOf(hosts))
}

fn tombstones(document: &mut Document, count: usize) {
    for index in 0..count {
        document.rules.insert(
            format!("pessimal::production::alerts::rule::01994f3b-0a4d-7e21-8c55-{index:012x}"),
            rule_entry(1_789_430_500_000, 1, None),
        );
    }
}

#[test]
fn fifty_realistic_rules_and_two_hundred_tombstones_fit_the_budget() {
    let mut document = Document::default();
    for index in 0..50 {
        let (urn, body) = realistic_rule(index);
        document
            .rules
            .insert(urn, rule_entry(1_789_430_455_000, 0, Some(&body)));
    }
    tombstones(&mut document, 200);
    let text = document.encode();
    assert!(
        text.len() < SETTINGS_SYNC_BUDGET_BYTES,
        "{} bytes",
        text.len()
    );

    let applied = synced(Some("production"), Some(30), Some("[]"));
    let result = step(Some(&text), None, &applied, None);
    assert_eq!(result.status, SettingsSyncStatus::UpToDate);
    assert_eq!(result.publish_document.as_deref(), Some(text.as_str()));
}

#[test]
fn an_over_budget_document_is_kept_locally_and_not_published() {
    let mut document = Document::default();
    for index in 0..200 {
        let (urn, body) = realistic_rule(index);
        document
            .rules
            .insert(urn, rule_entry(1_789_430_455_000, 0, Some(&body)));
    }
    let text = document.encode();
    assert!(text.len() > SETTINGS_SYNC_BUDGET_BYTES);

    let applied = synced(Some("production"), Some(30), Some("[]"));
    let result = step(Some(&text), None, &applied, None);
    let bytes = u64::try_from(text.len()).expect("fits");
    assert_eq!(result.status, SettingsSyncStatus::TooLarge { bytes });
    assert_eq!(result.publish_document, None);
    assert_eq!(result.replica_document.as_deref(), Some(text.as_str()));
    assert!(
        result
            .settings
            .rules_json
            .is_some_and(|json| json.len() > 2)
    );
}

// Projection

#[test]
fn projection_passes_through_only_values_core_accepts() {
    let applied = synced(Some("prod"), Some(30), Some("[]"));

    let mut accepted = Document::default();
    accepted
        .settings
        .insert(ENVIRONMENT.to_owned(), register(1, 0, Some("\"staging\"")));
    accepted
        .settings
        .insert(POLL_INTERVAL_SECONDS.to_owned(), register(1, 0, Some("60")));
    let projection = project(&accepted, &applied);
    assert_eq!(projection.settings.environment.as_deref(), Some("staging"));
    assert_eq!(projection.settings.poll_interval_seconds, Some(60));
    assert!(projection.rejected.is_empty());

    let mut refused = Document::default();
    refused.settings.insert(
        ENVIRONMENT.to_owned(),
        register(1, 0, Some("\"st::aging\"")),
    );
    refused
        .settings
        .insert(POLL_INTERVAL_SECONDS.to_owned(), register(1, 0, Some("0")));
    let projection = project(&refused, &applied);
    assert_eq!(projection.settings.environment.as_deref(), Some("prod"));
    assert_eq!(projection.settings.poll_interval_seconds, Some(30));
    assert_eq!(projection.rejected.len(), 2);

    let mut mistyped = Document::default();
    mistyped
        .settings
        .insert(ENVIRONMENT.to_owned(), register(1, 0, Some("42")));
    mistyped.settings.insert(
        POLL_INTERVAL_SECONDS.to_owned(),
        register(1, 0, Some("\"60\"")),
    );
    let projection = project(&mistyped, &applied);
    assert_eq!(projection.settings, applied);
    assert_eq!(projection.rejected.len(), 2);
}

#[test]
fn a_null_register_removes_the_value() {
    let applied = synced(Some("prod"), Some(30), Some("[]"));
    let mut cleared = Document::default();
    cleared
        .settings
        .insert(ENVIRONMENT.to_owned(), register(1, 0, None));
    let projection = project(&cleared, &applied);
    assert_eq!(projection.settings.environment, None);
    assert_eq!(projection.settings.poll_interval_seconds, Some(30));
}

#[test]
fn projected_rules_come_out_in_creation_order() {
    // The URN of the newer rule sorts first as a string, so map order alone would be wrong.
    let (older_urn, older_body) = rule("zeta", UUID_1);
    let (newer_urn, newer_body) = rule("alpha", UUID_3);
    let (middle_urn, middle_body) = rule("mid", UUID_2);
    let mut document = Document::default();
    document
        .rules
        .insert(newer_urn, rule_entry(5, 0, Some(&newer_body)));
    document
        .rules
        .insert(older_urn, rule_entry(9, 0, Some(&older_body)));
    document
        .rules
        .insert(middle_urn, rule_entry(1, 0, Some(&middle_body)));

    let projection = project(&document, &synced(None, None, None));
    assert_eq!(
        projection.settings.rules_json.as_deref(),
        Some(rules_json(&[&older_body, &middle_body, &newer_body]).as_str())
    );
    assert_eq!(projection.hidden_rule_count, 0);
}

#[test]
fn a_rule_whose_id_differs_from_its_key_is_not_projected() {
    let (urn_1, _) = rule("prod", UUID_1);
    let (_, body_2) = rule("prod", UUID_2);
    let mut document = Document::default();
    document
        .rules
        .insert(urn_1, rule_entry(1, 0, Some(&body_2)));
    let projection = project(&document, &synced(None, None, None));
    assert_eq!(projection.settings.rules_json.as_deref(), Some("[]"));
    assert_eq!(projection.hidden_rule_count, 1);
}

#[test]
fn rules_without_history_keep_the_applied_value_and_tombstones_alone_give_an_empty_list() {
    let applied = synced(Some("prod"), Some(30), Some("[\"kept as is\"]"));
    assert_eq!(project(&Document::default(), &applied).settings, applied);

    let mut deleted = Document::default();
    deleted
        .rules
        .insert("urn-1".to_owned(), rule_entry(1, 0, None));
    assert_eq!(
        project(&deleted, &applied).settings.rules_json.as_deref(),
        Some("[]")
    );
}
