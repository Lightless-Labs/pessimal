//! Where usage reports go, and where that comes from.
//!
//! Two routes in, one type out, matching how the sibling projects already do this rather than
//! inventing a third way:
//!
//! **The clients** get it over FFI. The release build passes `--action_env=SIGNOZ_OTLP_ENDPOINT=…`
//! and `--action_env=SIGNOZ_OTLP_INGESTION_KEY=…`, a `genrule` expands those into a plist merged
//! into the bundle, and Swift reads them from `Bundle.main.infoDictionary` and hands them across.
//! That is kumbaya's and phil-connors' mechanism verbatim; both ship it, and it keeps the credential
//! out of Rust compile units entirely — no build-time env plumbing, nothing to gitignore, no risk of
//! a key reaching a public repository through a tracked file.
//!
//! **The agent** gets it from [`Destination::from_build`], which reads `option_env!`, because a
//! binary has no bundle to read a plist from. A plain `cargo build` sets neither, so it has nowhere
//! to report.
//!
//! Either way the "test builds cannot phone home" guarantee is structural rather than a runtime
//! check: a `genrule` with an unset variable expands to an empty string, `option_env!` yields `None`,
//! and [`Destination::new`] treats both as no destination at all.

use std::fmt;

/// The env var carrying the OTLP/HTTP base URL, e.g. `https://ingest.eu2.signoz.cloud`.
///
/// Named for the Doppler secret in `prd_ios_deployment` rather than prefixed `PESSIMAL_`, so the
/// release script is a pass-through and there is no renaming step to get wrong.
pub const ENDPOINT_VAR: &str = "SIGNOZ_OTLP_ENDPOINT";
/// The env var carrying the ingestion credential.
pub const KEY_VAR: &str = "SIGNOZ_OTLP_INGESTION_KEY";

/// The header SigNoz wants an ingestion key in.
///
/// Not a build-time variable: both sibling projects hardcode this exact string, in their Rust
/// servers and their Swift clients, and they are in production against the same SigNoz. Note it is
/// *not* the `signoz-access-token` that `pessimal_agent_core`'s preset sends — that one is for the
/// operator's own collector, which is a different endpoint with a different credential.
///
/// A self-hosted deployment that needs something else can still pass it to [`Destination::new`].
pub const INGESTION_KEY_HEADER: &str = "signoz-ingestion-key";

/// A credential that does not appear in logs.
///
/// `Debug` is the realistic leak: a `tracing::debug!(?destination)` or a `dbg!` in a test is how an
/// ingestion key ends up in a CI transcript. The inner value is reachable only through
/// [`Secret::expose`], which reads like what it is at the call site.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

/// A complete, validated destination.
#[derive(Clone, PartialEq, Eq)]
pub struct Destination {
    endpoint: String,
    header_name: String,
    key: Secret,
}

impl fmt::Debug for Destination {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Destination")
            .field("endpoint", &self.endpoint)
            .field("header_name", &self.header_name)
            .field("key", &self.key)
            .finish()
    }
}

impl Destination {
    /// The destination this *binary* was built with, or `None` if it was built without one.
    ///
    /// For the agent. The clients take the same values from their bundle's plist and pass them to
    /// [`Destination::new`] across FFI instead — an app has a bundle to read, a daemon does not.
    ///
    /// `None` is the expected answer for every build except an official release, and
    /// `pessimal_usage::Consent` turns it into `DenialReason::NoDestination` rather than an error:
    /// a build that cannot report is not a build that is broken.
    #[must_use]
    pub fn from_build() -> Option<Self> {
        Self::new(
            option_env!("SIGNOZ_OTLP_ENDPOINT")?,
            INGESTION_KEY_HEADER,
            option_env!("SIGNOZ_OTLP_INGESTION_KEY")?,
        )
    }

    /// A destination from the values an app found in its bundle.
    ///
    /// The convenience the FFI layer actually calls: it has two strings from `infoDictionary`, both
    /// possibly empty because the `genrule` expands an unset variable to `""`, and it wants either a
    /// destination or a clean `None`.
    #[must_use]
    pub fn from_bundle(endpoint: &str, key: &str) -> Option<Self> {
        Self::new(endpoint, INGESTION_KEY_HEADER, key)
    }

    /// Validates the three parts. `None` if any is unusable.
    ///
    /// An empty string is treated as absent, because that is what a half-filled env file produces
    /// and it should behave like no destination rather than like a destination that always fails.
    /// A plain-`http` endpoint is accepted only for loopback: a credential must not cross a network
    /// in clear text, but a developer pointing at a local collector should not have to fight this.
    #[must_use]
    pub fn new(endpoint: &str, header_name: &str, key: &str) -> Option<Self> {
        let endpoint = endpoint.trim().trim_end_matches('/');
        let header_name = header_name.trim();
        let key = key.trim();
        if endpoint.is_empty() || header_name.is_empty() || key.is_empty() {
            return None;
        }
        if !Self::is_acceptable_endpoint(endpoint) {
            return None;
        }
        Some(Self {
            endpoint: endpoint.to_owned(),
            header_name: header_name.to_owned(),
            key: Secret::new(key),
        })
    }

    fn is_acceptable_endpoint(endpoint: &str) -> bool {
        if let Some(rest) = endpoint.strip_prefix("https://") {
            return !rest.is_empty();
        }
        if let Some(rest) = endpoint.strip_prefix("http://") {
            return is_loopback_authority(rest);
        }
        false
    }

