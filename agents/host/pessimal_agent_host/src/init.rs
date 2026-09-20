//! `pessimal-agent init`: the onboarding flow's effects.
//!
//! Every decision is in [`pessimal_agent_core::onboarding`], which has no I/O and is tested there.
//! What is here is the parts that touch the world: asking questions on a terminal, reading the
//! answers, writing the file, exporting one batch to see whether the backend accepts it, and
//! starting the service.
//!
//! Questions and progress go to **stderr**, not stdout: the path of the file that was written is
//! the only thing printed to stdout, so `config=$(pessimal-agent init --non-interactive …)` works.

use std::fs;
use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use chrono::Duration;
use pessimal_agent_core::config::{AgentConfig, CollectionConfig};
use pessimal_agent_core::error::{AgentError, Result};
use pessimal_agent_core::onboarding::{
    self, Answers, ConfigLocation, KeyPlacement, LAUNCH_AGENT_LABEL, ServicePlan, SystemFacts,
    answer, merge_environment_file, render_toml,
};
use pessimal_agent_core::preset::{BackendPreset, ExportProtocol};
use pessimal_agent_core::resource::AgentIdentity;
use pessimal_agent_core::{CachedCollector, build_meter_provider, meter, register_instruments};
use pessimal_core::clock::SystemClock;

use tracing::field::{Field, Visit};
use tracing_subscriber::Layer;
use tracing_subscriber::filter::{LevelFilter, Targets};
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;

use crate::host_collector::HostCollector;
use crate::{AGENT_VERSION, detect_host_name};

/// Export failures the OTLP exporter logged, while [`ARMED`] is set.
///
/// The SDK does not return the backend's reason: a rejected key surfaces as
/// `InternalFailure("Failed to flush")`, and the actual answer — `Unauthenticated: Invalid or
/// missing key` — is a DEBUG event from the exporter that nobody at an `info` filter ever sees.
/// Onboarding that says "it failed" and not "your key was rejected" is not onboarding, so `init`
/// listens for those events while it exports.
static EXPORT_FAILURES: Mutex<Vec<String>> = Mutex::new(Vec::new());
static ARMED: AtomicBool = AtomicBool::new(false);

/// The layer that collects them. Installed by `main` for every run; it records nothing until
/// [`verify_export`] arms it, so a running agent pays one atomic load per exporter event.
pub struct ExportDiagnosticsLayer;

/// The layer, filtered to the exporter's own target so nothing else is even offered to it.
#[must_use]
pub fn export_diagnostics_layer<S>() -> impl Layer<S>
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
{
    ExportDiagnosticsLayer.with_filter(
        Targets::new()
            .with_target("opentelemetry-otlp", LevelFilter::DEBUG)
            .with_target("opentelemetry_sdk", LevelFilter::DEBUG),
    )
}

impl<S: tracing::Subscriber> Layer<S> for ExportDiagnosticsLayer {
    fn on_event(&self, event: &tracing::Event<'_>, _context: Context<'_, S>) {
        if !ARMED.load(Ordering::Relaxed) {
            return;
        }
        let mut visitor = FailureVisitor::default();
        event.record(&mut visitor);
        if let Some(reason) = visitor.into_reason()
            && let Ok(mut failures) = EXPORT_FAILURES.lock()
            && !failures.contains(&reason)
        {
            failures.push(reason);
        }
    }
}

/// Picks the export failure out of an exporter event's fields.
#[derive(Default)]
struct FailureVisitor {
    name: Option<String>,
    code: Option<String>,
    message: Option<String>,
    error: Option<String>,
}

impl FailureVisitor {
    /// A sentence, or `None` when this event is not an export failure.
    fn into_reason(self) -> Option<String> {
        if !self
            .name
            .as_deref()
            .is_some_and(|name| name.contains("ExportFailed"))
        {
            return None;
        }
        match (self.code, self.message, self.error) {
            (Some(code), Some(message), _) => Some(format!("{code}: {message}")),
            (Some(code), None, _) => Some(code),
            (None, Some(message), _) => Some(message),
            (None, None, Some(error)) => Some(error),
            (None, None, None) => None,
        }
    }
}

