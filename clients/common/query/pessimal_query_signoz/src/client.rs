//! The SigNoz adapter.

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use pessimal_core::{
    CoreError, Host, HostId, HostSelector, LivenessPolicy, MetricKind, MetricPoint, MetricSeries,
    OsFamily, Result, SeriesRequest, TelemetryQuery, TimeRange,
};
use reqwest::{Client, StatusCode};

use crate::naming::{
    HOST_NAME_ATTRIBUTE, MetricNaming, OS_TYPE_ATTRIBUTE, SERVICE_VERSION_ATTRIBUTE,
    aggregation_for,
};
use crate::wire::{
    Aggregation, BuilderSpec, Filter, GroupByKey, QueryRangeRequest, QueryRangeResponse, TimeSeries,
};

/// The API key header. Not `Authorization`.
const API_KEY_HEADER: &str = "SIGNOZ-API-KEY";
const QUERY_RANGE_PATH: &str = "/api/v5/query_range";
const BACKEND_NAME: &str = "SigNoz";

/// How to reach a SigNoz instance.
#[derive(Debug, Clone)]
pub struct SignozConfig {
    /// Base URL, e.g. `https://eu.signoz.cloud`. The API path is appended.
    pub base_url: String,
    /// A service-account API key.
    pub api_key: String,
    /// Which naming convention this instance uses.
    pub naming: MetricNaming,
    /// How often the agents beat.
    ///
    /// This is the step `list_hosts` queries at, and it has to be this fine. A bucket's timestamp
    /// is the *start* of its bucket, so querying a 30-minute window in one bucket would report
    /// every host's last heartbeat as 30 minutes old and mark the whole fleet down.
    pub heartbeat_interval: Duration,
}

impl SignozConfig {
    /// # Errors
    /// Returns [`CoreError::Backend`] if the base URL has no scheme.
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>) -> Result<Self> {
        let base_url = base_url.into();
        if !base_url.starts_with("http://") && !base_url.starts_with("https://") {
            return Err(CoreError::Backend(format!(
                "SigNoz base URL {base_url:?} must start with http:// or https://"
            )));
        }
        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_owned(),
            api_key: api_key.into(),
            naming: MetricNaming::default(),
            // Taken from the domain's own default so the two cannot drift apart.
            heartbeat_interval: LivenessPolicy::default().heartbeat_interval(),
        })
    }

    #[must_use]
    pub fn with_naming(mut self, naming: MetricNaming) -> Self {
        self.naming = naming;
        self
    }

    /// Overrides the assumed heartbeat interval. Must match what the agents are configured with,
    /// or liveness will be measured against the wrong yardstick.
    #[must_use]
    pub fn with_heartbeat_interval(mut self, interval: Duration) -> Self {
        self.heartbeat_interval = interval;
        self
    }

    #[must_use]
    pub fn query_range_url(&self) -> String {
        format!("{}{QUERY_RANGE_PATH}", self.base_url)
    }
}

/// Reads metrics back out of SigNoz.
#[derive(Debug, Clone)]
pub struct SignozQuery {
    config: SignozConfig,
    http: Client,
}

impl SignozQuery {
    /// # Errors
    /// Returns [`CoreError::Backend`] if the HTTP client cannot be built.
    pub fn new(config: SignozConfig) -> Result<Self> {
        // Explicit, because `cargo test --workspace` unifies features and would otherwise
        // leave rustls enabled here, testing a different TLS stack than the one iOS ships.
        let http = Client::builder()
            .use_native_tls()
            .build()
            .map_err(|error| {
                CoreError::Backend(format!("could not build an HTTP client: {error}"))
            })?;
        Ok(Self { config, http })
    }

    /// Builds a client over an existing [`reqwest::Client`], so the caller can control timeouts,
    /// proxies, and connection reuse.
    #[must_use]
    pub fn with_client(config: SignozConfig, http: Client) -> Self {
        Self { config, http }
    }

