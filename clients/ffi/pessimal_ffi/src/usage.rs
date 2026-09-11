//! Usage reporting, as the FFI layer sees it.
//!
//! Three jobs, none of them a decision: mint the process identity, turn what a poll produced into
//! an allowlisted span, and batch the result so one poll is not one HTTP request.
//!
//! The policy all lives in `pessimal_usage`. What lives here is the part that needs a process, a
//! clock and a runtime.

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use chrono::{DateTime, Utc};
use pessimal_client_core::error::PollFailureKind;
use pessimal_client_core::view::FleetView;
use pessimal_usage::attribute::{BackendKind, DeviceClass, Outcome, Platform};
use pessimal_usage::consent::{Consent, DenialReason};
use pessimal_usage::resource::{Service, UsageResource};
use pessimal_usage::span::{TraceId, UsageSpan, UsageTrace};
use pessimal_usage_otlp::{Destination, UsageSink};
use uuid::Uuid;

use crate::session::recover;

/// Traces buffered before a flush is triggered.
///
/// Ten, not kumbaya's twenty spans, because their unit is a span from user activity and ours is a
/// trace from a poll timer: at a 30-second interval this is a request every five minutes, which is
/// the right order for a phone's radio. There is no debounce timer to go with it — unlike an app
/// driven by taps, a poller always has a next event coming, and a backgrounding app flushes
/// explicitly through [`UsageReporter::flush`].
const BATCH_TRACES: usize = 10;

/// Hard ceiling on buffered traces. Reached only when every flush has been failing.
///
/// Oldest are dropped, and the drops are counted. Diagnostics are not billing: a bounded buffer that
/// loses the oldest is the correct trade, and an unbounded one would grow for the life of a process
/// that cannot reach the network.
const MAX_BUFFERED_TRACES: usize = 64;

/// This process's `service.instance.id`.
///
/// Minted here and **never accepted from the caller**. `UsageResource::new` takes any `Uuid`, and
/// `HostId` is also a `Uuid`, so this is the one value on a span that the allowlist cannot constrain
/// structurally — a plausible future edit could pass a host's id and leak a fleet member's identity.
/// Taking it out of the caller's hands closes that.
///
/// `OnceLock` and not a fresh id per [`UsageReporter`]: "session-scoped" means the process, so a
/// `FleetSession` rebuilt when the user edits a setting keeps the same identity rather than
/// presenting as a new install every time the backend URL is corrected.
fn instance_id() -> Uuid {
    static INSTANCE: OnceLock<Uuid> = OnceLock::new();
    *INSTANCE.get_or_init(Uuid::now_v7)
}

/// The environment, as `pessimal_usage::Consent` wants it.
///
/// `vars_os` rather than `vars`: the latter panics on an entry that is not valid UTF-8, and a
/// monitoring app crashing at launch because something in the environment is Latin-1 would be an
/// embarrassing way to collect diagnostics. A name we cannot read is a name we were not looking for.
fn readable_env() -> BTreeMap<String, String> {
    std::env::vars_os()
        .filter_map(|(name, value)| Some((name.into_string().ok()?, value.into_string().ok()?)))
        .collect()
}

/// Which platform the client is running on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum UsagePlatformRecord {
    Ios,
    Macos,
    Linux,
    Windows,
}

impl From<UsagePlatformRecord> for Platform {
    fn from(record: UsagePlatformRecord) -> Self {
        match record {
            UsagePlatformRecord::Ios => Self::Ios,
            UsagePlatformRecord::Macos => Self::Macos,
            UsagePlatformRecord::Linux => Self::Linux,
            UsagePlatformRecord::Windows => Self::Windows,
        }
    }
}

/// Coarse device shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum UsageDeviceClassRecord {
    Phone,
    Tablet,
    Mac,
    Server,
}

impl From<UsageDeviceClassRecord> for DeviceClass {
    fn from(record: UsageDeviceClassRecord) -> Self {
        match record {
            UsageDeviceClassRecord::Phone => Self::Phone,
            UsageDeviceClassRecord::Tablet => Self::Tablet,
            UsageDeviceClassRecord::Mac => Self::Mac,
            UsageDeviceClassRecord::Server => Self::Server,
        }
    }
}

/// Why reporting is off.
///
/// Crosses the boundary so a settings screen can say *which* switch is in effect. A user who turned
/// reporting off, turned it back on, and still sees it off because `CI=true` is set has no other way
/// to find out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum UsageDenialReasonRecord {
    /// The user's own switch, or `PESSIMAL_USAGE_REPORTING=off`.
    OptedOut,
    /// `DO_NOT_TRACK` is set.
    DoNotTrack,
    /// Running under CI.
    ContinuousIntegration,
    /// This build carries no destination — every build except an official release.
    NoDestination,
}

