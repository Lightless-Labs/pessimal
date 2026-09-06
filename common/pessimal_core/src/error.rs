//! Domain error type.

use thiserror::Error;

pub type Result<T> = std::result::Result<T, CoreError>;

/// Errors the domain can raise. Adapter failures are folded into [`CoreError::Backend`] with a
/// human-readable message; the domain never depends on an adapter's error type.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum CoreError {
    #[error("invalid URN: {0}")]
    InvalidUrn(String),

    #[error("invalid alert rule: {0}")]
    InvalidRule(String),

    #[error("invalid time range: {0}")]
    InvalidTimeRange(String),

    #[error("unknown metric: {0}")]
    UnknownMetric(String),

    #[error("no such alert rule: {0}")]
    RuleNotFound(String),

    #[error("telemetry backend error: {0}")]
    Backend(String),

    #[error("not authorised for the telemetry backend")]
    Unauthorized,

    #[error("telemetry backend is unreachable: {0}")]
    Unreachable(String),
}