    async fn post(&self, request: &QueryRangeRequest) -> Result<QueryRangeResponse> {
        let response = self
            .http
            .post(self.config.query_range_url())
            .header(API_KEY_HEADER, &self.config.api_key)
            .json(request)
            .send()
            .await
            .map_err(|error| {
                if error.is_timeout() || error.is_connect() {
                    CoreError::Unreachable(error.to_string())
                } else {
                    CoreError::Backend(error.to_string())
                }
            })?;

        let status = response.status();
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(CoreError::Unauthorized);
        }
        if !status.is_success() {
            // The body usually says more than the status does; include a bounded slice of it.
            let body = response.text().await.unwrap_or_default();
            let detail: String = body.chars().take(512).collect();
            return Err(CoreError::Backend(format!(
                "SigNoz returned {status}: {detail}"
            )));
        }

        response.json().await.map_err(|error| {
            CoreError::Backend(format!("could not parse the SigNoz response: {error}"))
        })
    }

    /// Builds the query body for one metric over one window.
    fn build_request(
        &self,
        metric: MetricKind,
        selector: &HostSelector,
        range: TimeRange,
        step: Duration,
    ) -> QueryRangeRequest {
        let (time_aggregation, space_aggregation) = aggregation_for(metric);
        QueryRangeRequest::time_series(
            range.start().timestamp_millis(),
            range.end().timestamp_millis(),
            BuilderSpec {
                name: "A",
                signal: "metrics",
                step_interval: step.num_seconds().max(1),
                aggregations: vec![Aggregation {
                    metric_name: self.config.naming.metric_name(metric),
                    temporality: "Unspecified",
                    time_aggregation,
                    space_aggregation,
                }],
                filter: self.filter_for(selector),
                group_by: self
                    .config
                    .naming
                    .group_by(metric)
                    .into_iter()
                    .map(|name| GroupByKey { name })
                    .collect(),
                disabled: false,
            },
        )
    }

    /// Translates a host selector into SigNoz's filter expression language.
    ///
    /// Returns `None` for [`HostSelector::All`], which means "no filter". An empty
    /// [`HostSelector::AnyOf`] is different: it selects nothing, and is expressed as a filter that
    /// cannot match rather than as no filter at all.
    fn filter_for(&self, selector: &HostSelector) -> Option<Filter> {
        let key = self.config.naming.attribute(HOST_NAME_ATTRIBUTE);
        match selector {
            HostSelector::All => None,
            HostSelector::Host(host) => Some(Filter {
                expression: format!("{key} = {}", quote(host.as_str())),
            }),
            HostSelector::AnyOf(hosts) => {
                let rendered: Vec<String> = hosts.iter().map(|h| quote(h.as_str())).collect();
                Some(Filter {
                    expression: format!("{key} IN [{}]", rendered.join(", ")),
                })
            }
        }
    }

    /// Turns one returned series into a [`MetricSeries`], or `None` if it carries no host.
    fn to_metric_series(&self, metric: MetricKind, series: &TimeSeries) -> Option<MetricSeries> {
        let mut labels = series.label_map();
        let host_key = self.config.naming.attribute(HOST_NAME_ATTRIBUTE);
        let host = labels.remove(&host_key)?;

        let points: Vec<MetricPoint> = series
            .values
            .iter()
            // A partial bucket does not cover its whole step; SigNoz says to ignore it, and
            // charting one puts a misleading dip at the edge of every window.
            .filter(|value| !value.partial)
            .filter_map(|value| {
                let at = DateTime::from_timestamp_millis(value.timestamp)?;
                value.value.as_finite().map(|v| MetricPoint::new(at, v))
            })
            .collect();

        Some(MetricSeries::new(HostId::new(host), metric, points).with_attributes(labels))
    }
}

/// Renders a value as a single-quoted literal for SigNoz's filter expression language.
fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\\', "\\\\").replace('\'', "\\'"))
}

