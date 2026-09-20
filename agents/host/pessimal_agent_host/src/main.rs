//! The Pessimal host telemetry agent.
//!
//! Reads a TOML config, applies `PESSIMAL_*` environment overrides, and hands the OpenTelemetry
//! SDK an observable view of the host. The SDK owns the export schedule; this process exists to
//! keep it alive and to shut it down cleanly.

mod host_collector;
mod init;

use std::collections::BTreeMap;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use chrono::Duration;
use clap::{Parser, Subcommand};
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
    ///
    /// Optional rather than defaulted, because `init` needs to tell "the user named a path" from
    /// "nobody said": with a default it could not, and would write beside the working directory
    /// instead of where the service reads.
    #[arg(short, long, env = "PESSIMAL_CONFIG")]
    config: Option<PathBuf>,

    /// Validate the configuration, print what would be exported, and exit without connecting.
    #[arg(long)]
    check: bool,

    /// Take one sample and print it, then exit. Does not export.
    #[arg(long)]
    sample: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

/// What the agent does instead of running.
#[derive(Debug, Subcommand)]
enum Command {
    /// Set the agent up: ask where to send metrics, write the config, test it, start the service.
    Init(init::InitArgs),
}

fn main() -> ExitCode {
    // A daemon logs to stderr, and only colours it when something is there to read it.
    //
    // Two layers, each with its own filter, rather than one global filter: `init`'s export check
    // needs the exporter's DEBUG events to report why a backend refused a batch, and those must
    // not also appear on a user's terminal. PESSIMAL_LOG still decides what is printed.
    use tracing_subscriber::Layer;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    let printed = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal())
        .with_filter(
            tracing_subscriber::EnvFilter::try_from_env("PESSIMAL_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        );
    tracing_subscriber::registry()
        .with(printed)
        .with(init::export_diagnostics_layer())
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
    if let Some(Command::Init(args)) = &cli.command {
        return init::run(cli.config.clone(), args);
    }

    // Where `init` writes, not just the working directory: a config written to Homebrew's prefix
    // must be the one `pessimal-agent --check` reads, or the agent reports that its own config does
    // not exist. The error names every place it looked.
    let config_path = match cli.config.clone() {
        Some(path) => path,
        None => init::find_config()?,
    };
    let mut config = AgentConfig::from_file(&config_path)?;
    tracing::debug!(path = %config_path.display(), "read configuration");
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
    print_temperature_note(config, &mut collector);
    Ok(())
}

/// Says what became of the host's temperature sensors.
///
/// A host with no sensors, a host whose sensors none of the configured labels name, and a host
/// that was never asked for temperatures all print the same nothing otherwise. Only the agent can
/// tell those apart, so it says which one this is rather than leaving the reader to guess.
fn print_temperature_note(config: &AgentConfig, collector: &mut HostCollector) {
    if !config.collection.temperatures {
        println!(
            "hw.temperature: not collected; set [collection] temperatures = true to report it"
        );
        return;
    }
    let selection = collector.sensor_selection();
    if selection.seen.is_empty() {
        println!("hw.temperature: this host reports no sensors");
    } else if selection.kept.is_empty() {
        println!(
            "hw.temperature: no sensor matches [collection] temperature_sensors; \
             this host reports {}",
            selection.seen.join(", ")
        );
    } else {
        println!(
            "hw.temperature: reporting {} of this host's {} sensors",
            selection.kept.len(),
            selection.seen.len()
        );
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn there_is_no_api_key_argument() {
        // A key on the command line is visible in `ps` to every user on the machine, so `init`
        // takes it from a prompt or from stdin. This is the test that keeps someone from adding
        // the "convenient" flag back.
        Cli::try_parse_from(["pessimal-agent", "init", "--api-key", "secret"])
            .expect_err("--api-key must not exist");
        Cli::try_parse_from(["pessimal-agent", "init", "--api-key-stdin"])
            .expect("--api-key-stdin is how a script passes one");
    }

    #[test]
    fn the_config_path_is_absent_rather_than_defaulted() {
        // `init` tells "the user named a path" from "nobody said" by this being None, and writes
        // where the service reads instead of beside the working directory.
        let cli = Cli::try_parse_from(["pessimal-agent"]).expect("parses");
        assert_eq!(cli.config, None);
        let named =
            Cli::try_parse_from(["pessimal-agent", "--config", "/tmp/x.toml"]).expect("parses");
        assert_eq!(named.config, Some(PathBuf::from("/tmp/x.toml")));
    }
}
