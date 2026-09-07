//! HTTP-level behaviour of the SigNoz adapter, against a mock `/api/v5/query_range`.
//!
//! The response bodies here follow SigNoz's own Go response types rather than a guess at them; see
//! `src/wire.rs`. What these tests prove is how the adapter behaves given those shapes — the auth
//! header it sends, the filter it builds, which points it keeps, and how it maps failures.

use chrono::{Duration, TimeZone, Utc};
use pessimal_core::{
    CoreError, HostId, HostSelector, MetricKind, OsFamily, SeriesRequest, TelemetryQuery, TimeRange,
};
use pessimal_query_signoz::{MetricNaming, SignozConfig, SignozQuery};
use serde_json::{Value, json};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const API_KEY: &str = "test-key";

fn range() -> TimeRange {
    let end = Utc.timestamp_opt(1_742_604_372, 0).single().expect("valid");
    TimeRange::ending_at(end, Duration::minutes(30)).expect("valid")
}

fn adapter(server: &MockServer) -> SignozQuery {
    SignozQuery::new(SignozConfig::new(server.uri(), API_KEY).expect("valid config"))
        .expect("client builds")
}

/// One series, in the shape SigNoz returns.
fn series(labels: &[(&str, Value)], values: &[Value]) -> Value {
    json!({
        "labels": labels.iter().map(|(name, value)| json!({
            "key": {"name": name, "signal": "", "fieldContext": "", "fieldDataType": ""},
            "value": value
        })).collect::<Vec<_>>(),
        "values": values
    })
}

fn response(series: &[Value]) -> Value {
    json!({
        "type": "time_series",
        "data": {"results": [{"queryName": "A", "aggregations": [{
            "index": 0, "alias": "", "meta": {}, "series": series
        }]}]},
        "meta": {}
    })
}

fn point(timestamp_ms: i64, value: &Value) -> Value {
    json!({"timestamp": timestamp_ms, "value": value})
}

