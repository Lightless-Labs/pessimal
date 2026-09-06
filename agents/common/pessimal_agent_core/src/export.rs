//! OTLP export.
//!
//! Builds the meter provider and registers one observable instrument per modelled metric. The SDK
//! drives everything from here: there is no export loop in the agent, only a process that stays
//! alive.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration as StdDuration;

use opentelemetry::KeyValue;
use opentelemetry::metrics::{Meter, MeterProvider};
use opentelemetry_otlp::{
    MetricExporter, Protocol, WithExportConfig, WithHttpConfig, WithTonicConfig,
};
use opentelemetry_sdk::metrics::{PeriodicReader, SdkMeterProvider};
use pessimal_core::{Clock, InstrumentKind, MetricKind};

use crate::collector::CachedCollector;
use crate::config::AgentConfig;
use crate::error::{AgentError, Result};
use crate::preset::ExportProtocol;
use crate::resource::AgentIdentity;

/// The instrumentation scope every Pessimal metric is emitted under.
pub const INSTRUMENTATION_SCOPE: &str = "pessimal.agent";

/// The OTLP/HTTP path metrics are posted to.
const METRICS_PATH: &str = "/v1/metrics";

/// Resolves the URL the HTTP exporter should post to.
///
/// `export.endpoint` is a *base* URL, the way `OTEL_EXPORTER_OTLP_ENDPOINT` is: an operator
/// configures `http://collector:4318` and both protocols work from the same value. The OTLP crate
/// treats an explicitly-set endpoint as signal-specific and would post to `/`, which every
/// collector answers with a 404, so the signal path is appended here.
///
/// Appending is idempotent, so an endpoint that already names the path is left alone, and a
/// gateway prefix such as `https://gw.example/otlp` becomes `https://gw.example/otlp/v1/metrics`.
fn metrics_endpoint(endpoint: &str) -> String {
    let trimmed = endpoint.trim_end().trim_end_matches('/');
    if trimmed.ends_with(METRICS_PATH) {
        trimmed.to_owned()
    } else {
        format!("{trimmed}{METRICS_PATH}")
    }
}

/// Builds the OTLP metric exporter for the configured protocol and headers.
///
/// # Errors
/// Returns [`AgentError::Exporter`] if the endpoint or credentials are rejected, and
/// [`AgentError::Config`] if the preset's requirements are unmet.
pub fn build_exporter(config: &AgentConfig) -> Result<MetricExporter> {
    let headers = config.export.resolved_headers()?;
    let timeout = StdDuration::from_secs(config.export.timeout_seconds);

    for name in config.export.overridden_preset_headers() {
        tracing::warn!(
            header = %name,
            preset = %config.export.preset,
            "an explicit header overrides one the preset would have set"
        );
    }

    match config.export.protocol {
        ExportProtocol::Grpc => {
            let mut metadata = tonic::metadata::MetadataMap::new();
            for (name, value) in &headers {
                let key: tonic::metadata::MetadataKey<_> = name.parse().map_err(|_| {
                    AgentError::Config(format!("{name:?} is not a valid header name"))
                })?;
                let value = value.parse().map_err(|_| {
                    AgentError::Config(format!("the value for header {name:?} is not valid ASCII"))
                })?;
                metadata.insert(key, value);
            }
            MetricExporter::builder()
                .with_tonic()
                .with_endpoint(&config.export.endpoint)
                .with_timeout(timeout)
                .with_metadata(metadata)
                .build()
        }
        ExportProtocol::HttpProtobuf => {
            let headers: HashMap<String, String> = headers.into_iter().collect();
            MetricExporter::builder()
                .with_http()
                .with_endpoint(metrics_endpoint(&config.export.endpoint))
                .with_protocol(Protocol::HttpBinary)
                .with_timeout(timeout)
                .with_headers(headers)
                .build()
        }
    }
    .map_err(|error| AgentError::Exporter(error.to_string()))
}

/// Builds the meter provider: exporter, periodic reader at the configured interval, and the
/// agent's resource attributes.
///
/// # Errors
/// As [`build_exporter`].
pub fn build_meter_provider(
    config: &AgentConfig,
    identity: &AgentIdentity,
) -> Result<SdkMeterProvider> {
    let exporter = build_exporter(config)?;
    let reader = PeriodicReader::builder(exporter)
        .with_interval(StdDuration::from_secs(config.export.interval_seconds))
        .build();

    Ok(SdkMeterProvider::builder()
        .with_resource(identity.resource())
        .with_reader(reader)
        .build())
}

