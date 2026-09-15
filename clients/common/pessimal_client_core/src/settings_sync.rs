//! Settings sync between a user's devices: the settings document, its merge, and one round of
//! the read, merge and write-back loop.
//!
//! # The mailbox
//!
//! The platform carries the encoded document in a mailbox, which on Apple platforms is the iCloud
//! key-value store. This module assumes only two things about it: a read may return an old value,
//! and a write may be lost. It assumes nothing else.
//!
//! # The document
//!
//! A [`Document`] holds one [`Register`] per setting name and one [`RuleEntry`] per alert rule URN.
//! Every entry carries a [`Stamp`]. [`join`] keeps the larger entry for each key under a fixed
//! total order, so it is commutative, associative and idempotent. Devices that hold the same
//! edits hold the same document and encode the same bytes, whatever order the copies arrived in.
//! A deleted rule stays in the document as a tombstone, which is above every live entry for its
//! URN.
//!
//! Payloads are JSON text stored inside JSON strings. The merge copies them and never re-encodes
//! them, so content a newer build wrote passes through unchanged.
//!
//! # Purity
//!
//! No I/O and no clock read. [`record_edits`] takes the current time as a parameter.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use chrono::Duration;
use pessimal_core::AlertRule;
use serde::Deserialize;

use crate::config::{FleetConfig, PollTuning};
use crate::error::ClientError;
use crate::rules::validate_rule;

/// The document format this build reads and writes.
///
/// It changes only when the stamp shape, the merge order or the entry shape changes. A new
/// synced setting is a new register name and keeps the format.
pub const SETTINGS_SYNC_FORMAT: u64 = 1;

/// The largest `ms` a stamp may carry: 9999-12-31T23:59:59.999Z.
pub const MAX_MS: u64 = 253_402_300_799_999;

/// The largest encoded document [`step`] publishes.
pub const SETTINGS_SYNC_BUDGET_BYTES: usize = 64 * 1024;

/// The register holding the environment name, as a JSON string.
pub const ENVIRONMENT: &str = "environment";

/// The register holding the poll interval in whole seconds, as a JSON integer.
pub const POLL_INTERVAL_SECONDS: &str = "poll_interval_seconds";

const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// When an entry was written: milliseconds since the Unix epoch, then a counter.
///
/// Ordered by `ms`, then `seq`. The derived order follows the field order. `ms` is never above
/// [`MAX_MS`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Stamp {
    ms: u64,
    seq: u32,
}

impl Stamp {
    /// The stamp of every entry [`bootstrap`] creates.
    pub const ZERO: Self = Self { ms: 0, seq: 0 };

    /// `None` when `ms` is above [`MAX_MS`].
    #[must_use]
    pub const fn new(ms: u64, seq: u32) -> Option<Self> {
        if ms <= MAX_MS {
            Some(Self { ms, seq })
        } else {
            None
        }
    }

    #[must_use]
    pub const fn ms(self) -> u64 {
        self.ms
    }

    #[must_use]
    pub const fn seq(self) -> u32 {
        self.seq
    }
}

/// One synced setting.
///
/// `value` is the JSON text of the setting, or `None` when the setting was cleared. Ordered by
/// `at`, then `value`, with `None` first and strings compared by bytes. The derived order follows
/// the field order.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Register {
    pub at: Stamp,
    pub value: Option<String>,
}

/// One synced alert rule, keyed by its URN in [`Document::rules`].
///
/// `rule` is the JSON text of the rule, or `None` when the rule was deleted. Ordered by
/// `rule.is_none()`, then `at`, then `rule`, so a tombstone is above every live entry.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RuleEntry {
    pub at: Stamp,
    pub rule: Option<String>,
}

impl Ord for RuleEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.rule.is_none(), self.at, &self.rule).cmp(&(
            other.rule.is_none(),
            other.at,
            &other.rule,
        ))
    }
}

impl PartialOrd for RuleEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// The settings a user's devices share, in format [`SETTINGS_SYNC_FORMAT`].
///
/// A key missing from a map has never been set. It is below every entry.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(try_from = "SettingsDocumentWire")]
pub struct Document {
    pub settings: BTreeMap<String, Register>,
    pub rules: BTreeMap<String, RuleEntry>,
}

