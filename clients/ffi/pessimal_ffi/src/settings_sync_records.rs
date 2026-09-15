//! Settings sync, in shapes UniFFI can carry.
//!
//! Mirrors `pessimal_client_core::settings_sync`. Core makes every decision. The documents cross as
//! the text core encoded, and this module does not read them. The app passes the current time in,
//! so nothing here reads a clock.

use pessimal_client_core::settings_sync::{
    SettingsEdit, SettingsSyncError, SettingsSyncStatus, SettingsSyncStep, SyncedSettings,
    record_edits, step,
};

use crate::convert::FfiError;

/// The three synced values as the device stores them. `None` is a value that is not stored.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct SyncedSettingsRecord {
    pub environment: Option<String>,
    pub poll_interval_seconds: Option<i64>,
    /// A JSON array of alert rules, as `FleetConfigRecord::rules_json` carries it.
    pub rules_json: Option<String>,
}

impl From<SyncedSettings> for SyncedSettingsRecord {
    fn from(settings: SyncedSettings) -> Self {
        let SyncedSettings {
            environment,
            poll_interval_seconds,
            rules_json,
        } = settings;
        Self {
            environment,
            poll_interval_seconds,
            rules_json,
        }
    }
}

impl From<SyncedSettingsRecord> for SyncedSettings {
    fn from(record: SyncedSettingsRecord) -> Self {
        let SyncedSettingsRecord {
            environment,
            poll_interval_seconds,
            rules_json,
        } = record;
        Self {
            environment,
            poll_interval_seconds,
            rules_json,
        }
    }
}

/// What one sync round found. Mirrors core's `SettingsSyncStatus`.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum SettingsSyncStatusRecord {
    UpToDate,
    PausedNewerFormat { format: u64 },
    RemoteReplaced,
    RemoteUnreadable,
    TooLarge { bytes: u64 },
    BootstrapDeferred { message: String },
}

impl From<SettingsSyncStatus> for SettingsSyncStatusRecord {
    fn from(status: SettingsSyncStatus) -> Self {
        match status {
            SettingsSyncStatus::UpToDate => Self::UpToDate,
            SettingsSyncStatus::PausedNewerFormat { format } => Self::PausedNewerFormat { format },
            SettingsSyncStatus::RemoteReplaced => Self::RemoteReplaced,
            SettingsSyncStatus::RemoteUnreadable => Self::RemoteUnreadable,
            SettingsSyncStatus::TooLarge { bytes } => Self::TooLarge { bytes },
            SettingsSyncStatus::BootstrapDeferred { message } => {
                Self::BootstrapDeferred { message }
            }
        }
    }
}

/// The decisions of one sync round. Mirrors core's `SettingsSyncStep`.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct SettingsSyncStepRecord {
    /// The device's new merged copy. Persist it before anything else.
    pub replica_document: Option<String>,
    /// The text to write to the mailbox, if any.
    pub publish_document: Option<String>,
    /// The values to store and apply.
    pub settings: SyncedSettingsRecord,
    pub status: SettingsSyncStatusRecord,
    /// Live rules that are not applied, because this build cannot decode or accept them.
    pub hidden_rule_count: u32,
    /// Core's reasons for synced values it refused.
    pub rejected: Vec<String>,
    /// The digest to store once `publish_document` is written.
    pub replaced_digest: Option<String>,
}

impl From<SettingsSyncStep> for SettingsSyncStepRecord {
    fn from(step: SettingsSyncStep) -> Self {
        let SettingsSyncStep {
            replica_document,
            publish_document,
            settings,
            status,
            hidden_rule_count,
            rejected,
            replaced_digest,
        } = step;
        Self {
            replica_document,
            publish_document,
            settings: settings.into(),
            status: status.into(),
            hidden_rule_count,
            rejected,
            replaced_digest,
        }
    }
}

/// The result of a local Save. Mirrors core's `SettingsEdit`.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct SettingsSyncEditRecord {
    /// The device's new merged copy. Persist it before anything else.
    pub replica_document: String,
    /// The values to store and apply. A value can be `None`, for example the poll interval after a
    /// first-run Save that keeps the default.
    pub settings: SyncedSettingsRecord,
}

impl From<SettingsEdit> for SettingsSyncEditRecord {
    fn from(edit: SettingsEdit) -> Self {
        let SettingsEdit {
            replica_document,
            settings,
        } = edit;
        Self {
            replica_document,
            settings: settings.into(),
        }
    }
}

