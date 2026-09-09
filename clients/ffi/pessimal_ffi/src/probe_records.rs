//! The "test connection" answer, in the shapes UniFFI can carry.
//!
//! [`pessimal_client_core::probe_backend`] has already decided everything a settings screen needs
//! to say: whether the endpoint answered, whether the credentials worked, and whether a real
//! roster query over the real roster window found anything. This module restates that answer and
//! adds nothing to it — no re-judging of what "usable" means, no second opinion on the wording.
//!
//! Four shape changes, each with a reason that is not merely "UniFFI made me":
//!
//! - [`pessimal_core::TimeRange`] becomes two `i64` millisecond fields rather than a record of its
//!   own. The probe carries exactly one range, and a shared `TimeRange` record would be a name in
//!   the flat Swift namespace that this module has no business claiming.
//! - [`BackendProbe::is_usable`] and [`BackendProbe::message`] become the `usable` and `message`
//!   fields of [`BackendProbeRecord`]. A record crosses the boundary as plain data with no
//!   methods, so recomputing either one in Swift would move a product decision ("connected but
//!   empty is not usable") and an operator-facing sentence out of the core that owns them.
//! - [`pessimal_core::HostId`] becomes `String`, via [`crate::convert::host_id_to_string`], in the
//!   order the core produced — which it already sorted.
//! - [`FailureSource::Series`] carries the metric's OTel instrument name as a `String` rather than
//!   a mirrored [`pessimal_core::MetricKind`]. See [`ProbeFailedRequest`].
//!
//! The names here are deliberately `Probe`-prefixed. [`PollFailure`] reaches Swift from the fleet
//! view as well as from a probe, and those surfaces are mirrored in sibling modules; record and
//! enum names share one flat Swift namespace regardless of Rust module, so a plain
//! `PollFailureRecord` here would be a bindgen-time redeclaration rather than a shared type.

use pessimal_client_core::{BackendProbe, FailureSource, PollFailure, PollFailureKind};
use pessimal_core::MetricKind;

use crate::convert::{FfiError, datetime_to_millis, host_id_to_string};

/// Why a probe failed, coarse enough for a settings screen to route on.
///
/// A one-for-one mirror of [`PollFailureKind`], kept as four cases rather than collapsed into a
/// message because the case *is* the routing decision: `Unauthorized` is the one that offers an
/// "Open Settings" button, `Unreachable` is the one that means "check the URL or the network", and
/// `Malformed` is the one that means the backend answered something we could not read — three
/// different repairs behind one sentence if Swift cannot tell them apart.
///
/// Both `From` impls are exhaustive with no `_` arm, so a fifth [`PollFailureKind`] stops this file
/// compiling instead of reaching the app as whichever neighbour sat under the wildcard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum ProbeFailureKind {
    /// Credentials are missing, wrong, or expired.
    Unauthorized,
    /// The backend was never reached: DNS, TLS, a dead network.
    Unreachable,
    /// The backend answered, unhappily. A 500, a rejected query.
    Backend,
    /// The backend answered something that could not be turned into domain values.
    Malformed,
}

/// Case for case, with the core's own spelling kept.
///
/// The cases are *not* renamed to read better in Swift. Sibling modules mirror the same domain
/// enum for the fleet view, and a case this module spelled differently would leave the app holding
/// two vocabularies for one concept — a worse outcome than any wording gained.
impl From<PollFailureKind> for ProbeFailureKind {
    fn from(kind: PollFailureKind) -> Self {
        match kind {
            PollFailureKind::Unauthorized => Self::Unauthorized,
            PollFailureKind::Unreachable => Self::Unreachable,
            PollFailureKind::Backend => Self::Backend,
            PollFailureKind::Malformed => Self::Malformed,
        }
    }
}

/// The inverse, so the mirror is checked by a round trip rather than by reading two lists.
///
/// Infallible and total in both directions: every case here came from a [`PollFailureKind`] and
/// has exactly one counterpart. It exists for the test, and for any future caller that has to hand
/// a Swift-chosen kind back to the core.
impl From<ProbeFailureKind> for PollFailureKind {
    fn from(kind: ProbeFailureKind) -> Self {
        match kind {
            ProbeFailureKind::Unauthorized => Self::Unauthorized,
            ProbeFailureKind::Unreachable => Self::Unreachable,
            ProbeFailureKind::Backend => Self::Backend,
            ProbeFailureKind::Malformed => Self::Malformed,
        }
    }
}

