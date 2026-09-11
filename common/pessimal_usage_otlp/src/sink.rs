//! Shipping usage traces over OTLP/HTTP.
//!
//! Three rules this module exists to enforce, all of them consequences of what usage reporting is
//! for:
//!
//! 1. **A failure here is never a failure for the caller.** [`UsageSink::send`] returns `()`. An
//!    app that cannot report its own diagnostics is an app that reports its own diagnostics badly,
//!    not a broken app, and a poll must produce the same view whether or not reporting worked.
//! 2. **Failures are still visible.** Swallowing them silently is the irony this milestone would be
//!    embarrassed by, so every outcome lands in [`Diagnostics`], which a settings screen can show.
//! 3. **Nothing is buffered to disk.** Dropping on failure is correct for diagnostics: a retry queue
//!    is a durability promise we do not need and a pile of other people's data we do not want.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use pessimal_usage::resource::UsageResource;
use pessimal_usage::span::UsageTrace;
use reqwest::Client;

use crate::destination::Destination;

/// How long a report may take before it is abandoned.
///
/// Short on purpose, and shorter than any poll interval: a report that has not landed in ten seconds
/// has lost its race with the next one, and holding a request open costs the radio on a phone.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Counts, for a diagnostics surface. Cheap enough to keep always.
///
/// Deliberately not a `tracing` span: the client has no subscriber installed, so `tracing` output
/// would go nowhere, which is the state this whole milestone is fixing.
#[derive(Debug, Default)]
pub struct Diagnostics {
    sent: AtomicU64,
    rejected: AtomicU64,
    failed: AtomicU64,
    spans_reported: AtomicU64,
}

/// A point-in-time copy, for handing across FFI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DiagnosticsSnapshot {
    /// Reports the backend accepted.
    pub sent: u64,
    /// Reports the backend answered, but with a refusal — a 4xx credential or shape problem.
    pub rejected: u64,
    /// Reports that never got an answer: unreachable, timed out, TLS refused.
    pub failed: u64,
    /// Spans in accepted reports. The number worth comparing against what the backend shows.
    pub spans_reported: u64,
}

impl Diagnostics {
    #[must_use]
    pub fn snapshot(&self) -> DiagnosticsSnapshot {
        DiagnosticsSnapshot {
            sent: self.sent.load(Ordering::Relaxed),
            rejected: self.rejected.load(Ordering::Relaxed),
            failed: self.failed.load(Ordering::Relaxed),
            spans_reported: self.spans_reported.load(Ordering::Relaxed),
        }
    }
}

/// Why a report did not land. Kept coarse, and never carrying a response body: the body is the one
/// place a backend would hand us text we have no business storing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendFailure {
    /// No answer: connection refused, timed out, DNS, TLS.
    Unreachable,
    /// The credential was refused.
    Unauthorized,
    /// The backend answered with a refusal other than 401/403.
    Rejected(u16),
}

/// Ships encoded traces to one destination.
///
/// Construct it only once consent is granted — see [`crate::consent_permits`]. That is the
/// structural half of opt-out: a sink that does not exist cannot report, whereas a sink that checks
/// a flag before each send is one missing check away from reporting anyway.
#[derive(Debug, Clone)]
pub struct UsageSink {
    destination: Destination,
    resource: UsageResource,
    http: Client,
    diagnostics: Arc<Diagnostics>,
}

impl UsageSink {
    /// # Errors
    /// Returns the reqwest error if an HTTP client cannot be built, which in practice means a
    /// platform without a usable TLS stack.
    pub fn new(destination: Destination, resource: UsageResource) -> Result<Self, reqwest::Error> {
        // The manifest selects `reqwest/rustls-no-provider`, so nothing installs a provider unless
        // we do. On the client that is belt and braces, since `ring` is the only provider in the
        // graph. In the *agent* it is load-bearing: that graph enables both `ring` and `aws-lc-rs`
        // at once — `opentelemetry-otlp`'s `reqwest-rustls` and `tls-ring` features, on one line in
        // the workspace manifest — and rustls refuses to choose between two, panicking with
        // "Could not automatically determine the process-level CryptoProvider". Measured, not
        // guessed: see step 4 of docs/plans/2026-09-11-m8-usage-reporting.md.
        crate::install_crypto_provider();

        let http = Client::builder()
            .timeout(REQUEST_TIMEOUT)
            // The ingestion key travels in a custom header, and reqwest only strips `Authorization`
            // across a host change — a custom header would be replayed to wherever a redirect
            // points. An ingestion endpoint has no reason to redirect.
            .redirect(reqwest::redirect::Policy::none())
            .build()?;

        Ok(Self {
            destination,
            resource,
            http,
            diagnostics: Arc::new(Diagnostics::default()),
        })
    }

