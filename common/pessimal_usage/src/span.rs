//! Spans, and the trace they hang together in.
//!
//! Phase 1 models the client's poll cycle only. Agent spans are a separate phase — see the plan —
//! and arrive as additional [`UsageSpan`] variants, which is why this is an enum rather than a
//! trait: the complete set of things Pessimal will report is small, known, and worth reading in one
//! place.

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::attribute::{Attribute, AttributeKey, BackendKind, HttpStatusClass, Outcome};

/// A 16-byte trace id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TraceId([u8; 16]);

impl TraceId {
    /// Minted from a UUIDv7, per the project's id convention. The v7 layout puts a timestamp in the
    /// leading bytes, which is harmless here and occasionally useful when reading raw ids.
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::now_v7().into_bytes())
    }

    #[must_use]
    pub fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// Lowercase hex, which is what OTLP/JSON wants — not the base64 that plain `ProtoJSON` would
    /// produce for a `bytes` field. Getting this wrong is a 400 from a collector and, per the
    /// spike, possibly something quieter elsewhere.
    #[must_use]
    pub fn hex(&self) -> String {
        hex(&self.0)
    }
}

impl Default for TraceId {
    fn default() -> Self {
        Self::new()
    }
}

/// An 8-byte span id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SpanId([u8; 8]);

impl SpanId {
    /// The *trailing* eight bytes of a fresh UUIDv7.
    ///
    /// Deliberately not the leading eight: those are v7's millisecond timestamp, so two spans
    /// started in the same millisecond would collide. The trailing bytes are the random ones.
    #[must_use]
    pub fn new() -> Self {
        let bytes = Uuid::now_v7().into_bytes();
        let mut id = [0u8; 8];
        id.copy_from_slice(&bytes[8..16]);
        Self(id)
    }

    #[must_use]
    pub fn from_bytes(bytes: [u8; 8]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub fn hex(&self) -> String {
        hex(&self.0)
    }
}

impl Default for SpanId {
    fn default() -> Self {
        Self::new()
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from_digit(u32::from(byte >> 4), 16).unwrap_or('0'));
        out.push(char::from_digit(u32::from(byte & 0x0f), 16).unwrap_or('0'));
    }
    out
}

/// OTLP's span kind, narrowed to the two values Pessimal produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpanKind {
    /// Work inside the process.
    Internal,
    /// An outbound call to the operator's backend.
    Client,
}

impl SpanKind {
    /// OTLP's integer encoding. `1` and `3`; the names are in the proto, the numbers are the wire.
    #[must_use]
    pub fn code(self) -> u8 {
        match self {
            Self::Internal => 1,
            Self::Client => 3,
        }
    }
}

/// What happened, in the only vocabulary this crate can express.
///
/// Every field is an allowlisted enum or a count. There is no variant carrying a message, a URL or a
/// host name, and that is the whole design: see [`crate::attribute`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UsageSpan {
    /// One complete poll cycle. The root of a client trace.
    ClientPoll {
        outcome: Outcome,
        backend: BackendKind,
        hosts: u32,
        rules: u32,
        alerts_firing: u32,
    },
    /// Deciding what to request. Pure, and usually microseconds — it is here because a trace with a
    /// hole in it is harder to read than one with a fast span in it.
    ClientPlan { requests_planned: u32 },
    /// One request to the operator's backend.
    ClientGather {
        outcome: Outcome,
        status_class: Option<HttpStatusClass>,
        series_returned: u32,
    },
    /// Folding responses into a view, and evaluating alert rules.
    ClientFold {
        outcome: Outcome,
        alerts_firing: u32,
    },
    /// The settings screen's "test connection".
    ClientProbe {
        outcome: Outcome,
        status_class: Option<HttpStatusClass>,
    },
}

