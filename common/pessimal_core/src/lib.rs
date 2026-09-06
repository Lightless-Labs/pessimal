//! Pessimal domain core.
//!
//! Everything here is technology-agnostic: no HTTP, no OTLP, no filesystem, no clock reads.
//! Adapters supply time via [`Clock`] and telemetry via [`TelemetryQuery`].

pub mod alert;
pub mod clock;
pub mod error;
pub mod host;
pub mod liveness;
pub mod metric;
pub mod ports;
pub mod urn;

pub use alert::{AlertEvaluation, AlertRule, AlertState, Comparator};
pub use clock::Clock;
pub use error::{CoreError, Result};
pub use host::{Host, HostId, HostSelector, OsFamily};
pub use liveness::{Liveness, LivenessPolicy};
pub use metric::{MetricKind, MetricPoint, MetricSeries, MetricUnit, TimeRange};
pub use ports::{AlertRuleRepository, SeriesRequest, TelemetryQuery};
pub use urn::Urn;