impl Document {
    /// True when the document holds no entry at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.settings.is_empty() && self.rules.is_empty()
    }

    /// The largest stamp of any entry, or [`Stamp::ZERO`] for an empty document.
    #[must_use]
    pub fn top_stamp(&self) -> Stamp {
        self.settings
            .values()
            .map(|register| register.at)
            .chain(self.rules.values().map(|entry| entry.at))
            .max()
            .unwrap_or(Stamp::ZERO)
    }

    /// The canonical encoding: compact JSON, keys `format`, `settings`, `rules` in that order, map
    /// keys sorted by bytes, and strings escaped the way `serde_json` escapes them.
    ///
    /// Equal documents encode to equal bytes.
    #[must_use]
    pub fn encode(&self) -> String {
        let mut out = String::with_capacity(64);
        out.push_str("{\"format\":");
        out.push_str(&SETTINGS_SYNC_FORMAT.to_string());
        out.push_str(",\"settings\":{");
        for (index, (name, register)) in self.settings.iter().enumerate() {
            push_entry(
                &mut out,
                index,
                name,
                register.at,
                "value",
                register.value.as_deref(),
            );
        }
        out.push_str("},\"rules\":{");
        for (index, (urn, entry)) in self.rules.iter().enumerate() {
            push_entry(
                &mut out,
                index,
                urn,
                entry.at,
                "rule",
                entry.rule.as_deref(),
            );
        }
        out.push_str("}}");
        out
    }
}

fn push_entry(
    out: &mut String,
    index: usize,
    key: &str,
    at: Stamp,
    payload_key: &str,
    payload: Option<&str>,
) {
    if index > 0 {
        out.push(',');
    }
    push_json_string(out, key);
    out.push_str(":{\"at\":[");
    out.push_str(&at.ms.to_string());
    out.push(',');
    out.push_str(&at.seq.to_string());
    out.push_str("],\"");
    out.push_str(payload_key);
    out.push_str("\":");
    match payload {
        Some(text) => push_json_string(out, text),
        None => out.push_str("null"),
    }
    out.push('}');
}

/// Writes `text` as a JSON string with the same escapes `serde_json` uses.
fn push_json_string(out: &mut String, text: &str) {
    out.push('"');
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            control if control < ' ' => {
                let code = u32::from(control);
                out.push_str("\\u00");
                out.push(char::from_digit(code >> 4, 16).unwrap_or('0'));
                out.push(char::from_digit(code & 0xf, 16).unwrap_or('0'));
            }
            other => out.push(other),
        }
    }
    out.push('"');
}

fn json_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    push_json_string(&mut out, text);
    out
}

/// The only shape [`Document`] deserialises through.
///
/// Unknown keys are ignored at every level. Known keys are required and must have the right type.
#[derive(Deserialize)]
struct SettingsDocumentWire {
    format: u64,
    settings: BTreeMap<String, FromObject<RegisterWire>>,
    rules: BTreeMap<String, FromObject<RuleEntryWire>>,
}

/// Reads `T` only from a JSON object. A derived struct deserialiser also accepts an array.
struct FromObject<T>(T);

impl<'de, T: serde::de::DeserializeOwned> Deserialize<'de> for FromObject<T> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let object = serde_json::Map::deserialize(deserializer)?;
        serde_json::from_value(serde_json::Value::Object(object))
            .map(FromObject)
            .map_err(serde::de::Error::custom)
    }
}

#[derive(Deserialize)]
struct RegisterWire {
    at: (u64, u32),
    // `deserialize_with` makes the key required: a missing key is invalid, not a clear.
    #[serde(deserialize_with = "Option::deserialize")]
    value: Option<String>,
}

#[derive(Deserialize)]
struct RuleEntryWire {
    at: (u64, u32),
    #[serde(deserialize_with = "Option::deserialize")]
    rule: Option<String>,
}

impl TryFrom<SettingsDocumentWire> for Document {
    type Error = String;

    fn try_from(wire: SettingsDocumentWire) -> Result<Self, Self::Error> {
        if wire.format != SETTINGS_SYNC_FORMAT {
            return Err(format!(
                "format {} is not format {SETTINGS_SYNC_FORMAT}",
                wire.format
            ));
        }
        let settings = wire
            .settings
            .into_iter()
            .map(|(name, FromObject(register))| {
                let at = stamp_from_wire(register.at)?;
                Ok((
                    name,
                    Register {
                        at,
                        value: register.value,
                    },
                ))
            })
            .collect::<Result<_, String>>()?;
        let rules = wire
            .rules
            .into_iter()
            .map(|(urn, FromObject(entry))| {
                let at = stamp_from_wire(entry.at)?;
                Ok((
                    urn,
                    RuleEntry {
                        at,
                        rule: entry.rule,
                    },
                ))
            })
            .collect::<Result<_, String>>()?;
        Ok(Self { settings, rules })
    }
}