impl Visit for FailureVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        let value = value.to_owned();
        match field.name() {
            "name" => self.name = Some(value),
            name if name.ends_with("code") => self.code = Some(value),
            name if name.ends_with("message") => self.message = Some(value),
            "error" | "reason" => self.error = Some(value),
            _ => {}
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        // The exporter records some fields with Debug rather than as strings, and a quoted string
        // reads badly in a message, so the quotes come off.
        let rendered = format!("{value:?}");
        self.record_str(field, rendered.trim_matches('"'));
    }
}

/// Arguments for `init`.
///
/// There is deliberately no `--api-key`: a command-line argument is visible in `ps` to every user
/// on the machine. The key is typed at the prompt, or read from stdin with `--api-key-stdin`.
// Command-line switches, each independent of the others. Grouping them into enums to please the
// lint would change what clap generates, which is the user-facing part.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, clap::Args)]
pub struct InitArgs {
    /// Do not ask anything. Every value comes from these arguments or an existing config.
    #[arg(long)]
    pub non_interactive: bool,

    /// otlp, signoz, clickstack or honeycomb.
    #[arg(long)]
    pub preset: Option<BackendPreset>,

    /// Where metrics are sent, e.g. <https://ingest.eu.signoz.cloud:443>.
    #[arg(long)]
    pub endpoint: Option<String>,

    /// grpc or http/protobuf.
    #[arg(long)]
    pub protocol: Option<ExportProtocol>,

    /// The fleet this host belongs to.
    #[arg(long)]
    pub environment: Option<String>,

    /// Seconds between exports.
    #[arg(long)]
    pub interval_seconds: Option<u64>,

    /// Honeycomb's dataset. Ignored by every other preset.
    #[arg(long)]
    pub dataset: Option<String>,

    /// Read the API key from stdin, up to the first newline.
    #[arg(long)]
    pub api_key_stdin: bool,

    /// Report hardware temperatures, or not. Omitted, the question is asked.
    #[arg(long, num_args = 0..=1, default_missing_value = "true")]
    pub temperatures: Option<bool>,

    /// Which sensors to report, by the labels `--sample` lists. Omitted, every one of them.
    #[arg(long, value_delimiter = ',')]
    pub temperature_sensors: Option<Vec<String>>,

    /// Overwrite an existing config without asking. The previous file is kept as `<name>.bak`
    /// either way.
    #[arg(long)]
    pub force: bool,

    /// Start the service without asking.
    #[arg(long)]
    pub start: bool,

    /// Write the config and stop: no export, no service.
    #[arg(long)]
    pub no_verify: bool,
}

