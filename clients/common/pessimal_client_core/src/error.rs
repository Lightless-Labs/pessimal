//! The crate's two failure vocabularies, kept deliberately apart.
//!
//! [`ClientError`] is what a fallible call *returns*; [`PollFailure`] is what a poll *records*.
//! The split is not stylistic. A type cannot be both a `uniffi::Error` and a record field enum in
//! one crate, so keeping the vocabularies apart is what lets a `PollFailure` sit inside a
//! `FleetView` and inside a serialisable `PollObservation` while `ClientError` stays in return
//! position only.
//!
//! Both conversions from [`pessimal_core::CoreError`] are exhaustive matches with no wildcard
//! arm. That is the load-bearing part of this module: a wildcard is how a new core variant
//! silently becomes the wrong client error, and flattening a whole error with `.to_string()` is
//! how `Unauthorized` stops being distinguishable one layer above — the settings screen can only
//! offer an "Open Settings" button if the variant survives the trip.

use std::cmp::Reverse;

use chrono::{DateTime, Utc};
use pessimal_core::{CoreError, MetricKind};
use serde::{Deserialize, Serialize};

/// Everything a fallible call in this crate can return.
///
/// Flat by design: `pessimal_ffi` mirrors it as the one `uniffi::Error`, and a nested error would
/// mirror as an opaque string. It never appears in field position — see [`PollFailure`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ClientError {
    #[error("invalid poll tuning: {0}")]
    InvalidTuning(String),

    #[error("invalid client configuration: {0}")]
    InvalidConfig(String),

    #[error("invalid alert rule: {0}")]
    InvalidRule(String),

    #[error("invalid URN: {0}")]
    InvalidUrn(String),

    #[error("invalid time range: {0}")]
    InvalidTimeRange(String),

    #[error("unknown metric: {0}")]
    UnknownMetric(String),

    #[error("no such alert rule: {0}")]
    RuleNotFound(String),

    #[error("unknown host: {0}")]
    UnknownHost(String),

    #[error("telemetry backend error: {0}")]
    Backend(String),

    #[error("not authorised for the telemetry backend")]
    Unauthorized,

    #[error("telemetry backend is unreachable: {0}")]
    Unreachable(String),
}

/// This crate's `Result`. `lib.rs` re-exports it by name and must never glob `pessimal_core`,
/// whose own `Result<T>` would shadow it.
pub type Result<T> = std::result::Result<T, ClientError>;

/// Exhaustive `match`, never `.to_string()` on the whole error: the variant is what drives
/// backoff, the auth cliff, and whether the banner offers an "Open Settings" button.
///
/// There is no `_` arm on purpose. A new [`CoreError`] variant must fail to compile here so
/// whoever adds it decides what a client should do with it, rather than inheriting whichever
/// mapping happened to sit under the wildcard.
impl From<CoreError> for ClientError {
    fn from(error: CoreError) -> Self {
        match error {
            CoreError::InvalidUrn(message) => Self::InvalidUrn(message),
            CoreError::InvalidRule(message) => Self::InvalidRule(message),
            CoreError::InvalidTimeRange(message) => Self::InvalidTimeRange(message),
            CoreError::UnknownMetric(message) => Self::UnknownMetric(message),
            CoreError::RuleNotFound(message) => Self::RuleNotFound(message),
            CoreError::Backend(message) => Self::Backend(message),
            CoreError::Unauthorized => Self::Unauthorized,
            CoreError::Unreachable(message) => Self::Unreachable(message),
        }
    }
}

/// What kind of thing went wrong, coarse enough for the UI to route on and for the poller to
/// decide whether coming back sooner could possibly help.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PollFailureKind {
    /// The credentials are wrong, missing, or expired.
    Unauthorized,
    /// We never reached the backend: DNS, TLS, a dead network.
    Unreachable,
    /// The backend answered, unhappily. A 500, a query rejection.
    Backend,
    /// The backend answered something we could not turn into domain values — a metric name it
    /// does not know, a range it will not accept, a response we cannot map.
    Malformed,
}

impl PollFailureKind {
    /// `Unauthorized -> Unauthorized`, `Unreachable -> Unreachable`, `Backend -> Backend`,
    /// every other `CoreError` -> `Malformed`. The single classification point.
    ///
    /// Exhaustive with an or-pattern rather than a `_`, for the same reason
    /// `From<CoreError> for ClientError` is: a new core variant should stop the build, not
    /// quietly become `Malformed` and read to an operator as a naming mismatch that is not there.
    #[must_use]
    pub fn from_core(error: &CoreError) -> Self {
        match error {
            CoreError::Unauthorized => Self::Unauthorized,
            CoreError::Unreachable(_) => Self::Unreachable,
            CoreError::Backend(_) => Self::Backend,
            CoreError::InvalidUrn(_)
            | CoreError::InvalidRule(_)
            | CoreError::InvalidTimeRange(_)
            | CoreError::UnknownMetric(_)
            | CoreError::RuleNotFound(_) => Self::Malformed,
        }
    }