fn stamp_from_wire((ms, seq): (u64, u32)) -> Result<Stamp, String> {
    Stamp::new(ms, seq).ok_or_else(|| format!("stamp ms {ms} is above {MAX_MS}"))
}

/// What a stored or received text holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Classified {
    /// There is no text.
    Absent,
    /// A document in a format newer than this build reads.
    Newer(u64),
    /// Not a JSON object, or `format` is missing, not an integer, or negative.
    Foreign,
    /// `format` is not newer, but the text is not a valid format 1 document.
    Invalid,
    Valid(Document),
}

/// Decodes in two stages: `format` alone first, then the whole document only when the format is
/// not newer. Every device applies the same checks, so every device classifies a text the same way.
#[must_use]
pub fn classify(text: Option<&str>) -> Classified {
    let Some(text) = text else {
        return Classified::Absent;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return Classified::Foreign;
    };
    let Some(format) = value
        .as_object()
        .and_then(|object| object.get("format"))
        .and_then(serde_json::Value::as_u64)
    else {
        return Classified::Foreign;
    };
    if format > SETTINGS_SYNC_FORMAT {
        return Classified::Newer(format);
    }
    serde_json::from_value::<Document>(value).map_or(Classified::Invalid, Classified::Valid)
}

/// For each key of either document, the larger entry.
#[must_use]
pub fn join(left: &Document, right: &Document) -> Document {
    let mut joined = left.clone();
    for (name, register) in &right.settings {
        keep_larger(&mut joined.settings, name, register);
    }
    for (urn, entry) in &right.rules {
        keep_larger(&mut joined.rules, urn, entry);
    }
    joined
}

fn keep_larger<E: Ord + Clone>(map: &mut BTreeMap<String, E>, key: &str, entry: &E) {
    match map.get_mut(key) {
        Some(existing) => {
            if *entry > *existing {
                existing.clone_from(entry);
            }
        }
        None => {
            map.insert(key.to_owned(), entry.clone());
        }
    }
}

/// Why settings sync could not record an edit.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SettingsSyncError {
    /// Core refused an edited value. The message is core's.
    #[error(transparent)]
    Refused(#[from] ClientError),

    /// No stamp is left above the document's largest one. That needs four billion Saves inside
    /// the last millisecond of the year 9999.
    #[error("no settings stamp is left after [{MAX_MS}, {max}]", max = u32::MAX)]
    StampSpaceExhausted,
}

/// A new stamp, larger than every stamp in `document`.
///
/// `now_ms` is clamped to `0..=MAX_MS`. When it is not past the document's largest stamp, the
/// counter goes up instead, so a clock that steps backwards still gives a larger stamp.
///
/// # Errors
/// [`SettingsSyncError::StampSpaceExhausted`] when the document already holds
/// `[MAX_MS, u32::MAX]`.
pub fn tick(document: &Document, now_ms: i64) -> Result<Stamp, SettingsSyncError> {
    let top = document.top_stamp();
    let now = u64::try_from(now_ms).unwrap_or(0).min(MAX_MS);
    if now > top.ms {
        Ok(Stamp { ms: now, seq: 0 })
    } else if top.seq < u32::MAX {
        Ok(Stamp {
            ms: top.ms,
            seq: top.seq + 1,
        })
    } else if top.ms < MAX_MS {
        Ok(Stamp {
            ms: top.ms + 1,
            seq: 0,
        })
    } else {
        Err(SettingsSyncError::StampSpaceExhausted)
    }
}

/// The three synced values as the device stores them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncedSettings {
    pub environment: Option<String>,
    pub poll_interval_seconds: Option<i64>,
    /// A JSON array of alert rules.
    pub rules_json: Option<String>,
}