/// Runs the flow. `config_flag` is `--config`, or `PESSIMAL_CONFIG`, when either was given.
///
/// # Errors
/// Returns [`AgentError`] if there is nowhere to write, an answer cannot be accepted in
/// non-interactive mode, the file cannot be written, or the verification export fails.
pub fn run(config_flag: Option<PathBuf>, args: &InitArgs) -> Result<()> {
    let facts = gather_facts(config_flag);
    let location = ConfigLocation::resolve(&facts)?;
    say(&format!(
        "Config file: {} ({})",
        location.path.display(),
        location.reason
    ));

    let existing = read_existing(&location.path);
    if existing.is_some() {
        say(
            "A config is already there; its values are the defaults below, and it will be kept as a .bak.",
        );
    }

    let answers = if args.non_interactive {
        from_arguments(args, existing.as_ref())?
    } else {
        ask(args, existing.as_ref())?
    };
    let config = answers.into_config(existing)?;

    if !args.force
        && !args.non_interactive
        && location.path.exists()
        && !confirm("Write it?", true)?
    {
        return Err(AgentError::Config("nothing was written".to_owned()));
    }
    // Where the key goes decides the config's mode: see `KeyPlacement`.
    let placement = KeyPlacement::decide(&location.path, &facts);
    let mut on_disk = config.clone();
    if let KeyPlacement::EnvironmentFile(path) = &placement
        && let Some(key) = on_disk.export.api_key.take()
    {
        write_environment_file(path, &key)?;
        say(&format!(
            "Wrote the key to {} (mode 0600), which systemd reads as root. The config itself stays              readable, because the unit runs the agent as a transient user that has to read it.",
            path.display()
        ));
    }
    write_config(&location.path, &on_disk, placement.config_mode())?;
    say(&format!(
        "Wrote {} (mode {:04o}).",
        location.path.display(),
        placement.config_mode()
    ));

    if args.no_verify {
        say("Skipped the test export, as --no-verify asks.");
    } else {
        say("Exporting one batch to check the backend accepts it...");
        match verify_export(&config) {
            Ok(()) => say("The backend accepted a batch."),
            Err(reason) => {
                // One message, not two: `main` prints what is returned here, so saying it as well
                // would print the same failure twice. The config is written and valid — only the
                // backend refused — so the guidance is part of the error rather than a suggestion
                // to start over, which would throw away answers that are probably right.
                return Err(AgentError::Export(format!(
                    "{reason}. The config is written, at {}. Fix the endpoint or the key, then run `pessimal-agent init` again.",
                    location.path.display()
                )));
            }
        }
    }

    let plan = ServicePlan::detect(&facts);
    say(&plan.advice(&location.path));
    if let Some(command) = plan.command() {
        let wanted = args.start
            || (!args.non_interactive
                && confirm(&format!("Run `{}` now?", command.join(" ")), true)?);
        if wanted {
            run_command(&command)?;
            say("Started.");
        }
    }

    // stdout carries the path alone, so it can be captured.
    println!("{}", location.path.display());
    Ok(())
}

/// The config file to read when nobody named one: the first of the places `init` can write that
/// actually exists.
///
/// # Errors
/// Returns [`AgentError::Config`] naming every path tried when none of them holds a config.
pub fn find_config() -> Result<PathBuf> {
    onboarding::find_config(&gather_facts(None), std::path::Path::exists)
}

// ---------------------------------------------------------------------------------------------
// Facts

fn gather_facts(config_flag: Option<PathBuf>) -> SystemFacts {
    let homebrew_prefix = homebrew_prefix();
    let homebrew_config_dir_exists = homebrew_prefix
        .as_ref()
        .is_some_and(|prefix| prefix.join("etc/pessimal").is_dir());

    SystemFacts {
        explicit_config: config_flag,
        homebrew_prefix,
        homebrew_config_dir_exists,
        xdg_config_home: std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from),
        home: std::env::var_os("HOME").map(PathBuf::from),
        is_root: is_root(),
        is_macos: cfg!(target_os = "macos"),
        has_systemctl: on_path("systemctl"),
        systemd_unit_installed: [
            "/etc/systemd/system/pessimal-agent.service",
            "/usr/lib/systemd/system/pessimal-agent.service",
            "/lib/systemd/system/pessimal-agent.service",
        ]
        .iter()
        .any(|path| Path::new(path).exists()),
        launch_agent_loaded: launch_agent_loaded(),
    }
}

fn homebrew_prefix() -> Option<PathBuf> {
    if !on_path("brew") {
        return None;
    }
    let output = Command::new("brew").arg("--prefix").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let prefix = String::from_utf8(output.stdout).ok()?.trim().to_owned();
    (!prefix.is_empty()).then(|| PathBuf::from(prefix))
}

/// Whether the effective user is root.
///
/// Through `id -u` rather than `libc::geteuid`, because the agent's dependency graph is also what
/// Bazel resolves (`MODULE.bazel` reads this workspace's `Cargo.lock`), and one process spawn in an
/// interactive command is cheaper than a new crate in that graph.
fn is_root() -> bool {
    Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .is_some_and(|uid| uid.trim() == "0")
}