/// Which request failed, mirroring [`FailureSource`].
///
/// A probe only ever reports [`Self::Roster`]: both failure paths in
/// [`pessimal_client_core::probe_backend`] stamp `FailureSource::Roster`, because the only requests
/// a probe makes are `check_connection` and `list_hosts`. The `Series` case is here so the type is
/// a faithful mirror of the source enum — a `From` impl that silently dropped a variant is exactly
/// the rot this crate's destructuring rule exists to prevent — not because the probe can produce
/// one.
///
/// The metric travels as its OTel instrument name rather than as a mirrored
/// [`MetricKind`]: twelve duplicated variants in a module whose real code path never emits any of
/// them would be twelve places to forget when the domain grows a metric, and `MetricKind` belongs
/// to the modules that actually expose metrics. The name is the domain's own
/// [`MetricKind::otel_name`], and the way back is the domain's own [`MetricKind::from_otel_name`],
/// so there is no second copy of that table here.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum ProbeFailedRequest {
    /// `list_hosts`. The only request whose failure a probe can report.
    Roster,
    /// One metric query, named by its OTel instrument name.
    Series {
        /// For example `system.cpu.utilization`.
        metric_otel_name: String,
    },
}

impl From<FailureSource> for ProbeFailedRequest {
    fn from(source: FailureSource) -> Self {
        match source {
            FailureSource::Roster => Self::Roster,
            FailureSource::Series { metric } => Self::Series {
                metric_otel_name: metric.otel_name().to_owned(),
            },
        }
    }
}

/// The inverse, fallible only because a `String` can name a metric the domain does not model.
impl TryFrom<ProbeFailedRequest> for FailureSource {
    type Error = FfiError;

    /// # Errors
    /// [`FfiError::UnknownMetric`], carrying the domain's own message, when `metric_otel_name` is
    /// not one of [`MetricKind::ALL`]. The check is [`MetricKind::from_otel_name`]'s, not a second
    /// copy of the name table written here.
    fn try_from(request: ProbeFailedRequest) -> Result<Self, Self::Error> {
        match request {
            ProbeFailedRequest::Roster => Ok(Self::Roster),
            ProbeFailedRequest::Series { metric_otel_name } => Ok(Self::Series {
                metric: MetricKind::from_otel_name(&metric_otel_name)?,
            }),
        }
    }
}

/// One failed request inside a probe: [`PollFailure`] as Swift sees it.
///
/// `message` crosses verbatim. It is already the `CoreError` `Display` string the core chose and
/// carries its own prefix ("not authorised for the telemetry backend"), so re-prefixing it on
/// either side of the boundary would stutter.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ProbeFailureRecord {
    pub kind: ProbeFailureKind,
    /// Mirrors `PollFailure::source`, renamed because the type it carries is, and always
    /// [`ProbeFailedRequest::Roster`] for a probe; see that type for both reasons.
    pub request: ProbeFailedRequest,
    pub message: String,
    /// Epoch milliseconds. The instant the probe was taken, not a second clock read.
    pub at_millis: i64,
    /// [`PollFailureKind::is_actionable`]: retrying cannot help, the user must change something.
    ///
    /// Precomputed rather than left to a Swift `switch` on `kind`. "Which failures does an Open
    /// Settings button belong on" is a product rule, and the core is where it is written; a
    /// `kind == .unauthorized` in the view layer is a second copy of that rule, free to disagree
    /// the day a fifth kind arrives.
    pub actionable: bool,
}

/// Destructured with no `..`, so a new [`PollFailure`] field is a compile error here rather than a
/// value that quietly stops reaching Swift.
impl From<PollFailure> for ProbeFailureRecord {
    fn from(failure: PollFailure) -> Self {
        let PollFailure {
            kind,
            source,
            message,
            at,
        } = failure;
        Self {
            kind: kind.into(),
            request: source.into(),
            message,
            at_millis: datetime_to_millis(at),
            actionable: kind.is_actionable(),
        }
    }
}