impl From<DenialReason> for UsageDenialReasonRecord {
    fn from(reason: DenialReason) -> Self {
        match reason {
            DenialReason::OptedOut => Self::OptedOut,
            DenialReason::DoNotTrack => Self::DoNotTrack,
            DenialReason::ContinuousIntegration => Self::ContinuousIntegration,
            DenialReason::NoDestination => Self::NoDestination,
        }
    }
}

/// What the app passes in so a reporter can be built — or, just as validly, not built.
///
/// `endpoint` and `ingestion_key` come from the bundle's plist, which a `genrule` fills from
/// `--action_env` at release time and leaves empty otherwise. Empty is not an error: it is a build
/// that cannot report, which is what a simulator build should be.
///
/// Note what is *not* here: any identifier. The instance id is minted in Rust — see [`instance_id`].
#[derive(Debug, Clone, uniffi::Record)]
pub struct UsageReportingRecord {
    /// OTLP/HTTP base URL, no signal path. Empty when this build was given none.
    pub endpoint: String,
    /// SigNoz ingestion key. Empty when this build was given none.
    pub ingestion_key: String,
    /// The user's switch. `true` means do not report.
    pub opted_out: bool,
    pub platform: UsagePlatformRecord,
    pub device_class: UsageDeviceClassRecord,
    /// `CFBundleShortVersionString`. Dropped unless it parses as a dotted numeric version.
    pub app_version: String,
    /// `CFBundleVersion`, where it is an integer.
    pub app_build: Option<u32>,
    /// The OS version. Trimmed to major and minor, and dropped unless numeric.
    pub os_version: String,
}

/// Counters a settings screen can show, plus why reporting is off if it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct UsageDiagnosticsRecord {
    pub enabled: bool,
    pub denial: Option<UsageDenialReasonRecord>,
    /// Reports the backend accepted.
    pub sent: u64,
    /// Reports the backend refused — a bad credential, a rejected shape.
    pub rejected: u64,
    /// Reports that got no answer at all.
    pub failed: u64,
    /// Spans in accepted reports.
    pub spans_reported: u64,
    /// Traces waiting for the next flush.
    pub buffered: u64,
    /// Traces discarded because the buffer was full. Non-zero means flushes have been failing.
    pub dropped: u64,
}

impl UsageDiagnosticsRecord {
    /// What a session with no reporter answers. Carries the reason so the UI is not left guessing.
    #[must_use]
    pub fn disabled(denial: Option<UsageDenialReasonRecord>) -> Self {
        Self {
            enabled: false,
            denial,
            sent: 0,
            rejected: 0,
            failed: 0,
            spans_reported: 0,
            buffered: 0,
            dropped: 0,
        }
    }
}

/// Buffers traces and ships them in batches.
///
/// Exists only when consent allowed it to — see [`UsageReporter::build`]. That is the structural
/// half of opt-out: there is no disabled reporter to forget to check.
#[derive(Debug)]
pub struct UsageReporter {
    sink: UsageSink,
    buffer: Mutex<VecDeque<UsageTrace>>,
    dropped: AtomicU64,
}

impl UsageReporter {
    /// A reporter, if consent and a destination both allow one.
    ///
    /// Returns `(None, Some(reason))` when they do not, so the caller can report *why* without
    /// re-deriving it. Never returns an error: a reporter that cannot be built is not a session that
    /// cannot start.
    #[must_use]
    pub fn build(record: &UsageReportingRecord) -> (Option<Self>, Option<UsageDenialReasonRecord>) {
        let destination = Destination::from_bundle(&record.endpoint, &record.ingestion_key);
        let consent = Consent::resolve(record.opted_out, destination.is_some(), &readable_env());

        match (consent, destination) {
            (Consent::Granted, Some(destination)) => {
                let resource = UsageResource::new(
                    Service::Client,
                    record.platform.into(),
                    record.device_class.into(),
                    instance_id(),
                    &record.app_version,
                    record.app_build,
                    &record.os_version,
                );
                match UsageSink::new(destination, resource) {
                    Ok(sink) => (
                        Some(Self {
                            sink,
                            buffer: Mutex::new(VecDeque::new()),
                            dropped: AtomicU64::new(0),
                        }),
                        None,
                    ),
                    // A platform with no usable TLS stack. Not a reason to refuse to launch.
                    Err(_) => (None, Some(UsageDenialReasonRecord::NoDestination)),
                }
            }
            (Consent::Denied(reason), _) => (None, Some(reason.into())),
            // `Consent::resolve` already denies without a destination; this arm cannot be reached
            // through it, and is not the place to assume that.
            (Consent::Granted, None) => (None, Some(UsageDenialReasonRecord::NoDestination)),
        }
    }