impl UsageSpan {
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::ClientPoll { .. } => "pessimal.client.poll",
            Self::ClientPlan { .. } => "pessimal.client.plan",
            Self::ClientGather { .. } => "pessimal.client.gather",
            Self::ClientFold { .. } => "pessimal.client.fold",
            Self::ClientProbe { .. } => "pessimal.client.probe",
        }
    }

    #[must_use]
    pub fn kind(&self) -> SpanKind {
        match self {
            Self::ClientGather { .. } | Self::ClientProbe { .. } => SpanKind::Client,
            _ => SpanKind::Internal,
        }
    }

    /// The outcome this span reports, where it reports one. Drives the span's status code, so a
    /// backend's error rate counts what we would count.
    #[must_use]
    pub fn outcome(&self) -> Option<Outcome> {
        match self {
            Self::ClientPoll { outcome, .. }
            | Self::ClientGather { outcome, .. }
            | Self::ClientFold { outcome, .. }
            | Self::ClientProbe { outcome, .. } => Some(*outcome),
            Self::ClientPlan { .. } => None,
        }
    }

    #[must_use]
    pub fn attributes(&self) -> Vec<Attribute> {
        match self {
            Self::ClientPoll {
                outcome,
                backend,
                hosts,
                rules,
                alerts_firing,
            } => vec![
                Attribute::enumerated(AttributeKey::Outcome, outcome.as_str()),
                Attribute::enumerated(AttributeKey::BackendKind, backend.as_str()),
                Attribute::count(AttributeKey::HostsCount, *hosts),
                Attribute::count(AttributeKey::RulesCount, *rules),
                Attribute::count(AttributeKey::AlertsFiring, *alerts_firing),
            ],
            Self::ClientPlan { requests_planned } => vec![Attribute::count(
                AttributeKey::RequestsPlanned,
                *requests_planned,
            )],
            Self::ClientGather {
                outcome,
                status_class,
                series_returned,
            } => {
                let mut attributes = vec![
                    Attribute::enumerated(AttributeKey::Outcome, outcome.as_str()),
                    Attribute::count(AttributeKey::SeriesReturned, *series_returned),
                ];
                if let Some(class) = status_class {
                    attributes.push(Attribute::count(
                        AttributeKey::HttpStatusClass,
                        u32::from(class.digit()),
                    ));
                }
                attributes
            }
            Self::ClientFold {
                outcome,
                alerts_firing,
            } => vec![
                Attribute::enumerated(AttributeKey::Outcome, outcome.as_str()),
                Attribute::count(AttributeKey::AlertsFiring, *alerts_firing),
            ],
            Self::ClientProbe {
                outcome,
                status_class,
            } => {
                let mut attributes = vec![Attribute::enumerated(
                    AttributeKey::Outcome,
                    outcome.as_str(),
                )];
                if let Some(class) = status_class {
                    attributes.push(Attribute::count(
                        AttributeKey::HttpStatusClass,
                        u32::from(class.digit()),
                    ));
                }
                attributes
            }
        }
    }
}

/// A span with its place in the tree and its timings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedSpan {
    pub id: SpanId,
    /// `None` for the root. Encoded by *omitting* `parentSpanId` rather than sending zeros — the
    /// spike showed a collector normalising zeros away, but the spec's answer is absence.
    pub parent: Option<SpanId>,
    pub started: DateTime<Utc>,
    pub ended: DateTime<Utc>,
    pub payload: UsageSpan,
}

/// One trace: a root span and its descendants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageTrace {
    pub trace_id: TraceId,
    pub spans: Vec<RecordedSpan>,
}

impl UsageTrace {
    #[must_use]
    pub fn new(trace_id: TraceId) -> Self {
        Self {
            trace_id,
            spans: Vec::new(),
        }
    }

