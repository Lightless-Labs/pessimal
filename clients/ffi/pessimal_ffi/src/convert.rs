//! The boundary's vocabulary: the one error that crosses it, and the scalar shape changes every
//! other module in this crate depends on.
//!
//! Nothing here decides anything. `pessimal_client_core` owns every contract; this module only
//! restates values in shapes UniFFI can carry. Where a conversion can fail it fails because the
//! *representation* cannot hold the value, never because the value is judged wrong — restating a
//! semantic rule here is how two copies of a rule drift apart.
//!
//! The shape changes and their reasons, gathered here so no sibling module reinvents one:
//!
//! - `DateTime<Utc>` becomes `i64` epoch milliseconds. UniFFI carries no date type, and
//!   milliseconds is both what `Date(timeIntervalSince1970:)` wants and what the JSON fixtures
//!   already record.
//! - `chrono::Duration` becomes `i64` seconds. Same absence; seconds because every duration this
//!   product exposes is a poll interval or a staleness threshold, and no screen renders finer.
//! - `Urn` and `HostId` become `String`. Newtypes do not cross, and both already round-trip
//!   through their own `Display`.
//! - `BTreeMap<String, String>` becomes a key-sorted `Vec<AttributeRecord>`. Maps do not cross,
//!   and the sort is what keeps a re-render a no-op for the app's list diffing.
//! - `usize` becomes `u32`, saturating. `usize` does not cross; saturating because a count that
//!   large is a bug, and a panic mid-poll reaches Swift as an opaque crash rather than a message.

use std::collections::BTreeMap;

use chrono::{DateTime, Duration, Utc};
use pessimal_client_core::ClientError;
use pessimal_core::{CoreError, HostId, Urn};

/// Every failure that can reach Swift, flat and struct-shaped so a message always survives.
///
/// Struct-shaped rather than tuple- or unit-shaped throughout: a unit variant reaches Swift with
/// nothing to show a user, and the first time one is thrown from a real code path somebody adds a
/// message to it and breaks every `switch` in the app. Giving all of them a `message` field up
/// front costs one `String` and removes that whole class of churn.
///
/// The first eleven variants mirror [`ClientError`] one-for-one. They are *not* collapsed into a
/// single "something went wrong" case because the variant is the routing decision on the Swift
/// side: [`FfiError::Unauthorized`] is the one that offers an "Open Settings" button and stops the
/// poll timer, [`FfiError::Unreachable`] is the one that backs off and retries, and a banner that
/// cannot tell them apart is a banner that lies to the user.
///
/// This is the crate's only `uniffi::Error`. [`ClientError`] itself cannot be, because it lives in
/// `pessimal_client_core`, which has no `uniffi` dependency and must not grow one — the client
/// core is shared with non-Apple consumers.
///
/// Deliberately *not* `#[uniffi(flat_error)]`: the flat derive generates a `Lift` impl whose body
/// is `panic!("Can't lift flat errors")`, so the day a foreign trait method returns this type the
/// first Swift-thrown error takes the process down. It compiles either way; only the crash tells
/// you which one you picked.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, uniffi::Error)]
pub enum FfiError {
    /// A poll interval, threshold, or multiplier the client core rejected.
    #[error("invalid poll tuning: {message}")]
    InvalidTuning { message: String },

    /// The backend endpoint, credentials, or metric selection is not usable as given.
    #[error("invalid client configuration: {message}")]
    InvalidConfig { message: String },

    /// An alert rule the domain refused to build.
    #[error("invalid alert rule: {message}")]
    InvalidRule { message: String },

    /// A string that is not a Pessimal URN.
    #[error("invalid URN: {message}")]
    InvalidUrn { message: String },

    /// A start/end pair the domain refused.
    #[error("invalid time range: {message}")]
    InvalidTimeRange { message: String },

    /// A metric name outside [`pessimal_core::MetricKind`].
    #[error("unknown metric: {message}")]
    UnknownMetric { message: String },

    /// No rule with that URN is stored.
    #[error("no such alert rule: {message}")]
    RuleNotFound { message: String },

    /// No host with that id is in the current state.
    #[error("unknown host: {message}")]
    UnknownHost { message: String },

    /// The telemetry backend answered, unhappily.
    #[error("telemetry backend error: {message}")]
    Backend { message: String },

