//! The Pessimal host telemetry agent.
//!
//! Reads a TOML config, applies `PESSIMAL_*` environment overrides, and hands the OpenTelemetry
//! SDK an observable view of the host. The SDK owns the export schedule; this process exists to
//! keep it alive and to shut it down cleanly.

mod host_collector;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use chrono::Duration;
use clap::Parser;
use host_collector::HostCollector;
use pessimal_agent_core::collector::CachedCollector;
use pessimal_agent_core::config::AgentConfig;
use pessimal_agent_core::error::{AgentError, Result};
use pessimal_agent_core::resource::AgentIdentity;
use pessimal_agent_core::{build_meter_provider, meter, register_instruments};
use pessimal_core::clock::SystemClock;

const AGENT_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Parser)]
#[command(
    name = "pessimal-agent",
    version,
    about = "Collect host telemetry and export it over OTLP."
)]
struct Cli {
    /// Path to the TOML configuration file.
    #[arg(short, long, env = "PESSIMAL_CONFIG", default_value = "pessimal.toml")]
    config: PathBuf,

    /// Validate the configuration, print what would be exported, and exit without connecting.
    #[arg(long)]
    check: bool,

    /// Take one sample and print it, then exit. Does not export.
    #[arg(long)]
    sample: bool,
}

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("PESSIMAL_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    match run(&Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(%error, "pessimal-agent failed");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: &Cli) -> Result<()> {
    let mut config = AgentConfig::from_file(&cli.config)?;
    config.apply_env(&environment())?;

    if cli.sample {
        return print_sample(&config);
    }

    let host_name = detect_host_name();
    let identity = AgentIdentity::from_config(&config.resource, &host_name, AGENT_VERSION);

    tracing::info!(
        host = %identity.host_name,
        environment = %identity.environment,
        preset = %config.export.preset,
        endpoint = %config.export.endpoint,
        protocol = %config.export.protocol,
        interval_seconds = config.export.interval_seconds,
        "pessimal-agent configured"
    );

    if cli.check {
        // Building the exporter validates the endpoint and credentials without exporting. The
        // gRPC builder needs a runtime context even though it connects lazily.
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| {
                AgentError::Export(format!("could not start the async runtime: {error}"))
            })?;
        let _guard = runtime.enter();
        pessimal_agent_core::export::build_exporter(&config)?;
        tracing::info!("configuration is valid");
        return Ok(());
    }

    serve(&config, &identity)
}

/// Runs until interrupted.
///
/// The runtime is built here rather than by wrapping `main` in `#[tokio::main]` so the `--check`
/// and `--sample` paths, which need no runtime, do not pay for one — and so that this function can
/// control exactly which parts of the lifecycle run *inside* a runtime context.
///
/// That control matters. The tonic gRPC exporter needs a runtime alive for the process lifetime,
/// and captures a handle to it when the channel is built. The HTTP exporter uses a *blocking*
/// reqwest client, which refuses to run inside a runtime context at all. So construction happens
/// under an `enter()` guard that is dropped immediately, and the final flush in `shutdown()`
/// happens outside any runtime context — which is what both exporters want.
fn serve(config: &AgentConfig, identity: &AgentIdentity) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .map_err(|error| {
            AgentError::Export(format!("could not start the async runtime: {error}"))
        })?;

    let provider = {
        let _guard = runtime.enter();
        build_meter_provider(config, identity)?
    };

    // Half the export interval: long enough that one export cycle's callbacks share a single
    // sample, short enough that the next cycle always resamples.
    let max_age =
        Duration::seconds(i64::try_from(config.export.interval_seconds).unwrap_or(i64::MAX) / 2)
            .max(Duration::seconds(1));

    let collector = Arc::new(CachedCollector::new(
        Box::new(HostCollector::new(&config.collection)),
        max_age,
        SystemClock,
    ));

    let instruments = register_instruments(&meter(&provider), &collector);
    tracing::info!(
        instruments = instruments.instrument_count(),
        "exporting; press Ctrl-C to stop"
    );

    runtime.block_on(wait_for_shutdown());

    tracing::info!(
        beats = instruments.beats(),
        collection_failures = collector.total_failures(),
        "shutting down; flushing a final export"
    );
    drop(instruments);
    provider
        .shutdown()
        .map_err(|error| AgentError::Export(error.to_string()))
}

/// Resolves on SIGINT, and on SIGTERM where the platform has it — a service manager stopping the
/// agent should get the same clean final flush as a Ctrl-C.
async fn wait_for_shutdown() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut terminate = match signal(SignalKind::terminate()) {
            Ok(stream) => stream,
            Err(error) => {
                tracing::warn!(%error, "could not listen for SIGTERM; SIGINT only");
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

fn print_sample(config: &AgentConfig) -> Result<()> {
    use pessimal_agent_core::collector::MetricCollector;

    let mut collector = HostCollector::new(&config.collection);
    // The first sample after construction carries a CPU delta measured over a few milliseconds.
    // Sample twice so the printed value covers a usable window.
    std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
    let observations = collector.collect(chrono::Utc::now())?;

    for observation in observations {
        let attributes = if observation.attributes.is_empty() {
            String::new()
        } else {
            let pairs: Vec<String> = observation
                .attributes
                .iter()
                .map(|(name, value)| format!("{name}={value}"))
                .collect();
            format!("  {{{}}}", pairs.join(", "))
        };
        println!(
            "{:<40} {:>18.4} {}{}",
            observation.kind.otel_name(),
            observation.value,
            observation.kind.unit().otel_unit(),
            attributes
        );
    }
    Ok(())
}

/// The process environment as a plain map, so config override logic stays pure.
fn environment() -> BTreeMap<String, String> {
    std::env::vars().collect()
}

/// The system hostname, or a placeholder that is obviously wrong rather than plausibly wrong.
fn detect_host_name() -> String {
    match hostname::get() {
        Ok(name) => name.to_string_lossy().into_owned(),
        Err(error) => {
            tracing::warn!(
                %error,
                "could not read the system hostname; set resource.host_name in the config"
            );
            "unknown-host".to_owned()
        }
    }
}