fn launch_agent_loaded() -> bool {
    if !cfg!(target_os = "macos") {
        return false;
    }
    let Some(uid) = Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
    else {
        return false;
    };
    Command::new("launchctl")
        .arg("print")
        .arg(format!("gui/{}/{LAUNCH_AGENT_LABEL}", uid.trim()))
        .output()
        .is_ok_and(|output| output.status.success())
}

fn on_path(binary: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|directory| directory.join(binary).is_file())
}

// ---------------------------------------------------------------------------------------------
// Answers

fn read_existing(path: &Path) -> Option<AgentConfig> {
    if !path.exists() {
        return None;
    }
    match AgentConfig::from_file(path) {
        Ok(config) => Some(config),
        Err(error) => {
            // Not fatal: a config this agent cannot read is exactly what init is for. It is kept
            // as a .bak, so nothing is lost by writing a fresh one over it.
            say(&format!(
                "The config already there could not be read ({error}); starting from defaults."
            ));
            None
        }
    }
}

fn from_arguments(args: &InitArgs, existing: Option<&AgentConfig>) -> Result<Answers> {
    let preset = args
        .preset
        .or_else(|| existing.map(|config| config.export.preset))
        .ok_or_else(|| {
            AgentError::Config("--preset is required with --non-interactive".to_owned())
        })?;
    let endpoint = match args.endpoint.as_deref() {
        Some(value) => answer::endpoint_for(value, preset)?,
        None => existing
            .map(|config| config.export.endpoint.clone())
            .ok_or_else(|| {
                AgentError::Config("--endpoint is required with --non-interactive".to_owned())
            })?,
    };
    let environment = match args.environment.as_deref() {
        Some(value) => answer::environment(value)?,
        None => existing
            .map(|config| config.resource.environment.clone())
            .ok_or_else(|| {
                AgentError::Config("--environment is required with --non-interactive".to_owned())
            })?,
    };
    let api_key = read_key_argument(args, preset, existing)?;
    let temperatures = args
        .temperatures
        .or_else(|| existing.map(|config| config.collection.temperatures))
        .unwrap_or(false);
    let temperature_sensors = if temperatures {
        args.temperature_sensors.clone().unwrap_or_else(|| {
            existing.map_or_else(Vec::new, |config| {
                config.collection.temperature_sensors.clone()
            })
        })
    } else {
        Vec::new()
    };

    Ok(Answers {
        preset,
        endpoint,
        protocol: args
            .protocol
            .or_else(|| existing.map(|config| config.export.protocol))
            .unwrap_or_default(),
        api_key,
        dataset: args
            .dataset
            .clone()
            .or_else(|| existing.and_then(|config| config.export.dataset.clone())),
        environment,
        interval_seconds: args
            .interval_seconds
            .or_else(|| existing.map(|config| config.export.interval_seconds))
            .unwrap_or(pessimal_agent_core::config::DEFAULT_INTERVAL_SECONDS),
        temperatures,
        temperature_sensors,
    })
}

fn read_key_argument(
    args: &InitArgs,
    preset: BackendPreset,
    existing: Option<&AgentConfig>,
) -> Result<Option<String>> {
    if args.api_key_stdin {
        let mut line = String::new();
        std::io::stdin()
            .lock()
            .read_line(&mut line)
            .map_err(|error| {
                AgentError::Config(format!("could not read the key from stdin: {error}"))
            })?;
        return answer::api_key(&line, preset);
    }
    // An existing key is kept rather than dropped: re-running init to change the environment must
    // not silently unauthenticate the agent.
    Ok(existing.and_then(|config| config.export.api_key.clone()))
}