    /// Buffers one trace, and says whether that filled a batch.
    ///
    /// The caller decides what to do about a full batch, because only the caller is in an async
    /// context. Returns `false` when there is nothing to do yet, which is the common case.
    pub fn record(&self, trace: UsageTrace) -> bool {
        if trace.is_empty() {
            return false;
        }
        let mut buffer = recover(self.buffer.lock());
        buffer.push_back(trace);
        while buffer.len() > MAX_BUFFERED_TRACES {
            buffer.pop_front();
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
        buffer.len() >= BATCH_TRACES
    }

    /// Sends everything buffered. Never fails, and never tells the caller anything: the outcome is a
    /// diagnostics fact, not control flow.
    ///
    /// The buffer is drained *before* the request, so a slow send does not block the next poll from
    /// buffering and a failed one drops rather than retrying. That is the deliberate no-retry,
    /// no-disk-buffer position: this is diagnostics, not billing.
    pub async fn flush(&self) {
        let batch: Vec<UsageTrace> = {
            let mut buffer = recover(self.buffer.lock());
            buffer.drain(..).collect()
        };
        if batch.is_empty() {
            return;
        }
        self.sink.send(&batch).await;
    }

    #[must_use]
    pub fn diagnostics(&self) -> UsageDiagnosticsRecord {
        let snapshot = self.sink.diagnostics();
        let buffered = recover(self.buffer.lock()).len();
        UsageDiagnosticsRecord {
            enabled: true,
            denial: None,
            sent: snapshot.sent,
            rejected: snapshot.rejected,
            failed: snapshot.failed,
            spans_reported: snapshot.spans_reported,
            buffered: buffered as u64,
            dropped: self.dropped.load(Ordering::Relaxed),
        }
    }
}

/// Builds the trace for one completed poll.
///
/// Reads only counts and kinds off the view. Notably it does **not** read `PollFailure::message`,
/// which is where the operator's backend would tell us about their collector, their hosts and their
/// queries — the allowlist has no variant that could carry it, so this is enforced rather than
/// merely intended.
#[must_use]
pub fn poll_trace(
    view: &FleetView,
    rules: u32,
    backend: BackendKind,
    started: DateTime<Utc>,
    ended: DateTime<Utc>,
) -> UsageTrace {
    let mut trace = UsageTrace::new(TraceId::new());
    trace.record(
        UsageSpan::ClientPoll {
            outcome: poll_outcome(view),
            backend,
            hosts: u32::try_from(view.hosts.len()).unwrap_or(u32::MAX),
            rules,
            alerts_firing: u32::try_from(view.firing().len()).unwrap_or(u32::MAX),
        },
        None,
        started,
        ended,
    );
    trace
}

/// How this poll ended, from the view it produced.
///
/// `consecutive_failures == 0` is the clean case and needs no failure to inspect. Otherwise the
/// *last* failure is necessarily this poll's, since a clean poll would have reset the counter.
///
/// The `Partial` case uses core's own distinction rather than inventing one: `last_roster_at` is
/// "last poll with a listed roster, whatever the series did". If that is this poll's instant, the
/// roster landed and only some series failed — which is a materially different day for an operator
/// than a poll that reached nothing at all.
#[must_use]
pub fn poll_outcome(view: &FleetView) -> Outcome {
    let freshness = &view.freshness;
    if freshness.consecutive_failures == 0 {
        return Outcome::Ok;
    }
    if freshness.as_of.is_some() && freshness.last_roster_at == freshness.as_of {
        return Outcome::Partial;
    }
    match freshness.last_failure.as_ref().map(|failure| failure.kind) {
        Some(PollFailureKind::Unauthorized) => Outcome::Unauthorized,
        Some(PollFailureKind::Unreachable) => Outcome::Unreachable,
        Some(PollFailureKind::Malformed) => Outcome::DecodeError,
        // `None` joins `Backend` rather than getting an arm of its own: failures counted with none
        // recorded is not a state core produces, and reporting it as a backend error beats inventing
        // a variant for an impossible case.
        Some(PollFailureKind::Backend) | None => Outcome::BackendError,
    }
}

/// Maps a backend's display name to an allowlisted kind.
///
/// Takes a string and returns an enum, which is the safe direction: an adapter named anything at all
/// — including a test fake — becomes one of four known values, so no name can travel as an
/// attribute. Unknown means `Otlp`, since that is what "some OTLP endpoint we have no preset for"
/// is.
#[must_use]
pub fn backend_kind_from_name(name: &str) -> BackendKind {
    match name.to_ascii_lowercase().as_str() {
        "signoz" => BackendKind::Signoz,
        "clickstack" | "hyperdx" => BackendKind::Clickstack,
        "honeycomb" => BackendKind::Honeycomb,
        _ => BackendKind::Otlp,
    }
}

#[cfg(test)]
mod tests {
    use chrono::Duration;
    use pessimal_client_core::testing::ScriptedQuery;
    use pessimal_client_core::{FleetConfig, FleetState, PollTuning, poll_once};
    use pessimal_core::{CoreError, Host, HostId, MetricKind, OsFamily};
    use pessimal_usage::attribute::AttributeKey;