/// What a "test connection" actually learned: [`BackendProbe`] as Swift sees it.
///
/// `connected` is kept beside `failure` rather than derived from it, for the reason the core
/// states: `connected: true` with `failure: Some(..)` is a reachable and genuinely distinct
/// outcome — the URL and key are fine and the roster query itself errored — and it points at a
/// different repair from "cannot reach" or "bad key".
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct BackendProbeRecord {
    /// The adapter's own name for the backend, e.g. `SigNoz`.
    pub backend_name: String,
    /// `check_connection`'s verdict alone: something answered and the credentials worked.
    pub connected: bool,
    pub failure: Option<ProbeFailureRecord>,
    pub hosts_seen: u32,
    /// Up to five host ids, in the order the core sorted them.
    ///
    /// Sorted by the core rather than here, and not re-sorted here: an operator re-running the
    /// test must not watch five names shuffle, and a second sort in the bridge would be a second
    /// place for that order to be decided.
    pub sample_hosts: Vec<String>,
    /// Epoch milliseconds. The start of the `list_hosts` window the probe actually asked over —
    /// the same window a poll's roster uses, so "0 hosts here" means what "an empty roster" means.
    pub window_start_millis: i64,
    /// Epoch milliseconds, equal to the `now` the caller supplied.
    pub window_end_millis: i64,
    /// [`BackendProbe::is_usable`]: connected *and* at least one host seen.
    ///
    /// Both halves, because `check_connection` returning `Ok(())` proves only that something
    /// answered — a backend holding no Pessimal data at all passes it. Precomputed for the same
    /// reason as `actionable`: the definition of usable is the core's, and a Swift-side
    /// `connected && hostsSeen > 0` is a copy of it that can drift.
    pub usable: bool,
    /// [`BackendProbe::message`]: the one line a settings screen shows.
    ///
    /// Generated by the core, including the window phrase ("in the last 210s" rather than a
    /// rounded "3 minutes", in the one message whose whole purpose is to be believed). Formatting
    /// it here or in Swift would be a second voice saying almost the same thing.
    pub message: String,
}