/// A refusal keeps core's variant and message. No stamp left is a fault, so it is `Internal`.
fn sync_error_to_ffi(error: SettingsSyncError) -> FfiError {
    match error {
        SettingsSyncError::Refused(refusal) => FfiError::from(refusal),
        SettingsSyncError::StampSpaceExhausted => FfiError::Internal {
            message: error.to_string(),
        },
    }
}

/// One sync round: core's `settings_sync::step`.
#[uniffi::export]
#[must_use]
#[allow(
    clippy::needless_pass_by_value,
    reason = "UniFFI lifts a foreign string as an owned `String`; `&str` cannot cross the boundary"
)]
pub fn settings_sync_step(
    local_document: Option<String>,
    remote_document: Option<String>,
    applied: SyncedSettingsRecord,
    last_replaced_digest: Option<String>,
) -> SettingsSyncStepRecord {
    step(
        local_document.as_deref(),
        remote_document.as_deref(),
        &applied.into(),
        last_replaced_digest.as_deref(),
    )
    .into()
}

/// A local Save: core's `settings_sync::record_edits`. `now_unix_ms` is the current time, in
/// milliseconds since the Unix epoch.
///
/// # Errors
/// The [`FfiError`] variant core's refusal maps to, with core's message, when core refuses an
/// edited value. [`FfiError::Internal`] when no stamp is left.
#[uniffi::export]
#[allow(
    clippy::needless_pass_by_value,
    reason = "UniFFI lifts a foreign string as an owned `String`; `&str` cannot cross the boundary"
)]
pub fn settings_sync_record_edits(
    local_document: Option<String>,
    applied: SyncedSettingsRecord,
    base: SyncedSettingsRecord,
    edited: SyncedSettingsRecord,
    now_unix_ms: i64,
) -> Result<SettingsSyncEditRecord, FfiError> {
    record_edits(
        local_document.as_deref(),
        &applied.into(),
        &base.into(),
        &edited.into(),
        now_unix_ms,
    )
    .map(SettingsSyncEditRecord::from)
    .map_err(sync_error_to_ffi)
}

#[cfg(test)]
mod tests {
    use chrono::Duration;
    use pessimal_client_core::settings_sync::{
        self as core_sync, Classified, Document, ENVIRONMENT, MAX_MS, Register, RuleEntry,
        SettingsSyncError, SettingsSyncStatus, Stamp, SyncedSettings, classify, fnv1a64_hex,
    };
    use pessimal_client_core::{ClientError, draft_rule};
    use pessimal_core::{AlertRule, Comparator, HostSelector, MetricKind};

    use crate::convert::FfiError;

    use super::{
        SettingsSyncStatusRecord, SettingsSyncStepRecord, SyncedSettingsRecord,
        settings_sync_record_edits, settings_sync_step,
    };

    const NOW_MS: i64 = 1_789_430_400_123;
    const RULE_UUID: &str = "01994f3a-6c1e-7d3b-9a2f-4b8c1d2e3f40";
    const OPAQUE_URN: &str =
        "pessimal::production::alerts::rule::01994f3b-0a4d-7e21-8c55-6f7a8b9c0d1e";

    /// One rule core accepts, as a JSON array. The id is fixed, so every call gives the same text.
    fn rules_json() -> String {
        let drafted = draft_rule(
            "production",
            "CPU high",
            MetricKind::CpuUtilization,
            Comparator::GreaterThan,
            0.9,
            Duration::seconds(300),
            HostSelector::All,
        )
        .expect("a valid rule");
        let mut value = serde_json::to_value(&drafted).expect("a rule serialises");
        value["id"]["id"] = serde_json::Value::from(RULE_UUID);
        let rule: AlertRule = serde_json::from_value(value).expect("the rule decodes");
        serde_json::to_string(&[rule]).expect("a rule serialises")
    }

    fn record(
        environment: Option<&str>,
        poll_interval_seconds: Option<i64>,
        rules_json: Option<&str>,
    ) -> SyncedSettingsRecord {
        SyncedSettingsRecord {
            environment: environment.map(str::to_owned),
            poll_interval_seconds,
            rules_json: rules_json.map(str::to_owned),
        }
    }

    fn applied() -> SyncedSettingsRecord {
        record(Some("production"), Some(30), Some(&rules_json()))
    }

    fn stamp(ms: u64, seq: u32) -> Stamp {
        Stamp::new(ms, seq).expect("a stamp inside the range")
    }