fn ask(args: &InitArgs, existing: Option<&AgentConfig>) -> Result<Answers> {
    say("");
    say("Press Enter to accept the value in brackets.");

    let preset = match args.preset {
        Some(preset) => preset,
        None => ask_until(
            "Backend (otlp, signoz, clickstack, honeycomb)",
            { existing.map_or(BackendPreset::Signoz, |config| config.export.preset) }.wire_name(),
            str::parse::<BackendPreset>,
        )?,
    };

    let endpoint = match args.endpoint.as_deref() {
        Some(value) => answer::endpoint_for(value, preset)?,
        None => ask_until(
            "Endpoint URL",
            &existing.map_or_else(String::new, |config| config.export.endpoint.clone()),
            |raw| answer::endpoint_for(raw, preset),
        )?,
    };

    let protocol = match args.protocol {
        Some(protocol) => protocol,
        None => ask_until(
            "Protocol (grpc, http/protobuf)",
            &existing
                .map_or(ExportProtocol::Grpc, |config| config.export.protocol)
                .to_string(),
            str::parse::<ExportProtocol>,
        )?,
    };

    let api_key = if args.api_key_stdin {
        read_key_argument(args, preset, existing)?
    } else {
        let had_key = existing.and_then(|config| config.export.api_key.clone());
        let prompt = if had_key.is_some() {
            "API key (Enter keeps the one already in the config)"
        } else {
            "API key (Enter for none)"
        };
        let typed = ask_secret(prompt)?;
        if typed.trim().is_empty() && had_key.is_some() {
            had_key
        } else {
            answer::api_key(&typed, preset)?
        }
    };

    let dataset = if preset == BackendPreset::Honeycomb {
        Some(ask_until(
            "Honeycomb dataset",
            &existing
                .and_then(|config| config.export.dataset.clone())
                .unwrap_or_default(),
            |raw| {
                if raw.trim().is_empty() {
                    Err(AgentError::Config("Honeycomb needs a dataset".to_owned()))
                } else {
                    Ok(raw.trim().to_owned())
                }
            },
        )?)
    } else {
        existing.and_then(|config| config.export.dataset.clone())
    };

    let environment = match args.environment.as_deref() {
        Some(value) => answer::environment(value)?,
        None => ask_until(
            "Environment",
            &existing.map_or_else(
                || "production".to_owned(),
                |config| config.resource.environment.clone(),
            ),
            answer::environment,
        )?,
    };

    let interval_seconds = match args.interval_seconds {
        Some(seconds) => seconds,
        None => ask_until(
            "Seconds between exports",
            &existing
                .map_or(
                    pessimal_agent_core::config::DEFAULT_INTERVAL_SECONDS,
                    |config| config.export.interval_seconds,
                )
                .to_string(),
            answer::interval_seconds,
        )?,
    };

    let (temperatures, temperature_sensors) = ask_about_temperatures(args, existing)?;

    Ok(Answers {
        preset,
        endpoint,
        protocol,
        api_key,
        dataset,
        environment,
        interval_seconds,
        temperatures,
        temperature_sensors,
    })
}

/// Whether to report temperatures, and which sensors.
///
/// Asked against the sensors this host actually offers, listed by name, because the labels are not
/// guessable: on Apple silicon they are HID product strings, on Linux they are synthesised from
/// hwmon files, and nobody can type one they have not seen. A host that offers none is told so and
/// the question ends there — turning collection on would report nothing.
fn ask_about_temperatures(
    args: &InitArgs,
    existing: Option<&AgentConfig>,
) -> Result<(bool, Vec<String>)> {
    if let Some(wanted) = args.temperatures {
        let sensors = args.temperature_sensors.clone().unwrap_or_else(|| {
            existing.map_or_else(Vec::new, |config| {
                config.collection.temperature_sensors.clone()
            })
        });
        return Ok((wanted, if wanted { sensors } else { Vec::new() }));
    }

    let offered = sensors_on_this_host();
    if offered.is_empty() {
        say("This host reports no temperature sensors, so temperatures are left off.");
        return Ok((false, Vec::new()));
    }

    say("");
    say(&format!(
        "This host reports {} temperature sensors:",
        offered.len()
    ));
    for label in &offered {
        say(&format!("  {label}"));
    }

    let was_on = existing.is_some_and(|config| config.collection.temperatures);
    if !confirm("Report temperatures?", was_on)? {
        return Ok((false, Vec::new()));
    }

    let previous = existing
        .map(|config| config.collection.temperature_sensors.join(", "))
        .filter(|list| !list.is_empty())
        .unwrap_or_else(|| "all".to_owned());
    let chosen = ask_until(
        "Which sensors, separated by commas (or `all`)",
        &previous,
        |raw| select_sensor_names(raw, &offered),
    )?;
    Ok((true, chosen))
}