    /// Credentials are missing, wrong, or expired. The one failure retrying cannot fix.
    ///
    /// The one variant whose `Display` is the bare message. [`ClientError::Unauthorized`] carries
    /// no payload, so the message it converts to is that error's own rendered sentence; prefixing
    /// it the way every neighbour does would print the same words twice.
    #[error("{message}")]
    Unauthorized { message: String },

    /// The backend was never reached: DNS, TLS, a dead network.
    #[error("telemetry backend is unreachable: {message}")]
    Unreachable { message: String },

    /// A session method was called before the session was constructed, or after a constructor
    /// failed and Swift kept the handle anyway.
    ///
    /// Struct-shaped like every other variant even though the name alone carries the meaning, so
    /// the message can name *which* piece of state is missing without a breaking change later.
    #[error("not initialised: {message}")]
    NotInitialized { message: String },

    /// A bridge fault: something that cannot happen unless this crate or the generated bindings
    /// are wrong.
    ///
    /// Per the design, this is the only variant an exported `poll` may raise. Real backend
    /// problems arrive *inside* the view as `PollFailure` data, because a poll that fails is
    /// still a poll whose last good view must survive; an exception from `poll` would blank the
    /// UI for something the user can neither see nor fix.
    #[error("internal error: {message}")]
    Internal { message: String },
}

/// Exhaustive, one arm per variant, no `_`.
///
/// The wildcard is the whole risk: a new [`ClientError`] variant that quietly inherits some
/// neighbour's mapping reaches the app as the wrong banner with the wrong button, and nothing in
/// Rust CI notices. Without it, adding a variant to the client core stops this file compiling and
/// whoever added it decides what Swift should do about it.
///
/// The payload string crosses verbatim rather than `error.to_string()`: the variant already
/// carries the classification, so re-encoding it into the message would only give the Swift side
/// two sources of truth that can disagree. [`ClientError::Unauthorized`] has no payload, so it is
/// the one variant whose rendered `Display` becomes the message.
impl From<ClientError> for FfiError {
    fn from(error: ClientError) -> Self {
        match error {
            ClientError::InvalidTuning(message) => Self::InvalidTuning { message },
            ClientError::InvalidConfig(message) => Self::InvalidConfig { message },
            ClientError::InvalidRule(message) => Self::InvalidRule { message },
            ClientError::InvalidUrn(message) => Self::InvalidUrn { message },
            ClientError::InvalidTimeRange(message) => Self::InvalidTimeRange { message },
            ClientError::UnknownMetric(message) => Self::UnknownMetric { message },
            ClientError::RuleNotFound(message) => Self::RuleNotFound { message },
            ClientError::UnknownHost(message) => Self::UnknownHost { message },
            ClientError::Backend(message) => Self::Backend { message },
            ClientError::Unauthorized => Self::Unauthorized {
                message: ClientError::Unauthorized.to_string(),
            },
            ClientError::Unreachable(message) => Self::Unreachable { message },
        }
    }
}

/// Exhaustive for the same reason [`From<ClientError>`](FfiError) is.
///
/// Written directly rather than as `ClientError::from(error).into()` so that a future divergence
/// between the two hops is a decision somebody makes here rather than one they inherit. The test
/// module pins the two paths to agree today; when they must stop agreeing, that test is the place
/// the intent gets recorded.
///
/// [`CoreError`] has no `InvalidTuning`, `InvalidConfig`, or `UnknownHost`: those are client-level
/// concepts, so no arm below produces them.
impl From<CoreError> for FfiError {
    fn from(error: CoreError) -> Self {
        match error {
            CoreError::InvalidUrn(message) => Self::InvalidUrn { message },
            CoreError::InvalidRule(message) => Self::InvalidRule { message },
            CoreError::InvalidTimeRange(message) => Self::InvalidTimeRange { message },
            CoreError::UnknownMetric(message) => Self::UnknownMetric { message },
            CoreError::RuleNotFound(message) => Self::RuleNotFound { message },
            CoreError::Backend(message) => Self::Backend { message },
            CoreError::Unauthorized => Self::Unauthorized {
                message: CoreError::Unauthorized.to_string(),
            },
            CoreError::Unreachable(message) => Self::Unreachable { message },
        }
    }
}