    /// A valid document holding content this build does not apply: an unknown register name, a
    /// rule body that does not decode, and strings that need escaping.
    fn opaque_document() -> String {
        let mut document = Document::default();
        document.settings.insert(
            ENVIRONMENT.to_owned(),
            Register {
                at: stamp(7, 0),
                value: Some("\"a::b\"".to_owned()),
            },
        );
        document.settings.insert(
            "z_newer_setting".to_owned(),
            Register {
                at: stamp(3, 1),
                value: Some("{\"é\":\"日本\\n\\\"\",\"tab\":\"\t\"}".to_owned()),
            },
        );
        document.rules.insert(
            OPAQUE_URN.to_owned(),
            RuleEntry {
                at: stamp(9, 2),
                rule: Some("{\"metric\":\"future.metric\"}".to_owned()),
            },
        );
        document.encode()
    }

    #[test]
    fn synced_settings_cross_in_both_directions_unchanged() {
        for original in [
            record(None, None, None),
            record(Some(""), Some(0), Some("")),
            applied(),
        ] {
            let core = SyncedSettings::from(original.clone());
            assert_eq!(core.environment, original.environment);
            assert_eq!(core.poll_interval_seconds, original.poll_interval_seconds);
            assert_eq!(core.rules_json, original.rules_json);
            assert_eq!(SyncedSettingsRecord::from(core), original);
        }
    }

    #[test]
    fn every_status_crosses_with_its_payload() {
        let pairs = [
            (
                SettingsSyncStatus::UpToDate,
                SettingsSyncStatusRecord::UpToDate,
            ),
            (
                SettingsSyncStatus::PausedNewerFormat { format: 7 },
                SettingsSyncStatusRecord::PausedNewerFormat { format: 7 },
            ),
            (
                SettingsSyncStatus::RemoteReplaced,
                SettingsSyncStatusRecord::RemoteReplaced,
            ),
            (
                SettingsSyncStatus::RemoteUnreadable,
                SettingsSyncStatusRecord::RemoteUnreadable,
            ),
            (
                SettingsSyncStatus::TooLarge { bytes: 70_000 },
                SettingsSyncStatusRecord::TooLarge { bytes: 70_000 },
            ),
            (
                SettingsSyncStatus::BootstrapDeferred {
                    message: "no rules".to_owned(),
                },
                SettingsSyncStatusRecord::BootstrapDeferred {
                    message: "no rules".to_owned(),
                },
            ),
        ];
        for (core, expected) in pairs {
            assert_eq!(SettingsSyncStatusRecord::from(core), expected);
        }
    }

    #[test]
    fn a_step_record_carries_every_field_of_cores_step() {
        let foreign = "not json";
        let cases = [
            (
                None,
                None,
                applied(),
                None,
                SettingsSyncStatusRecord::UpToDate,
            ),
            (
                None,
                Some(opaque_document()),
                applied(),
                None,
                SettingsSyncStatusRecord::UpToDate,
            ),
            (
                None,
                Some(foreign.to_owned()),
                applied(),
                None,
                SettingsSyncStatusRecord::RemoteReplaced,
            ),
            (
                None,
                Some(foreign.to_owned()),
                applied(),
                Some(fnv1a64_hex(foreign)),
                SettingsSyncStatusRecord::RemoteUnreadable,
            ),
            (
                None,
                Some("{\"format\":2,\"surprise\":true}".to_owned()),
                applied(),
                None,
                SettingsSyncStatusRecord::PausedNewerFormat { format: 2 },
            ),
            (
                None,
                None,
                record(Some("production"), None, Some("not json")),
                None,
                SettingsSyncStatusRecord::BootstrapDeferred {
                    message: core_sync::bootstrap(&SyncedSettings {
                        environment: Some("production".to_owned()),
                        poll_interval_seconds: None,
                        rules_json: Some("not json".to_owned()),
                    })
                    .expect_err("the rules do not decode")
                    .to_string(),
                },
            ),
        ];

        for (local, remote, applied, digest, expected_status) in cases {
            let core = core_sync::step(
                local.as_deref(),
                remote.as_deref(),
                &SyncedSettings::from(applied.clone()),
                digest.as_deref(),
            );
            let SettingsSyncStepRecord {
                replica_document,
                publish_document,
                settings,
                status,
                hidden_rule_count,
                rejected,
                replaced_digest,
            } = settings_sync_step(local, remote, applied, digest);

            assert_eq!(status, expected_status);
            assert_eq!(replica_document, core.replica_document);
            assert_eq!(publish_document, core.publish_document);
            assert_eq!(settings.environment, core.settings.environment);
            assert_eq!(
                settings.poll_interval_seconds,
                core.settings.poll_interval_seconds
            );
            assert_eq!(settings.rules_json, core.settings.rules_json);
            assert_eq!(hidden_rule_count, core.hidden_rule_count);
            assert_eq!(rejected, core.rejected);
            assert_eq!(replaced_digest, core.replaced_digest);
        }
    }