async fn mount(server: &MockServer, body: &Value) {
    Mock::given(method("POST"))
        .and(path("/api/v5/query_range"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(server)
        .await;
}

/// The body of the single request the server received.
async fn sent_body(server: &MockServer) -> Value {
    let requests = server.received_requests().await.expect("recording enabled");
    assert_eq!(requests.len(), 1, "expected exactly one request");
    serde_json::from_slice(&requests[0].body).expect("a JSON body")
}

fn spec(body: &Value) -> &Value {
    &body["compositeQuery"]["queries"][0]["spec"]
}

#[tokio::test]
async fn sends_the_api_key_in_signoz_s_own_header() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v5/query_range"))
        .and(header("SIGNOZ-API-KEY", API_KEY))
        .respond_with(ResponseTemplate::new(200).set_body_json(response(&[])))
        .mount(&server)
        .await;

    adapter(&server)
        .check_connection()
        .await
        .expect("the header matcher is the assertion");
}

#[tokio::test]
async fn an_empty_result_passes_the_connection_check() {
    let server = MockServer::start().await;
    mount(&server, &response(&[])).await;

    // No agent reporting is not the same as no connection.
    adapter(&server)
        .check_connection()
        .await
        .expect("connected");
}

#[tokio::test]
async fn parses_a_series_into_points() {
    let server = MockServer::start().await;
    mount(
        &server,
        &response(&[series(
            &[("host.name", json!("web-1"))],
            &[
                point(1_742_602_572_000, &json!(0.21)),
                point(1_742_602_632_000, &json!(0.34)),
            ],
        )]),
    )
    .await;

    let result = adapter(&server)
        .query_series(&SeriesRequest::new(
            MetricKind::CpuUtilization,
            HostSelector::All,
            range(),
            Duration::seconds(60),
        ))
        .await
        .expect("query succeeds");

    assert_eq!(result.len(), 1);
    assert_eq!(result[0].host, HostId::new("web-1"));
    assert_eq!(result[0].kind, MetricKind::CpuUtilization);
    assert_eq!(result[0].points().len(), 2);
    assert!((result[0].latest().expect("has points").value - 0.34).abs() < 1e-9);
}

#[tokio::test]
async fn drops_partial_buckets_and_gaps_but_keeps_the_rest() {
    let server = MockServer::start().await;
    mount(
        &server,
        &response(&[series(
            &[("host.name", json!("web-1"))],
            &[
                json!({"timestamp": 1_742_602_512_000_i64, "value": 0.99, "partial": true}),
                point(1_742_602_572_000, &json!("NaN")),
                point(1_742_602_632_000, &json!(null)),
                point(1_742_602_692_000, &json!(0.34)),
            ],
        )]),
    )
    .await;

    let result = adapter(&server)
        .query_series(&SeriesRequest::new(
            MetricKind::CpuUtilization,
            HostSelector::All,
            range(),
            Duration::seconds(60),
        ))
        .await
        .expect("a gap must not fail the query");

    assert_eq!(
        result[0].points().len(),
        1,
        "only the one usable point survives"
    );
    assert!((result[0].points()[0].value - 0.34).abs() < 1e-9);
}

#[tokio::test]
async fn keeps_the_labels_that_distinguish_series_of_the_same_metric() {
    let server = MockServer::start().await;
    mount(
        &server,
        &response(&[
            series(
                &[
                    ("host.name", json!("web-1")),
                    ("system.filesystem.mountpoint", json!("/")),
                ],
                &[point(1_742_602_572_000, &json!(0.5))],
            ),
            series(
                &[
                    ("host.name", json!("web-1")),
                    ("system.filesystem.mountpoint", json!("/data")),
                ],
                &[point(1_742_602_572_000, &json!(0.9))],
            ),
        ]),
    )
    .await;

    let result = adapter(&server)
        .query_series(&SeriesRequest::new(
            MetricKind::FilesystemUtilization,
            HostSelector::Host(HostId::new("web-1")),
            range(),
            Duration::seconds(60),
        ))
        .await
        .expect("query succeeds");

    assert_eq!(
        result.len(),
        2,
        "two mount points are two series, not an average"
    );
    let mounts: Vec<&str> = result
        .iter()
        .filter_map(|s| {
            s.attributes
                .get("system.filesystem.mountpoint")
                .map(String::as_str)
        })
        .collect();
    assert!(mounts.contains(&"/"));
    assert!(mounts.contains(&"/data"));
    assert!(
        result
            .iter()
            .all(|s| !s.attributes.contains_key("host.name")),
        "the host is a field, not a leftover label"
    );
}

#[tokio::test]
async fn a_series_without_a_host_is_skipped_rather_than_guessed_at() {
    let server = MockServer::start().await;
    mount(
        &server,
        &response(&[series(&[], &[point(1_742_602_572_000, &json!(0.5))])]),
    )
    .await;

    let result = adapter(&server)
        .query_series(&SeriesRequest::new(
            MetricKind::CpuUtilization,
            HostSelector::All,
            range(),
            Duration::seconds(60),
        ))
        .await
        .expect("query succeeds");
    assert!(result.is_empty());
}

#[tokio::test]
async fn builds_a_filter_expression_for_a_single_host() {
    let server = MockServer::start().await;
    mount(&server, &response(&[])).await;

    adapter(&server)
        .query_series(&SeriesRequest::new(
            MetricKind::CpuUtilization,
            HostSelector::Host(HostId::new("web-1")),
            range(),
            Duration::seconds(60),
        ))
        .await
        .expect("query succeeds");

    let body = sent_body(&server).await;
    assert_eq!(spec(&body)["filter"]["expression"], "host.name = 'web-1'");
    assert_eq!(
        spec(&body)["aggregations"][0]["metricName"],
        "system.cpu.utilization"
    );
    assert_eq!(spec(&body)["stepInterval"], 60);
}

#[tokio::test]
async fn escapes_a_quote_in_a_hostname_rather_than_breaking_the_expression() {
    let server = MockServer::start().await;
    mount(&server, &response(&[])).await;

    adapter(&server)
        .query_series(&SeriesRequest::new(
            MetricKind::CpuUtilization,
            HostSelector::Host(HostId::new("o'brien")),
            range(),
            Duration::seconds(60),
        ))
        .await
        .expect("query succeeds");

    let body = sent_body(&server).await;
    assert_eq!(
        spec(&body)["filter"]["expression"],
        r"host.name = 'o\'brien'"
    );
}

#[tokio::test]
async fn selecting_every_host_sends_no_filter_at_all() {
    let server = MockServer::start().await;
    mount(&server, &response(&[])).await;

    adapter(&server)
        .query_series(&SeriesRequest::new(
            MetricKind::CpuUtilization,
            HostSelector::All,
            range(),
            Duration::seconds(60),
        ))
        .await
        .expect("query succeeds");

    assert!(spec(&sent_body(&server).await).get("filter").is_none());
}

#[tokio::test]
async fn selecting_no_hosts_makes_no_request_at_all() {
    let server = MockServer::start().await;
    mount(&server, &response(&[])).await;

    let result = adapter(&server)
        .query_series(&SeriesRequest::new(
            MetricKind::CpuUtilization,
            HostSelector::AnyOf(vec![]),
            range(),
            Duration::seconds(60),
        ))
        .await
        .expect("an empty selection is not an error");

    assert!(result.is_empty());
    assert!(
        server
            .received_requests()
            .await
            .expect("recording")
            .is_empty(),
        "an empty selection matches nothing; asking the backend is pure waste"
    );
}

#[tokio::test]
async fn underscored_naming_changes_both_the_metric_and_its_labels() {
    let server = MockServer::start().await;
    mount(
        &server,
        &response(&[series(
            &[("host_name", json!("web-1"))],
            &[point(1_742_602_572_000, &json!(0.5))],
        )]),
    )
    .await;

    let adapter = SignozQuery::new(
        SignozConfig::new(server.uri(), API_KEY)
            .expect("valid")
            .with_naming(MetricNaming::Underscored),
    )
    .expect("client builds");

    let result = adapter
        .query_series(&SeriesRequest::new(
            MetricKind::CpuUtilization,
            HostSelector::Host(HostId::new("web-1")),
            range(),
            Duration::seconds(60),
        ))
        .await
        .expect("query succeeds");

    let body = sent_body(&server).await;
    assert_eq!(
        spec(&body)["aggregations"][0]["metricName"],
        "system_cpu_utilization"
    );
    assert_eq!(spec(&body)["filter"]["expression"], "host_name = 'web-1'");
    assert_eq!(result[0].host, HostId::new("web-1"));
}

#[tokio::test]
async fn lists_hosts_with_their_os_version_and_newest_heartbeat() {
    let server = MockServer::start().await;
    mount(
        &server,
        &response(&[
            series(
                &[
                    ("host.name", json!("web-1")),
                    ("os.type", json!("linux")),
                    ("service.version", json!("0.1.0")),
                ],
                &[
                    point(1_742_602_572_000, &json!(10.0)),
                    point(1_742_602_632_000, &json!(11.0)),
                ],
            ),
            series(
                &[("host.name", json!("mac-1")), ("os.type", json!("darwin"))],
                &[point(1_742_602_572_000, &json!(3.0))],
            ),
        ]),
    )
    .await;

    let hosts = adapter(&server).list_hosts(range()).await.expect("lists");

    assert_eq!(hosts.len(), 2);
    assert_eq!(hosts[0].id, HostId::new("mac-1"), "hosts come back sorted");
    assert_eq!(hosts[0].os, OsFamily::Darwin);
    assert!(hosts[0].agent_version.is_none());

    assert_eq!(hosts[1].id, HostId::new("web-1"));
    assert_eq!(hosts[1].os, OsFamily::Linux);
    assert_eq!(hosts[1].agent_version.as_deref(), Some("0.1.0"));
    assert_eq!(
        hosts[1].last_heartbeat,
        Some(Utc.timestamp_opt(1_742_602_632, 0).single().expect("valid")),
        "the newest beat, not the first"
    );
}

#[tokio::test]
async fn lists_hosts_at_the_heartbeat_interval_not_the_window_length() {
    let server = MockServer::start().await;
    mount(&server, &response(&[])).await;

    adapter(&server).list_hosts(range()).await.expect("lists");

    // A bucket is timestamped at its start. Querying a 30-minute window in one bucket would put
    // every host's last heartbeat 30 minutes in the past and mark the entire fleet down.
    let body = sent_body(&server).await;
    assert_eq!(
        spec(&body)["stepInterval"],
        30,
        "the step must be the heartbeat interval, not the window"
    );
}

#[tokio::test]
async fn an_overridden_heartbeat_interval_is_the_step_that_gets_sent() {
    let server = MockServer::start().await;
    mount(&server, &response(&[])).await;

    let adapter = SignozQuery::new(
        SignozConfig::new(server.uri(), API_KEY)
            .expect("valid")
            .with_heartbeat_interval(Duration::seconds(10)),
    )
    .expect("client builds");
    adapter.list_hosts(range()).await.expect("lists");

    assert_eq!(spec(&sent_body(&server).await)["stepInterval"], 10);
}

#[tokio::test]
async fn a_heartbeat_query_uses_an_aggregation_valid_for_a_cumulative_sum() {
    let server = MockServer::start().await;
    mount(&server, &response(&[])).await;

    adapter(&server)
        .check_connection()
        .await
        .expect("connected");

    let body = sent_body(&server).await;
    let aggregation = &spec(&body)["aggregations"][0];
    assert_eq!(aggregation["timeAggregation"], "max");
    assert_eq!(aggregation["spaceAggregation"], "max");
}

#[tokio::test]
async fn a_partial_bucket_does_not_make_a_stale_host_look_alive() {
    let server = MockServer::start().await;
    mount(
        &server,
        &response(&[series(
            &[("host.name", json!("web-1"))],
            &[
                point(1_742_602_572_000, &json!(10.0)),
                json!({"timestamp": 1_742_604_000_000_i64, "value": 10.0, "partial": true}),
            ],
        )]),
    )
    .await;

    let hosts = adapter(&server).list_hosts(range()).await.expect("lists");
    assert_eq!(
        hosts[0].last_heartbeat,
        Some(Utc.timestamp_opt(1_742_602_572, 0).single().expect("valid"))
    );
}

#[tokio::test]
async fn one_host_reported_under_two_label_sets_is_listed_once() {
    let server = MockServer::start().await;
    // An agent upgrade mid-window changes service.version, so SigNoz returns two series.
    mount(
        &server,
        &response(&[
            series(
                &[
                    ("host.name", json!("web-1")),
                    ("service.version", json!("0.1.0")),
                ],
                &[point(1_742_602_572_000, &json!(10.0))],
            ),
            series(
                &[
                    ("host.name", json!("web-1")),
                    ("service.version", json!("0.2.0")),
                ],
                &[point(1_742_602_632_000, &json!(1.0))],
            ),
        ]),
    )
    .await;

    let hosts = adapter(&server).list_hosts(range()).await.expect("lists");
    assert_eq!(hosts.len(), 1);
    assert_eq!(
        hosts[0].agent_version.as_deref(),
        Some("0.2.0"),
        "the newer series wins"
    );
}

#[tokio::test]
async fn a_rejected_key_is_reported_as_unauthorised_not_as_a_backend_error() {
    for status in [401, 403] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(status))
            .mount(&server)
            .await;

        let error = adapter(&server)
            .check_connection()
            .await
            .expect_err("should fail");
        assert!(
            matches!(error, CoreError::Unauthorized),
            "{status}: {error}"
        );
    }
}

