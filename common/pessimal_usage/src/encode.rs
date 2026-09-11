//! OTLP/HTTP JSON encoding for traces.
//!
//! Hand-rolled rather than taken from `opentelemetry-otlp`, for two reasons recorded in the plan:
//! that crate's `reqwest-rustls` feature drags in `aws-lc-rs`, which is the cross-compilation
//! problem `pessimal_query_signoz` exists to avoid, and its `PeriodicReader` is a forever-task,
//! which is wrong for an app that gets backgrounded. Five span shapes do not need an SDK.
//!
//! OTLP/JSON is **not** plain `ProtoJSON`. The deviations that matter, all verified against
//! otelcol-contrib 0.160.0 by `scripts/otlp-trace-probe.py`:
//!
//! * `traceId` / `spanId` are lowercase **hex**; `ProtoJSON` would base64 a `bytes` field. A
//!   collector answers base64 with a 400.
//! * 64-bit fields — `startTimeUnixNano`, `endTimeUnixNano`, `intValue` — are **decimal strings**.
//!   The collector happens to accept JSON numbers, and to read them without float64 rounding, but
//!   that leniency is the receiver's and not the spec's.
//! * `kind` and `status.code` are integers.
//! * a root span **omits** `parentSpanId`.

use serde_json::{Map, Value, json};

use crate::attribute::{Attribute, AttributeValue};
use crate::resource::UsageResource;
use crate::span::{RecordedSpan, UsageTrace};

/// The instrumentation scope every Pessimal usage span is reported under.
pub const SCOPE_NAME: &str = "pessimal.usage";

/// OTLP `Status.code`: unset, ok, error.
const STATUS_UNSET: u8 = 0;
const STATUS_OK: u8 = 1;
const STATUS_ERROR: u8 = 2;

/// Encodes one `ExportTraceServiceRequest`.
///
/// Traces share one resource because one process reports them. An empty `traces` still produces a
/// valid body; the caller is expected not to send it.
#[must_use]
pub fn export_trace_request(resource: &UsageResource, traces: &[UsageTrace]) -> Value {
    let spans: Vec<Value> = traces
        .iter()
        .flat_map(|trace| {
            trace
                .spans
                .iter()
                .map(move |span| encode_span(&trace.trace_id.hex(), span))
        })
        .collect();

    json!({
        "resourceSpans": [{
            "resource": { "attributes": encode_attributes(&resource.attributes()) },
            "scopeSpans": [{
                "scope": { "name": SCOPE_NAME, "version": env!("CARGO_PKG_VERSION") },
                "spans": spans,
            }],
        }],
    })
}

fn encode_span(trace_id_hex: &str, span: &RecordedSpan) -> Value {
    let mut encoded = Map::new();
    encoded.insert("traceId".to_owned(), Value::String(trace_id_hex.to_owned()));
    encoded.insert("spanId".to_owned(), Value::String(span.id.hex()));
    // Omitted entirely for a root. A zero-filled id would be normalised away by the collector we
    // tested, but absence is what the spec asks for and what every backend must handle.
    if let Some(parent) = span.parent {
        encoded.insert("parentSpanId".to_owned(), Value::String(parent.hex()));
    }
    encoded.insert(
        "name".to_owned(),
        Value::String(span.payload.name().to_owned()),
    );
    encoded.insert(
        "kind".to_owned(),
        Value::Number(span.payload.kind().code().into()),
    );
    encoded.insert(
        "startTimeUnixNano".to_owned(),
        Value::String(unix_nanos(&span.started)),
    );
    encoded.insert(
        "endTimeUnixNano".to_owned(),
        Value::String(unix_nanos(&span.ended)),
    );
    encoded.insert(
        "attributes".to_owned(),
        encode_attributes(&span.payload.attributes()),
    );

    let code = match span.payload.outcome() {
        None => STATUS_UNSET,
        Some(outcome) if outcome.is_error() => STATUS_ERROR,
        Some(_) => STATUS_OK,
    };
    // No `message`: the human-readable half of a status is exactly where a backend error body would
    // end up, so the status carries its code alone.
    encoded.insert("status".to_owned(), json!({ "code": code }));

    Value::Object(encoded)
}

