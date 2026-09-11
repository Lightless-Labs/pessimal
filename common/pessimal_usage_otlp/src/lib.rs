//! Ships Pessimal's own usage traces to an OTLP/HTTP endpoint.
//!
//! The I/O half of [`pessimal_usage`], kept in its own crate for the same reason
//! `pessimal_query_signoz` is: the domain stays free of reqwest, tokio and TLS, and the crate that
//! is not free of them is the one whose manifest has to argue about providers.
//!
//! The destination is baked in at build time and absent from any build that was not given one, so a
//! source build and the CI simulator build cannot report. See [`destination`].

pub mod destination;
pub mod sink;

pub use destination::{Destination, Secret};
pub use sink::{Diagnostics, DiagnosticsSnapshot, SendFailure, UsageSink};

use pessimal_usage::consent::Consent;

/// Installs rustls's crypto provider, once per process.
///
/// The manifest selects `reqwest/rustls-no-provider`, which means what it says: reqwest installs no
/// provider and panics when a client is built without one. Here that panic would cross UniFFI as an
/// app crash, or take down an agent mid-export, so it is installed rather than left to whichever
/// crate happens to build a client first.
///
/// In the agent this is not merely tidy. That graph enables `ring` **and** `aws-lc-rs` at once, and
/// rustls refuses to pick between two providers from crate features — so without this call the
/// generic builder panics. Mirrors `pessimal_query_signoz::install_crypto_provider`; both being
/// present is harmless, since the second `install_default` simply loses the race and says so.
pub fn install_crypto_provider() {
    static INSTALL: std::sync::Once = std::sync::Once::new();
    INSTALL.call_once(|| {
        // ring, not aws-lc-rs: the clients cross-compile to iOS and Android, where aws-lc-rs is the
        // usual source of build grief.
        let _already_installed = rustls::crypto::ring::default_provider().install_default();
    });
}

/// Builds a sink if, and only if, consent allows one to exist.
///
/// This is the structural half of opt-out. Returning `None` rather than a disabled sink means there
/// is no object in the program that could report: not a flag to forget to check, not a branch to get
/// backwards. A caller that wants to explain the `None` to a user asks
/// [`pessimal_usage::Consent::resolve`] for the reason.
///
/// # Errors
/// Returns the reqwest error if consent is granted but an HTTP client cannot be built.
pub fn sink_if_permitted(
    consent: Consent,
    destination: Option<Destination>,
    resource: pessimal_usage::resource::UsageResource,
) -> Result<Option<UsageSink>, reqwest::Error> {
    match (consent, destination) {
        (Consent::Granted, Some(destination)) => Ok(Some(UsageSink::new(destination, resource)?)),
        _ => Ok(None),
    }
}

/// Whether consent permits reporting at all. A readability alias used at the call sites that read
/// better as a question.
#[must_use]
pub fn consent_permits(consent: Consent) -> bool {
    consent.is_granted()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use pessimal_usage::attribute::{DeviceClass, Platform};
    use pessimal_usage::consent::{Consent, DO_NOT_TRACK, DenialReason};
    use pessimal_usage::resource::{Service, UsageResource};
    use uuid::Uuid;

    use super::*;

    fn resource() -> UsageResource {
        UsageResource::new(
            Service::Client,
            Platform::Ios,
            DeviceClass::Phone,
            Uuid::now_v7(),
            "0.1.0",
            None,
            "18.4",
        )
    }

    fn destination() -> Option<Destination> {
        Destination::new("https://ingest.example.com", "h", "k")
    }

    #[test]
    fn a_denied_consent_yields_no_sink_at_all() {
        for denial in [
            DenialReason::OptedOut,
            DenialReason::DoNotTrack,
            DenialReason::ContinuousIntegration,
            DenialReason::NoDestination,
        ] {
            let sink = sink_if_permitted(Consent::Denied(denial), destination(), resource())
                .expect("no client is built");
            assert!(sink.is_none(), "{denial:?} produced a sink");
        }
    }

    #[test]
    fn a_granted_consent_with_no_destination_still_yields_no_sink() {
        // Belt and braces: `Consent::resolve` already denies when there is no destination, but this
        // function must not be the place that assumes the caller got that right.
        let sink = sink_if_permitted(Consent::Granted, None, resource()).expect("no client");
        assert!(sink.is_none());
    }

    #[test]
    fn a_granted_consent_with_a_destination_yields_one() {
        let sink =
            sink_if_permitted(Consent::Granted, destination(), resource()).expect("client builds");
        assert!(sink.is_some());
    }

    #[test]
    fn the_end_to_end_default_reports_and_do_not_track_does_not() {
        let on = Consent::resolve(false, true, &BTreeMap::new());
        assert!(
            sink_if_permitted(on, destination(), resource())
                .expect("builds")
                .is_some(),
            "opt-out means the default builds a sink"
        );

        let env: BTreeMap<String, String> = [(DO_NOT_TRACK.to_owned(), "1".to_owned())]
            .into_iter()
            .collect();
        let off = Consent::resolve(false, true, &env);
        assert!(
            sink_if_permitted(off, destination(), resource())
                .expect("builds")
                .is_none()
        );
    }

    #[test]
    fn installing_the_crypto_provider_twice_is_fine() {
        install_crypto_provider();
        install_crypto_provider();
    }
}