#[tokio::test]
async fn a_server_error_carries_the_body_because_the_status_alone_says_little() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500).set_body_string("metric not found: typo.metric"))
        .mount(&server)
        .await;

    let error = adapter(&server)
        .check_connection()
        .await
        .expect_err("should fail");
    match error {
        CoreError::Backend(message) => {
            assert!(message.contains("typo.metric"), "{message}");
            assert!(message.contains("500"), "{message}");
        }
        other => panic!("expected a backend error, got {other}"),
    }
}

#[tokio::test]
async fn an_unparseable_body_is_a_backend_error_not_a_panic() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html>proxy error</html>"))
        .mount(&server)
        .await;

    let error = adapter(&server)
        .check_connection()
        .await
        .expect_err("should fail");
    assert!(matches!(error, CoreError::Backend(_)), "{error}");
}

#[tokio::test]
async fn an_unreachable_endpoint_is_distinguished_from_a_backend_error() {
    // Port 1 on loopback refuses connections immediately.
    let adapter =
        SignozQuery::new(SignozConfig::new("http://127.0.0.1:1", API_KEY).expect("valid"))
            .expect("client builds");

    let error = adapter.check_connection().await.expect_err("should fail");
    assert!(
        matches!(error, CoreError::Unreachable(_)),
        "a client that cannot reach the backend should say so, not blame it: {error}"
    );
}