    /// True only for `Unauthorized`: retrying cannot help, the user must change something.
    ///
    /// This is what routes the "Open Settings" button and what makes the fold advise
    /// `PollAdvice::Stop` instead of burning battery on a problem only the user can fix.
    #[must_use]
    pub fn is_actionable(self) -> bool {
        matches!(self, Self::Unauthorized)
    }

    /// True when simply coming back later could plausibly succeed with nothing else changed.
    ///
    /// Three-way split, not the negation of [`Self::is_actionable`]: `Malformed` is neither
    /// transient nor user-fixable. A metric name the backend does not know will still be unknown
    /// on the next poll, and there is no settings screen that repairs it.
    #[must_use]
    pub fn is_transient(self) -> bool {
        matches!(self, Self::Unreachable | Self::Backend)
    }

    /// Severity rank for [`worst_failure`]: `Unauthorized` 3, `Unreachable` 2, `Backend` 1,
    /// `Malformed` 0.
    #[must_use]
    pub fn rank(self) -> u8 {
        match self {
            Self::Unauthorized => 3,
            Self::Unreachable => 2,
            Self::Backend => 1,
            Self::Malformed => 0,
        }
    }
}

/// Which request failed. Carried so the UI can name the metric, and so [`worst_failure`] can
/// tie-break without depending on `join_all` completion order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FailureSource {
    /// `list_hosts`. The one request whose failure freezes the whole picture.
    Roster,
    /// One metric query.
    Series { metric: MetricKind },
}

/// One request's failure, recorded as data rather than raised as an error.
///
/// This is the only failure type that ever sits in a struct field: inside a `PollObservation`, so
/// a production poll replays as a fixture, and inside a `FleetView`, so the banner can name what
/// went wrong without the app parsing a message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PollFailure {
    pub kind: PollFailureKind,
    pub source: FailureSource,
    pub message: String,
    pub at: DateTime<Utc>,
}

impl PollFailure {
    /// Classifies `error` and keeps its rendered message for the banner.
    ///
    /// `at` is supplied rather than read from a clock: every failure in one poll is stamped at
    /// the poll's single instant, which is what makes an observation a self-contained fixture.
    #[must_use]
    pub fn from_core(error: &CoreError, source: FailureSource, at: DateTime<Utc>) -> Self {
        Self {
            kind: PollFailureKind::from_core(error),
            source,
            message: error.to_string(),
            at,
        }
    }
}

/// Roster sorts before any series, and series sort by [`MetricKind`] declaration order.
///
/// `Option` orders `None` before `Some`, so the leading discriminant is belt and braces rather
/// than the whole mechanism.
fn source_key(source: FailureSource) -> (u8, Option<MetricKind>) {
    match source {
        FailureSource::Roster => (0, None),
        FailureSource::Series { metric } => (1, Some(metric)),
    }
}

/// The total order [`worst_failure`] minimises over. Smallest is worst: severity rank descending,
/// then roster before series, then metric order, then message, then instant.
type RankingKey<'a> = (Reverse<u8>, u8, Option<MetricKind>, &'a str, DateTime<Utc>);

/// The design pins three tie-breaks — rank descending, roster before series, then metric order.
/// Those three still leave two failures tied when the same metric is queried twice in one plan,
/// which is exactly the focused host's overview-plus-detail pair: both can fail with the same
/// kind and source and different messages. The trailing `(message, at)` closes that hole, so any
/// two failures that compare equal here are equal in every field and the choice between them is
/// not a choice at all.
fn ranking_key(failure: &PollFailure) -> RankingKey<'_> {
    let (source_order, metric) = source_key(failure.source);
    (
        Reverse(failure.kind.rank()),
        source_order,
        metric,
        failure.message.as_str(),
        failure.at,
    )
}

/// The failure a user should be shown when several requests failed at once. Ranked by
/// `kind.rank()` descending, tie-broken `Roster` before `Series`, then by [`MetricKind`] order.
/// Deterministic: the banner text must not depend on which future finished first.
#[must_use]
pub fn worst_failure(failures: &[PollFailure]) -> Option<&PollFailure> {
    failures
        .iter()
        .min_by(|left, right| ranking_key(left).cmp(&ranking_key(right)))
}