/// Required the moment any foreign (Swift-implemented) trait method returns [`FfiError`].
///
/// UniFFI reaches for this impl when Swift throws something the trait's declaration does not
/// mention. Without it the generated code falls back to a panic, and because no foreign trait
/// exists in this crate *yet*, the omission would not be found until one is added and an app in
/// the field crashes on a Swift error nobody anticipated. It costs three lines now, so it is
/// written now rather than discovered later.
impl From<uniffi::UnexpectedUniFFICallbackError> for FfiError {
    fn from(error: uniffi::UnexpectedUniFFICallbackError) -> Self {
        Self::Internal {
            message: error.reason,
        }
    }
}

/// One attribute of a metric series, as a record because maps do not cross the boundary.
///
/// A two-field record rather than a `Vec<String>` of `"key=value"` pairs: the app groups
/// filesystem rows by `mount` and network rows by `interface`, and re-splitting a string on the
/// Swift side to do that would put a parser in the UI layer.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct AttributeRecord {
    pub key: String,
    pub value: String,
}

/// Epoch milliseconds, the boundary's only representation of an instant.
///
/// Infallible: `DateTime<Utc>` spans roughly ±262 000 years, whose milliseconds fit `i64` with
/// three orders of magnitude to spare, so there is no overflow to report and no reason to make
/// every caller unwrap a `Result` that cannot be `Err`.
#[must_use]
pub fn datetime_to_millis(at: DateTime<Utc>) -> i64 {
    at.timestamp_millis()
}

/// The reverse, which *can* fail because `i64` is wider than `DateTime<Utc>`.
///
/// Returns an error rather than panicking or clamping. Clamping would silently turn a bad
/// timestamp into a plausible one and let a poll compute freshness against a fictional instant;
/// panicking crosses UniFFI as an opaque process abort with no message, which is the single worst
/// outcome available at a boundary.
///
/// # Errors
/// [`FfiError::Internal`] when `millis` is outside the range `DateTime<Utc>` can represent. It is
/// `Internal` and not an input error on purpose: Swift's `Date` cannot produce a value out of this
/// range, so reaching it means this crate or the generated bindings computed something wrong. The
/// design also reserves `Internal` as the only variant an exported `poll` may raise, and `poll`
/// takes its `now` through this function.
pub fn millis_to_datetime(millis: i64) -> Result<DateTime<Utc>, FfiError> {
    DateTime::from_timestamp_millis(millis).ok_or_else(|| FfiError::Internal {
        message: format!("{millis} ms since the epoch is not a representable instant"),
    })
}

/// [`datetime_to_millis`] over an optional instant, because `Option<DateTime<Utc>>` is the common
/// case — `last_heartbeat`, `last_success`, and every "never seen" field are optional.
///
/// `None` stays `None` rather than becoming a sentinel like `0` or `-1`: a host that has never
/// reported and a host that reported at the Unix epoch must not render the same.
#[must_use]
pub fn optional_datetime_to_millis(at: Option<DateTime<Utc>>) -> Option<i64> {
    at.map(datetime_to_millis)
}

/// The reverse of [`optional_datetime_to_millis`].
///
/// # Errors
/// [`FfiError::Internal`] when a present value is outside the representable range; see
/// [`millis_to_datetime`].
pub fn optional_millis_to_datetime(millis: Option<i64>) -> Result<Option<DateTime<Utc>>, FfiError> {
    millis.map(millis_to_datetime).transpose()
}

/// Whole seconds, the boundary's only representation of a span.
///
/// Truncates toward zero, which loses nothing in practice: every duration this product exposes is
/// a poll interval or a staleness threshold, all of which are configured and displayed in seconds
/// or minutes. Infallible — `num_seconds` is always in range for an `i64`.
#[must_use]
pub fn duration_to_seconds(duration: Duration) -> i64 {
    duration.num_seconds()
}

/// The reverse, fallible because `chrono::Duration` counts milliseconds internally and so spans
/// roughly a thousandth of the seconds an `i64` can name.
///
/// # Errors
/// [`FfiError::Internal`] when `seconds` exceeds what `chrono::Duration` can hold. As with
/// [`millis_to_datetime`], a value this large cannot come from a user-facing control, so it means
/// the bridge miscomputed rather than that somebody typed something wrong.
pub fn seconds_to_duration(seconds: i64) -> Result<Duration, FfiError> {
    Duration::try_seconds(seconds).ok_or_else(|| FfiError::Internal {
        message: format!("{seconds} s is not a representable duration"),
    })
}