/// A document built from the device's stored values, with every entry at [`Stamp::ZERO`].
///
/// Absent values produce no entry.
///
/// # Errors
/// [`ClientError::InvalidRule`] when `applied.rules_json` is not a JSON array of rules core
/// decodes. Sync stays off for the device until a Save replaces the rules.
pub fn bootstrap(applied: &SyncedSettings) -> Result<Document, ClientError> {
    let rules = applied
        .rules_json
        .as_deref()
        .map(decode_rules)
        .transpose()?;
    bootstrap_with(applied, rules.as_deref())
}

fn bootstrap_with(
    applied: &SyncedSettings,
    rules: Option<&[AlertRule]>,
) -> Result<Document, ClientError> {
    let mut document = Document::default();
    if let Some(environment) = &applied.environment {
        document.settings.insert(
            ENVIRONMENT.to_owned(),
            Register {
                at: Stamp::ZERO,
                value: Some(json_string(environment)),
            },
        );
    }
    if let Some(seconds) = applied.poll_interval_seconds {
        document.settings.insert(
            POLL_INTERVAL_SECONDS.to_owned(),
            Register {
                at: Stamp::ZERO,
                value: Some(seconds.to_string()),
            },
        );
    }
    for rule in rules.unwrap_or_default() {
        let entry = RuleEntry {
            at: Stamp::ZERO,
            rule: Some(encode_rule(rule)?),
        };
        keep_larger(&mut document.rules, &rule.id().to_string(), &entry);
    }
    Ok(document)
}

/// The result of a local Save.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsEdit {
    /// The device's new merged copy. Persist it before anything else.
    pub replica_document: String,
    /// The values to store and apply.
    ///
    /// A value that no device ever set and this Save did not change keeps the applied value, which
    /// can be `None`. On a first run, a Save that keeps the default poll interval returns
    /// `poll_interval_seconds: None`.
    pub settings: SyncedSettings,
}

/// Records a local Save as stamped entries.
///
/// Only the values that differ between `base` (what the settings screen started from) and
/// `edited` are stamped, so a remote change that arrived while the screen was open survives the
/// Save. Rules in `base` and missing from `edited` become tombstones. A `base.rules_json` that is
/// `None` or does not decode deletes nothing. An `edited.rules_json` of `None` changes no rule.
///
/// When `local_document` is not a valid document, the device's stored values are the starting
/// point. Stored rules that do not decode are left out of it, because the Save replaces them.
///
/// # Errors
/// [`SettingsSyncError::Refused`] with core's message when core refuses an edited environment,
/// poll interval or rule. Nothing is recorded then.
/// [`SettingsSyncError::StampSpaceExhausted`] as for [`tick`].
pub fn record_edits(
    local_document: Option<&str>,
    applied: &SyncedSettings,
    base: &SyncedSettings,
    edited: &SyncedSettings,
    now_ms: i64,
) -> Result<SettingsEdit, SettingsSyncError> {
    let edited_rules = check_edited(edited)?;
    let document = if let Classified::Valid(document) = classify(local_document) {
        document
    } else {
        let readable = applied
            .rules_json
            .as_deref()
            .and_then(|text| decode_rules(text).ok());
        bootstrap_with(applied, readable.as_deref())?
    };

    let at = tick(&document, now_ms)?;
    let mut edits = Document::default();
    if edited.environment != base.environment {
        edits.settings.insert(
            ENVIRONMENT.to_owned(),
            Register {
                at,
                value: edited.environment.as_deref().map(json_string),
            },
        );
    }
    let base_interval = base
        .poll_interval_seconds
        .or_else(|| Some(default_poll_interval_seconds()));
    if edited.poll_interval_seconds != base_interval {
        edits.settings.insert(
            POLL_INTERVAL_SECONDS.to_owned(),
            Register {
                at,
                value: edited
                    .poll_interval_seconds
                    .map(|seconds| seconds.to_string()),
            },
        );
    }
    if let Some(edited_rules) = &edited_rules {
        record_rule_edits(&document, base, edited_rules, at, &mut edits)?;
    }

    let recorded = join(&document, &edits);
    // With no rule entries at all, the projection keeps the stored rules. After a Save, the
    // stored rules are the ones the user saved.
    let fallback = SyncedSettings {
        rules_json: edited
            .rules_json
            .clone()
            .or_else(|| applied.rules_json.clone()),
        ..applied.clone()
    };
    Ok(SettingsEdit {
        replica_document: recorded.encode(),
        settings: project(&recorded, &fallback).settings,
    })
}

