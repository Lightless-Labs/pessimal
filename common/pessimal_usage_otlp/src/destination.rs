//! Where usage reports go, and where that comes from.
//!
//! The destination is *baked into the build*, never configured at runtime. That is what makes the
//! "test builds cannot phone home" guarantee structural: a `cargo build` from source, and the CI
//! simulator build, are given no endpoint and so have nowhere to report. The alternative — a runtime
//! default compiled in as a literal — would put the credential in a public repository.
//!
//! The values arrive through `option_env!`, fed by a `rustc_env_files` entry under Bazel and by the
//! environment under Cargo. Verified against `rules_rust`: an absent name is `None`, an **empty** env
//! file is also `None` rather than `Some("")`, and changing a value invalidates the cached action so
//! a keyless build cannot be reused. `rustc_env_files` rather than `--define` on purpose, because a
//! `--define` value is visible in `ps` and in whatever the CI step echoes.

use std::fmt;

/// Build-time name for the OTLP/HTTP base URL, e.g. `https://ingest.eu.signoz.cloud:443`.
pub const ENDPOINT_VAR: &str = "PESSIMAL_USAGE_OTLP_ENDPOINT";
/// Build-time name for the header the credential travels in.
///
/// A name rather than a hardcoded constant because the right one is backend- and tier-specific —
/// SigNoz Cloud wants `signoz-ingestion-key`, a self-hosted install `signoz-access-token` — and
/// getting it wrong is a silent non-delivery. Better configured once at release time than guessed in
/// source.
pub const HEADER_NAME_VAR: &str = "PESSIMAL_USAGE_OTLP_HEADER";
/// Build-time name for the credential itself.
pub const KEY_VAR: &str = "PESSIMAL_USAGE_OTLP_KEY";

/// The default header, used when a build supplies an endpoint and key but no header name.
const DEFAULT_HEADER: &str = "signoz-ingestion-key";

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
    /// The destination this binary was built with, or `None` if it was built without one.
    ///
    /// `None` is the expected answer for every build except an official release, and
    /// `pessimal_usage::Consent` turns it into `DenialReason::NoDestination` rather than an error:
    /// a build that cannot report is not a build that is broken.
    #[must_use]
    pub fn from_build() -> Option<Self> {
        Self::new(
            option_env!("PESSIMAL_USAGE_OTLP_ENDPOINT")?,
            option_env!("PESSIMAL_USAGE_OTLP_HEADER").unwrap_or(DEFAULT_HEADER),
            option_env!("PESSIMAL_USAGE_OTLP_KEY")?,
        )
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
    fn a_secret_is_redacted_on_its_own_too() {
        assert_eq!(format!("{:?}", Secret::new("abc")), "<redacted>");
        assert_eq!(Secret::new("abc").expose(), "abc");
    }
}