/// A URN as its canonical five-segment string.
///
/// The string form is the one already used in JSON fixtures, in log lines, and as the key of the
/// evaluation maps inside `FleetState`, so Swift and Rust are looking at the same characters.
#[must_use]
pub fn urn_to_string(urn: &Urn) -> String {
    urn.to_string()
}

/// Parses a URN string back into the domain newtype.
///
/// The parse is [`Urn::from_str`](std::str::FromStr)'s, not a second one written here: `Urn` owns
/// what a valid URN is, and a copy of that rule in the bridge would be a copy that rots.
///
/// # Errors
/// [`FfiError::InvalidUrn`], carrying the domain's own message, when `raw` is not a URN.
pub fn urn_from_string(raw: &str) -> Result<Urn, FfiError> {
    raw.parse::<Urn>().map_err(FfiError::from)
}

/// A host id as its string, which is all it ever was.
///
/// [`HostId`] wraps the operator-visible hostname the telemetry backend groups by. Unwrapping it
/// here loses nothing except the type-level distinction, which UniFFI cannot carry anyway.
#[must_use]
pub fn host_id_to_string(id: &HostId) -> String {
    id.as_str().to_owned()
}

/// The reverse. Infallible: [`HostId`] accepts any string, because the backend — not Pessimal —
/// decides what a hostname looks like, and rejecting one here would hide a host the operator can
/// plainly see in their own backend.
#[must_use]
pub fn host_id_from_string(raw: String) -> HostId {
    HostId::from(raw)
}

/// A `BTreeMap` of attributes as a `Vec` of records, in key order.
///
/// The ordering is load-bearing, not cosmetic. `SwiftUI`'s `ForEach` diffs by identity and
/// re-renders on any change it sees, so an attribute list whose order wobbled between polls would
/// animate rows that did not actually change. `BTreeMap` iterates in key order already, so this
/// is a `collect` and the sort is a property of the source type rather than a step that can be
/// forgotten — the direction that actually does the sorting is [`attributes_from_records`].
#[must_use]
pub fn attributes_to_records(attributes: &BTreeMap<String, String>) -> Vec<AttributeRecord> {
    attributes
        .iter()
        .map(|(key, value)| AttributeRecord {
            key: key.clone(),
            value: value.clone(),
        })
        .collect()
}

/// The reverse, which is also where an arbitrarily ordered `Vec` from Swift becomes canonical.
///
/// Collecting into a `BTreeMap` sorts by key and, for a duplicated key, keeps the last entry —
/// `BTreeMap`'s own `FromIterator` behaviour, adopted rather than re-litigated. Feeding the result
/// back through [`attributes_to_records`] therefore yields the same `Vec` no matter what order the
/// records arrived in, which is exactly the determinism the diffing depends on.
#[must_use]
pub fn attributes_from_records(records: Vec<AttributeRecord>) -> BTreeMap<String, String> {
    records
        .into_iter()
        .map(|record| (record.key, record.value))
        .collect()
}