/// Registers an observable instrument for every modelled metric.
///
/// The returned handles must be kept alive: dropping an instrument unregisters its callback, and
/// the metric silently stops being exported.
pub fn register_instruments<C>(
    meter: &Meter,
    collector: &Arc<CachedCollector<C>>,
) -> RegisteredInstruments
where
    C: Clock + 'static,
{
    let mut gauges = Vec::new();
    let mut counters = Vec::new();

    for kind in MetricKind::ALL {
        // The agent's own counters are driven by the exporter, not by a host sample.
        if matches!(
            kind,
            MetricKind::AgentHeartbeat | MetricKind::AgentCollectionFailures
        ) {
            continue;
        }
        let source = Arc::clone(collector);
        let observe = move |observer: &dyn opentelemetry::metrics::AsyncInstrument<f64>| {
            for observation in source.snapshot().observations_of(kind) {
                let attributes: Vec<KeyValue> = observation
                    .attributes
                    .iter()
                    .map(|(name, value)| KeyValue::new(name.clone(), value.clone()))
                    .collect();
                observer.observe(observation.value, &attributes);
            }
        };

        match kind.instrument_kind() {
            InstrumentKind::Gauge => gauges.push(
                meter
                    .f64_observable_gauge(kind.otel_name())
                    .with_unit(kind.unit().otel_unit())
                    .with_description(kind.display_name())
                    .with_callback(observe)
                    .build(),
            ),
            InstrumentKind::Counter => counters.push(
                meter
                    .f64_observable_counter(kind.otel_name())
                    .with_unit(kind.unit().otel_unit())
                    .with_description(kind.display_name())
                    .with_callback(observe)
                    .build(),
            ),
        }
    }

    // The heartbeat counts export cycles, not samples: it says the process is alive and can reach
    // the backend. Whether it can still read the host is what collection_failures reports.
    let beats = Arc::new(AtomicU64::new(0));
    let beat_source = Arc::clone(&beats);
    let heartbeat = meter
        .u64_observable_counter(MetricKind::AgentHeartbeat.otel_name())
        .with_unit(MetricKind::AgentHeartbeat.unit().otel_unit())
        .with_description("Export cycles completed since the agent started")
        .with_callback(move |observer| {
            let beat = beat_source.fetch_add(1, Ordering::Relaxed) + 1;
            observer.observe(beat, &[]);
        })
        .build();

    let failure_source = Arc::clone(collector);
    let failures = meter
        .u64_observable_counter(MetricKind::AgentCollectionFailures.otel_name())
        .with_unit(MetricKind::AgentCollectionFailures.unit().otel_unit())
        .with_description("Host sampling failures since the agent started")
        .with_callback(move |observer| {
            observer.observe(failure_source.total_failures(), &[]);
        })
        .build();

    RegisteredInstruments {
        gauges,
        counters,
        heartbeat,
        failures,
        beats,
    }
}

/// Handles for the registered instruments. Keep this alive for as long as the agent runs.
pub struct RegisteredInstruments {
    gauges: Vec<opentelemetry::metrics::ObservableGauge<f64>>,
    counters: Vec<opentelemetry::metrics::ObservableCounter<f64>>,
    heartbeat: opentelemetry::metrics::ObservableCounter<u64>,
    failures: opentelemetry::metrics::ObservableCounter<u64>,
    beats: Arc<AtomicU64>,
}

impl RegisteredInstruments {
    /// How many export cycles have run. Test and diagnostic use.
    #[must_use]
    pub fn beats(&self) -> u64 {
        self.beats.load(Ordering::Relaxed)
    }

    /// How many host-metric instruments are registered.
    #[must_use]
    pub fn instrument_count(&self) -> usize {
        self.gauges.len() + self.counters.len()
    }
}

impl std::fmt::Debug for RegisteredInstruments {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegisteredInstruments")
            .field("gauges", &self.gauges.len())
            .field("counters", &self.counters.len())
            .field("beats", &self.beats())
            .finish_non_exhaustive()
    }
}

/// A meter over the agent's instrumentation scope.
#[must_use]
pub fn meter(provider: &SdkMeterProvider) -> Meter {
    provider.meter(INSTRUMENTATION_SCOPE)
}

/// Silences the `unused` warning for fields kept solely to hold registrations open.
impl Drop for RegisteredInstruments {
    fn drop(&mut self) {
        let _ = (&self.heartbeat, &self.failures, &self.counters);
    }
}

#[cfg(test)]
mod tests {
    use super::metrics_endpoint;

    #[test]
    fn appends_the_signal_path_to_a_base_endpoint() {
        assert_eq!(
            metrics_endpoint("http://localhost:4318"),
            "http://localhost:4318/v1/metrics"
        );
    }

    #[test]
    fn tolerates_a_trailing_slash() {
        assert_eq!(
            metrics_endpoint("http://localhost:4318/"),
            "http://localhost:4318/v1/metrics"
        );
    }

    #[test]
    fn is_idempotent_when_the_path_is_already_there() {
        assert_eq!(
            metrics_endpoint("http://localhost:4318/v1/metrics"),
            "http://localhost:4318/v1/metrics"
        );
    }

    #[test]
    fn preserves_a_gateway_path_prefix() {
        assert_eq!(
            metrics_endpoint("https://gw.example/otlp"),
            "https://gw.example/otlp/v1/metrics"
        );
    }
}
