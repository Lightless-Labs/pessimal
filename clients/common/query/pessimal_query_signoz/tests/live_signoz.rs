//! Opt-in verification against a real SigNoz instance.
//!
//! Everything else in this crate's tests runs against a mock whose response bodies were derived
//! from SigNoz's own Go types. That makes the *structure* authoritative but says nothing about
//! *behaviour*: which labels are actually populated, whether a non-finite value really arrives as a
//! string, whether the first bucket really is partial, and whether the aggregations we send are
//! accepted at all.
//!
//! These tests answer that, and are skipped unless both variables are set, so CI never needs a
//! credential:
//!
//! ```sh
//! PESSIMAL_LIVE_SIGNOZ_URL=https://your-org.eu2.signoz.cloud \
//! PESSIMAL_LIVE_SIGNOZ_KEY=... \
//!   cargo test -p pessimal_query_signoz --test live_signoz -- --nocapture --test-threads=1
//! ```
//!
//! Never hardcode a key here. The whole point of the gate is that the secret lives in the
//! environment of whoever is running it.

use chrono::{Duration, Utc};
use pessimal_core::{HostSelector, MetricKind, SeriesRequest, TelemetryQuery, TimeRange};
use pessimal_query_signoz::{MetricNaming, SignozConfig, SignozQuery};

/// Returns the configured adapter, or `None` when the environment is not set up.
fn live() -> Option<SignozQuery> {
    let url = std::env::var("PESSIMAL_LIVE_SIGNOZ_URL").ok()?;
    let key = std::env::var("PESSIMAL_LIVE_SIGNOZ_KEY").ok()?;
    let naming = match std::env::var("PESSIMAL_LIVE_SIGNOZ_NAMING").as_deref() {
        Ok("underscored") => MetricNaming::Underscored,
        _ => MetricNaming::Dotted,
    };
    Some(
        SignozQuery::new(
            SignozConfig::new(url, key)
                .expect("a live URL and key")
                .with_naming(naming),
        )
        .expect("client builds"),
    )
}

macro_rules! live_or_skip {
    () => {
        match live() {
            Some(adapter) => adapter,
            None => {
                eprintln!("skipped: set PESSIMAL_LIVE_SIGNOZ_URL and PESSIMAL_LIVE_SIGNOZ_KEY");
                return;
            }
        }
    };
}

#[tokio::test]
async fn the_endpoint_answers_and_the_key_is_accepted() {
    let adapter = live_or_skip!();
    adapter
        .check_connection()
        .await
        .expect("check_connection should succeed against a live instance");
    println!(
        "LIVE: check_connection ok, backend = {}",
        adapter.backend_name()
    );
}

#[tokio::test]
async fn the_aggregations_we_send_are_accepted_for_every_modelled_metric() {
    let adapter = live_or_skip!();
    let range = TimeRange::ending_at(Utc::now(), Duration::minutes(30)).expect("valid");

    // A rejected aggregation is the failure mode that would break the settings screen's "test
    // connection" without breaking anything else, so every metric is exercised rather than one.
    let mut rejected = Vec::new();
    for metric in MetricKind::ALL {
        let request = SeriesRequest::new(metric, HostSelector::All, range, Duration::seconds(60));
        match adapter.query_series(&request).await {
            Ok(series) => println!(
                "LIVE: {:<40} ok, {} series",
                metric.otel_name(),
                series.len()
            ),
            Err(error) => {
                println!("LIVE: {:<40} REJECTED: {error}", metric.otel_name());
                rejected.push((metric, error.to_string()));
            }
        }
    }
    assert!(rejected.is_empty(), "the backend rejected: {rejected:?}");
}

#[tokio::test]
async fn list_hosts_parses_whatever_the_instance_returns() {
    let adapter = live_or_skip!();
    let range = TimeRange::ending_at(Utc::now(), Duration::minutes(30)).expect("valid");

    let hosts = adapter
        .list_hosts(range)
        .await
        .expect("list_hosts should not error");
    println!("LIVE: list_hosts returned {} host(s)", hosts.len());
    for host in &hosts {
        println!(
            "LIVE:   host os={} version={:?} last_heartbeat={:?}",
            host.os, host.agent_version, host.last_heartbeat
        );
    }
    // An empty roster is a pass: it means no Pessimal agent is reporting here, which is a fact
    // about the instance and not a defect in the adapter.
}
