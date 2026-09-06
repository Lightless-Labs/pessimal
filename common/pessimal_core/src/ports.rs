//! Ports the domain requires of the outside world.
//!
//! Adapters implement these; the domain never names an adapter. Every method returns
//! [`crate::CoreError`] so a `SigNoz` failure and a Honeycomb failure are the same shape to
//! everything above.

use async_trait::async_trait;
use chrono::Duration;

use crate::alert::AlertRule;
use crate::error::Result;
use crate::host::{Host, HostId, HostSelector};
use crate::metric::{MetricKind, MetricSeries, TimeRange};
use crate::urn::Urn;

/// What to fetch in one query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeriesRequest {
    pub metric: MetricKind,
    pub selector: HostSelector,
    pub range: TimeRange,
    /// Requested spacing between points. Backends treat this as a hint and may return a coarser
    /// step for a long window.
    pub step: Duration,
}

impl SeriesRequest {
    #[must_use]
    pub fn new(
        metric: MetricKind,
        selector: HostSelector,
        range: TimeRange,
        step: Duration,
    ) -> Self {
        Self {
            metric,
            selector,
            range,
            step,
        }
    }
}

/// Read-side port over a telemetry backend — `SigNoz`, `ClickStack`, Honeycomb, anything that can
/// answer "this metric, these hosts, this window".
///
/// Polling only: there is no subscription method, by design. Push arrives as a separate port when
/// it arrives.
#[async_trait]
pub trait TelemetryQuery: Send + Sync {
    /// Hosts that reported anything in `range`, newest heartbeat included where known.
    ///
    /// # Errors
    /// Returns [`crate::CoreError::Backend`], [`crate::CoreError::Unauthorized`], or
    /// [`crate::CoreError::Unreachable`] depending on how the backend failed.
    async fn list_hosts(&self, range: TimeRange) -> Result<Vec<Host>>;

    /// One series per matching host, in whatever order the backend returned them.
    ///
    /// # Errors
    /// As [`TelemetryQuery::list_hosts`].
    async fn query_series(&self, request: &SeriesRequest) -> Result<Vec<MetricSeries>>;

    /// A cheap round trip that proves the endpoint answers and the credentials work. Used by the
    /// settings screen's "test connection".
    ///
    /// # Errors
    /// As [`TelemetryQuery::list_hosts`].
    async fn check_connection(&self) -> Result<()>;

    /// A short label for the backend behind this adapter, e.g. `"SigNoz"`. Shown in the UI so an
    /// operator can tell which backend a reading came from.
    fn backend_name(&self) -> &str;
}

/// Persistence for user-configured alert rules. Implemented on the client side — rules are the
/// user's, not the backend's.
#[async_trait]
pub trait AlertRuleRepository: Send + Sync {
    /// # Errors
    /// Returns [`crate::CoreError::Backend`] if the underlying store fails.
    async fn list(&self) -> Result<Vec<AlertRule>>;

    /// # Errors
    /// Returns [`crate::CoreError::Backend`] if the underlying store fails.
    async fn save(&self, rule: &AlertRule) -> Result<()>;

    /// # Errors
    /// Returns [`crate::CoreError::RuleNotFound`] if no rule has that id.
    async fn delete(&self, id: &Urn) -> Result<()>;

    /// # Errors
    /// Returns [`crate::CoreError::Backend`] if the underlying store fails.
    async fn get(&self, id: &Urn) -> Result<Option<AlertRule>>;
}

/// Convenience: request every modelled metric for one host over a window.
#[must_use]
pub fn all_metrics_for(host: &HostId, range: TimeRange, step: Duration) -> Vec<SeriesRequest> {
    MetricKind::ALL
        .into_iter()
        .map(|metric| SeriesRequest::new(metric, HostSelector::Host(host.clone()), range, step))
        .collect()
}