    use super::*;

    fn config() -> FleetConfig {
        FleetConfig::new("test", PollTuning::default()).expect("a valid environment")
    }

    fn reporting(endpoint: &str, key: &str, opted_out: bool) -> UsageReportingRecord {
        UsageReportingRecord {
            endpoint: endpoint.to_owned(),
            ingestion_key: key.to_owned(),
            opted_out,
            platform: UsagePlatformRecord::Ios,
            device_class: UsageDeviceClassRecord::Phone,
            app_version: "0.1.0".to_owned(),
            app_build: Some(26),
            os_version: "18.4".to_owned(),
        }
    }

    /// A view the core really produced, rather than one assembled by hand: the outcome mapping reads
    /// `freshness`, and a hand-built `FreshnessInputs` would let a wrong reading pass.
    async fn view_from(query: ScriptedQuery) -> FleetView {
        let config = config();
        let state = FleetState::new("SigNoz".to_owned());
        let now = Utc::now();
        let update = poll_once(&query, &config, &state, now)
            .await
            .expect("the default tuning plans");
        update.view
    }

    fn host(id: &str) -> Host {
        Host::new(HostId::new(id), OsFamily::Linux)
    }

    #[tokio::test]
    async fn a_clean_poll_reports_ok() {
        let view =
            view_from(ScriptedQuery::new("SigNoz").with_hosts(Ok(vec![host("web-1")]))).await;
        assert_eq!(poll_outcome(&view), Outcome::Ok);
    }

    #[tokio::test]
    async fn a_failure_kind_maps_to_an_outcome_without_its_message() {
        for (error, expected) in [
            (CoreError::Unauthorized, Outcome::Unauthorized),
            (
                CoreError::Unreachable("dns failure for signoz.acme.corp".to_owned()),
                Outcome::Unreachable,
            ),
            (
                CoreError::Backend("500: host web-1.internal not found".to_owned()),
                Outcome::BackendError,
            ),
            (
                CoreError::UnknownMetric("system.cpu.utilisation".to_owned()),
                Outcome::DecodeError,
            ),
        ] {
            let view = view_from(ScriptedQuery::new("SigNoz").with_hosts(Err(error))).await;
            assert_eq!(poll_outcome(&view), expected);
        }
    }

    #[tokio::test]
    async fn a_listed_roster_with_a_failing_series_is_partial() {
        // The distinction an operator actually cares about: the backend is up and answering, one
        // metric is broken. Core already draws it as `last_roster_at`, so we do not invent it.
        let view = view_from(
            ScriptedQuery::new("SigNoz")
                .with_hosts(Ok(vec![host("web-1")]))
                .with_series(
                    MetricKind::CpuUtilization,
                    Err(CoreError::Backend("no such metric".to_owned())),
                ),
        )
        .await;
        assert_eq!(poll_outcome(&view), Outcome::Partial);
    }

    #[tokio::test]
    async fn a_span_from_a_real_poll_carries_only_allowlisted_keys() {
        // The end-to-end guard: whatever a poll produced, what leaves must be on the list.
        let view = view_from(
            ScriptedQuery::new("SigNoz").with_hosts(Err(CoreError::Backend(
                "500 from https://signoz.acme.corp: host db-7.internal unknown".to_owned(),
            ))),
        )
        .await;
        let now = Utc::now();
        let trace = poll_trace(
            &view,
            3,
            BackendKind::Signoz,
            now,
            now + Duration::seconds(1),
        );

        let permitted: Vec<&str> = AttributeKey::ALL.iter().map(|k| k.otel_name()).collect();
        let mut seen = 0;
        for span in &trace.spans {
            for attribute in span.payload.attributes() {
                assert!(
                    permitted.contains(&attribute.key.otel_name()),
                    "{} is not allowlisted",
                    attribute.key.otel_name()
                );
                seen += 1;
            }
        }
        assert!(seen > 0, "the span carried no attributes at all");

        // And specifically: nothing resembling the message above survived anywhere in the payload.
        let resource = UsageResource::new(
            Service::Client,
            Platform::Ios,
            DeviceClass::Phone,
            instance_id(),
            "0.1.0",
            Some(26),
            "18.4",
        );
        let body = pessimal_usage::export_trace_request(&resource, &[trace]).to_string();
        for leaked in ["signoz.acme.corp", "db-7.internal", "500"] {
            assert!(
                !body.contains(leaked),
                "{leaked} reached the payload: {body}"
            );
        }
    }