/// Validates every edited value with core. Returns the decoded rules, if any were edited.
fn check_edited(edited: &SyncedSettings) -> Result<Option<Vec<AlertRule>>, ClientError> {
    if let Some(environment) = &edited.environment {
        check_environment(environment)?;
    }
    if let Some(seconds) = edited.poll_interval_seconds {
        check_poll_interval(seconds)?;
    }
    let Some(text) = &edited.rules_json else {
        return Ok(None);
    };
    let rules = decode_rules(text)?;
    for rule in &rules {
        validate_rule(rule)?;
    }
    Ok(Some(rules))
}

fn record_rule_edits(
    document: &Document,
    base: &SyncedSettings,
    edited: &[AlertRule],
    at: Stamp,
    edits: &mut Document,
) -> Result<(), ClientError> {
    let edited: BTreeMap<String, &AlertRule> = edited
        .iter()
        .map(|rule| (rule.id().to_string(), rule))
        .collect();
    let base: Option<BTreeMap<String, AlertRule>> = base
        .rules_json
        .as_deref()
        .and_then(|text| decode_rules(text).ok())
        .map(|rules| {
            rules
                .into_iter()
                .map(|rule| (rule.id().to_string(), rule))
                .collect()
        });

    for (urn, rule) in &edited {
        let unchanged = base
            .as_ref()
            .and_then(|base| base.get(urn))
            .is_some_and(|before| same_rule(before, rule));
        let already_live = document
            .rules
            .get(urn)
            .and_then(|entry| entry.rule.as_deref())
            .and_then(|body| serde_json::from_str::<AlertRule>(body).ok())
            .is_some_and(|stored| same_rule(&stored, rule));
        if !unchanged && !already_live {
            edits.rules.insert(
                urn.clone(),
                RuleEntry {
                    at,
                    rule: Some(encode_rule(rule)?),
                },
            );
        }
    }
    if let Some(base) = &base {
        for urn in base.keys().filter(|urn| !edited.contains_key(*urn)) {
            edits
                .rules
                .insert(urn.clone(), RuleEntry { at, rule: None });
        }
    }
    Ok(())
}

/// Compares the encoded rules. This covers every field, and it tells thresholds apart by their
/// bits, because `serde_json` writes the shortest text that reads back to the same `f64`.
fn same_rule(left: &AlertRule, right: &AlertRule) -> bool {
    matches!((encode_rule(left), encode_rule(right)), (Ok(left), Ok(right)) if left == right)
}

fn decode_rules(text: &str) -> Result<Vec<AlertRule>, ClientError> {
    serde_json::from_str(text).map_err(|error| ClientError::InvalidRule(error.to_string()))
}

fn encode_rule(rule: &AlertRule) -> Result<String, ClientError> {
    serde_json::to_string(rule).map_err(|error| ClientError::InvalidRule(error.to_string()))
}

fn check_environment(environment: &str) -> Result<(), ClientError> {
    FleetConfig::new(environment, PollTuning::default()).map(|_| ())
}

/// The interval must pass core's tuning checks with every other field at its default.
fn check_poll_interval(seconds: i64) -> Result<(), ClientError> {
    let interval = Duration::try_seconds(seconds).ok_or_else(|| {
        ClientError::InvalidTuning(format!(
            "poll interval {seconds}s is not a representable duration"
        ))
    })?;
    let defaults = PollTuning::default();
    PollTuning::new(
        defaults.liveness(),
        interval,
        defaults.metric_step(),
        defaults.chart_window(),
        defaults.max_staleness(),
        defaults.backend_lag_allowance(),
        defaults.forget_host_after(),
        defaults.max_retained_hosts(),
    )
    .map(|_| ())
}

fn default_poll_interval_seconds() -> i64 {
    PollTuning::default().poll_interval().num_seconds()
}

/// What one round of the loop found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingsSyncStatus {
    UpToDate,
    /// The remote document is in a newer format. It is not merged and not written over.
    PausedNewerFormat {
        format: u64,
    },
    /// The remote text was not a readable document, and this round replaces it.
    RemoteReplaced,
    /// The remote text is not a readable document, and this device already replaced that same
    /// text once.
    RemoteUnreadable,
    /// The merged document is over [`SETTINGS_SYNC_BUDGET_BYTES`], so it is not published.
    TooLarge {
        bytes: u64,
    },
    /// The stored rules do not decode, so sync is off until a Save replaces them.
    BootstrapDeferred {
        message: String,
    },
}