    /// Appends a span and hands back its id, so a caller can parent the next one to it.
    ///
    /// Takes the timings rather than reading a clock: this crate has no I/O, and a test that cannot
    /// choose its timestamps cannot assert on them.
    pub fn record(
        &mut self,
        payload: UsageSpan,
        parent: Option<SpanId>,
        started: DateTime<Utc>,
        ended: DateTime<Utc>,
    ) -> SpanId {
        let id = SpanId::new();
        self.spans.push(RecordedSpan {
            id,
            parent,
            started,
            ended,
            payload,
        });
        id
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.spans.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_render_as_lowercase_hex_of_the_right_length() {
        let trace = TraceId::from_bytes([0xab; 16]);
        assert_eq!(trace.hex(), "ab".repeat(16));
        assert_eq!(trace.hex().len(), 32);

        let span = SpanId::from_bytes([0x0f, 0xf0, 0, 1, 2, 3, 4, 5]);
        assert_eq!(
            span.hex(),
            "0ff0000102030405",
            "a high nibble of 0 must not be dropped"
        );
        assert_eq!(span.hex().len(), 16);
    }

    #[test]
    fn span_ids_take_the_random_half_of_a_uuidv7_not_the_timestamp() {
        // Two ids minted in the same millisecond must still differ. Taking v7's leading bytes —
        // which are the clock — would make this fail.
        let ids: std::collections::HashSet<_> = (0..64).map(|_| SpanId::new()).collect();
        assert_eq!(ids.len(), 64, "span ids collided within one millisecond");
    }

    #[test]
    fn trace_ids_are_distinct() {
        let ids: std::collections::HashSet<_> = (0..64).map(|_| TraceId::new().hex()).collect();
        assert_eq!(ids.len(), 64);
    }

    #[test]
    fn a_gather_without_a_response_omits_the_status_class() {
        let span = UsageSpan::ClientGather {
            outcome: Outcome::Unreachable,
            status_class: None,
            series_returned: 0,
        };
        let keys: Vec<_> = span.attributes().into_iter().map(|a| a.key).collect();
        assert!(!keys.contains(&AttributeKey::HttpStatusClass));
        // The outcome still says what happened, which is the point: a connection that never
        // produced a status is not the same as one that produced a 500.
        assert!(keys.contains(&AttributeKey::Outcome));
    }

    #[test]
    fn a_gather_with_a_response_carries_only_the_class() {
        let span = UsageSpan::ClientGather {
            outcome: Outcome::BackendError,
            status_class: HttpStatusClass::of(503),
            series_returned: 0,
        };
        let status = span
            .attributes()
            .into_iter()
            .find(|a| a.key == AttributeKey::HttpStatusClass)
            .expect("present");
        assert_eq!(status.value, crate::attribute::AttributeValue::Count(5));
    }

    #[test]
    fn outbound_spans_are_client_kind_and_the_rest_internal() {
        let gather = UsageSpan::ClientGather {
            outcome: Outcome::Ok,
            status_class: HttpStatusClass::of(200),
            series_returned: 3,
        };
        assert_eq!(gather.kind(), SpanKind::Client);
        assert_eq!(
            UsageSpan::ClientPlan {
                requests_planned: 4
            }
            .kind(),
            SpanKind::Internal
        );
    }

    #[test]
    fn a_plan_span_reports_no_outcome_so_it_cannot_skew_an_error_rate() {
        assert!(
            UsageSpan::ClientPlan {
                requests_planned: 1
            }
            .outcome()
            .is_none()
        );
    }

    #[test]
    fn recording_builds_a_tree() {
        let now = Utc::now();
        let mut trace = UsageTrace::new(TraceId::new());
        assert!(trace.is_empty());

        let root = trace.record(
            UsageSpan::ClientPoll {
                outcome: Outcome::Ok,
                backend: BackendKind::Signoz,
                hosts: 2,
                rules: 1,
                alerts_firing: 0,
            },
            None,
            now,
            now + chrono::Duration::milliseconds(400),
        );
        let child = trace.record(
            UsageSpan::ClientPlan {
                requests_planned: 20,
            },
            Some(root),
            now,
            now + chrono::Duration::milliseconds(1),
        );

        assert_eq!(trace.spans.len(), 2);
        assert_eq!(trace.spans[0].parent, None, "the root has no parent");
        assert_eq!(trace.spans[1].parent, Some(root));
        assert_ne!(child, root);
    }
}