/// Destructured with no `..`; see [`ProbeFailureRecord`]'s impl.
///
/// `is_usable()` and `message()` are read *before* the destructure, because both borrow the whole
/// probe and the destructure moves it.
impl From<BackendProbe> for BackendProbeRecord {
    fn from(probe: BackendProbe) -> Self {
        let usable = probe.is_usable();
        let message = probe.message();
        let BackendProbe {
            backend_name,
            connected,
            failure,
            hosts_seen,
            sample_hosts,
            window,
        } = probe;
        Self {
            backend_name,
            connected,
            failure: failure.map(ProbeFailureRecord::from),
            // `hosts_seen` is already a `u32` in the domain — the core saturated the `usize` at
            // its own boundary, so there is nothing to convert and nothing to decide here.
            hosts_seen,
            sample_hosts: sample_hosts.iter().map(host_id_to_string).collect(),
            window_start_millis: datetime_to_millis(window.start()),
            window_end_millis: datetime_to_millis(window.end()),
            usable,
            message,
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, Duration, Utc};
    use pessimal_client_core::{BackendProbe, FailureSource, PollFailure, PollFailureKind};
    use pessimal_core::{CoreError, HostId, MetricKind, TimeRange};

    use super::{BackendProbeRecord, ProbeFailedRequest, ProbeFailureKind, ProbeFailureRecord};
    use crate::convert::{FfiError, datetime_to_millis};

    /// Every kind paired with the case it must reach Swift as. A round trip alone cannot catch two
    /// arms swapped in both directions, which is precisely the mistake that renames the button.
    const KIND_PAIRS: [(PollFailureKind, ProbeFailureKind); 4] = [
        (
            PollFailureKind::Unauthorized,
            ProbeFailureKind::Unauthorized,
        ),
        (PollFailureKind::Unreachable, ProbeFailureKind::Unreachable),
        (PollFailureKind::Backend, ProbeFailureKind::Backend),
        (PollFailureKind::Malformed, ProbeFailureKind::Malformed),
    ];

    const WINDOW_SECONDS: i64 = 210;

    fn at() -> DateTime<Utc> {
        DateTime::from_timestamp(1_757_000_000, 0).expect("a valid fixture instant")
    }

    fn window() -> TimeRange {
        TimeRange::ending_at(at(), Duration::seconds(WINDOW_SECONDS)).expect("a positive window")
    }

    /// A roster failure, the only source a probe can produce.
    fn roster_failure(error: &CoreError) -> PollFailure {
        PollFailure::from_core(error, FailureSource::Roster, at())
    }

    /// Connected, and the backend answered with nothing. The outcome the two-step probe exists for.
    fn connected_but_empty() -> BackendProbe {
        BackendProbe {
            backend_name: "SigNoz".to_owned(),
            connected: true,
            failure: None,
            hosts_seen: 0,
            sample_hosts: Vec::new(),
            window: window(),
        }
    }

    /// `check_connection` itself refused: not connected, and a failure to explain why.
    fn rejected() -> BackendProbe {
        BackendProbe {
            backend_name: "SigNoz".to_owned(),
            connected: false,
            failure: Some(roster_failure(&CoreError::Unauthorized)),
            hosts_seen: 0,
            sample_hosts: Vec::new(),
            window: window(),
        }
    }

    /// Two hosts, in the order the core sorted them.
    fn populated() -> BackendProbe {
        BackendProbe {
            backend_name: "SigNoz".to_owned(),
            connected: true,
            failure: None,
            hosts_seen: 2,
            sample_hosts: vec![HostId::new("web-1"), HostId::new("web-2")],
            window: window(),
        }
    }

    /// Connected, and the roster query behind it errored. The state `connected` exists to expose.
    fn connected_then_broken() -> BackendProbe {
        BackendProbe {
            backend_name: "SigNoz".to_owned(),
            connected: true,
            failure: Some(roster_failure(&CoreError::Backend("500".to_owned()))),
            hosts_seen: 0,
            sample_hosts: Vec::new(),
            window: window(),
        }
    }

    #[test]
    fn every_failure_kind_round_trips_through_its_record() {
        for (domain, record) in KIND_PAIRS {
            assert_eq!(
                ProbeFailureKind::from(domain),
                record,
                "{domain:?} must reach Swift as {record:?}"
            );
            assert_eq!(
                PollFailureKind::from(record),
                domain,
                "{record:?} must come back as {domain:?}"
            );
        }

        let mirrored: Vec<ProbeFailureKind> = KIND_PAIRS
            .iter()
            .map(|&(domain, _)| ProbeFailureKind::from(domain))
            .collect();
        for (index, kind) in mirrored.iter().enumerate() {
            assert!(
                !mirrored[index + 1..].contains(kind),
                "{kind:?} is the image of two domain kinds; the mirror collapses a case"
            );
        }
    }

    #[test]
    fn every_failed_request_round_trips_through_its_record() {
        assert_eq!(
            FailureSource::try_from(ProbeFailedRequest::from(FailureSource::Roster))
                .expect("Roster needs no parsing"),
            FailureSource::Roster
        );

        for metric in MetricKind::ALL {
            let source = FailureSource::Series { metric };
            let record = ProbeFailedRequest::from(source);
            assert_eq!(
                record,
                ProbeFailedRequest::Series {
                    metric_otel_name: metric.otel_name().to_owned()
                },
                "the metric crosses as its OTel instrument name"
            );
            assert_eq!(
                FailureSource::try_from(record).expect("a modelled metric resolves"),
                source,
                "{metric} did not survive the round trip"
            );
        }
    }

    #[test]
    fn an_unmodelled_metric_name_fails_the_reverse_with_the_domains_message() {
        let error = FailureSource::try_from(ProbeFailedRequest::Series {
            metric_otel_name: "system.paging.faults".to_owned(),
        })
        .expect_err("a name the domain does not model cannot resolve");

        assert_eq!(
            error,
            FfiError::UnknownMetric {
                message: "system.paging.faults".to_owned()
            },
            "the message is the domain's own, not a sentence written in the bridge"
        );
    }

    #[test]
    fn a_failure_record_carries_the_cores_verdicts_rather_than_recomputing_them() {
        for (domain_kind, record_kind) in KIND_PAIRS {
            let failure = PollFailure {
                kind: domain_kind,
                source: FailureSource::Roster,
                message: "something went wrong".to_owned(),
                at: at(),
            };
            let record = ProbeFailureRecord::from(failure.clone());

            assert_eq!(record.kind, record_kind);
            assert_eq!(record.request, ProbeFailedRequest::Roster);
            assert_eq!(
                record.message, failure.message,
                "the message crosses verbatim"
            );
            assert_eq!(record.at_millis, datetime_to_millis(at()));
            assert_eq!(
                record.actionable,
                domain_kind.is_actionable(),
                "actionable is the core's rule, not a Swift-side kind comparison"
            );
        }
    }

    #[test]
    fn a_series_failure_still_mirrors_even_though_a_probe_cannot_produce_one() {
        let record = ProbeFailureRecord::from(PollFailure::from_core(
            &CoreError::Backend("query rejected".to_owned()),
            FailureSource::Series {
                metric: MetricKind::MemoryUtilization,
            },
            at(),
        ));

        assert_eq!(
            record.request,
            ProbeFailedRequest::Series {
                metric_otel_name: "system.memory.utilization".to_owned()
            },
            "the Series arm exists so no source is ever silently dropped"
        );
        assert_eq!(record.kind, ProbeFailureKind::Backend);
        assert!(!record.actionable);
    }

    #[test]
    fn the_four_probe_outcomes_translate_field_for_field() {
        for probe in [
            connected_but_empty(),
            rejected(),
            populated(),
            connected_then_broken(),
        ] {
            let record = BackendProbeRecord::from(probe.clone());

            assert_eq!(record.backend_name, probe.backend_name);
            assert_eq!(record.connected, probe.connected);
            assert_eq!(record.hosts_seen, probe.hosts_seen);
            assert_eq!(
                record
                    .failure
                    .as_ref()
                    .map(|failure| failure.message.clone()),
                probe
                    .failure
                    .as_ref()
                    .map(|failure| failure.message.clone()),
                "a failure neither appears nor vanishes in translation"
            );
            assert_eq!(
                record.usable,
                probe.is_usable(),
                "usable is the core's definition"
            );
            assert_eq!(
                record.message,
                probe.message(),
                "the sentence is the core's, generated once"
            );
            assert_eq!(
                record.window_start_millis,
                datetime_to_millis(at()) - 210_000
            );
            assert_eq!(record.window_end_millis, datetime_to_millis(at()));
        }
    }

    #[test]
    fn the_four_probe_outcomes_keep_four_distinct_sentences() {
        let messages: Vec<String> = [
            connected_but_empty(),
            rejected(),
            populated(),
            connected_then_broken(),
        ]
        .into_iter()
        .map(|probe| BackendProbeRecord::from(probe).message)
        .collect();

        for (index, message) in messages.iter().enumerate() {
            assert!(
                !message.is_empty(),
                "a settings screen is never handed an empty line"
            );
            assert!(
                !messages[index + 1..].contains(message),
                "two different outcomes share one sentence: {message}"
            );
        }
    }

    #[test]
    fn the_host_sample_crosses_as_strings_in_the_cores_order() {
        let record = BackendProbeRecord::from(populated());

        assert_eq!(
            record.sample_hosts,
            vec!["web-1".to_owned(), "web-2".to_owned()],
            "the order is the core's sort, neither re-sorted nor reversed here"
        );

        let descending = BackendProbe {
            sample_hosts: vec![HostId::new("web-2"), HostId::new("web-1")],
            ..populated()
        };
        assert_eq!(
            BackendProbeRecord::from(descending).sample_hosts,
            vec!["web-2".to_owned(), "web-1".to_owned()],
            "the bridge does not sort; it would be a second place deciding the order"
        );
    }

    #[test]
    fn only_an_unauthorized_probe_is_actionable() {
        let rejected = BackendProbeRecord::from(rejected());
        let broken = BackendProbeRecord::from(connected_then_broken());

        assert_eq!(
            rejected
                .failure
                .as_ref()
                .map(|failure| (failure.kind, failure.actionable)),
            Some((ProbeFailureKind::Unauthorized, true)),
            "the one failure that routes an Open Settings button"
        );
        assert_eq!(
            broken
                .failure
                .as_ref()
                .map(|failure| (failure.kind, failure.actionable)),
            Some((ProbeFailureKind::Backend, false))
        );
        assert!(
            broken.connected && !broken.usable,
            "connected with a failed roster is the outcome `connected` exists to expose"
        );
    }
}