    #[test]
    fn documents_cross_byte_for_byte() {
        let first = settings_sync_step(None, None, applied(), None);
        let replica = first.replica_document.expect("a bootstrap replica");
        assert_eq!(first.publish_document.as_deref(), Some(replica.as_str()));

        let second = settings_sync_step(
            Some(replica.clone()),
            Some(replica.clone()),
            applied(),
            None,
        );
        assert_eq!(second.replica_document.as_deref(), Some(replica.as_str()));
        assert_eq!(second.publish_document, None);

        let edited = record(Some("staging"), Some(30), Some(&rules_json()));
        let edit = settings_sync_record_edits(
            Some(replica.clone()),
            applied(),
            applied(),
            edited.clone(),
            NOW_MS,
        )
        .expect("core accepts the edit");
        let core = core_sync::record_edits(
            Some(&replica),
            &SyncedSettings::from(applied()),
            &SyncedSettings::from(applied()),
            &SyncedSettings::from(edited),
            NOW_MS,
        )
        .expect("core accepts the edit");
        assert_eq!(edit.replica_document, core.replica_document);
        assert_eq!(edit.settings, SyncedSettingsRecord::from(core.settings));

        let opaque = opaque_document();
        for text in [edit.replica_document, opaque] {
            let round = settings_sync_step(Some(text.clone()), Some(text.clone()), applied(), None);
            assert_eq!(round.replica_document.as_deref(), Some(text.as_str()));
            assert_eq!(round.publish_document, None);
        }
    }

    #[test]
    fn the_save_time_crosses_unchanged() {
        let edit = settings_sync_record_edits(
            None,
            applied(),
            applied(),
            record(Some("staging"), Some(30), Some(&rules_json())),
            NOW_MS,
        )
        .expect("core accepts the edit");

        let Classified::Valid(document) = classify(Some(&edit.replica_document)) else {
            panic!("the replica is a valid document: {}", edit.replica_document);
        };
        let now = u64::try_from(NOW_MS).expect("a positive time");
        assert_eq!(
            document
                .settings
                .get(ENVIRONMENT)
                .map(|register| register.at),
            Some(stamp(now, 0))
        );
        assert_eq!(edit.settings.environment.as_deref(), Some("staging"));
    }

    #[test]
    fn a_refused_edit_carries_cores_variant_and_message() {
        type Variant = fn(String) -> FfiError;
        let cases: [(SyncedSettingsRecord, Variant); 3] = [
            (record(Some("a::b"), Some(30), Some("[]")), |message| {
                FfiError::InvalidConfig { message }
            }),
            (record(Some("production"), Some(0), Some("[]")), |message| {
                FfiError::InvalidTuning { message }
            }),
            (
                record(Some("production"), Some(30), Some("not json")),
                |message| FfiError::InvalidRule { message },
            ),
        ];
        for (edited, variant) in cases {
            let refusal = core_sync::record_edits(
                None,
                &SyncedSettings::from(applied()),
                &SyncedSettings::from(applied()),
                &SyncedSettings::from(edited.clone()),
                NOW_MS,
            )
            .expect_err("core refuses the edit");
            let message = match refusal {
                SettingsSyncError::Refused(
                    ClientError::InvalidConfig(message)
                    | ClientError::InvalidTuning(message)
                    | ClientError::InvalidRule(message),
                ) => message,
                other => panic!("unexpected refusal {other:?}"),
            };

            let error = settings_sync_record_edits(None, applied(), applied(), edited, NOW_MS)
                .expect_err("the edit is refused");
            assert_eq!(error, variant(message));
        }
    }

    #[test]
    fn exhausted_stamps_cross_as_an_internal_error() {
        let mut document = Document::default();
        document.settings.insert(
            ENVIRONMENT.to_owned(),
            Register {
                at: stamp(MAX_MS, u32::MAX),
                value: Some("\"production\"".to_owned()),
            },
        );

        let error = settings_sync_record_edits(
            Some(document.encode()),
            applied(),
            applied(),
            record(Some("staging"), Some(30), Some(&rules_json())),
            NOW_MS,
        )
        .expect_err("no stamp is left");
        assert_eq!(
            error,
            FfiError::Internal {
                message: SettingsSyncError::StampSpaceExhausted.to_string(),
            }
        );
    }
}