#[async_trait]
impl TelemetryQuery for SignozQuery {
    async fn list_hosts(&self, range: TimeRange) -> Result<Vec<Host>> {
        // The heartbeat is the one metric every agent emits unconditionally, so it is the
        // cheapest complete roll call. Grouping by os.type and service.version fills the rest of
        // the host record from the same round trip.
        let naming = self.config.naming;
        let (time_aggregation, space_aggregation) = aggregation_for(MetricKind::AgentHeartbeat);
        let request = QueryRangeRequest::time_series(
            range.start().timestamp_millis(),
            range.end().timestamp_millis(),
            BuilderSpec {
                name: "A",
                signal: "metrics",
                // The heartbeat interval, not the window length. A bucket is timestamped at
                // its start, so a coarse step would age every host by up to a full bucket and
                // report a healthy fleet as down.
                step_interval: self.config.heartbeat_interval.num_seconds().max(1),
                aggregations: vec![Aggregation {
                    metric_name: naming.metric_name(MetricKind::AgentHeartbeat),
                    temporality: "Unspecified",
                    time_aggregation,
                    space_aggregation,
                }],
                filter: None,
                group_by: [
                    HOST_NAME_ATTRIBUTE,
                    OS_TYPE_ATTRIBUTE,
                    SERVICE_VERSION_ATTRIBUTE,
                ]
                .into_iter()
                .map(|name| GroupByKey {
                    name: naming.attribute(name),
                })
                .collect(),
                disabled: false,
            },
        );

        let response = self.post(&request).await?;
        let host_key = naming.attribute(HOST_NAME_ATTRIBUTE);
        let os_key = naming.attribute(OS_TYPE_ATTRIBUTE);
        let version_key = naming.attribute(SERVICE_VERSION_ATTRIBUTE);

        let mut hosts: Vec<Host> = Vec::new();
        for series in response
            .data
            .results
            .iter()
            .flat_map(|result| result.aggregations.iter())
            .flat_map(|bucket| bucket.series.iter())
        {
            let labels = series.label_map();
            let Some(name) = labels.get(&host_key) else {
                continue;
            };

            let os = labels
                .get(&os_key)
                .map_or(OsFamily::Other, |value| OsFamily::from_otel_value(value));
            let mut host = Host::new(HostId::new(name.clone()), os);
            if let Some(version) = labels.get(&version_key) {
                host = host.with_agent_version(version.clone());
            }
            // A partial bucket would report a heartbeat later than the one actually recorded, which
            // would make a stale host look alive.
            if let Some(latest) = series
                .values
                .iter()
                .filter(|value| !value.partial && value.value.as_finite().is_some())
                .filter_map(|value| DateTime::from_timestamp_millis(value.timestamp))
                .max()
            {
                host = host.with_last_heartbeat(latest);
            }

            // The same host can appear more than once when a label the query groups by changes
            // mid-window — an agent upgrade changes service.version. Keep the newest.
            match hosts.iter_mut().find(|existing| existing.id == host.id) {
                Some(existing) if existing.last_heartbeat < host.last_heartbeat => *existing = host,
                Some(_) => {}
                None => hosts.push(host),
            }
        }

        hosts.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(hosts)
    }

    async fn query_series(&self, request: &SeriesRequest) -> Result<Vec<MetricSeries>> {
        if matches!(&request.selector, HostSelector::AnyOf(hosts) if hosts.is_empty()) {
            return Ok(Vec::new());
        }

        let body = self.build_request(
            request.metric,
            &request.selector,
            request.range,
            request.step,
        );
        let response = self.post(&body).await?;

        Ok(response
            .data
            .results
            .iter()
            .flat_map(|result| result.aggregations.iter())
            .flat_map(|bucket| bucket.series.iter())
            .filter_map(|series| self.to_metric_series(request.metric, series))
            .collect())
    }

    async fn check_connection(&self) -> Result<()> {
        let now = Utc::now();
        let range = TimeRange::ending_at(now, Duration::minutes(5))?;
        let request = self.build_request(
            MetricKind::AgentHeartbeat,
            &HostSelector::All,
            range,
            Duration::minutes(5),
        );
        // An empty result is a pass: it proves the endpoint answered and the key was accepted,
        // which is all this checks. Whether any agent is reporting is a different question.
        self.post(&request).await.map(|_| ())
    }

    fn backend_name(&self) -> &str {
        BACKEND_NAME
    }
}
