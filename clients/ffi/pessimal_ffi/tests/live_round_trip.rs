//! Opt-in end-to-end verification: a real backend, through the adapter, through the fold, into the
//! records Swift actually receives.
//!
//! This is the only test in the repo that exercises the whole read path at once. The crate-level
//! tests prove each link in isolation against doubles; this proves they compose against a backend
//! that was never written to agree with us.
//!
//! Skipped unless both variables are set, so CI never needs a credential:
//!
//! ```sh
//! PESSIMAL_LIVE_SIGNOZ_URL=https://your-org.eu2.signoz.cloud \
//! PESSIMAL_LIVE_SIGNOZ_KEY=... \
//!   cargo test -p pessimal_ffi --test live_round_trip -- --nocapture
//! ```
//!
//! It asserts the shape of the answer, never its values: whether a host is busy right now is not
//! something a test can know.

use std::sync::Arc;

use chrono::Utc;
use pessimal_client_core::config::{FleetConfig, PollTuning};
use pessimal_client_core::fold::FleetState;
use pessimal_client_core::gather::poll_once;
use pessimal_core::{LivenessPolicy, TelemetryQuery};
use pessimal_query_signoz::{SignozConfig, SignozQuery};

fn live() -> Option<Arc<dyn TelemetryQuery>> {
    let url = std::env::var("PESSIMAL_LIVE_SIGNOZ_URL").ok()?;
    let key = std::env::var("PESSIMAL_LIVE_SIGNOZ_KEY").ok()?;
    let policy = LivenessPolicy::default();
    let config = SignozConfig::for_policy(url, key, &policy).expect("valid config");
    Some(Arc::new(SignozQuery::new(config).expect("client builds")))
}

#[tokio::test]
async fn a_real_backend_folds_into_a_renderable_view() {
    let Some(query) = live() else {
        eprintln!("skipped: set PESSIMAL_LIVE_SIGNOZ_URL and PESSIMAL_LIVE_SIGNOZ_KEY");
        return;
    };

    let policy = LivenessPolicy::default();
    let tuning = PollTuning::from_liveness(policy);
    let config = FleetConfig::new("live-verify", tuning).expect("valid config");
    let state = FleetState::new("SigNoz");
    let now = Utc::now();

    let update = poll_once(query.as_ref(), &config, &state, now)
        .await
        .expect("poll_once should not error; backend failures arrive inside the view");

    let view = &update.view;
    println!(
        "LIVE: backend={} hosts={}",
        view.backend_name,
        view.hosts.len()
    );
    for host in &view.hosts {
        println!(
            "LIVE:   {} liveness={:?} severity={:?} metrics={}",
            host.id,
            host.liveness,
            host.severity,
            host.metrics.len()
        );
        for metric in &host.metrics {
            println!(
                "LIVE:     {:<40} availability={:?} latest={:?}",
                metric.kind.otel_name(),
                metric.availability,
                metric.latest
            );
        }
    }

    // A failed poll is a legitimate outcome and must not be read as an empty fleet, so the
    // assertion is on the failure channel rather than on finding hosts. The fold records the worst
    // failure on the freshness inputs, which is what the UI banner reads.
    assert!(
        view.freshness.last_failure.is_none(),
        "the live backend reported a failure: {:?}",
        view.freshness.last_failure
    );

    // Whatever the backend said, the fold must produce something the apps can draw.
    assert_eq!(view.backend_name, "SigNoz");

    if view.hosts.is_empty() {
        eprintln!(
            "note: no agent is reporting to this instance; the fold was exercised but not the roster"
        );
        return;
    }

    // With a roster, every host must carry a decided liveness and at least one metric row --
    // a host with no rows at all would render as an empty card.
    for host in &view.hosts {
        assert!(!host.metrics.is_empty(), "{} has no metric rows", host.id);
    }
}