/// The decisions of one round of the loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsSyncStep {
    /// The device's new merged copy. Persist it before anything else. `None` only when the
    /// bootstrap was deferred.
    pub replica_document: Option<String>,
    /// The text to write to the mailbox, if any.
    pub publish_document: Option<String>,
    /// The values to store and apply.
    pub settings: SyncedSettings,
    pub status: SettingsSyncStatus,
    /// Live rules that are not applied, because this build cannot decode or accept them.
    pub hidden_rule_count: u32,
    /// Core's reasons for synced values it refused.
    pub rejected: Vec<String>,
    /// The digest to store once `publish_document` is written.
    pub replaced_digest: Option<String>,
}

/// One round: merge the device's copy with the mailbox and decide what to persist, apply and
/// write back.
///
/// It writes back only when the merged document differs from the remote one. Unreadable remote
/// text is replaced at most once per distinct text: `last_replaced_digest` is the digest this
/// device stored the last time it replaced one.
#[must_use]
pub fn step(
    local_document: Option<&str>,
    remote_document: Option<&str>,
    applied: &SyncedSettings,
    last_replaced_digest: Option<&str>,
) -> SettingsSyncStep {
    let local = if let Classified::Valid(document) = classify(local_document) {
        document
    } else {
        match bootstrap(applied) {
            Ok(document) => document,
            Err(error) => {
                return SettingsSyncStep {
                    replica_document: None,
                    publish_document: None,
                    settings: applied.clone(),
                    status: SettingsSyncStatus::BootstrapDeferred {
                        message: error.to_string(),
                    },
                    hidden_rule_count: 0,
                    rejected: Vec::new(),
                    replaced_digest: None,
                };
            }
        }
    };

    let mut replaced_digest = None;
    let (merged, mut publish, mut status) = match classify(remote_document) {
        Classified::Absent => {
            let publish = !local.is_empty();
            (local, publish, SettingsSyncStatus::UpToDate)
        }
        Classified::Newer(format) => (
            local,
            false,
            SettingsSyncStatus::PausedNewerFormat { format },
        ),
        Classified::Foreign | Classified::Invalid => {
            let digest = fnv1a64_hex(remote_document.unwrap_or_default());
            if last_replaced_digest == Some(digest.as_str()) {
                (local, false, SettingsSyncStatus::RemoteUnreadable)
            } else {
                replaced_digest = Some(digest);
                (local, true, SettingsSyncStatus::RemoteReplaced)
            }
        }
        Classified::Valid(remote) => {
            let merged = join(&local, &remote);
            let publish = merged != remote;
            (merged, publish, SettingsSyncStatus::UpToDate)
        }
    };

    let text = merged.encode();
    if publish && text.len() > SETTINGS_SYNC_BUDGET_BYTES {
        publish = false;
        status = SettingsSyncStatus::TooLarge {
            bytes: u64::try_from(text.len()).unwrap_or(u64::MAX),
        };
        replaced_digest = None;
    }
    let Projection {
        settings,
        hidden_rule_count,
        rejected,
    } = project(&merged, applied);
    SettingsSyncStep {
        publish_document: publish.then(|| text.clone()),
        replica_document: Some(text),
        settings,
        status,
        hidden_rule_count,
        rejected,
        replaced_digest,
    }
}

/// What the running app may apply from a document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Projection {
    pub settings: SyncedSettings,
    /// Live rules that are not applied, because this build cannot decode or accept them.
    pub hidden_rule_count: u32,
    /// Core's reasons for synced values it refused.
    pub rejected: Vec<String>,
}

/// The values the app may apply from `document`, falling back to `applied`.
///
/// - A setting with no register keeps the applied value. A cleared register gives `None`. A value
///   core refuses keeps the applied value and adds core's reason to `rejected`.
/// - Rules: every live entry whose body decodes, passes [`validate_rule`], and has the id of its
///   key, sorted by the UUID in the URN and then by URN. The bodies are copied, not re-encoded.
///   With no rule entry at all, not even a tombstone, the applied rules are kept.
#[must_use]
pub fn project(document: &Document, applied: &SyncedSettings) -> Projection {
    let mut rejected = Vec::new();
    let environment = project_register(
        document.settings.get(ENVIRONMENT),
        applied.environment.as_ref(),
        decode_environment,
        &mut rejected,
    );
    let poll_interval_seconds = project_register(
        document.settings.get(POLL_INTERVAL_SECONDS),
        applied.poll_interval_seconds.as_ref(),
        decode_poll_interval,
        &mut rejected,
    );
    let (rules_json, hidden_rule_count) = project_rules(document, applied.rules_json.as_deref());
    Projection {
        settings: SyncedSettings {
            environment,
            poll_interval_seconds,
            rules_json,
        },
        hidden_rule_count,
        rejected,
    }
}

