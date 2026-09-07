//! Deserialisation must be held to the same invariants the constructors enforce.
//!
//! Every type here has private fields and a validating constructor, and every one of them derived
//! `Deserialize` — which walks straight around that constructor. The failures were not theoretical:
//! a restored policy could call a host down before calling it stale, and a restored series could
//! return the wrong sample from `latest()` without erroring, feeding a wrong value to the alert
//! evaluator.
//!
//! Each test round-trips a valid value, corrupts one field on the wire, and requires the
//! deserialiser to refuse it.

#![allow(
    clippy::float_cmp,
    reason = "fixture values round-trip exactly; no arithmetic involved"
)]

use chrono::{DateTime, Duration, Utc};
use pessimal_core::{
    AlertRule, Comparator, HostSelector, LivenessPolicy, MetricKind, MetricPoint, MetricSeries,
    TimeRange, Urn,
};
use serde_json::{Value, json};

fn at(offset: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(1_760_000_000 + offset, 0).expect("valid timestamp")
}

fn wire<T: serde::Serialize>(value: &T) -> Value {
    serde_json::to_value(value).expect("serialisable")
}

#[test]
fn a_valid_policy_still_round_trips() {
    let policy = LivenessPolicy::new(Duration::seconds(30), 3, 5).expect("valid");
    let restored: LivenessPolicy = serde_json::from_value(wire(&policy)).expect("round trip");
    assert_eq!(restored, policy);
}

#[test]
fn a_policy_whose_thresholds_are_inverted_is_refused() {
    let mut v = wire(&LivenessPolicy::new(Duration::seconds(30), 3, 5).expect("valid"));
    v["stale_after_intervals"] = json!(9);
    v["down_after_intervals"] = json!(1);

    // Accepting this gives stale=270s and down=30s: a host is called down before it is called
    // stale, inverting the whole liveness model with nothing to report it.
    assert!(serde_json::from_value::<LivenessPolicy>(v).is_err());
}

#[test]
fn a_policy_with_a_zero_interval_is_refused() {
    let mut v = wire(&LivenessPolicy::new(Duration::seconds(30), 3, 5).expect("valid"));
    v["heartbeat_interval"] = json!([0, 0]);
    assert!(serde_json::from_value::<LivenessPolicy>(v).is_err());
}

#[test]
fn a_time_range_that_ends_before_it_starts_is_refused() {
    let mut v = wire(&TimeRange::new(at(0), at(60)).expect("valid"));
    let (start, end) = (v["start"].clone(), v["end"].clone());
    v["start"] = end;
    v["end"] = start;
    assert!(serde_json::from_value::<TimeRange>(v).is_err());
}

#[test]
fn restored_series_points_are_sorted_like_constructed_ones() {
    let series = MetricSeries::new(
        "web-1".into(),
        MetricKind::CpuUtilization,
        vec![MetricPoint::new(at(0), 0.1), MetricPoint::new(at(120), 0.9)],
    );
    let mut v = wire(&series);
    v["points"]
        .as_array_mut()
        .expect("points is an array")
        .reverse();

    let restored: MetricSeries = serde_json::from_value(v).expect("restores");
    assert_eq!(
        restored.latest().expect("has points").value,
        0.9,
        "latest() reads the last element, so an unsorted restore returns the wrong sample silently"
    );
    assert_eq!(restored, series);
}

#[test]
fn restored_series_attributes_survive() {
    let mut attributes = std::collections::BTreeMap::new();
    attributes.insert(
        "system.filesystem.mountpoint".to_owned(),
        "/data".to_owned(),
    );
    let series = MetricSeries::new(
        "web-1".into(),
        MetricKind::FilesystemUtilization,
        vec![MetricPoint::new(at(0), 0.5)],
    )
    .with_attributes(attributes.clone());

    let restored: MetricSeries = serde_json::from_value(wire(&series)).expect("restores");
    assert_eq!(restored.attributes, attributes);
}

#[test]
fn a_urn_with_an_empty_segment_is_refused() {
    let mut v = wire(&Urn::new("prod", "alerts", "rule").expect("valid"));
    v["environment"] = json!("");

    // `pessimal::::alerts::rule::<uuid>` does not parse back from its own Display.
    assert!(serde_json::from_value::<Urn>(v).is_err());
}

#[test]
fn a_urn_carrying_the_separator_is_refused() {
    let mut v = wire(&Urn::new("prod", "alerts", "rule").expect("valid"));
    v["service"] = json!("a::b");
    assert!(serde_json::from_value::<Urn>(v).is_err());
}

fn a_rule() -> AlertRule {
    AlertRule::new(
        "prod",
        "CPU hot",
        MetricKind::CpuUtilization,
        Comparator::GreaterThan,
        0.9,
        Duration::seconds(300),
    )
    .expect("valid")
}

#[test]
fn a_rule_with_an_empty_name_is_refused() {
    let mut v = wire(&a_rule());
    v["name"] = json!("   ");
    assert!(serde_json::from_value::<AlertRule>(v).is_err());
}

#[test]
fn a_rule_with_a_negative_dwell_is_refused() {
    let mut v = wire(&a_rule());
    v["for_duration"] = json!([-5, 0]);
    assert!(serde_json::from_value::<AlertRule>(v).is_err());
}

#[test]
fn a_rule_whose_dwell_outruns_the_calendar_is_refused() {
    // chrono's `TimeDelta` serde impl is a `[secs, nanos]` pair bounded only by `TimeDelta`'s own
    // range, which is ~1100x what a timestamp can hold. A dwell from that band deserialises
    // cleanly and then overflows the first time anything computes `since + for_duration`.
    let mut v = wire(&a_rule());
    v["for_duration"] = json!([10_000_000_000_000_i64, 0]);
    assert!(serde_json::from_value::<AlertRule>(v).is_err());

    let mut boundary = wire(&a_rule());
    boundary["for_duration"] = json!([AlertRule::MAX_FOR_DURATION.num_seconds(), 0]);
    assert!(
        serde_json::from_value::<AlertRule>(boundary).is_ok(),
        "the ceiling itself must survive a round trip, or the bound rejects rules we mint"
    );
}

#[test]
fn a_time_range_whose_window_predates_representable_time_is_refused() {
    assert!(TimeRange::ending_at(at(0), Duration::seconds(10_000_000_000_000)).is_err());
}

#[test]
fn a_restored_rule_keeps_its_identity_and_its_settings() {
    let rule = a_rule()
        .with_selector(HostSelector::Host("web-1".into()))
        .disabled();
    let restored: AlertRule = serde_json::from_value(wire(&rule)).expect("round trip");

    assert_eq!(
        restored.id(),
        rule.id(),
        "a fresh id would orphan every alert evaluation keyed to this rule"
    );
    assert_eq!(restored.selector, rule.selector);
    assert!(!restored.enabled);
    assert_eq!(restored, rule);
}