    /// The signal-specific path. Traces only, for now — a metrics path would be `/v1/metrics`, and
    /// posting traces to it is a 404 rather than anything informative.
    #[must_use]
    pub fn traces_url(&self) -> String {
        format!("{}/v1/traces", self.endpoint)
    }

    #[must_use]
    pub fn header_name(&self) -> &str {
        &self.header_name
    }

    #[must_use]
    pub fn key(&self) -> &Secret {
        &self.key
    }
}

/// Whether an authority is loopback and nothing else.
///
/// A prefix test is not enough: `localhost.acme.corp` starts with `localhost` and resolves to
/// somebody else's machine, so the host must *end* after the loopback name — at a port, a path, or
/// the end of the string.
fn is_loopback_authority(authority: &str) -> bool {
    const LOOPBACK: [&str; 3] = ["localhost", "127.0.0.1", "[::1]"];
    LOOPBACK.iter().any(|host| {
        authority
            .strip_prefix(host)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with(':') || rest.starts_with('/'))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_complete_destination_builds() {
        let destination = Destination::new(
            "https://ingest.example.com:443",
            "signoz-ingestion-key",
            "k",
        )
        .expect("valid");
        assert_eq!(
            destination.traces_url(),
            "https://ingest.example.com:443/v1/traces"
        );
    }

    #[test]
    fn a_trailing_slash_does_not_double_up_the_path() {
        let destination = Destination::new("https://ingest.example.com/", "h", "k").expect("valid");
        assert_eq!(
            destination.traces_url(),
            "https://ingest.example.com/v1/traces"
        );
    }

    #[test]
    fn any_missing_part_means_no_destination() {
        assert!(Destination::new("", "h", "k").is_none());
        assert!(Destination::new("https://e", "", "k").is_none());
        assert!(Destination::new("https://e", "h", "").is_none());
        // Whitespace-only is the realistic shape of a half-filled env file.
        assert!(Destination::new("  ", "h", "k").is_none());
        assert!(Destination::new("https://e", "h", "   ").is_none());
    }

    #[test]
    fn plain_http_is_refused_except_on_loopback() {
        assert!(Destination::new("http://ingest.example.com", "h", "k").is_none());
        assert!(Destination::new("http://localhost:4318", "h", "k").is_some());
        assert!(Destination::new("http://127.0.0.1:4318", "h", "k").is_some());
        assert!(Destination::new("http://[::1]:4318", "h", "k").is_some());
        // Not a scheme we know, so not acceptable.
        assert!(Destination::new("ingest.example.com", "h", "k").is_none());
        assert!(Destination::new("grpc://ingest.example.com", "h", "k").is_none());
    }

    #[test]
    fn a_hostname_that_merely_starts_like_loopback_is_still_refused() {
        // `localhost.acme.corp` resolves to someone else's machine, so a credential must not cross
        // the network to it in clear text. A prefix check would have let all of these through.
        for impostor in [
            "http://localhost.acme.corp",
            "http://127.0.0.1.acme.corp",
            "http://localhostile.example",
        ] {
            assert!(
                Destination::new(impostor, "h", "k").is_none(),
                "accepted {impostor}"
            );
        }
        // The genuine articles, with and without a port or path, still pass.
        for loopback in [
            "http://localhost",
            "http://localhost:4318",
            "http://127.0.0.1:4318/",
            "http://[::1]:4318",
        ] {
            assert!(
                Destination::new(loopback, "h", "k").is_some(),
                "refused {loopback}"
            );
        }
    }

    #[test]
    fn the_key_is_redacted_in_debug_output() {
        let destination =
            Destination::new("https://ingest.example.com", "h", "super-secret").expect("valid");
        let rendered = format!("{destination:?}");
        assert!(
            !rendered.contains("super-secret"),
            "the key appeared in Debug output: {rendered}"
        );
        assert!(rendered.contains("<redacted>"));
        // The endpoint is not a secret and is worth seeing when diagnosing.
        assert!(rendered.contains("ingest.example.com"));
    }

    #[test]
    fn a_bundle_with_unset_values_yields_no_destination() {
        // The `genrule` expands an unset `--action_env` to an empty string, so this is the exact
        // shape a simulator or local build produces. It must be "no destination", not a destination
        // that fails every send.
        assert!(Destination::from_bundle("", "").is_none());
        assert!(Destination::from_bundle("https://ingest.eu2.signoz.cloud", "").is_none());
        assert!(Destination::from_bundle("", "key").is_none());
    }

    #[test]
    fn a_bundle_with_both_values_reports_to_signozs_ingestion_header() {
        let destination = Destination::from_bundle("https://ingest.eu2.signoz.cloud", "key")
            .expect("both values present");
        assert_eq!(destination.header_name(), "signoz-ingestion-key");
        assert_eq!(
            destination.traces_url(),
            "https://ingest.eu2.signoz.cloud/v1/traces"
        );
    }

    #[test]
    fn the_ingestion_header_is_not_the_operators_access_token() {
        // `pessimal_agent_core`'s SigNoz preset sends `signoz-access-token` to the *operator's*
        // collector. Confusing the two is a silent non-delivery, so the distinction is asserted.
        assert_ne!(INGESTION_KEY_HEADER, "signoz-access-token");
        assert_eq!(INGESTION_KEY_HEADER, "signoz-ingestion-key");
    }

    #[test]
    fn a_secret_is_redacted_on_its_own_too() {
        assert_eq!(format!("{:?}", Secret::new("abc")), "<redacted>");
        assert_eq!(Secret::new("abc").expose(), "abc");
    }
}