/// A collection length as a `u32`, saturating.
///
/// `usize` does not cross the boundary, and every value converted here is a count of hosts, alerts,
/// or samples — quantities that reach four digits on a bad day. Saturating rather than `try_from`
/// and `?` because there is no honest error to report at four billion hosts and no caller who could
/// act on one; rather than panic mid-poll (an opaque crash on the Swift side) the number pins at
/// `u32::MAX`, which is visibly absurd and therefore diagnosable.
#[must_use]
pub fn count_to_u32(count: usize) -> u32 {
    u32::try_from(count).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::mem::discriminant;

    use chrono::{DateTime, Duration, Utc};
    use pessimal_client_core::ClientError;
    use pessimal_core::{CoreError, HostId, Urn};

    use super::{
        AttributeRecord, FfiError, attributes_from_records, attributes_to_records, count_to_u32,
        datetime_to_millis, duration_to_seconds, host_id_from_string, host_id_to_string,
        millis_to_datetime, optional_datetime_to_millis, optional_millis_to_datetime,
        seconds_to_duration, urn_from_string, urn_to_string,
    };

    fn at() -> DateTime<Utc> {
        DateTime::from_timestamp_millis(1_757_000_000_123).expect("a valid fixture instant")
    }

    /// Every [`ClientError`] variant, once, paired with the [`FfiError`] it must become.
    ///
    /// The `covered` fold below matches exhaustively with no wildcard, so adding a variant to the
    /// client core stops this file compiling; the bitmask assertion catches the other half — a
    /// variant named in that match but forgotten in the list.
    fn client_error_fixtures() -> Vec<(ClientError, FfiError)> {
        let fixtures = vec![
            (
                ClientError::InvalidTuning("interval".to_owned()),
                FfiError::InvalidTuning {
                    message: "interval".to_owned(),
                },
            ),
            (
                ClientError::InvalidConfig("endpoint".to_owned()),
                FfiError::InvalidConfig {
                    message: "endpoint".to_owned(),
                },
            ),
            (
                ClientError::InvalidRule("threshold".to_owned()),
                FfiError::InvalidRule {
                    message: "threshold".to_owned(),
                },
            ),
            (
                ClientError::InvalidUrn("bad urn".to_owned()),
                FfiError::InvalidUrn {
                    message: "bad urn".to_owned(),
                },
            ),
            (
                ClientError::InvalidTimeRange("bad range".to_owned()),
                FfiError::InvalidTimeRange {
                    message: "bad range".to_owned(),
                },
            ),
            (
                ClientError::UnknownMetric("no.such.metric".to_owned()),
                FfiError::UnknownMetric {
                    message: "no.such.metric".to_owned(),
                },
            ),
            (
                ClientError::RuleNotFound("missing".to_owned()),
                FfiError::RuleNotFound {
                    message: "missing".to_owned(),
                },
            ),
            (
                ClientError::UnknownHost("web-9".to_owned()),
                FfiError::UnknownHost {
                    message: "web-9".to_owned(),
                },
            ),
            (
                ClientError::Backend("500 from SigNoz".to_owned()),
                FfiError::Backend {
                    message: "500 from SigNoz".to_owned(),
                },
            ),
            (
                ClientError::Unauthorized,
                FfiError::Unauthorized {
                    message: "not authorised for the telemetry backend".to_owned(),
                },
            ),
            (
                ClientError::Unreachable("dns failure".to_owned()),
                FfiError::Unreachable {
                    message: "dns failure".to_owned(),
                },
            ),
        ];

        let covered = fixtures
            .iter()
            .map(|(error, _)| match error {
                ClientError::InvalidTuning(_) => 0b0000_0000_0000_0001_u16,
                ClientError::InvalidConfig(_) => 0b0000_0000_0000_0010,
                ClientError::InvalidRule(_) => 0b0000_0000_0000_0100,
                ClientError::InvalidUrn(_) => 0b0000_0000_0000_1000,
                ClientError::InvalidTimeRange(_) => 0b0000_0000_0001_0000,
                ClientError::UnknownMetric(_) => 0b0000_0000_0010_0000,
                ClientError::RuleNotFound(_) => 0b0000_0000_0100_0000,
                ClientError::UnknownHost(_) => 0b0000_0000_1000_0000,
                ClientError::Backend(_) => 0b0000_0001_0000_0000,
                ClientError::Unauthorized => 0b0000_0010_0000_0000,
                ClientError::Unreachable(_) => 0b0000_0100_0000_0000,
            })
            .fold(0_u16, |seen, bit| seen | bit);
        assert_eq!(
            covered, 0b0000_0111_1111_1111,
            "the fixture list must name every ClientError variant exactly once"
        );

        fixtures
    }

    /// Every [`CoreError`] variant, once. Same construction, same reason.
    fn core_error_fixtures() -> Vec<(CoreError, FfiError)> {
        let fixtures = vec![
            (
                CoreError::InvalidUrn("bad urn".to_owned()),
                FfiError::InvalidUrn {
                    message: "bad urn".to_owned(),
                },
            ),
            (
                CoreError::InvalidRule("bad rule".to_owned()),
                FfiError::InvalidRule {
                    message: "bad rule".to_owned(),
                },
            ),
            (
                CoreError::InvalidTimeRange("bad range".to_owned()),
                FfiError::InvalidTimeRange {
                    message: "bad range".to_owned(),
                },
            ),
            (
                CoreError::UnknownMetric("no.such.metric".to_owned()),
                FfiError::UnknownMetric {
                    message: "no.such.metric".to_owned(),
                },
            ),
            (
                CoreError::RuleNotFound("missing".to_owned()),
                FfiError::RuleNotFound {
                    message: "missing".to_owned(),
                },
            ),
            (
                CoreError::Backend("500 from SigNoz".to_owned()),
                FfiError::Backend {
                    message: "500 from SigNoz".to_owned(),
                },
            ),
            (
                CoreError::Unauthorized,
                FfiError::Unauthorized {
                    message: "not authorised for the telemetry backend".to_owned(),
                },
            ),
            (
                CoreError::Unreachable("dns failure".to_owned()),
                FfiError::Unreachable {
                    message: "dns failure".to_owned(),
                },
            ),
        ];

        let covered = fixtures
            .iter()
            .map(|(error, _)| match error {
                CoreError::InvalidUrn(_) => 0b0000_0001_u8,
                CoreError::InvalidRule(_) => 0b0000_0010,
                CoreError::InvalidTimeRange(_) => 0b0000_0100,
                CoreError::UnknownMetric(_) => 0b0000_1000,
                CoreError::RuleNotFound(_) => 0b0001_0000,
                CoreError::Backend(_) => 0b0010_0000,
                CoreError::Unauthorized => 0b0100_0000,
                CoreError::Unreachable(_) => 0b1000_0000,
            })
            .fold(0_u8, |seen, bit| seen | bit);
        assert_eq!(
            covered,
            u8::MAX,
            "the fixture list must name every CoreError variant exactly once"
        );

        fixtures
    }

    /// Every [`FfiError`] variant, once, so the `Display` test below cannot silently skip one.
    fn every_ffi_error() -> Vec<FfiError> {
        let all = vec![
            FfiError::InvalidTuning {
                message: "tuning".to_owned(),
            },
            FfiError::InvalidConfig {
                message: "config".to_owned(),
            },
            FfiError::InvalidRule {
                message: "rule".to_owned(),
            },
            FfiError::InvalidUrn {
                message: "urn".to_owned(),
            },
            FfiError::InvalidTimeRange {
                message: "range".to_owned(),
            },
            FfiError::UnknownMetric {
                message: "metric".to_owned(),
            },
            FfiError::RuleNotFound {
                message: "rule-id".to_owned(),
            },
            FfiError::UnknownHost {
                message: "host".to_owned(),
            },
            FfiError::Backend {
                message: "backend".to_owned(),
            },
            FfiError::Unauthorized {
                message: "unauthorised".to_owned(),
            },
            FfiError::Unreachable {
                message: "unreachable".to_owned(),
            },
            FfiError::NotInitialized {
                message: "no session".to_owned(),
            },
            FfiError::Internal {
                message: "bridge fault".to_owned(),
            },
        ];

        let covered = all
            .iter()
            .map(|error| match error {
                FfiError::InvalidTuning { .. } => 0b0000_0000_0000_0001_u16,
                FfiError::InvalidConfig { .. } => 0b0000_0000_0000_0010,
                FfiError::InvalidRule { .. } => 0b0000_0000_0000_0100,
                FfiError::InvalidUrn { .. } => 0b0000_0000_0000_1000,
                FfiError::InvalidTimeRange { .. } => 0b0000_0000_0001_0000,
                FfiError::UnknownMetric { .. } => 0b0000_0000_0010_0000,
                FfiError::RuleNotFound { .. } => 0b0000_0000_0100_0000,
                FfiError::UnknownHost { .. } => 0b0000_0000_1000_0000,
                FfiError::Backend { .. } => 0b0000_0001_0000_0000,
                FfiError::Unauthorized { .. } => 0b0000_0010_0000_0000,
                FfiError::Unreachable { .. } => 0b0000_0100_0000_0000,
                FfiError::NotInitialized { .. } => 0b0000_1000_0000_0000,
                FfiError::Internal { .. } => 0b0001_0000_0000_0000,
            })
            .fold(0_u16, |seen, bit| seen | bit);
        assert_eq!(
            covered, 0b0001_1111_1111_1111,
            "the list must name every FfiError variant exactly once"
        );

        all
    }

    #[test]
    fn every_client_error_variant_converts_to_its_ffi_counterpart() {
        for (error, expected) in client_error_fixtures() {
            let rendered = error.to_string();
            assert_eq!(
                FfiError::from(error),
                expected,
                "converting a client error rendered as {rendered}"
            );
        }
    }

    /// A copy-paste slip that mapped two client errors onto one FFI variant would still satisfy
    /// the fixture table above if the expectations were edited to match. Distinctness is the
    /// property that actually matters: the Swift side routes on the variant, so two client
    /// failures that collapse into one case are two banners the app can no longer tell apart.
    #[test]
    fn no_two_client_errors_collapse_onto_the_same_ffi_variant() {
        let variants: Vec<_> = client_error_fixtures()
            .into_iter()
            .map(|(error, _)| discriminant(&FfiError::from(error)))
            .collect();

        for (index, variant) in variants.iter().enumerate() {
            assert!(
                !variants[index + 1..].contains(variant),
                "two ClientError variants map to the same FfiError variant"
            );
        }
    }

    #[test]
    fn every_core_error_variant_converts_to_its_ffi_counterpart() {
        for (error, expected) in core_error_fixtures() {
            let rendered = error.to_string();
            assert_eq!(
                FfiError::from(error),
                expected,
                "converting a core error rendered as {rendered}"
            );
        }
    }

    /// The direct `From<CoreError>` and the two-hop route through `ClientError` are separate
    /// `match`es, so nothing but this test stops them drifting. If they ever must differ, this is
    /// the assertion that has to be changed deliberately.
    #[test]
    fn the_direct_core_conversion_agrees_with_the_route_through_client_error() {
        for (error, _) in core_error_fixtures() {
            let direct = FfiError::from(error.clone());
            let two_hop = FfiError::from(ClientError::from(error.clone()));
            assert_eq!(direct, two_hop, "the two routes disagree for {error}");
        }
    }

    #[test]
    fn an_unexpected_callback_error_becomes_internal() {
        let unexpected = uniffi::UnexpectedUniFFICallbackError::new("swift threw something else");
        assert_eq!(
            FfiError::from(unexpected),
            FfiError::Internal {
                message: "swift threw something else".to_owned(),
            }
        );
    }

    /// Every variant must render something a human can read: an empty banner is worse than a
    /// wrong one, because it looks like the app has no opinion.
    #[test]
    fn every_ffi_error_renders_its_message() {
        for error in every_ffi_error() {
            let rendered = error.to_string();
            assert!(!rendered.is_empty(), "{error:?} renders as nothing");
            let message = match &error {
                FfiError::InvalidTuning { message }
                | FfiError::InvalidConfig { message }
                | FfiError::InvalidRule { message }
                | FfiError::InvalidUrn { message }
                | FfiError::InvalidTimeRange { message }
                | FfiError::UnknownMetric { message }
                | FfiError::RuleNotFound { message }
                | FfiError::UnknownHost { message }
                | FfiError::Backend { message }
                | FfiError::Unauthorized { message }
                | FfiError::Unreachable { message }
                | FfiError::NotInitialized { message }
                | FfiError::Internal { message } => message,
            };
            assert!(
                rendered.contains(message.as_str()),
                "{error:?} drops its message when rendered"
            );
        }
    }

    #[test]
    fn an_instant_round_trips_through_epoch_millis() {
        let instant = at();
        assert_eq!(
            millis_to_datetime(datetime_to_millis(instant)).expect("a fixture instant is in range"),
            instant
        );
        assert_eq!(datetime_to_millis(instant), 1_757_000_000_123);
    }

    #[test]
    fn an_out_of_range_instant_is_an_error_rather_than_a_panic() {
        for millis in [i64::MIN, i64::MAX] {
            let error = millis_to_datetime(millis).expect_err("out of range must not convert");
            assert!(
                matches!(error, FfiError::Internal { .. }),
                "{error:?} should be Internal: an unrepresentable instant is a bridge fault"
            );
        }
    }

    #[test]
    fn an_optional_instant_keeps_the_difference_between_never_and_the_epoch() {
        assert_eq!(optional_datetime_to_millis(None), None);
        assert_eq!(
            optional_millis_to_datetime(None).expect("None converts"),
            None
        );

        let epoch = DateTime::from_timestamp_millis(0).expect("the epoch is representable");
        assert_eq!(optional_datetime_to_millis(Some(epoch)), Some(0));
        assert_eq!(
            optional_millis_to_datetime(Some(0)).expect("the epoch converts"),
            Some(epoch)
        );

        assert!(optional_millis_to_datetime(Some(i64::MAX)).is_err());
    }

    #[test]
    fn a_duration_round_trips_through_seconds() {
        for duration in [
            Duration::zero(),
            Duration::seconds(30),
            Duration::minutes(5),
            Duration::seconds(-90),
        ] {
            assert_eq!(
                seconds_to_duration(duration_to_seconds(duration)).expect("in range"),
                duration
            );
        }
    }

    #[test]
    fn a_sub_second_duration_truncates_toward_zero() {
        assert_eq!(duration_to_seconds(Duration::milliseconds(1_999)), 1);
        assert_eq!(duration_to_seconds(Duration::milliseconds(-1_999)), -1);
    }

    #[test]
    fn an_out_of_range_duration_is_an_error_rather_than_a_panic() {
        for seconds in [i64::MIN, i64::MAX] {
            let error = seconds_to_duration(seconds).expect_err("out of range must not convert");
            assert!(
                matches!(error, FfiError::Internal { .. }),
                "{error:?} should be Internal: an unrepresentable duration is a bridge fault"
            );
        }
    }

    #[test]
    fn a_urn_round_trips_through_its_string() {
        let urn = Urn::new("prod", "agent", "host").expect("valid segments");
        assert_eq!(
            urn_from_string(&urn_to_string(&urn)).expect("its own rendering parses"),
            urn
        );
    }

    #[test]
    fn an_unparseable_urn_carries_the_domains_own_message() {
        let error = urn_from_string("not-a-urn").expect_err("a bare word is not a URN");
        let FfiError::InvalidUrn { message } = &error else {
            panic!("{error:?} should be InvalidUrn");
        };
        assert!(
            message.contains("segments"),
            "the domain's message should survive, got {message}"
        );
    }

    #[test]
    fn a_host_id_round_trips_through_its_string() {
        let id = HostId::new("web-1.eu-west-1");
        assert_eq!(host_id_from_string(host_id_to_string(&id)), id);
        assert_eq!(host_id_to_string(&id), "web-1.eu-west-1");
    }

    #[test]
    fn attributes_round_trip_in_key_order() {
        let mut attributes = BTreeMap::new();
        attributes.insert("mountpoint".to_owned(), "/".to_owned());
        attributes.insert("device".to_owned(), "disk0s1".to_owned());
        attributes.insert("type".to_owned(), "apfs".to_owned());

        let records = attributes_to_records(&attributes);
        assert_eq!(
            records.iter().map(|r| r.key.as_str()).collect::<Vec<_>>(),
            ["device", "mountpoint", "type"],
            "records must come out in key order"
        );
        assert_eq!(attributes_from_records(records), attributes);
    }

    /// The determinism the `SwiftUI` diffing depends on: whatever order Swift hands the records
    /// back in, the next `to_records` produces the same `Vec`.
    #[test]
    fn records_from_swift_are_reordered_into_the_canonical_sequence() {
        let scrambled = vec![
            AttributeRecord {
                key: "type".to_owned(),
                value: "apfs".to_owned(),
            },
            AttributeRecord {
                key: "device".to_owned(),
                value: "disk0s1".to_owned(),
            },
            AttributeRecord {
                key: "mountpoint".to_owned(),
                value: "/".to_owned(),
            },
        ];

        let canonical = attributes_to_records(&attributes_from_records(scrambled.clone()));
        assert_eq!(
            canonical.iter().map(|r| r.key.as_str()).collect::<Vec<_>>(),
            ["device", "mountpoint", "type"]
        );

        let mut reversed = scrambled;
        reversed.reverse();
        assert_eq!(
            attributes_to_records(&attributes_from_records(reversed)),
            canonical,
            "the canonical sequence must not depend on the inbound order"
        );
    }

    #[test]
    fn a_duplicated_attribute_key_keeps_the_last_value() {
        let records = vec![
            AttributeRecord {
                key: "device".to_owned(),
                value: "first".to_owned(),
            },
            AttributeRecord {
                key: "device".to_owned(),
                value: "last".to_owned(),
            },
        ];
        assert_eq!(
            attributes_from_records(records)
                .get("device")
                .map(String::as_str),
            Some("last")
        );
    }

    #[test]
    fn an_empty_attribute_map_round_trips_as_an_empty_vec() {
        assert!(attributes_to_records(&BTreeMap::new()).is_empty());
        assert!(attributes_from_records(Vec::new()).is_empty());
    }

    #[test]
    fn a_count_saturates_rather_than_wrapping_or_panicking() {
        assert_eq!(count_to_u32(0), 0);
        assert_eq!(count_to_u32(42), 42);
        assert_eq!(count_to_u32(u32::MAX as usize), u32::MAX);
        assert_eq!(count_to_u32(usize::MAX), u32::MAX);
    }
}