#[cfg(test)]
mod tests {
    use super::{ClientError, FailureSource, PollFailure, PollFailureKind, worst_failure};
    use chrono::{DateTime, Utc};
    use pessimal_core::{CoreError, MetricKind};

    const ALL_KINDS: [PollFailureKind; 4] = [
        PollFailureKind::Unauthorized,
        PollFailureKind::Unreachable,
        PollFailureKind::Backend,
        PollFailureKind::Malformed,
    ];

    fn at() -> DateTime<Utc> {
        DateTime::from_timestamp(1_757_000_000, 0).expect("a valid fixture instant")
    }

    fn failure(kind: PollFailureKind, source: FailureSource, message: &str) -> PollFailure {
        PollFailure {
            kind,
            source,
            message: message.to_owned(),
            at: at(),
        }
    }

    fn series(metric: MetricKind) -> FailureSource {
        FailureSource::Series { metric }
    }

    /// Every `CoreError` variant, once, paired with the kind it must classify to.
    ///
    /// The `match` below is exhaustive with no wildcard, so adding a `CoreError` variant stops
    /// this file compiling; the bitmask assertion catches the other half — a variant named in the
    /// match but forgotten in the list.
    fn core_error_fixtures() -> Vec<(CoreError, PollFailureKind)> {
        let fixtures = vec![
            (
                CoreError::InvalidUrn("bad urn".to_owned()),
                PollFailureKind::Malformed,
            ),
            (
                CoreError::InvalidRule("bad rule".to_owned()),
                PollFailureKind::Malformed,
            ),
            (
                CoreError::InvalidTimeRange("bad range".to_owned()),
                PollFailureKind::Malformed,
            ),
            (
                CoreError::UnknownMetric("no.such.metric".to_owned()),
                PollFailureKind::Malformed,
            ),
            (
                CoreError::RuleNotFound("missing".to_owned()),
                PollFailureKind::Malformed,
            ),
            (
                CoreError::Backend("500 from SigNoz".to_owned()),
                PollFailureKind::Backend,
            ),
            (CoreError::Unauthorized, PollFailureKind::Unauthorized),
            (
                CoreError::Unreachable("dns failure".to_owned()),
                PollFailureKind::Unreachable,
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
            "the fixture list must name every CoreError variant"
        );

        fixtures
    }

    /// Every ordering of `failures` must pick `expected`. This is the whole point of the ranking:
    /// `join_all` preserves plan order, but which requests fail is not something a test can pin,
    /// so the ranking has to be total rather than merely usually-agreeing.
    fn assert_worst_is_stable(failures: &[PollFailure], expected: &PollFailure) {
        for ordering in permutations(failures) {
            assert_eq!(
                worst_failure(&ordering),
                Some(expected),
                "ordering {ordering:?} picked a different failure"
            );
        }
    }

    fn permutations<T: Clone>(items: &[T]) -> Vec<Vec<T>> {
        if items.is_empty() {
            return vec![Vec::new()];
        }
        let mut orderings = Vec::new();
        for (index, item) in items.iter().enumerate() {
            let mut rest = items.to_vec();
            rest.remove(index);
            for mut tail in permutations(&rest) {
                tail.insert(0, item.clone());
                orderings.push(tail);
            }
        }
        orderings
    }

    #[test]
    fn every_core_error_variant_classifies() {
        for (error, expected) in core_error_fixtures() {
            assert_eq!(
                PollFailureKind::from_core(&error),
                expected,
                "classifying {error}"
            );
        }
    }

    #[test]
    fn every_core_error_variant_converts_to_its_client_counterpart() {
        // One arm per variant, no wildcard: `Unauthorized` has to still be `Unauthorized` on the
        // other side or the settings screen has nothing to branch on.
        assert_eq!(
            ClientError::from(CoreError::InvalidUrn("u".to_owned())),
            ClientError::InvalidUrn("u".to_owned())
        );
        assert_eq!(
            ClientError::from(CoreError::InvalidRule("r".to_owned())),
            ClientError::InvalidRule("r".to_owned())
        );
        assert_eq!(
            ClientError::from(CoreError::InvalidTimeRange("t".to_owned())),
            ClientError::InvalidTimeRange("t".to_owned())
        );
        assert_eq!(
            ClientError::from(CoreError::UnknownMetric("m".to_owned())),
            ClientError::UnknownMetric("m".to_owned())
        );
        assert_eq!(
            ClientError::from(CoreError::RuleNotFound("id".to_owned())),
            ClientError::RuleNotFound("id".to_owned())
        );
        assert_eq!(
            ClientError::from(CoreError::Backend("500".to_owned())),
            ClientError::Backend("500".to_owned())
        );
        assert_eq!(
            ClientError::from(CoreError::Unauthorized),
            ClientError::Unauthorized
        );
        assert_eq!(
            ClientError::from(CoreError::Unreachable("dns".to_owned())),
            ClientError::Unreachable("dns".to_owned())
        );
        // The count is pinned by `core_error_fixtures`, whose match is exhaustive.
        assert_eq!(core_error_fixtures().len(), 8);
    }

    #[test]
    fn only_unauthorized_is_actionable() {
        for kind in ALL_KINDS {
            assert_eq!(
                kind.is_actionable(),
                kind == PollFailureKind::Unauthorized,
                "{kind:?} is actionable only if the user can fix it"
            );
        }

        // Transience is a separate axis, not the negation: `Malformed` is neither.
        assert!(PollFailureKind::Unreachable.is_transient());
        assert!(PollFailureKind::Backend.is_transient());
        assert!(!PollFailureKind::Unauthorized.is_transient());
        assert!(!PollFailureKind::Malformed.is_transient());
    }

    #[test]
    fn a_poll_failure_round_trips_through_json() {
        // Record-and-replay is the fixture strategy for the whole crate, so a `PollFailure` has
        // to survive a round trip byte for byte — including the metric that names the tile.
        for failure in [
            failure(
                PollFailureKind::Unauthorized,
                FailureSource::Roster,
                "not authorised for the telemetry backend",
            ),
            failure(
                PollFailureKind::Backend,
                series(MetricKind::FilesystemUtilization),
                "telemetry backend error: 500",
            ),
        ] {
            let json = serde_json::to_string(&failure).expect("a poll failure serialises");
            let restored: PollFailure =
                serde_json::from_str(&json).expect("a poll failure deserialises");
            assert_eq!(restored, failure);
        }
    }

    #[test]
    fn worst_failure_is_independent_of_outcome_order() {
        // Rank dominates everything else: an expired key beats a dead roster query.
        assert_worst_is_stable(
            &[
                failure(
                    PollFailureKind::Backend,
                    series(MetricKind::NetworkIo),
                    "500",
                ),
                failure(PollFailureKind::Unreachable, FailureSource::Roster, "dns"),
                failure(
                    PollFailureKind::Unauthorized,
                    series(MetricKind::CpuUtilization),
                    "401 on cpu",
                ),
                failure(PollFailureKind::Malformed, FailureSource::Roster, "garbage"),
            ],
            &failure(
                PollFailureKind::Unauthorized,
                series(MetricKind::CpuUtilization),
                "401 on cpu",
            ),
        );

        // First tie-break: the roster failing is the one that freezes the whole picture.
        assert_worst_is_stable(
            &[
                failure(
                    PollFailureKind::Unreachable,
                    series(MetricKind::MemoryUtilization),
                    "dns on memory",
                ),
                failure(
                    PollFailureKind::Unreachable,
                    FailureSource::Roster,
                    "dns on roster",
                ),
                failure(
                    PollFailureKind::Unreachable,
                    series(MetricKind::CpuUtilization),
                    "dns on cpu",
                ),
            ],
            &failure(
                PollFailureKind::Unreachable,
                FailureSource::Roster,
                "dns on roster",
            ),
        );

        // Second tie-break: MetricKind declaration order, so the message names the same metric
        // every time rather than whichever request lost the race.
        assert_worst_is_stable(
            &[
                failure(
                    PollFailureKind::Backend,
                    series(MetricKind::NetworkIo),
                    "500",
                ),
                failure(
                    PollFailureKind::Backend,
                    series(MetricKind::CpuUtilization),
                    "500",
                ),
                failure(
                    PollFailureKind::Backend,
                    series(MetricKind::LoadAverage1m),
                    "500",
                ),
            ],
            &failure(
                PollFailureKind::Backend,
                series(MetricKind::CpuUtilization),
                "500",
            ),
        );

        // Final tie-break: the focused host's overview and detail queries cover the same metric,
        // so one poll really can produce two failures identical in kind and source. Without a
        // message tie-break the banner would flip between two true sentences.
        assert_worst_is_stable(
            &[
                failure(
                    PollFailureKind::Backend,
                    series(MetricKind::CpuUtilization),
                    "detail window rejected",
                ),
                failure(
                    PollFailureKind::Backend,
                    series(MetricKind::CpuUtilization),
                    "another query failed",
                ),
            ],
            &failure(
                PollFailureKind::Backend,
                series(MetricKind::CpuUtilization),
                "another query failed",
            ),
        );
    }

    #[test]
    fn worst_failure_of_nothing_is_none() {
        assert_eq!(worst_failure(&[]), None);
    }
}