/// Turns the typed list into sensor labels, refusing a name this host does not offer.
///
/// Pure, and beside the flow rather than inside it, so "a typo is caught while the list is still on
/// screen" is a test rather than a hope.
fn select_sensor_names(raw: &str, offered: &[String]) -> Result<Vec<String>> {
    let value = raw.trim();
    if value.is_empty() || value.eq_ignore_ascii_case("all") {
        // Empty rather than every label: the config means "every sensor this host offers", which
        // stays true when the host grows one.
        return Ok(Vec::new());
    }
    let mut chosen = Vec::new();
    for name in value
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
    {
        let Some(label) = offered.iter().find(|label| label.as_str() == name) else {
            return Err(AgentError::Config(format!(
                "this host offers no sensor called {name:?}. Type the names as they are listed, or `all`."
            )));
        };
        if !chosen.contains(label) {
            chosen.push(label.clone());
        }
    }
    if chosen.is_empty() {
        return Err(AgentError::Config(
            "name at least one sensor, or `all`".to_owned(),
        ));
    }
    Ok(chosen)
}

/// The sensor labels this host offers right now.
fn sensors_on_this_host() -> Vec<String> {
    let probing = CollectionConfig {
        temperatures: true,
        ..CollectionConfig::default()
    };
    HostCollector::new(&probing).sensor_selection().seen
}

/// Asks until the answer is accepted, showing the reason each time.
///
/// Re-asking rather than failing is the point: an onboarding flow that exits on a typo in the last
/// question and throws away the other five answers is the thing this command replaces.
fn ask_until<T, E: std::fmt::Display>(
    question: &str,
    default: &str,
    parse: impl Fn(&str) -> std::result::Result<T, E>,
) -> Result<T> {
    loop {
        let raw = prompt_line(&format!("{question} [{default}]: "))?;
        let value = if raw.trim().is_empty() {
            default
        } else {
            raw.trim()
        };
        match parse(value) {
            Ok(parsed) => return Ok(parsed),
            Err(error) => say(&format!("  {error}")),
        }
    }
}

fn confirm(question: &str, default_yes: bool) -> Result<bool> {
    let hint = if default_yes { "Y/n" } else { "y/N" };
    loop {
        let raw = prompt_line(&format!("{question} [{hint}]: "))?;
        match raw.trim().to_ascii_lowercase().as_str() {
            "" => return Ok(default_yes),
            "y" | "yes" => return Ok(true),
            "n" | "no" => return Ok(false),
            _ => say("  answer y or n"),
        }
    }
}

fn prompt_line(question: &str) -> Result<String> {
    eprint!("{question}");
    std::io::stderr().flush().ok();
    let mut line = String::new();
    let read = std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|error| AgentError::Config(format!("could not read your answer: {error}")))?;
    if read == 0 {
        return Err(AgentError::Config(
            "stdin ended while asking a question; use --non-interactive with arguments".to_owned(),
        ));
    }
    Ok(line)
}

/// Asks without echoing what is typed.
///
/// Through `stty` rather than a crate: see [`is_root`] for why a new dependency is not free here.
/// If `stty` is missing, or stdin is not a terminal, the answer is read normally — and when it is
/// not a terminal there is nothing to echo it to anyway.
fn ask_secret(question: &str) -> Result<String> {
    let interactive = std::io::stdin().is_terminal();
    let hidden = interactive && set_echo(false);
    let answer = prompt_line(&format!("{question}: "));
    if hidden {
        set_echo(true);
        // The user's Enter was swallowed with the echo, so the cursor is still on the prompt line.
        eprintln!();
    }
    answer
}

