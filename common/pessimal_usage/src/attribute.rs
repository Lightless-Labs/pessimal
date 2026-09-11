//! The attribute allowlist, and the vocabulary it is built from.
//!
//! Pessimal's usage spans go to *our* backend, not the operator's. The interesting attributes are
//! therefore exactly the ones we must not send: the endpoint someone's collector lives at, their
//! host names, their alert thresholds, the body of a backend error. A reviewer cannot reliably catch
//! that being added later, so the allowlist is not a list to check against — it is the only
//! vocabulary this crate can express.
//!
//! The mechanism: [`AttributeValue`] has no variant that accepts arbitrary text. Strings arrive
//! either as `&'static str` from an enum's `as_str`, which no runtime value can forge, or as a
//! [`Version`], whose constructor accepts digits and dots and nothing else. There is deliberately
//! no `Attribute::new(key: &str, value: String)`.

use std::fmt;

/// A version number, narrowed to the one shape that cannot carry anything private.
///
/// `0.1.0` and `18.4` pass; `web-1.internal`, `https://signoz.acme.corp` and a UUID do not. This is
/// what lets two genuinely useful runtime strings into a span without opening the door to the rest.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version(String);

impl Version {
    /// Up to three dotted numeric components, for an application version.
    ///
    /// Returns `None` for anything else, including an empty string, a leading `v`, and a build
    /// suffix like `1.2.3-beta`. Rejecting rather than sanitising is deliberate: a caller that
    /// passes something unexpected should show up as a missing attribute, not as a half-scrubbed
    /// one.
    #[must_use]
    pub fn app(raw: &str) -> Option<Self> {
        Self::parse(raw, 3)
    }

    /// Major and minor only, for an operating system version.
    ///
    /// A patch level is dropped rather than rejected: `18.4.1` becomes `18.4`. Keeping it would
    /// narrow the population a span belongs to for no diagnostic gain worth having.
    #[must_use]
    pub fn os(raw: &str) -> Option<Self> {
        Self::parse(raw, 2)
    }

    fn parse(raw: &str, keep: usize) -> Option<Self> {
        let mut components = Vec::new();
        for component in raw.split('.') {
            if component.is_empty() || !component.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            // A component long enough to be something other than a version number is refused
            // rather than truncated.
            if component.len() > 6 {
                return None;
            }
            components.push(component);
        }
        if components.is_empty() {
            return None;
        }
        components.truncate(keep);
        Some(Self(components.join(".")))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Which kind of machine the report came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Ios,
    Macos,
    Linux,
    Windows,
}

impl Platform {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ios => "ios",
            Self::Macos => "macos",
            Self::Linux => "linux",
            Self::Windows => "windows",
        }
    }
}

/// Coarse device shape. Coarse on purpose: an exact model is a narrower cohort than anything we
/// would act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceClass {
    Phone,
    Tablet,
    Mac,
    Server,
}

impl DeviceClass {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Phone => "phone",
            Self::Tablet => "tablet",
            Self::Mac => "mac",
            Self::Server => "server",
        }
    }
}

/// Which backend the operator pointed us at — the *kind*, never the URL.
///
/// Mirrors the agent's `BackendPreset`, but deliberately does not reuse it: that type lives in
/// `pessimal_agent_core`, which the clients do not depend on, and this crate must not grow a
/// dependency in that direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    Otlp,
    Signoz,
    Clickstack,
    Honeycomb,
}

impl BackendKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Otlp => "otlp",
            Self::Signoz => "signoz",
            Self::Clickstack => "clickstack",
            Self::Honeycomb => "honeycomb",
        }
    }
}

/// How an operation ended. The *kind* of failure, never its message.
///
/// These map one-to-one onto `pessimal_core::CoreError`'s variants, which is the whole point: the
/// shape of a failure is the diagnostic, and the string inside it is the part that would leak a
/// hostname or a query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Ok,
    /// Some hosts or series answered and some did not.
    Partial,
    Unreachable,
    Unauthorized,
    BackendError,
    DecodeError,
    /// The operation was abandoned before it finished — a cancelled poll, a backgrounded app.
    Cancelled,
}

impl Outcome {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Partial => "partial",
            Self::Unreachable => "unreachable",
            Self::Unauthorized => "unauthorized",
            Self::BackendError => "backend_error",
            Self::DecodeError => "decode_error",
            Self::Cancelled => "cancelled",
        }
    }

    /// Whether this outcome should mark its span as an error for the backend's RED metrics.
    #[must_use]
    pub fn is_error(self) -> bool {
        !matches!(self, Self::Ok | Self::Cancelled)
    }
}

/// An HTTP status reduced to its class: `2` for 2xx, `4` for 4xx, and so on.
///
/// The class answers every question we would ask of it — is the backend refusing us, is it broken,
/// is it redirecting — while a full status code plus a URL starts to describe someone's
/// infrastructure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HttpStatusClass(u8);

impl HttpStatusClass {
    /// `None` for a status outside 100–599, which is not a status.
    #[must_use]
    pub fn of(status: u16) -> Option<Self> {
        match status {
            100..=599 => Some(Self(u8::try_from(status / 100).ok()?)),
            _ => None,
        }
    }

    #[must_use]
    pub fn digit(self) -> u8 {
        self.0
    }
}

/// Every key this crate may emit. Adding a variant is the visible, reviewable act that adding an
/// attribute ought to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AttributeKey {
    // Resource-level.
    ServiceName,
    ServiceVersion,
    ServiceInstanceId,
    AppBuild,
    Platform,
    OsVersion,
    DeviceClass,
    // Span-level.
    BackendKind,
    Outcome,
    HostsCount,
    RulesCount,
    AlertsFiring,
    RequestsPlanned,
    SeriesReturned,
    HttpStatusClass,
}

