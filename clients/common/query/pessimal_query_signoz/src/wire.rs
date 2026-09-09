//! The `/api/v5/query_range` request and response, as SigNoz defines them.
//!
//! Field names and shapes here are taken from SigNoz's own Go types
//! (`pkg/types/querybuildertypes/querybuildertypesv5/resp.go`), not from the prose documentation,
//! which does not publish the response body.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Request
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct QueryRangeRequest {
    /// Epoch milliseconds.
    pub start: i64,
    /// Epoch milliseconds.
    pub end: i64,
    #[serde(rename = "requestType")]
    pub request_type: &'static str,
    #[serde(rename = "compositeQuery")]
    pub composite_query: CompositeQuery,
}

impl QueryRangeRequest {
    /// A single-query time-series request.
    #[must_use]
    pub fn time_series(start_ms: i64, end_ms: i64, spec: BuilderSpec) -> Self {
        Self {
            start: start_ms,
            end: end_ms,
            request_type: "time_series",
            composite_query: CompositeQuery {
                queries: vec![Query {
                    query_type: "builder_query",
                    spec,
                }],
            },
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CompositeQuery {
    pub queries: Vec<Query>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Query {
    #[serde(rename = "type")]
    pub query_type: &'static str,
    pub spec: BuilderSpec,
}

#[derive(Debug, Clone, Serialize)]
pub struct BuilderSpec {
    pub name: &'static str,
    pub signal: &'static str,
    /// Seconds. Note the asymmetry with `start`/`end`, which are milliseconds.
    #[serde(rename = "stepInterval")]
    pub step_interval: i64,
    pub aggregations: Vec<Aggregation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filter: Option<Filter>,
    #[serde(rename = "groupBy")]
    pub group_by: Vec<GroupByKey>,
    pub disabled: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Aggregation {
    #[serde(rename = "metricName")]
    pub metric_name: String,
    pub temporality: &'static str,
    #[serde(rename = "timeAggregation")]
    pub time_aggregation: &'static str,
    #[serde(rename = "spaceAggregation")]
    pub space_aggregation: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct Filter {
    pub expression: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct GroupByKey {
    pub name: String,
}

// ---------------------------------------------------------------------------
// Response
// ---------------------------------------------------------------------------

/// The HTTP response envelope.
///
/// SigNoz's Go `QueryRangeResponse` — the type this module was first written against — is the
/// *inner* object. The handler wraps it again: `{status, data: {type, meta, data: {results}}}`.
/// Parsing the inner type directly against a live instance does not fail; it yields an empty result
/// set, because `results` defaults. That is why a mock built from the Go types agreed with the code
/// and both were wrong: only a real response has the outer layer.
///
/// `data` is deliberately required. If the envelope changes again, that must surface as a parse
/// error rather than as a fleet that appears to have no hosts.
#[derive(Debug, Clone, Deserialize)]
pub struct QueryRangeEnvelope {
    #[serde(default)]
    pub status: String,
    pub data: QueryRangeResponse,
}

impl QueryRangeEnvelope {
    /// Whether the backend reported success. Anything else is an error, not an empty fleet.
    #[must_use]
    pub fn is_success(&self) -> bool {
        // An older build, or a proxy, may omit the field entirely; absence is not failure.
        self.status.is_empty() || self.status.eq_ignore_ascii_case("success")
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct QueryRangeResponse {
    /// Required, not defaulted. Tolerating its absence is exactly what let the inner-shape parser
    /// succeed against a live instance and report an empty fleet: every field being optional means
    /// a wrong shape is indistinguishable from no data.
    pub data: QueryData,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct QueryData {
    /// Heterogeneous by request type. Only `time_series` shapes are requested, so anything that
    /// does not parse as one is skipped rather than failing the whole response.
    ///
    /// `Option` because the field arrives as an explicit `null`, not merely absent, when a query
    /// matched nothing — and `#[serde(default)]` does not cover an explicit null for a `Vec`.
    #[serde(default)]
    results: Option<Vec<TimeSeriesData>>,
}

impl QueryData {
    #[must_use]
    pub fn results(&self) -> &[TimeSeriesData] {
        self.results.as_deref().unwrap_or_default()
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct TimeSeriesData {
    #[serde(rename = "queryName", default)]
    pub query_name: String,
    /// Explicitly `null` when the query matched no series — which is what a temporality mismatch
    /// looks like, so this is a routine shape rather than an edge case.
    #[serde(default)]
    aggregations: Option<Vec<AggregationBucket>>,
}

impl TimeSeriesData {
    #[must_use]
    pub fn aggregations(&self) -> &[AggregationBucket] {
        self.aggregations.as_deref().unwrap_or_default()
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct AggregationBucket {
    #[serde(default)]
    series: Option<Vec<TimeSeries>>,
}

impl AggregationBucket {
    #[must_use]
    pub fn series(&self) -> &[TimeSeries] {
        self.series.as_deref().unwrap_or_default()
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct TimeSeries {
    #[serde(default)]
    labels: Option<Vec<Label>>,
    #[serde(default)]
    values: Option<Vec<TimeSeriesValue>>,
}

impl TimeSeries {
    #[must_use]
    pub fn labels(&self) -> &[Label] {
        self.labels.as_deref().unwrap_or_default()
    }

    #[must_use]
    pub fn values(&self) -> &[TimeSeriesValue] {
        self.values.as_deref().unwrap_or_default()
    }
}

impl TimeSeries {
    /// The series' labels flattened to a plain map, dropping any whose value is not a scalar.
    #[must_use]
    pub fn label_map(&self) -> BTreeMap<String, String> {
        self.labels()
            .iter()
            .filter_map(|label| {
                label
                    .value
                    .as_display_string()
                    .map(|value| (label.key.name.clone(), value))
            })
            .collect()
    }
}

/// A label. Note that `key` is an object, not a string — the obvious guess is wrong.
#[derive(Debug, Clone, Deserialize)]
pub struct Label {
    pub key: LabelKey,
    #[serde(default)]
    pub value: LabelValue,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LabelKey {
    #[serde(default)]
    pub name: String,
}

/// A label value, which SigNoz types as `any`.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum LabelValue {
    Text(String),
    Number(f64),
    Boolean(bool),
    /// Null, or a shape SigNoz may add later. Kept rather than rejected so one unexpected label
    /// cannot fail an otherwise usable response.
    Other(serde_json::Value),
}

impl Default for LabelValue {
    fn default() -> Self {
        Self::Other(serde_json::Value::Null)
    }
}

impl LabelValue {
    /// The value as a string, or `None` when it is not a scalar.
    #[must_use]
    pub fn as_display_string(&self) -> Option<String> {
        match self {
            Self::Text(text) => Some(text.clone()),
            Self::Number(number) => Some(number.to_string()),
            Self::Boolean(flag) => Some(flag.to_string()),
            Self::Other(_) => None,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct TimeSeriesValue {
    /// Epoch milliseconds.
    pub timestamp: i64,
    #[serde(default)]
    pub value: SampleValue,
    /// Set when the bucket does not cover its whole step — the first bucket of a window usually
    /// does not. SigNoz's own comment says these cannot be cached and should be ignored.
    #[serde(default)]
    pub partial: bool,
}

/// A sample value.
///
/// Declared `float64` in SigNoz's Go types, but every value is serialised through a sanitiser that
/// renders non-finite numbers as the **strings** `"NaN"`, `"Inf"` and `"-Inf"`. Deserialising
/// straight into `f64` therefore fails exactly when a host has a gap in its data, which is when the
/// client most needs to keep working.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum SampleValue {
    Number(f64),
    /// `"NaN"`, `"Inf"`, `"-Inf"`, or anything else non-numeric.
    NonFinite(String),
    /// Null, or a shape not seen before. Either way, a gap.
    Other(serde_json::Value),
}

impl Default for SampleValue {
    fn default() -> Self {
        Self::Other(serde_json::Value::Null)
    }
}

impl SampleValue {
    /// The value as a finite `f64`, or `None` for a gap.
    #[must_use]
    pub fn as_finite(&self) -> Option<f64> {
        match self {
            Self::Number(value) if value.is_finite() => Some(*value),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real envelope, captured verbatim from a live SigNoz Cloud instance on 2026-09-09.
    /// Host name and values are synthetic; the SHAPE is what matters and it is exactly as returned.
    ///
    /// Note the double nesting — `data.data.results` — and that `labels[].key` is an object. An
    /// earlier version of these fixtures was written from SigNoz's Go `QueryRangeResponse`, which
    /// is only the inner half, so the fixtures and the parser agreed with each other and both
    /// disagreed with the server.
    fn live_envelope(series: &str) -> String {
        format!(
            r#"{{
              "status": "success",
              "data": {{
                "type": "time_series",
                "meta": {{"rowsScanned": 2001, "bytesScanned": 50912, "durationMs": 30}},
                "data": {{"results": [{{"queryName": "A", "aggregations": [
                  {{"index": 0, "alias": "", "series": [{series}]}}
                ]}}]}}
              }}
            }}"#
        )
    }

    #[test]
    fn parses_the_real_response_envelope() {
        let body = live_envelope(
            r#"{"labels": [{"key": {"name": "host.name", "signal": "", "fieldContext": "", "fieldDataType": ""}, "value": "web-1"}],
                "values": [{"timestamp": 1788940740000, "value": 0.0649},
                           {"timestamp": 1788940800000, "value": 0.34}]}"#,
        );
        let envelope: QueryRangeEnvelope = serde_json::from_str(&body).expect("parses");
        assert!(envelope.is_success());

        let results = envelope.data.data.results();
        let series = &results[0].aggregations()[0].series()[0];
        assert_eq!(
            series.label_map().get("host.name").map(String::as_str),
            Some("web-1")
        );
        assert_eq!(series.values().len(), 2);
        assert_eq!(series.values()[0].value.as_finite(), Some(0.0649));
    }

    #[test]
    fn the_inner_shape_alone_is_rejected_rather_than_read_as_an_empty_fleet() {
        // This is the bug these fixtures used to encode: the inner object parsed happily and
        // yielded zero series, so a live instance looked like a fleet with no hosts.
        let inner = r#"{"type":"time_series","data":{"results":[]},"meta":{}}"#;
        assert!(
            serde_json::from_str::<QueryRangeEnvelope>(inner).is_err(),
            "an envelope without its outer `data` must fail loudly, not parse to empty"
        );
    }

    #[test]
    fn a_non_success_status_is_not_an_empty_fleet() {
        let body = r#"{"status":"error","data":{"type":"time_series","data":{"results":null}}}"#;
        let envelope: QueryRangeEnvelope = serde_json::from_str(body).expect("parses");
        assert!(!envelope.is_success());
    }

    #[test]
    fn an_absent_status_is_treated_as_success() {
        let body = r#"{"data":{"type":"time_series","data":{"results":null}}}"#;
        let envelope: QueryRangeEnvelope = serde_json::from_str(body).expect("parses");
        assert!(
            envelope.is_success(),
            "a proxy may strip it; absence is not failure"
        );
    }

    #[test]
    fn null_results_and_null_aggregations_are_empty_not_errors() {
        // Exactly what a temporality mismatch returns, so it is a routine shape.
        let body = r#"{"status":"success","data":{"type":"time_series","data":{"results":
            [{"queryName":"A","aggregations":null}]}}}"#;
        let envelope: QueryRangeEnvelope = serde_json::from_str(body).expect("parses");
        assert_eq!(envelope.data.data.results().len(), 1);
        assert!(envelope.data.data.results()[0].aggregations().is_empty());

        let body = r#"{"status":"success","data":{"type":"time_series","data":{"results":null}}}"#;
        let envelope: QueryRangeEnvelope = serde_json::from_str(body).expect("parses");
        assert!(envelope.data.data.results().is_empty());
    }

    #[test]
    fn a_label_key_is_an_object_not_a_string() {
        let series: TimeSeries = serde_json::from_str(
            r#"{"labels": [{"key": {"name": "os.type"}, "value": "linux"}], "values": []}"#,
        )
        .expect("parses");
        assert_eq!(series.labels()[0].key.name, "os.type");
    }

    #[test]
    fn non_finite_values_arrive_as_strings_and_do_not_break_parsing() {
        for rendered in ["\"NaN\"", "\"Inf\"", "\"-Inf\""] {
            let value: TimeSeriesValue =
                serde_json::from_str(&format!(r#"{{"timestamp": 1, "value": {rendered}}}"#))
                    .expect("a gap must not fail the whole response");
            assert!(value.value.as_finite().is_none(), "{rendered}");
        }
    }

    #[test]
    fn a_null_value_is_a_gap_not_an_error() {
        let value: TimeSeriesValue =
            serde_json::from_str(r#"{"timestamp": 1, "value": null}"#).expect("parses");
        assert!(value.value.as_finite().is_none());
    }

    #[test]
    fn partial_buckets_are_flagged() {
        let value: TimeSeriesValue =
            serde_json::from_str(r#"{"timestamp": 1, "value": 0.5, "partial": true}"#)
                .expect("parses");
        assert!(value.partial);

        let complete: TimeSeriesValue =
            serde_json::from_str(r#"{"timestamp": 1, "value": 0.5}"#).expect("parses");
        assert!(!complete.partial, "absent means complete");
    }

    #[test]
    fn numeric_and_boolean_label_values_are_stringified() {
        let series: TimeSeries = serde_json::from_str(
            r#"{"labels": [
                 {"key": {"name": "port"}, "value": 8080},
                 {"key": {"name": "tls"}, "value": true}
               ], "values": []}"#,
        )
        .expect("parses");
        let labels = series.label_map();
        assert_eq!(labels.get("port").map(String::as_str), Some("8080"));
        assert_eq!(labels.get("tls").map(String::as_str), Some("true"));
    }

    #[test]
    fn the_request_serialises_to_the_documented_shape() {
        let request = QueryRangeRequest::time_series(
            1_742_602_572_000,
            1_742_604_372_000,
            BuilderSpec {
                name: "A",
                signal: "metrics",
                step_interval: 60,
                aggregations: vec![Aggregation {
                    metric_name: "system.cpu.utilization".to_owned(),
                    temporality: "Unspecified",
                    time_aggregation: "avg",
                    space_aggregation: "avg",
                }],
                filter: Some(Filter {
                    expression: "host.name IN ['web-1']".to_owned(),
                }),
                group_by: vec![GroupByKey {
                    name: "host.name".to_owned(),
                }],
                disabled: false,
            },
        );

        let json = serde_json::to_value(&request).expect("serialises");
        assert_eq!(json["requestType"], "time_series");
        let spec = &json["compositeQuery"]["queries"][0]["spec"];
        assert_eq!(
            json["compositeQuery"]["queries"][0]["type"],
            "builder_query"
        );
        assert_eq!(spec["stepInterval"], 60);
        assert_eq!(spec["signal"], "metrics");
        assert_eq!(
            spec["aggregations"][0]["metricName"],
            "system.cpu.utilization"
        );
        assert_eq!(spec["groupBy"][0]["name"], "host.name");
        assert_eq!(spec["filter"]["expression"], "host.name IN ['web-1']");
    }

    #[test]
    fn an_absent_filter_is_omitted_rather_than_sent_as_null() {
        let request = QueryRangeRequest::time_series(
            0,
            1,
            BuilderSpec {
                name: "A",
                signal: "metrics",
                step_interval: 60,
                aggregations: vec![],
                filter: None,
                group_by: vec![],
                disabled: false,
            },
        );
        let json = serde_json::to_value(&request).expect("serialises");
        assert!(
            json["compositeQuery"]["queries"][0]["spec"]
                .get("filter")
                .is_none()
        );
    }
}