fn set_echo(on: bool) -> bool {
    Command::new("stty")
        .arg(if on { "echo" } else { "-echo" })
        .stdin(std::process::Stdio::inherit())
        .status()
        .is_ok_and(|status| status.success())
}

fn say(message: &str) {
    eprintln!("{message}");
}

// ---------------------------------------------------------------------------------------------
// Effects

/// Writes the config at mode 0600, keeping any previous file as `<name>.bak`.
///
/// Atomically: a temporary file in the same directory, then a rename. A half-written config is a
/// config the service cannot start with, and `init` is often run while the service is running.
fn write_config(path: &Path, config: &AgentConfig, mode: u32) -> Result<()> {
    let rendered = render_toml(config);
    // Parse what is about to be written, not what was built: a rendering bug must not reach disk.
    let parsed = AgentConfig::from_toml(&rendered)?;
    if &parsed != config {
        return Err(AgentError::Config(
            "the rendered config does not read back as what was configured; nothing was written"
                .to_owned(),
        ));
    }

    if let Some(directory) = path.parent() {
        fs::create_dir_all(directory).map_err(|error| {
            AgentError::Config(format!("could not create {}: {error}", directory.display()))
        })?;
    }
    if path.exists() {
        let backup = path.with_extension("toml.bak");
        fs::copy(path, &backup).map_err(|error| {
            AgentError::Config(format!(
                "could not back up to {}: {error}",
                backup.display()
            ))
        })?;
        say(&format!(
            "Kept the previous config as {}.",
            backup.display()
        ));
    }

    write_atomically(path, rendered.as_bytes(), mode)
}

/// Adds or replaces `PESSIMAL_API_KEY` in a systemd `EnvironmentFile`, at mode 0600.
fn write_environment_file(path: &Path, api_key: &str) -> Result<()> {
    let existing = fs::read_to_string(path).unwrap_or_default();
    let merged = merge_environment_file(&existing, api_key);
    if let Some(directory) = path.parent() {
        fs::create_dir_all(directory).map_err(|error| {
            AgentError::Config(format!("could not create {}: {error}", directory.display()))
        })?;
    }
    write_atomically(path, merged.as_bytes(), 0o600)
}

/// Writes through a temporary file in the same directory, then renames.
///
/// The mode is set before the rename, so the file is never briefly readable by anyone it should
/// not be.
fn write_atomically(path: &Path, contents: &[u8], mode: u32) -> Result<()> {
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, contents).map_err(|error| {
        AgentError::Config(format!("could not write {}: {error}", temporary.display()))
    })?;
    set_mode(&temporary, mode)?;
    fs::rename(&temporary, path)
        .map_err(|error| AgentError::Config(format!("could not move into place: {error}")))?;
    Ok(())
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).map_err(|error| {
        AgentError::Config(format!(
            "could not set mode {mode:04o} on {}: {error}",
            path.display()
        ))
    })
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> Result<()> {
    Ok(())
}

/// Exports one batch and reports what the backend said.
///
/// The same code path the running agent uses — the real exporter, the real collector, one flush —
/// because a check that builds an exporter without sending anything is what `--check` already does,
/// and it cannot tell a wrong key from a right one.
fn verify_export(config: &AgentConfig) -> std::result::Result<(), String> {
    if let Ok(mut failures) = EXPORT_FAILURES.lock() {
        failures.clear();
    }
    ARMED.store(true, Ordering::Relaxed);
    let outcome = export_once(config);
    ARMED.store(false, Ordering::Relaxed);

    // A reason, not an error: the caller wraps it once, and wrapping an AgentError::Export in
    // another prints "export failed: export failed:".
    outcome.map_err(|error| {
        let reasons = EXPORT_FAILURES
            .lock()
            .map(|failures| failures.join("; "))
            .unwrap_or_default();
        if reasons.is_empty() {
            // The SDK's own words. Nearly useless on their own — which is what the layer above is
            // for — but better than nothing when the exporter logged no reason at all.
            error.to_string()
        } else {
            // The backend's own words, which is what tells a wrong key from a wrong endpoint.
            format!("the backend refused it — {reasons}")
        }
    })
}