fn encode_attributes(attributes: &[Attribute]) -> Value {
    Value::Array(
        attributes
            .iter()
            .map(|attribute| {
                json!({
                    "key": attribute.key.otel_name(),
                    "value": encode_value(&attribute.value),
                })
            })
            .collect(),
    )
}

fn encode_value(value: &AttributeValue) -> Value {
    match value {
        AttributeValue::Enumerated(text) => json!({ "stringValue": text }),
        // `intValue` is an int64, so a decimal string.
        AttributeValue::Count(count) => json!({ "intValue": count.to_string() }),
        AttributeValue::Version(version) => json!({ "stringValue": version.as_str() }),
        AttributeValue::Identifier(id) => json!({ "stringValue": id.to_string() }),
    }
}

/// Nanoseconds since the epoch, as the decimal string OTLP/JSON wants.
///
/// `timestamp_nanos_opt` is `None` outside roughly 1677–2262. A timestamp that far out is a broken
/// device clock, and `0` is a less misleading answer than a panic or a wrapped value.
fn unix_nanos(at: &chrono::DateTime<chrono::Utc>) -> String {
    at.timestamp_nanos_opt().unwrap_or(0).to_string()
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};
    use uuid::Uuid;

    use super::*;
    use crate::attribute::{BackendKind, DeviceClass, HttpStatusClass, Outcome, Platform};
    use crate::resource::{Service, UsageResource};
    use crate::span::{SpanId, TraceId, UsageSpan};

    fn fixed_resource() -> UsageResource {
        UsageResource::new(
            Service::Client,
            Platform::Ios,
            DeviceClass::Phone,
            Uuid::parse_str("0199a0e4-0000-7000-8000-000000000001").expect("valid"),
            "0.1.0",
            Some(26),
            "18.4",
        )
    }

    /// A trace with deterministic ids and timestamps, so assertions can be exact.
    fn fixed_trace() -> UsageTrace {
        let started = Utc.timestamp_nanos(1_773_230_000_123_456_789);
        let mut trace = UsageTrace {
            trace_id: TraceId::from_bytes([0x11; 16]),
            spans: Vec::new(),
        };
        let root = SpanId::from_bytes([0x22; 8]);
        trace.spans.push(RecordedSpan {
            id: root,
            parent: None,
            started,
            ended: started + chrono::Duration::milliseconds(412),
            payload: UsageSpan::ClientPoll {
                outcome: Outcome::Ok,
                backend: BackendKind::Signoz,
                hosts: 12,
                rules: 3,
                alerts_firing: 1,
            },
        });
        trace.spans.push(RecordedSpan {
            id: SpanId::from_bytes([0x33; 8]),
            parent: Some(root),
            started,
            ended: started + chrono::Duration::milliseconds(388),
            payload: UsageSpan::ClientGather {
                outcome: Outcome::Ok,
                status_class: HttpStatusClass::of(200),
                series_returned: 24,
            },
        });
        trace
    }

    fn spans_of(body: &Value) -> &Vec<Value> {
        body["resourceSpans"][0]["scopeSpans"][0]["spans"]
            .as_array()
            .expect("spans array")
    }

    #[test]
    fn ids_are_hex_strings_of_the_right_length() {
        let body = export_trace_request(&fixed_resource(), &[fixed_trace()]);
        let span = &spans_of(&body)[0];
        assert_eq!(span["traceId"], Value::String("11".repeat(16)));
        assert_eq!(span["spanId"], Value::String("22".repeat(8)));
        assert_eq!(span["traceId"].as_str().expect("string").len(), 32);
        assert_eq!(span["spanId"].as_str().expect("string").len(), 16);
    }

    #[test]
    fn a_root_omits_parent_span_id_and_a_child_carries_it() {
        let body = export_trace_request(&fixed_resource(), &[fixed_trace()]);
        let spans = spans_of(&body);
        assert!(
            spans[0].get("parentSpanId").is_none(),
            "a root must omit the field, not zero it"
        );
        assert_eq!(spans[1]["parentSpanId"], Value::String("22".repeat(8)));
    }

    #[test]
    fn timestamps_are_decimal_strings_with_nanosecond_precision_intact() {
        let body = export_trace_request(&fixed_resource(), &[fixed_trace()]);
        let span = &spans_of(&body)[0];
        // The low digits are the assertion: a float64 round trip would lose them.
        assert_eq!(
            span["startTimeUnixNano"],
            Value::String("1773230000123456789".to_owned())
        );
        assert!(span["startTimeUnixNano"].is_string());
    }

    #[test]
    fn counts_are_decimal_strings_because_int_value_is_an_int64() {
        let body = export_trace_request(&fixed_resource(), &[fixed_trace()]);
        let attributes = spans_of(&body)[0]["attributes"]
            .as_array()
            .expect("attributes")
            .clone();
        let hosts = attributes
            .iter()
            .find(|a| a["key"] == "pessimal.hosts.count")
            .expect("present");
        assert_eq!(hosts["value"]["intValue"], Value::String("12".to_owned()));
    }

    #[test]
    fn kind_and_status_are_integers() {
        let body = export_trace_request(&fixed_resource(), &[fixed_trace()]);
        let spans = spans_of(&body);
        assert_eq!(spans[0]["kind"], json!(1), "internal");
        assert_eq!(spans[1]["kind"], json!(3), "client");
        assert_eq!(spans[0]["status"]["code"], json!(1), "ok");
    }

    #[test]
    fn a_failing_outcome_marks_the_span_as_an_error() {
        let started = Utc.timestamp_nanos(1_773_230_000_000_000_000);
        let trace = UsageTrace {
            trace_id: TraceId::from_bytes([0x44; 16]),
            spans: vec![RecordedSpan {
                id: SpanId::from_bytes([0x55; 8]),
                parent: None,
                started,
                ended: started,
                payload: UsageSpan::ClientGather {
                    outcome: Outcome::Unauthorized,
                    status_class: HttpStatusClass::of(401),
                    series_returned: 0,
                },
            }],
        };
        let body = export_trace_request(&fixed_resource(), &[trace]);
        assert_eq!(spans_of(&body)[0]["status"]["code"], json!(2));
    }

    #[test]
    fn a_span_with_no_outcome_leaves_the_status_unset() {
        let started = Utc.timestamp_nanos(1_773_230_000_000_000_000);
        let trace = UsageTrace {
            trace_id: TraceId::from_bytes([0x66; 16]),
            spans: vec![RecordedSpan {
                id: SpanId::from_bytes([0x77; 8]),
                parent: None,
                started,
                ended: started,
                payload: UsageSpan::ClientPlan {
                    requests_planned: 20,
                },
            }],
        };
        let body = export_trace_request(&fixed_resource(), &[trace]);
        assert_eq!(spans_of(&body)[0]["status"]["code"], json!(0));
    }

    #[test]
    fn a_status_never_carries_a_message() {
        // The message field is where a backend's error body would end up if anyone ever reached for
        // it. There must be no precedent for it in the encoder.
        let body = export_trace_request(&fixed_resource(), &[fixed_trace()]);
        for span in spans_of(&body) {
            assert!(
                span["status"].get("message").is_none(),
                "a status carried a message"
            );
        }
    }

    #[test]
    fn every_emitted_key_is_on_the_allowlist() {
        // The regression guard for the whole design. If a future span type invents a key, this fails
        // before it reaches anyone's backend.
        let permitted: Vec<&str> = crate::attribute::AttributeKey::ALL
            .iter()
            .map(|k| k.otel_name())
            .collect();
        let body = export_trace_request(&fixed_resource(), &[fixed_trace()]);

        let mut seen = Vec::new();
        for attribute in body["resourceSpans"][0]["resource"]["attributes"]
            .as_array()
            .expect("resource attributes")
        {
            seen.push(attribute["key"].as_str().expect("string").to_owned());
        }
        for span in spans_of(&body) {
            for attribute in span["attributes"].as_array().expect("attributes") {
                seen.push(attribute["key"].as_str().expect("string").to_owned());
            }
        }

        assert!(!seen.is_empty());
        for key in seen {
            assert!(
                permitted.contains(&key.as_str()),
                "{key} is not allowlisted"
            );
        }
    }

    #[test]
    fn the_scope_names_pessimals_own_instrumentation() {
        let body = export_trace_request(&fixed_resource(), &[fixed_trace()]);
        assert_eq!(
            body["resourceSpans"][0]["scopeSpans"][0]["scope"]["name"],
            json!("pessimal.usage")
        );
    }
}