    #[test]
    fn the_instance_id_is_stable_within_a_process() {
        // "Session-scoped" means the process. A `FleetSession` rebuilt when the user fixes a typo in
        // the backend URL must not present as a new install.
        assert_eq!(instance_id(), instance_id());
    }

    #[test]
    fn an_empty_destination_denies_with_no_destination() {
        let (reporter, denial) = UsageReporter::build(&reporting("", "", false));
        assert!(reporter.is_none());
        assert_eq!(denial, Some(UsageDenialReasonRecord::NoDestination));
    }

    #[test]
    fn opting_out_denies_even_with_a_destination() {
        let (reporter, denial) =
            UsageReporter::build(&reporting("https://ingest.eu2.signoz.cloud", "k", true));
        assert!(
            reporter.is_none(),
            "an opted-out build must hold no reporter"
        );
        assert_eq!(denial, Some(UsageDenialReasonRecord::OptedOut));
    }

    #[test]
    fn a_denied_session_still_reports_why() {
        let record = UsageDiagnosticsRecord::disabled(Some(UsageDenialReasonRecord::DoNotTrack));
        assert!(!record.enabled);
        assert_eq!(record.denial, Some(UsageDenialReasonRecord::DoNotTrack));
        assert_eq!(record.sent, 0);
    }

    #[test]
    fn recording_batches_and_the_buffer_is_bounded() {
        let (reporter, denial) =
            UsageReporter::build(&reporting("https://ingest.eu2.signoz.cloud", "k", false));
        // The environment this test runs in decides: CI sets `CI=true`, which is a denial by design.
        let Some(reporter) = reporter else {
            assert_eq!(
                denial,
                Some(UsageDenialReasonRecord::ContinuousIntegration),
                "the only expected denial here is CI"
            );
            return;
        };

        let now = Utc::now();
        let make = || {
            let mut trace = UsageTrace::new(TraceId::new());
            trace.record(
                UsageSpan::ClientPlan {
                    requests_planned: 1,
                },
                None,
                now,
                now,
            );
            trace
        };

        // Nothing to batch until the threshold.
        for _ in 1..BATCH_TRACES {
            assert!(!reporter.record(make()), "batched too early");
        }
        assert!(reporter.record(make()), "the batch should be full now");
        assert_eq!(reporter.diagnostics().buffered, BATCH_TRACES as u64);

        // Past the ceiling, the oldest go and the drops are counted rather than hidden.
        for _ in 0..(MAX_BUFFERED_TRACES * 2) {
            reporter.record(make());
        }
        let diagnostics = reporter.diagnostics();
        assert_eq!(diagnostics.buffered, MAX_BUFFERED_TRACES as u64);
        assert!(diagnostics.dropped > 0, "drops must be visible");
    }

    #[test]
    fn an_empty_trace_is_not_buffered() {
        let (reporter, _) =
            UsageReporter::build(&reporting("https://ingest.eu2.signoz.cloud", "k", false));
        if let Some(reporter) = reporter {
            assert!(!reporter.record(UsageTrace::new(TraceId::new())));
            assert_eq!(reporter.diagnostics().buffered, 0);
        }
    }

    #[test]
    fn a_backend_name_becomes_an_enum_and_never_travels_as_text() {
        assert_eq!(backend_kind_from_name("SigNoz"), BackendKind::Signoz);
        assert_eq!(backend_kind_from_name("hyperdx"), BackendKind::Clickstack);
        assert_eq!(backend_kind_from_name("Honeycomb"), BackendKind::Honeycomb);
        // A test double, a future adapter, anything at all: one of four known values.
        assert_eq!(backend_kind_from_name("Gated"), BackendKind::Otlp);
        assert_eq!(
            backend_kind_from_name("signoz.acme.corp"),
            BackendKind::Otlp
        );
    }
}