fn export_once(config: &AgentConfig) -> Result<()> {
    let identity = AgentIdentity::from_config(&config.resource, &detect_host_name(), AGENT_VERSION);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .map_err(|error| {
            AgentError::Export(format!("could not start the async runtime: {error}"))
        })?;

    let provider = {
        let _guard = runtime.enter();
        build_meter_provider(config, &identity)?
    };
    let collector = Arc::new(CachedCollector::new(
        Box::new(HostCollector::new(&config.collection)),
        Duration::seconds(1),
        SystemClock,
    ));
    let instruments = register_instruments(&meter(&provider), &collector);

    // force_flush collects every instrument and exports once, synchronously, so its result is the
    // backend's answer rather than a queue's.
    let flushed = provider.force_flush();
    drop(instruments);
    let shutdown = provider.shutdown();
    flushed.map_err(|error| AgentError::Export(error.to_string()))?;
    shutdown.map_err(|error| AgentError::Export(error.to_string()))
}

fn run_command(argv: &[String]) -> Result<()> {
    let (binary, arguments) = argv
        .split_first()
        .ok_or_else(|| AgentError::Config("no command to run".to_owned()))?;
    let status = Command::new(binary)
        .args(arguments)
        .status()
        .map_err(|error| AgentError::Config(format!("could not run `{binary}`: {error}")))?;
    if status.success() {
        Ok(())
    } else {
        Err(AgentError::Config(format!(
            "`{}` exited with {status}",
            argv.join(" ")
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offered() -> Vec<String> {
        vec![
            "PMU tdev1".to_owned(),
            "gas gauge battery".to_owned(),
            "NAND CH0 temp".to_owned(),
        ]
    }

    #[test]
    fn all_means_every_sensor_including_the_ones_this_host_grows_later() {
        // Empty, not the three labels: the config means "every sensor this host offers", which
        // stays true when a future macOS names one differently.
        for answer in ["", "  ", "all", "ALL"] {
            assert!(
                select_sensor_names(answer, &offered())
                    .expect("accepted")
                    .is_empty(),
                "{answer:?} should mean every sensor"
            );
        }
    }

    #[test]
    fn named_sensors_are_kept_in_the_order_they_were_typed() {
        assert_eq!(
            select_sensor_names("NAND CH0 temp, PMU tdev1", &offered()).expect("accepted"),
            vec!["NAND CH0 temp".to_owned(), "PMU tdev1".to_owned()]
        );
    }

    #[test]
    fn a_name_this_host_does_not_offer_is_refused_while_the_list_is_still_on_screen() {
        // These labels cannot be guessed — HID product strings on Apple silicon, synthesised hwmon
        // names on Linux — so a typo must be caught here rather than becoming a config that
        // reports nothing and explains nothing.
        let error = select_sensor_names("CPU", &offered()).expect_err("not offered");
        let message = format!("{error}");
        assert!(message.contains("\"CPU\""), "{message}");
        assert!(message.contains("as they are listed"), "{message}");
    }

    #[test]
    fn a_repeated_name_is_kept_once() {
        assert_eq!(
            select_sensor_names("PMU tdev1, PMU tdev1", &offered()).expect("accepted"),
            vec!["PMU tdev1".to_owned()]
        );
    }

    #[test]
    fn a_list_of_nothing_but_commas_is_refused() {
        select_sensor_names(",, ,", &offered()).expect_err("no sensor named");
    }
}