impl AttributeKey {
    /// The wire name. OpenTelemetry's own `service.*` keys where they exist, and `pessimal.*` for
    /// the rest, per the project's metric-naming convention.
    #[must_use]
    pub fn otel_name(self) -> &'static str {
        match self {
            Self::ServiceName => "service.name",
            Self::ServiceVersion => "service.version",
            Self::ServiceInstanceId => "service.instance.id",
            Self::AppBuild => "pessimal.app.build",
            Self::Platform => "pessimal.platform",
            Self::OsVersion => "pessimal.os.version",
            Self::DeviceClass => "pessimal.device.class",
            Self::BackendKind => "pessimal.backend.kind",
            Self::Outcome => "pessimal.outcome",
            Self::HostsCount => "pessimal.hosts.count",
            Self::RulesCount => "pessimal.rules.count",
            Self::AlertsFiring => "pessimal.alerts.firing",
            Self::RequestsPlanned => "pessimal.requests.planned",
            Self::SeriesReturned => "pessimal.series.returned",
            Self::HttpStatusClass => "pessimal.http.status_class",
        }
    }

    /// Every key, for the test that asserts nothing escapes the allowlist.
    pub const ALL: [Self; 15] = [
        Self::ServiceName,
        Self::ServiceVersion,
        Self::ServiceInstanceId,
        Self::AppBuild,
        Self::Platform,
        Self::OsVersion,
        Self::DeviceClass,
        Self::BackendKind,
        Self::Outcome,
        Self::HostsCount,
        Self::RulesCount,
        Self::AlertsFiring,
        Self::RequestsPlanned,
        Self::SeriesReturned,
        Self::HttpStatusClass,
    ];
}

/// What an attribute is allowed to hold.
///
/// There is no `Text(String)`. `Enumerated` takes `&'static str`, which in practice means it comes
/// from one of the `as_str` methods above — a runtime value cannot become one without someone
/// leaking a `String`, which is what `unsafe_code = "forbid"` and the lack of a constructor make
/// awkward enough to notice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttributeValue {
    Enumerated(&'static str),
    Count(u32),
    Version(Version),
    /// A UUID, rendered. Only ever the session instance id this crate mints itself.
    Identifier(uuid::Uuid),
}

/// One allowlisted key/value pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attribute {
    pub key: AttributeKey,
    pub value: AttributeValue,
}

impl Attribute {
    #[must_use]
    pub fn enumerated(key: AttributeKey, value: &'static str) -> Self {
        Self {
            key,
            value: AttributeValue::Enumerated(value),
        }
    }

    #[must_use]
    pub fn count(key: AttributeKey, value: u32) -> Self {
        Self {
            key,
            value: AttributeValue::Count(value),
        }
    }

    #[must_use]
    pub fn version(key: AttributeKey, value: Version) -> Self {
        Self {
            key,
            value: AttributeValue::Version(value),
        }
    }

    #[must_use]
    pub fn identifier(key: AttributeKey, value: uuid::Uuid) -> Self {
        Self {
            key,
            value: AttributeValue::Identifier(value),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_version_keeps_what_it_should() {
        assert_eq!(Version::app("0.1.0").expect("valid").as_str(), "0.1.0");
        assert_eq!(Version::app("1.2.3.4").expect("valid").as_str(), "1.2.3");
        assert_eq!(Version::os("18.4.1").expect("valid").as_str(), "18.4");
        assert_eq!(Version::os("26").expect("valid").as_str(), "26");
    }

    #[test]
    fn a_version_refuses_anything_that_could_carry_private_data() {
        for leak in [
            "web-1.internal",
            "https://signoz.acme.corp:4317",
            "v1.2.3",
            "1.2.3-beta",
            "",
            ".",
            "1..2",
            "acme",
            "0199a0e4-0000-7000-8000-000000000001",
            // Long enough to be a serial or an account id rather than a version component.
            "1234567",
        ] {
            assert!(Version::app(leak).is_none(), "app accepted {leak:?}");
            assert!(Version::os(leak).is_none(), "os accepted {leak:?}");
        }
    }

    #[test]
    fn a_status_class_is_the_leading_digit() {
        assert_eq!(HttpStatusClass::of(204).expect("valid").digit(), 2);
        assert_eq!(HttpStatusClass::of(401).expect("valid").digit(), 4);
        assert_eq!(HttpStatusClass::of(503).expect("valid").digit(), 5);
        assert!(HttpStatusClass::of(0).is_none());
        assert!(HttpStatusClass::of(600).is_none());
    }

    #[test]
    fn every_key_has_a_distinct_wire_name() {
        let mut names: Vec<&str> = AttributeKey::ALL.iter().map(|k| k.otel_name()).collect();
        names.sort_unstable();
        let count = names.len();
        names.dedup();
        assert_eq!(names.len(), count, "two keys share a wire name");
    }

    #[test]
    fn pessimals_own_keys_are_namespaced() {
        for key in AttributeKey::ALL {
            let name = key.otel_name();
            assert!(
                name.starts_with("pessimal.") || name.starts_with("service."),
                "{name} is neither a pessimal.* key nor an OTel service.* one"
            );
        }
    }

    #[test]
    fn an_outcome_knows_whether_it_is_an_error() {
        assert!(!Outcome::Ok.is_error());
        assert!(
            !Outcome::Cancelled.is_error(),
            "a cancelled poll is not a fault"
        );
        for bad in [
            Outcome::Partial,
            Outcome::Unreachable,
            Outcome::Unauthorized,
            Outcome::BackendError,
            Outcome::DecodeError,
        ] {
            assert!(bad.is_error(), "{bad:?}");
        }
    }
}