#[tokio::test]
async fn a_redirect_is_refused_rather_than_followed_to_another_host() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("location", "https://evil.example/api/v5/query_range"),
        )
        .mount(&server)
        .await;

    // The key travels in a custom header, and reqwest only strips Authorization across hosts.
    // Following this redirect would hand the credential to evil.example.
    let error = adapter(&server)
        .check_connection()
        .await
        .expect_err("should fail");
    match error {
        CoreError::Backend(message) => {
            assert!(message.contains("evil.example"), "{message}");
            assert!(message.contains("refusing to follow"), "{message}");
        }
        other => panic!("expected a backend error naming the redirect, got {other}"),
    }
}

#[test]
fn the_api_key_is_redacted_from_debug_output() {
    let config = SignozConfig::new("https://eu.signoz.cloud", "super-secret-key").expect("valid");

    // The obvious thing to do with a config that will not work is to log it.
    let rendered = format!("{config:?}");
    assert!(!rendered.contains("super-secret-key"), "{rendered}");
    assert!(rendered.contains("redacted"), "{rendered}");
    assert!(
        rendered.contains("eu.signoz.cloud"),
        "the rest must stay useful: {rendered}"
    );

    let adapter = SignozQuery::new(config).expect("client builds");
    assert!(!format!("{adapter:?}").contains("super-secret-key"));
}

#[test]
fn an_empty_api_key_is_rejected_at_construction() {
    assert!(SignozConfig::new("https://eu.signoz.cloud", "").is_err());
    assert!(SignozConfig::new("https://eu.signoz.cloud", "   ").is_err());
}

#[test]
fn a_base_url_without_a_scheme_is_rejected_at_construction() {
    assert!(SignozConfig::new("eu.signoz.cloud", API_KEY).is_err());
    assert!(SignozConfig::new("https://eu.signoz.cloud", API_KEY).is_ok());
}

#[test]
fn a_trailing_slash_does_not_produce_a_double_slash_in_the_path() {
    let config = SignozConfig::new("https://eu.signoz.cloud/", API_KEY).expect("valid");
    assert_eq!(
        config.query_range_url(),
        "https://eu.signoz.cloud/api/v5/query_range"
    );
}

/// Silences the unused-import warning for `Request`, which documents the recorded type.
#[allow(dead_code)]
fn _recorded_request_type(_: &Request) {}