    #[must_use]
    pub fn diagnostics(&self) -> DiagnosticsSnapshot {
        self.diagnostics.snapshot()
    }

    /// Sends one report, and returns nothing.
    ///
    /// Rule 1 of this module, in the signature. An outcome is recorded either way; a caller that
    /// wants to know consults [`UsageSink::diagnostics`], which is a diagnostics question and not a
    /// control-flow one.
    pub async fn send(&self, traces: &[UsageTrace]) {
        let _outcome: Result<(), SendFailure> = self.try_send(traces).await;
    }

    /// The same work, with the outcome returned. For tests, and for a settings screen that wants to
    /// report the result of an explicit "send a test report".
    ///
    /// # Errors
    /// Returns [`SendFailure`] when the report did not land: [`SendFailure::Unreachable`] for no
    /// answer at all, [`SendFailure::Unauthorized`] for a refused credential, and
    /// [`SendFailure::Rejected`] with the status for any other refusal. A caller on the poll path
    /// should use [`UsageSink::send`] instead, which cannot fail by construction.
    pub async fn try_send(&self, traces: &[UsageTrace]) -> Result<(), SendFailure> {
        let span_count: u64 = traces.iter().map(|trace| trace.spans.len() as u64).sum();
        if span_count == 0 {
            // Not a failure, and not worth a request. An empty report is a valid OTLP body, which
            // makes this easy to get wrong.
            return Ok(());
        }

        let body = pessimal_usage::export_trace_request(&self.resource, traces);
        let response = self
            .http
            .post(self.destination.traces_url())
            .header(
                self.destination.header_name(),
                self.destination.key().expose(),
            )
            .json(&body)
            .send()
            .await;

        match response {
            Err(_) => {
                // The error is not recorded or logged. reqwest's `Display` includes the URL, and
                // while this particular URL is ours, a habit of logging transport errors is how the
                // operator's endpoint ends up in a log the next time this code is copied.
                self.diagnostics.failed.fetch_add(1, Ordering::Relaxed);
                Err(SendFailure::Unreachable)
            }
            Ok(response) => {
                let status = response.status();
                if status.is_success() {
                    self.diagnostics.sent.fetch_add(1, Ordering::Relaxed);
                    self.diagnostics
                        .spans_reported
                        .fetch_add(span_count, Ordering::Relaxed);
                    Ok(())
                } else if status.as_u16() == 401 || status.as_u16() == 403 {
                    self.diagnostics.rejected.fetch_add(1, Ordering::Relaxed);
                    Err(SendFailure::Unauthorized)
                } else {
                    self.diagnostics.rejected.fetch_add(1, Ordering::Relaxed);
                    Err(SendFailure::Rejected(status.as_u16()))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use pessimal_usage::attribute::{BackendKind, DeviceClass, Outcome, Platform};
    use pessimal_usage::resource::Service;
    use pessimal_usage::span::{TraceId, UsageSpan};
    use uuid::Uuid;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    fn resource() -> UsageResource {
        UsageResource::new(
            Service::Client,
            Platform::Ios,
            DeviceClass::Phone,
            Uuid::now_v7(),
            "0.1.0",
            Some(26),
            "18.4",
        )
    }

    fn one_trace() -> UsageTrace {
        let now = Utc::now();
        let mut trace = UsageTrace::new(TraceId::new());
        trace.record(
            UsageSpan::ClientPoll {
                outcome: Outcome::Ok,
                backend: BackendKind::Signoz,
                hosts: 1,
                rules: 0,
                alerts_firing: 0,
            },
            None,
            now,
            now,
        );
        trace
    }

    fn sink_for(server: &MockServer) -> UsageSink {
        let destination = Destination::new(&server.uri(), "signoz-ingestion-key", "key")
            .expect("a wiremock uri is loopback http, which is permitted");
        UsageSink::new(destination, resource()).expect("client builds")
    }

    #[tokio::test]
    async fn a_report_goes_to_v1_traces_with_the_credential_header() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/traces"))
            .and(header("signoz-ingestion-key", "key"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .expect(1)
            .mount(&server)
            .await;

        let sink = sink_for(&server);
        assert_eq!(sink.try_send(&[one_trace()]).await, Ok(()));
        assert_eq!(sink.diagnostics().sent, 1);
        assert_eq!(sink.diagnostics().spans_reported, 1);
    }

    #[tokio::test]
    async fn an_empty_report_is_not_sent_at_all() {
        let server = MockServer::start().await;
        // No mock is mounted, so any request at all would fail the test.
        let sink = sink_for(&server);
        assert_eq!(sink.try_send(&[]).await, Ok(()));
        assert_eq!(
            sink.try_send(&[UsageTrace::new(TraceId::new())]).await,
            Ok(()),
            "a trace with no spans is also nothing to send"
        );
        assert_eq!(sink.diagnostics().sent, 0);
    }

    #[tokio::test]
    async fn a_refused_credential_is_recorded_as_rejected() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;

        let sink = sink_for(&server);
        assert_eq!(
            sink.try_send(&[one_trace()]).await,
            Err(SendFailure::Unauthorized)
        );
        assert_eq!(sink.diagnostics().rejected, 1);
        assert_eq!(sink.diagnostics().sent, 0);
        assert_eq!(
            sink.diagnostics().spans_reported,
            0,
            "spans are only counted once a backend accepts them"
        );
    }

    #[tokio::test]
    async fn a_server_error_is_recorded_with_its_status() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;

        let sink = sink_for(&server);
        assert_eq!(
            sink.try_send(&[one_trace()]).await,
            Err(SendFailure::Rejected(503))
        );
        assert_eq!(sink.diagnostics().rejected, 1);
    }

    #[tokio::test]
    async fn an_unreachable_endpoint_fails_as_data_rather_than_panicking() {
        // Port 9 is discard: it refuses rather than hanging.
        let destination =
            Destination::new("http://127.0.0.1:9", "h", "k").expect("loopback http is permitted");
        let sink = UsageSink::new(destination, resource()).expect("client builds");
        assert_eq!(
            sink.try_send(&[one_trace()]).await,
            Err(SendFailure::Unreachable)
        );
        assert_eq!(sink.diagnostics().failed, 1);

        // And the infallible entry point returns nothing at all, which is the contract the FFI
        // session relies on.
        sink.send(&[one_trace()]).await;
        assert_eq!(sink.diagnostics().failed, 2);
    }

    #[tokio::test]
    async fn the_body_is_what_the_encoder_produced() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let sink = sink_for(&server);
        let trace = one_trace();
        let expected =
            pessimal_usage::export_trace_request(&resource(), std::slice::from_ref(&trace));
        sink.try_send(&[trace]).await.expect("accepted");

        let requests = server.received_requests().await.expect("recorded");
        let body: serde_json::Value =
            serde_json::from_slice(&requests[0].body).expect("valid json");
        // The resource carries a fresh instance id per `resource()` call, so compare the spans.
        assert_eq!(
            body["resourceSpans"][0]["scopeSpans"][0]["spans"],
            expected["resourceSpans"][0]["scopeSpans"][0]["spans"]
        );
    }

    #[tokio::test]
    async fn a_redirect_is_not_followed_so_the_key_is_not_replayed() {
        let elsewhere = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&elsewhere)
            .await;

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(307).insert_header(
                "location",
                format!("{}/v1/traces", elsewhere.uri()).as_str(),
            ))
            .mount(&server)
            .await;

        let sink = sink_for(&server);
        assert_eq!(
            sink.try_send(&[one_trace()]).await,
            Err(SendFailure::Rejected(307)),
            "a redirect is an answer we refuse, not one we follow"
        );
    }
}