fn project_register<T: Clone>(
    register: Option<&Register>,
    applied: Option<&T>,
    decode: fn(&str) -> Result<T, String>,
    rejected: &mut Vec<String>,
) -> Option<T> {
    let Some(register) = register else {
        return applied.cloned();
    };
    let text = register.value.as_deref()?;
    match decode(text) {
        Ok(value) => Some(value),
        Err(message) => {
            rejected.push(message);
            applied.cloned()
        }
    }
}

fn decode_environment(text: &str) -> Result<String, String> {
    let environment: String = serde_json::from_str(text)
        .map_err(|_| format!("the synced environment {text} is not a JSON string"))?;
    check_environment(&environment).map_err(|error| error.to_string())?;
    Ok(environment)
}

fn decode_poll_interval(text: &str) -> Result<i64, String> {
    let seconds: i64 = serde_json::from_str(text)
        .map_err(|_| format!("the synced poll interval {text} is not a whole number of seconds"))?;
    check_poll_interval(seconds).map_err(|error| error.to_string())?;
    Ok(seconds)
}

fn project_rules(document: &Document, applied: Option<&str>) -> (Option<String>, u32) {
    if document.rules.is_empty() {
        return (applied.map(str::to_owned), 0);
    }
    let mut shown = Vec::new();
    let mut hidden: u32 = 0;
    for (urn, entry) in &document.rules {
        let Some(body) = entry.rule.as_deref() else {
            continue;
        };
        match serde_json::from_str::<AlertRule>(body) {
            Ok(rule) if rule.id().to_string() == *urn && validate_rule(&rule).is_ok() => {
                shown.push((rule.id().id(), urn.as_str(), body));
            }
            _ => hidden = hidden.saturating_add(1),
        }
    }
    shown.sort_unstable();
    let bodies: Vec<&str> = shown.iter().map(|(_, _, body)| *body).collect();
    (Some(format!("[{}]", bodies.join(","))), hidden)
}

/// The FNV-1a 64-bit digest of `text`, as 16 lowercase hex digits.
#[must_use]
pub fn fnv1a64_hex(text: &str) -> String {
    let mut hash = FNV_OFFSET_BASIS;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strings_are_escaped_exactly_as_serde_json_escapes_them() {
        let mut every_control = String::new();
        for code in 0_u8..0x20 {
            every_control.push(char::from(code));
        }
        for text in [
            "",
            "plain",
            "\"quoted\" and \\back\\slashed",
            "line\nbreak\r\ttab",
            &every_control,
            "\u{7f} delete, é, 日本, 🦀, / slash",
        ] {
            assert_eq!(
                json_string(text),
                serde_json::to_string(text).expect("a string serialises"),
                "{text:?}"
            );
        }
    }

    #[test]
    fn stamps_order_by_milliseconds_then_counter() {
        let early = Stamp::new(1, u32::MAX).expect("in range");
        let late = Stamp::new(2, 0).expect("in range");
        assert!(early < late);
        assert_eq!(Stamp::new(MAX_MS, 0).map(Stamp::ms), Some(MAX_MS));
        assert_eq!(Stamp::new(MAX_MS + 1, 0), None);
    }

    #[test]
    fn an_empty_document_encodes_as_three_keys() {
        assert_eq!(
            Document::default().encode(),
            "{\"format\":1,\"settings\":{},\"rules\":{}}"
        );
        assert_eq!(
            classify(Some(&Document::default().encode())),
            Classified::Valid(Document::default())
        );
    }

    #[test]
    fn keys_needing_escapes_survive_an_encode_and_decode() {
        let mut document = Document::default();
        document.settings.insert(
            "quote\" and \u{1}".to_owned(),
            Register {
                at: Stamp::new(3, 4).expect("in range"),
                value: Some("\"\\\u{2028}".to_owned()),
            },
        );
        assert_eq!(
            classify(Some(&document.encode())),
            Classified::Valid(document)
        );
    }
}
