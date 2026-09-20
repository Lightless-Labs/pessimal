//! What `pessimal-agent init` decides, with no I/O.
//!
//! Every decision the onboarding flow makes lives here: which answers are acceptable, where the
//! config file belongs, what the file says, and which service manager starts the agent on this
//! machine. The binary supplies the facts it gathered ([`SystemFacts`]) and performs the effects.
//!
//! The split is what makes onboarding testable. "A Homebrew install writes to the prefix the
//! service reads from" is a unit test here, not something discovered on a user's laptop.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::config::{
    AgentConfig, CollectionConfig, DEFAULT_INTERVAL_SECONDS, DEFAULT_TIMEOUT_SECONDS, ExportConfig,
    ResourceConfig,
};
use crate::error::{AgentError, Result};
use crate::preset::{BackendPreset, ExportProtocol};

/// The file `init` writes, under whichever directory it picks.
pub const CONFIG_FILE_NAME: &str = "pessimal.toml";

/// What the user is asked, once validated.
///
/// Held separately from [`AgentConfig`] because these are the only things `init` asks about: an
/// existing config's other fields — extra headers, resource attributes, filesystem list — are
/// carried through untouched rather than silently reset to a default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answers {
    pub preset: BackendPreset,
    pub endpoint: String,
    pub protocol: ExportProtocol,
    /// `None` when the backend needs no credential. SigNoz self-hosted is the case that has none:
    /// it ingests unauthenticated, so the preset accepts a missing token.
    pub api_key: Option<String>,
    /// Honeycomb only, where metrics go to a named dataset. `None` everywhere else.
    pub dataset: Option<String>,
    pub environment: String,
    pub interval_seconds: u64,
}

impl Answers {
    /// Builds the config, keeping everything `init` does not ask about from `existing`.
    ///
    /// # Errors
    /// Returns [`AgentError::Config`] if the answers do not make a valid config — a missing
    /// credential for a preset that needs one, most often.
    pub fn into_config(self, existing: Option<AgentConfig>) -> Result<AgentConfig> {
        let previous = existing.unwrap_or_else(|| AgentConfig {
            export: ExportConfig {
                preset: BackendPreset::default(),
                endpoint: String::new(),
                protocol: ExportProtocol::default(),
                interval_seconds: DEFAULT_INTERVAL_SECONDS,
                timeout_seconds: DEFAULT_TIMEOUT_SECONDS,
                api_key: None,
                dataset: None,
                headers: BTreeMap::new(),
            },
            resource: ResourceConfig::default(),
            collection: CollectionConfig::default(),
        });

        // A timeout longer than the interval makes exports overlap, which `validate` refuses. The
        // user is not asked about timeouts, so a shortened interval brings it down with it rather
        // than failing on a value they never chose.
        let timeout_seconds = previous.export.timeout_seconds.min(self.interval_seconds);

        let config = AgentConfig {
            export: ExportConfig {
                preset: self.preset,
                endpoint: self.endpoint.trim().to_owned(),
                protocol: self.protocol,
                interval_seconds: self.interval_seconds,
                timeout_seconds,
                api_key: self.api_key.filter(|key| !key.trim().is_empty()),
                dataset: self
                    .dataset
                    .filter(|dataset| !dataset.trim().is_empty())
                    .or(previous.export.dataset),
                ..previous.export
            },
            resource: ResourceConfig {
                environment: self.environment.trim().to_owned(),
                ..previous.resource
            },
            collection: previous.collection,
        };
        config.validate()?;
        // Credentials too, unlike `AgentConfig::from_file`: the user is answering the questions
        // right now, so "the environment will supply the key later" is not a case here.
        config.validate_credentials()?;
        Ok(config)
    }
}

/// Checks one answer at the moment it is typed, so the flow can ask again.
///
/// Each returns the cleaned value, or the sentence to show the user. They repeat rules
/// [`AgentConfig::validate`] also enforces, deliberately: validate is the gate, these are the
/// explanations, and a flow that only found out at the end would throw away every other answer.
pub mod answer {
    use super::{AgentError, BackendPreset, Result};

    /// Trims, requires an absolute http(s) URL, and refuses a SigNoz Cloud *console* address.
    ///
    /// The console address is the one in the browser, and it is the obvious thing to paste. It is
    /// also a single-page app that answers 200 to any path, so an OTLP exporter posting to it is
    /// told everything is fine, for ever, while nothing is ingested — measured on a user's Mac,
    /// 2026-09-16, where a test export "succeeded" against
    /// `https://infinite-chimp.us2.signoz.cloud/` and no host ever appeared.
    ///
    /// # Errors
    /// Returns [`AgentError::Config`] with a sentence naming what is wrong, and for the console
    /// address the ingest address to use instead.
    pub fn endpoint_for(raw: &str, preset: BackendPreset) -> Result<String> {
        let value = endpoint(raw)?;
        if preset != BackendPreset::Signoz {
            return Ok(value);
        }
        let host = value
            .split("://")
            .nth(1)
            .unwrap_or_default()
            .split(['/', ':'])
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        // Only SigNoz Cloud, whose addresses are all `<workspace>.<region>.signoz.cloud`. A
        // self-hosted instance has whatever hostname its owner gave it and is left alone.
        if !host.ends_with(".signoz.cloud") || host.starts_with("ingest.") {
            return Ok(value);
        }
        let region = host
            .strip_suffix(".signoz.cloud")
            .and_then(|rest| rest.rsplit('.').next())
            .filter(|region| !region.is_empty())
            .unwrap_or("<region>");
        Err(AgentError::Config(format!(
            "{host} is the SigNoz console, not its ingest endpoint. The console answers every \
             request, so metrics sent there are accepted and dropped. Use \
             https://ingest.{region}.signoz.cloud:443"
        )))
    }

    /// Trims, and requires an absolute http(s) URL.
    ///
    /// # Errors
    /// Returns [`AgentError::Config`] with a sentence naming what is wrong.
    pub fn endpoint(raw: &str) -> Result<String> {
        let value = raw.trim();
        if value.is_empty() {
            return Err(AgentError::Config(
                "an endpoint is required, for example https://ingest.eu.signoz.cloud:443"
                    .to_owned(),
            ));
        }
        if !(value.starts_with("http://") || value.starts_with("https://")) {
            return Err(AgentError::Config(format!(
                "{value:?} must start with http:// or https://"
            )));
        }
        if value.split("://").nth(1).is_none_or(str::is_empty) {
            return Err(AgentError::Config(format!(
                "{value:?} has no host after the scheme"
            )));
        }
        // A trailing slash is harmless for gRPC and wrong for HTTP, where the SDK appends
        // `/v1/metrics` to it and produces a double slash.
        Ok(value.trim_end_matches('/').to_owned())
    }

    /// Trims, and refuses the URN separator and whitespace inside the name.
    ///
    /// # Errors
    /// Returns [`AgentError::Config`] with a sentence naming what is wrong.
    pub fn environment(raw: &str) -> Result<String> {
        let value = raw.trim();
        if value.is_empty() {
            return Err(AgentError::Config(
                "an environment is required, for example production or infrastructure".to_owned(),
            ));
        }
        if value.contains("::") {
            return Err(AgentError::Config(
                "an environment cannot contain `::`, which separates the parts of an entity's URN"
                    .to_owned(),
            ));
        }
        if value.chars().any(char::is_whitespace) {
            return Err(AgentError::Config(format!(
                "{value:?} contains a space; environments are single words, like staging"
            )));
        }
        Ok(value.to_owned())
    }

    /// Parses the seconds between exports.
    ///
    /// # Errors
    /// Returns [`AgentError::Config`] for anything that is not a positive whole number of seconds.
    pub fn interval_seconds(raw: &str) -> Result<u64> {
        let value = raw.trim();
        let seconds: u64 = value.parse().map_err(|_| {
            AgentError::Config(format!("{value:?} is not a whole number of seconds"))
        })?;
        if seconds == 0 {
            return Err(AgentError::Config(
                "the interval must be at least 1 second".to_owned(),
            ));
        }
        Ok(seconds)
    }

    /// The API key, or `None` when the preset needs none and the user gave none.
    ///
    /// # Errors
    /// Returns [`AgentError::Config`] if the preset requires a credential and none was given.
    pub fn api_key(raw: &str, preset: BackendPreset) -> Result<Option<String>> {
        let value = raw.trim();
        if !value.is_empty() {
            return Ok(Some(value.to_owned()));
        }
        // The preset itself is the authority on whether a credential is required, which keeps this
        // from drifting when a preset changes. SigNoz answers "not required": a self-hosted
        // instance ingests unauthenticated, and only SigNoz Cloud needs the token.
        if preset
            .headers(&crate::preset::Credentials::default())
            .is_ok()
        {
            return Ok(None);
        }
        Err(AgentError::Config(format!(
            "{} needs an API key",
            preset.display_name()
        )))
    }
}

/// What the binary measured about this machine, for the decisions below.
///
/// A plain struct rather than a set of function calls so every case — Homebrew or not, root or not,
/// `systemctl` present or not — is a value a test can write down.
// Each bool is an independent measurement of the machine, not a state with invalid combinations:
// a two-variant enum per fact would say `HomebrewConfigDir::Present` where `true` reads better, and
// there is no state machine to extract.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SystemFacts {
    /// `--config`, or `PESSIMAL_CONFIG`. Wins over everything.
    pub explicit_config: Option<PathBuf>,
    /// `brew --prefix`, when `brew` is on `PATH`.
    pub homebrew_prefix: Option<PathBuf>,
    /// Whether `<homebrew prefix>/etc/pessimal` exists, which means the formula is installed.
    pub homebrew_config_dir_exists: bool,
    /// `$XDG_CONFIG_HOME`, when set.
    pub xdg_config_home: Option<PathBuf>,
    /// The user's home directory.
    pub home: Option<PathBuf>,
    /// Effective uid 0.
    pub is_root: bool,
    /// `cfg!(target_os = "macos")` in the binary.
    pub is_macos: bool,
    /// Whether `systemctl` is on `PATH`.
    pub has_systemctl: bool,
    /// Whether the systemd unit file is installed.
    pub systemd_unit_installed: bool,
    /// Whether `scripts/install-agent-launchd.sh`'s `LaunchAgent` is loaded.
    pub launch_agent_loaded: bool,
}

/// Where the config file goes, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigLocation {
    pub path: PathBuf,
    /// One sentence, shown before anything is written.
    pub reason: &'static str,
}

impl ConfigLocation {
    /// Picks the path. See the table in `docs/plans/2026-09-16-m10-agent-onboarding.md`.
    ///
    /// The Homebrew case is first among the defaults on purpose: `brew services` starts the agent
    /// with `--config <prefix>/etc/pessimal/pessimal.toml`, so a config written anywhere else would
    /// not be the one the service reads.
    ///
    /// # Errors
    /// Returns [`AgentError::Config`] when there is nowhere to write: no explicit path, no
    /// Homebrew, not root, and no home directory.
    pub fn resolve(facts: &SystemFacts) -> Result<Self> {
        if let Some(path) = &facts.explicit_config {
            return Ok(Self {
                path: path.clone(),
                reason: "you asked for this path",
            });
        }
        if facts.homebrew_config_dir_exists
            && let Some(prefix) = &facts.homebrew_prefix
        {
            return Ok(Self {
                path: prefix.join("etc/pessimal").join(CONFIG_FILE_NAME),
                reason: "the Homebrew formula is installed, and `brew services` reads this path",
            });
        }
        if facts.is_root && !facts.is_macos {
            return Ok(Self {
                path: Path::new("/etc/pessimal").join(CONFIG_FILE_NAME),
                reason: "you are root, and the systemd unit reads this path",
            });
        }
        if let Some(xdg) = &facts.xdg_config_home {
            return Ok(Self {
                path: xdg.join("pessimal").join(CONFIG_FILE_NAME),
                reason: "XDG_CONFIG_HOME is set",
            });
        }
        if let Some(home) = &facts.home {
            return Ok(Self {
                path: home.join(".config/pessimal").join(CONFIG_FILE_NAME),
                reason: "your home directory",
            });
        }
        Err(AgentError::Config(
            "there is nowhere to write a config: no --config, no Homebrew, not root, and no home \
             directory. Pass --config PATH."
                .to_owned(),
        ))
    }
}

/// Every place a config could be, in the order the agent should look when nobody named one.
///
/// Reading is not writing. `init` picks *one* place to write; a later `pessimal-agent --check` has
/// to find whatever is already there, wherever `init` put it — and after `init` wrote to Homebrew's
/// prefix, a bare `pessimal-agent` that only looked at `./pessimal.toml` said "No such file or
/// directory" while a perfectly good config sat where the service reads it. Measured on a user's
/// Mac, 2026-09-16.
///
/// The working directory comes first so a developer's local `pessimal.toml` still wins.
#[must_use]
pub fn config_candidates(facts: &SystemFacts) -> Vec<PathBuf> {
    let mut candidates = vec![PathBuf::from(CONFIG_FILE_NAME)];
    if let Some(prefix) = &facts.homebrew_prefix {
        candidates.push(prefix.join("etc/pessimal").join(CONFIG_FILE_NAME));
    }
    if let Some(xdg) = &facts.xdg_config_home {
        candidates.push(xdg.join("pessimal").join(CONFIG_FILE_NAME));
    }
    if let Some(home) = &facts.home {
        candidates.push(home.join(".config/pessimal").join(CONFIG_FILE_NAME));
    }
    candidates.push(Path::new("/etc/pessimal").join(CONFIG_FILE_NAME));
    candidates.dedup();
    candidates
}

/// The first candidate that exists, or every place that was looked at.
///
/// `exists` is injected so the order can be tested without a filesystem.
///
/// # Errors
/// Returns [`AgentError::Config`] naming every path tried, and `pessimal-agent init`, when none of
/// them is there.
pub fn find_config(facts: &SystemFacts, exists: impl Fn(&Path) -> bool) -> Result<PathBuf> {
    let candidates = config_candidates(facts);
    if let Some(found) = candidates.iter().find(|path| exists(path)) {
        return Ok(found.clone());
    }
    let tried = candidates
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    Err(AgentError::Config(format!(
        "no configuration file found. Looked in: {tried}. Run `pessimal-agent init` to write one,          or pass --config PATH."
    )))
}

/// Where the API key is written, which decides the config file's mode.
///
/// Not always the config file. The packaged systemd unit runs the agent under `DynamicUser=yes`, a
/// transient account that must be able to read `/etc/pessimal/pessimal.toml` — so that file cannot
/// be 0600, and a key inside it would be readable by that account and anyone else on the machine.
/// systemd reads `EnvironmentFile=-/etc/pessimal/pessimal.env` as root instead, which is why the
/// tarball's instructions put the key there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyPlacement {
    /// In the config file itself, which is then mode 0600. Everywhere the agent runs as the user
    /// who owns the config: Homebrew, a `LaunchAgent`, a per-user install.
    ConfigFile,
    /// In this environment file at mode 0600, as `PESSIMAL_API_KEY=…`, leaving the config readable.
    EnvironmentFile(PathBuf),
}

/// The environment file the packaged systemd unit reads.
pub const SYSTEMD_ENVIRONMENT_FILE: &str = "/etc/pessimal/pessimal.env";

impl KeyPlacement {
    /// Decides from where the config is going.
    ///
    /// Keyed on the config path rather than on "is this Linux": a root install at
    /// `/etc/pessimal/pessimal.toml` is the systemd layout whether or not the unit file is there
    /// yet, and a user-level config on the same machine is not.
    #[must_use]
    pub fn decide(config_path: &Path, facts: &SystemFacts) -> Self {
        if !facts.is_macos && config_path.starts_with("/etc/") {
            return Self::EnvironmentFile(PathBuf::from(SYSTEMD_ENVIRONMENT_FILE));
        }
        Self::ConfigFile
    }

    /// The mode the config file gets: nobody but the owner when it holds the key, readable when the
    /// service's own user has to read it.
    #[must_use]
    pub fn config_mode(&self) -> u32 {
        match self {
            Self::ConfigFile => 0o600,
            Self::EnvironmentFile(_) => 0o644,
        }
    }
}

/// Rewrites an environment file so it carries this key, keeping every other line.
///
/// A whole-file overwrite would drop what else is in there — `PESSIMAL_LOG=debug` is in the
/// tarball's own instructions — and re-running `init` must not silently turn someone's logging off.
#[must_use]
pub fn merge_environment_file(existing: &str, api_key: &str) -> String {
    const ASSIGNMENT: &str = "PESSIMAL_API_KEY=";
    let mut lines: Vec<String> = Vec::new();
    let mut replaced = false;
    for line in existing.lines() {
        if line.trim_start().starts_with(ASSIGNMENT) {
            lines.push(format!("{ASSIGNMENT}{api_key}"));
            replaced = true;
        } else {
            lines.push(line.to_owned());
        }
    }
    if !replaced {
        lines.push(format!("{ASSIGNMENT}{api_key}"));
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// How the agent is started on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServicePlan {
    /// `brew services start pessimal-agent`.
    BrewServices,
    /// The `LaunchAgent` from `scripts/install-agent-launchd.sh` is already loaded.
    LaunchAgentLoaded { label: String },
    /// `sudo systemctl enable --now pessimal-agent`.
    Systemd { needs_sudo: bool },
    /// Nothing to start: no service manager was found, so the agent runs in the foreground.
    Foreground,
}

/// The `LaunchAgent` label `scripts/install-agent-launchd.sh` uses.
pub const LAUNCH_AGENT_LABEL: &str = "com.lightless-labs.pessimal.agent";

impl ServicePlan {
    /// Chooses from the facts. Installs nothing, ever: writing a service unit is what
    /// `scripts/install-agent-launchd.sh` and the tarball's systemd unit are for.
    #[must_use]
    pub fn detect(facts: &SystemFacts) -> Self {
        // An already-loaded LaunchAgent comes first: telling a user to start a second copy through
        // Homebrew is how a host ends up reporting itself twice.
        if facts.launch_agent_loaded {
            return Self::LaunchAgentLoaded {
                label: LAUNCH_AGENT_LABEL.to_owned(),
            };
        }
        if facts.homebrew_config_dir_exists && facts.homebrew_prefix.is_some() {
            return Self::BrewServices;
        }
        if facts.has_systemctl && facts.systemd_unit_installed {
            return Self::Systemd {
                needs_sudo: !facts.is_root,
            };
        }
        Self::Foreground
    }

    /// The command to run, as argv. `None` when there is nothing to run.
    #[must_use]
    #[allow(clippy::match_same_arms)]
    pub fn command(&self) -> Option<Vec<String>> {
        let argv: Vec<&str> = match self {
            Self::BrewServices => vec!["brew", "services", "start", "pessimal-agent"],
            // Nothing to run: it is already running, which is not the same as Foreground's
            // "there is nothing here to run it", and the two print different advice.
            Self::LaunchAgentLoaded { .. } => return None,
            Self::Systemd { needs_sudo: true } => {
                vec!["sudo", "systemctl", "enable", "--now", "pessimal-agent"]
            }
            Self::Systemd { needs_sudo: false } => {
                vec!["systemctl", "enable", "--now", "pessimal-agent"]
            }
            Self::Foreground => return None,
        };
        Some(argv.into_iter().map(ToOwned::to_owned).collect())
    }

    /// What to tell the user, whether or not `init` runs the command.
    #[must_use]
    pub fn advice(&self, config_path: &Path) -> String {
        match self {
            Self::BrewServices => "Start it with: brew services start pessimal-agent\n\
                 It then starts again at every login."
                .to_owned(),
            Self::LaunchAgentLoaded { label } => format!(
                "The LaunchAgent {label} is already running this agent, and it starts at login.\n\
                 Pick it up with the new config: launchctl kickstart -k gui/$(id -u)/{label}"
            ),
            Self::Systemd { needs_sudo } => {
                let sudo = if *needs_sudo { "sudo " } else { "" };
                format!(
                    "Start it with: {sudo}systemctl enable --now pessimal-agent\n\
                     It then starts again at every boot."
                )
            }
            Self::Foreground => format!(
                "No service manager was found, so nothing starts this agent yet.\n\
                 Run it in the foreground with: pessimal-agent --config {}\n\
                 On macOS, scripts/install-agent-launchd.sh installs it as a LaunchAgent; on Linux, \
                 the tarball's pessimal-agent.service is a systemd unit.",
                config_path.display()
            ),
        }
    }
}

/// Renders the config as commented TOML.
///
/// Written by hand rather than through `toml::to_string`, because a generated file a person will
/// later edit should say what each value means. The result must parse back into the same
/// `AgentConfig`, which is what `a_rendered_config_parses_back_to_itself` checks.
#[must_use]
pub fn render_toml(config: &AgentConfig) -> String {
    let mut out = String::new();
    out.push_str("# Written by `pessimal-agent init`. Edit it by hand, or run init again.\n");
    out.push_str("# Every value can be overridden by a PESSIMAL_* environment variable.\n\n");

    render_export(&mut out, config);
    render_resource(&mut out, config);
    render_collection(&mut out, config);
    out
}

/// The `[export]` table: where metrics go and how often.
fn render_export(out: &mut String, config: &AgentConfig) {
    out.push_str("[export]\n");
    out.push_str("# The backend's shape: otlp, signoz, clickstack or honeycomb.\n");
    let _ = writeln!(
        out,
        "preset = {}",
        toml_string(config.export.preset.wire_name())
    );
    out.push_str("# Where metrics are sent.\n");
    let _ = writeln!(out, "endpoint = {}", toml_string(&config.export.endpoint));
    out.push_str(
        "# grpc or http. Prefer grpc: over http an authentication failure is reported as\n",
    );
    out.push_str("# a network error, which is much harder to read.\n");
    let _ = writeln!(
        out,
        "protocol = {}",
        toml_string(&config.export.protocol.to_string())
    );
    out.push_str(
        "# Seconds between exports. This is also the heartbeat the apps judge liveness by.\n",
    );
    let _ = writeln!(out, "interval_seconds = {}", config.export.interval_seconds);
    out.push_str("# Seconds before one export attempt is abandoned. Never above the interval.\n");
    let _ = writeln!(out, "timeout_seconds = {}", config.export.timeout_seconds);
    if let Some(key) = &config.export.api_key {
        out.push_str("# The ingestion key. This file is mode 0600 because of this line.\n");
        let _ = writeln!(out, "api_key = {}", toml_string(key));
    }
    if let Some(dataset) = &config.export.dataset {
        let _ = writeln!(out, "dataset = {}", toml_string(dataset));
    }
    if !config.export.headers.is_empty() {
        out.push_str("\n[export.headers]\n");
        for (name, value) in &config.export.headers {
            let _ = writeln!(out, "{} = {}", toml_key(name), toml_string(value));
        }
    }
}

/// The `[resource]` table: what this host calls itself.
fn render_resource(out: &mut String, config: &AgentConfig) {
    out.push_str("\n[resource]\n");
    let _ = writeln!(
        out,
        "service_name = {}",
        toml_string(&config.resource.service_name)
    );
    out.push_str(
        "# The fleet this host belongs to. The apps filter by it, and every alert rule id\n",
    );
    out.push_str("# embeds it. Changing it on a running host splits that host's history in two.\n");
    let _ = writeln!(
        out,
        "environment = {}",
        toml_string(&config.resource.environment)
    );
    if let Some(host_name) = &config.resource.host_name {
        out.push_str("# Overrides the detected hostname.\n");
        let _ = writeln!(out, "host_name = {}", toml_string(host_name));
    }
    if !config.resource.attributes.is_empty() {
        out.push_str("\n[resource.attributes]\n");
        for (name, value) in &config.resource.attributes {
            let _ = writeln!(out, "{} = {}", toml_key(name), toml_string(value));
        }
    }
}

/// The `[collection]` table: what is sampled.
///
/// Every key of `CollectionConfig` must appear here. `init` parses its own render back and
/// refuses to write when the two differ, so a key missing from this function does not lose a
/// setting quietly — it stops `init` running at all on a host that set it.
fn render_collection(out: &mut String, config: &AgentConfig) {
    out.push_str("\n[collection]\n");
    out.push_str(
        "# Mount points to report. Empty means every mount, which on a laptop is a great many.\n",
    );
    let _ = writeln!(
        out,
        "filesystems = [{}]",
        config
            .collection
            .filesystems
            .iter()
            .map(|mount| toml_string(mount))
            .collect::<Vec<_>>()
            .join(", ")
    );
    out.push_str("# Report each network interface separately rather than one host total.\n");
    let _ = writeln!(
        out,
        "per_interface_network = {}",
        config.collection.per_interface_network
    );
    out.push_str(
        "# Report hardware temperatures. Off by default: how many sensors a host has is unbounded\n",
    );
    out.push_str("# until it is measured, and every sensor is a series of its own.\n");
    let _ = writeln!(out, "temperatures = {}", config.collection.temperatures);
    out.push_str("# Sensor labels to report. Empty means every one the host offers.\n");
    let _ = writeln!(
        out,
        "temperature_sensors = [{}]",
        config
            .collection
            .temperature_sensors
            .iter()
            .map(|sensor| toml_string(sensor))
            .collect::<Vec<_>>()
            .join(", ")
    );
}

/// A TOML basic string. Escapes what the spec requires, so a key with a quote or a backslash in it
/// cannot break the file it is written into.
fn toml_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for character in value.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            control if control.is_control() => {
                let _ = write!(out, "\\u{:04X}", control as u32);
            }
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

/// A TOML key. Bare where the spec allows it, quoted otherwise — header names contain `-`, which is
/// bare-legal, but resource attributes contain `.`, which is not.
fn toml_key(key: &str) -> String {
    let bare = !key.is_empty()
        && key.chars().all(|character| {
            character.is_ascii_alphanumeric() || character == '_' || character == '-'
        });
    if bare {
        key.to_owned()
    } else {
        toml_string(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answers() -> Answers {
        Answers {
            preset: BackendPreset::Signoz,
            endpoint: "https://ingest.eu.signoz.cloud:443".to_owned(),
            protocol: ExportProtocol::Grpc,
            api_key: Some("key".to_owned()),
            dataset: None,
            environment: "infrastructure".to_owned(),
            interval_seconds: 30,
        }
    }

    #[test]
    fn answers_become_a_valid_config() {
        let config = answers().into_config(None).expect("valid");
        assert_eq!(config.export.preset, BackendPreset::Signoz);
        assert_eq!(config.resource.environment, "infrastructure");
        assert_eq!(config.export.api_key.as_deref(), Some("key"));
        config.validate().expect("the config validates");
    }

    #[test]
    fn what_init_does_not_ask_about_survives() {
        let existing = AgentConfig::from_toml(
            r#"
            [export]
            endpoint = "http://localhost:4317"
            [export.headers]
            "x-tenant" = "cabbage"
            [resource]
            host_name = "renamed"
            [resource.attributes]
            "deployment.region" = "eu-west-1"
            [collection]
            filesystems = ["/", "/data"]
            per_interface_network = true
            temperatures = true
            temperature_sensors = ["PMU tdev1", "gas gauge battery"]
        "#,
        )
        .expect("valid");

        let config = answers().into_config(Some(existing)).expect("valid");

        assert_eq!(
            config.export.headers.get("x-tenant").map(String::as_str),
            Some("cabbage")
        );
        assert_eq!(config.resource.host_name.as_deref(), Some("renamed"));
        assert_eq!(
            config
                .resource
                .attributes
                .get("deployment.region")
                .map(String::as_str),
            Some("eu-west-1")
        );
        assert_eq!(
            config.collection.filesystems,
            vec!["/".to_owned(), "/data".to_owned()]
        );
        assert!(config.collection.per_interface_network);
        assert!(config.collection.temperatures);
        assert_eq!(
            config.collection.temperature_sensors,
            vec!["PMU tdev1".to_owned(), "gas gauge battery".to_owned()]
        );
    }

    #[test]
    fn a_rendered_config_carries_every_collection_key() {
        // A key the renderer forgets is a key `init` destroys: `write_config` parses its own render
        // back and refuses to write when the two differ, so an un-rendered key makes `init` fail
        // outright on a host that set it. Adding `temperatures` to `CollectionConfig` without
        // adding it here did exactly that, and no test caught it because every fixture left the new
        // keys at their defaults, where "missing" and "default" parse the same.
        let config = AgentConfig::from_toml(
            r#"
            [export]
            endpoint = "http://localhost:4317"
            [collection]
            filesystems = []
            per_interface_network = true
            temperatures = true
            temperature_sensors = ["PMU tdev1"]
        "#,
        )
        .expect("valid");

        let rendered = render_toml(&config);
        assert_eq!(
            AgentConfig::from_toml(&rendered).expect("the render parses"),
            config,
            "every field of a non-default config must survive the round trip:\n{rendered}"
        );
    }

    #[test]
    fn a_shorter_interval_brings_the_timeout_down_with_it() {
        // The user is never asked about timeouts, so an interval below the existing timeout must
        // not produce a config that `validate` refuses for a value nobody chose.
        let existing = AgentConfig::from_toml(
            r#"
            [export]
            endpoint = "http://localhost:4317"
            interval_seconds = 60
            timeout_seconds = 30
        "#,
        )
        .expect("valid");
        let mut answers = answers();
        answers.interval_seconds = 5;

        let config = answers.into_config(Some(existing)).expect("valid");

        assert_eq!(config.export.interval_seconds, 5);
        assert_eq!(config.export.timeout_seconds, 5);
    }

    #[test]
    fn an_empty_key_is_no_key() {
        let mut answers = answers();
        answers.preset = BackendPreset::Otlp;
        answers.api_key = Some("   ".to_owned());
        answers.endpoint = "http://localhost:4317".to_owned();
        assert_eq!(
            answers.into_config(None).expect("valid").export.api_key,
            None
        );
    }

    #[test]
    fn a_preset_that_needs_a_key_refuses_without_one() {
        let mut answers = answers();
        answers.preset = BackendPreset::Clickstack;
        answers.api_key = None;
        let error = answers
            .into_config(None)
            .expect_err("clickstack needs a key");
        let message = format!("{error}").to_lowercase();
        assert!(
            message.contains("key") || message.contains("token"),
            "the error should name the missing credential: {error}"
        );
    }

    #[test]
    fn self_hosted_signoz_needs_no_token() {
        // SigNoz Cloud requires one; a self-hosted instance ingests unauthenticated, and onboarding
        // must not invent a requirement the backend does not have.
        let mut answers = answers();
        answers.endpoint = "http://signoz.internal:4317".to_owned();
        answers.api_key = None;
        let config = answers.into_config(None).expect("valid without a token");
        assert_eq!(config.export.api_key, None);
    }

    #[test]
    fn honeycomb_carries_its_dataset() {
        let mut answers = answers();
        answers.preset = BackendPreset::Honeycomb;
        answers.dataset = Some("pessimal".to_owned());
        let config = answers.into_config(None).expect("valid");
        assert_eq!(config.export.dataset.as_deref(), Some("pessimal"));
        let parsed = AgentConfig::from_toml(&render_toml(&config)).expect("the render parses");
        assert_eq!(parsed, config);
    }

    #[test]
    fn a_rendered_config_parses_back_to_itself() {
        let config = answers().into_config(None).expect("valid");
        let parsed = AgentConfig::from_toml(&render_toml(&config)).expect("the render parses");
        assert_eq!(parsed, config);
    }

    #[test]
    fn a_rendered_config_keeps_headers_and_attributes_that_need_quoting() {
        let existing = AgentConfig::from_toml(
            r#"
            [export]
            endpoint = "http://localhost:4317"
            [export.headers]
            "x-tenant" = "say \"cabbage\""
            [resource.attributes]
            "deployment.region" = "eu-west-1"
        "#,
        )
        .expect("valid");
        let config = answers().into_config(Some(existing)).expect("valid");

        let parsed = AgentConfig::from_toml(&render_toml(&config)).expect("the render parses");

        assert_eq!(parsed, config);
        assert_eq!(
            parsed.export.headers.get("x-tenant").map(String::as_str),
            Some("say \"cabbage\"")
        );
    }

    #[test]
    fn the_rendered_file_explains_the_fields_it_writes() {
        let rendered = render_toml(&answers().into_config(None).expect("valid"));
        assert!(
            rendered.contains("pessimal-agent init"),
            "it says where it came from"
        );
        assert!(
            rendered.contains("# The ingestion key"),
            "the key line is explained"
        );
        assert!(
            rendered.contains("heartbeat"),
            "the interval says what else depends on it"
        );
    }

    #[test]
    fn an_endpoint_must_be_an_absolute_http_url() {
        assert_eq!(
            answer::endpoint("  https://example.com:443 ").expect("valid"),
            "https://example.com:443"
        );
        for bad in ["", "   ", "example.com", "grpc://example.com", "https://"] {
            answer::endpoint(bad).expect_err(bad);
        }
    }

    #[test]
    fn the_signoz_console_address_is_refused_with_the_ingest_one() {
        let error = answer::endpoint_for(
            "https://infinite-chimp.us2.signoz.cloud/",
            BackendPreset::Signoz,
        )
        .expect_err("the console is not an ingest endpoint");
        let message = format!("{error}");
        assert!(
            message.contains("https://ingest.us2.signoz.cloud:443"),
            "{message}"
        );

        // The ingest address itself, and self-hosted instances, pass.
        assert_eq!(
            answer::endpoint_for("https://ingest.us2.signoz.cloud:443", BackendPreset::Signoz)
                .expect("valid"),
            "https://ingest.us2.signoz.cloud:443"
        );
        assert_eq!(
            answer::endpoint_for("http://signoz.internal:4317", BackendPreset::Signoz)
                .expect("valid"),
            "http://signoz.internal:4317"
        );
        // Another backend's host is none of this check's business.
        assert_eq!(
            answer::endpoint_for("https://anything.signoz.cloud", BackendPreset::Otlp)
                .expect("valid"),
            "https://anything.signoz.cloud"
        );
    }

    #[test]
    fn a_trailing_slash_is_trimmed() {
        // The HTTP exporter appends `/v1/metrics`, so a trailing slash makes a double slash.
        assert_eq!(
            answer::endpoint("https://ingest.eu.signoz.cloud:443/").expect("valid"),
            "https://ingest.eu.signoz.cloud:443"
        );
    }

    #[test]
    fn an_environment_is_one_word_without_the_urn_separator() {
        assert_eq!(
            answer::environment(" production ").expect("valid"),
            "production"
        );
        for bad in ["", "two words", "a::b"] {
            answer::environment(bad).expect_err(bad);
        }
    }

    #[test]
    fn an_interval_is_a_positive_number_of_seconds() {
        assert_eq!(answer::interval_seconds(" 45 ").expect("valid"), 45);
        for bad in ["", "0", "-1", "30s", "thirty"] {
            answer::interval_seconds(bad).expect_err(bad);
        }
    }

    #[test]
    fn a_key_is_required_exactly_when_the_preset_needs_one() {
        assert_eq!(
            answer::api_key("", BackendPreset::Otlp).expect("otlp needs none"),
            None
        );
        assert_eq!(
            answer::api_key("", BackendPreset::Signoz).expect("self-hosted signoz needs none"),
            None
        );
        assert_eq!(
            answer::api_key(" k ", BackendPreset::Signoz).expect("given"),
            Some("k".to_owned())
        );
        answer::api_key("", BackendPreset::Clickstack).expect_err("clickstack needs a key");
        answer::api_key("", BackendPreset::Honeycomb).expect_err("honeycomb needs a key");
    }

    fn macos_facts() -> SystemFacts {
        SystemFacts {
            is_macos: true,
            home: Some(PathBuf::from("/Users/someone")),
            ..SystemFacts::default()
        }
    }

    #[test]
    fn an_explicit_path_wins_over_everything() {
        let facts = SystemFacts {
            explicit_config: Some(PathBuf::from("/tmp/mine.toml")),
            homebrew_prefix: Some(PathBuf::from("/opt/homebrew")),
            homebrew_config_dir_exists: true,
            ..macos_facts()
        };
        assert_eq!(
            ConfigLocation::resolve(&facts).expect("resolves").path,
            PathBuf::from("/tmp/mine.toml")
        );
    }

    #[test]
    fn a_homebrew_install_writes_where_brew_services_reads() {
        let facts = SystemFacts {
            homebrew_prefix: Some(PathBuf::from("/opt/homebrew")),
            homebrew_config_dir_exists: true,
            ..macos_facts()
        };
        assert_eq!(
            ConfigLocation::resolve(&facts).expect("resolves").path,
            PathBuf::from("/opt/homebrew/etc/pessimal/pessimal.toml")
        );
    }

    #[test]
    fn brew_on_path_without_the_formula_is_not_a_homebrew_install() {
        let facts = SystemFacts {
            homebrew_prefix: Some(PathBuf::from("/opt/homebrew")),
            homebrew_config_dir_exists: false,
            ..macos_facts()
        };
        assert_eq!(
            ConfigLocation::resolve(&facts).expect("resolves").path,
            PathBuf::from("/Users/someone/.config/pessimal/pessimal.toml")
        );
    }

    #[test]
    fn root_on_linux_writes_where_the_systemd_unit_reads() {
        let facts = SystemFacts {
            is_root: true,
            is_macos: false,
            home: Some(PathBuf::from("/root")),
            ..SystemFacts::default()
        };
        assert_eq!(
            ConfigLocation::resolve(&facts).expect("resolves").path,
            PathBuf::from("/etc/pessimal/pessimal.toml")
        );
    }

    #[test]
    fn xdg_config_home_wins_over_the_home_directory() {
        let facts = SystemFacts {
            xdg_config_home: Some(PathBuf::from("/xdg")),
            ..macos_facts()
        };
        assert_eq!(
            ConfigLocation::resolve(&facts).expect("resolves").path,
            PathBuf::from("/xdg/pessimal/pessimal.toml")
        );
    }

    #[test]
    fn with_nowhere_to_write_the_error_says_what_to_pass() {
        let error = ConfigLocation::resolve(&SystemFacts::default()).expect_err("nowhere");
        assert!(format!("{error}").contains("--config"), "{error}");
    }

    #[test]
    fn a_config_is_looked_for_where_init_would_have_put_it() {
        // The bug this exists to stop: `init` writes to Homebrew's prefix, then a bare
        // `pessimal-agent --check` reads `./pessimal.toml`, finds nothing, and says the config does
        // not exist while it sits where the service reads it.
        let facts = SystemFacts {
            homebrew_prefix: Some(PathBuf::from("/opt/homebrew")),
            homebrew_config_dir_exists: true,
            ..macos_facts()
        };
        let brew = PathBuf::from("/opt/homebrew/etc/pessimal/pessimal.toml");
        let found = find_config(&facts, |path| path == brew).expect("found");
        assert_eq!(found, brew);
    }

    #[test]
    fn a_config_in_the_working_directory_still_wins() {
        let facts = SystemFacts {
            homebrew_prefix: Some(PathBuf::from("/opt/homebrew")),
            homebrew_config_dir_exists: true,
            ..macos_facts()
        };
        assert_eq!(
            find_config(&facts, |_| true).expect("found"),
            PathBuf::from("pessimal.toml")
        );
    }

    #[test]
    fn with_no_config_anywhere_the_error_lists_where_it_looked() {
        let facts = SystemFacts {
            homebrew_prefix: Some(PathBuf::from("/opt/homebrew")),
            ..macos_facts()
        };
        let error = find_config(&facts, |_| false).expect_err("nothing exists");
        let message = format!("{error}");
        assert!(
            message.contains("/opt/homebrew/etc/pessimal/pessimal.toml"),
            "{message}"
        );
        assert!(
            message.contains("/Users/someone/.config/pessimal/pessimal.toml"),
            "{message}"
        );
        assert!(message.contains("pessimal-agent init"), "{message}");
    }

    #[test]
    fn a_root_linux_config_puts_the_key_in_the_environment_file() {
        // The packaged unit runs as DynamicUser, so the config must stay readable and the key must
        // not be in it.
        let facts = SystemFacts {
            is_root: true,
            is_macos: false,
            ..SystemFacts::default()
        };
        let placement = KeyPlacement::decide(Path::new("/etc/pessimal/pessimal.toml"), &facts);
        assert_eq!(
            placement,
            KeyPlacement::EnvironmentFile(PathBuf::from(SYSTEMD_ENVIRONMENT_FILE))
        );
        assert_eq!(placement.config_mode(), 0o644);
    }

    #[test]
    fn every_other_layout_keeps_the_key_in_a_private_config() {
        let macos = KeyPlacement::decide(
            Path::new("/opt/homebrew/etc/pessimal/pessimal.toml"),
            &macos_facts(),
        );
        assert_eq!(macos, KeyPlacement::ConfigFile);
        assert_eq!(macos.config_mode(), 0o600);

        let linux_user = KeyPlacement::decide(
            Path::new("/home/someone/.config/pessimal/pessimal.toml"),
            &SystemFacts::default(),
        );
        assert_eq!(linux_user, KeyPlacement::ConfigFile);

        // macOS has an /etc too, and nothing there runs under DynamicUser.
        let macos_etc =
            KeyPlacement::decide(Path::new("/etc/pessimal/pessimal.toml"), &macos_facts());
        assert_eq!(macos_etc, KeyPlacement::ConfigFile);
    }

    #[test]
    fn merging_an_environment_file_keeps_the_other_lines() {
        let existing = "# comment\nPESSIMAL_LOG=debug\nPESSIMAL_API_KEY=old\n";
        let merged = merge_environment_file(existing, "new");
        assert_eq!(
            merged,
            "# comment\nPESSIMAL_LOG=debug\nPESSIMAL_API_KEY=new\n"
        );
    }

    #[test]
    fn merging_adds_the_key_when_the_file_has_none() {
        assert_eq!(
            merge_environment_file("PESSIMAL_LOG=debug\n", "k"),
            "PESSIMAL_LOG=debug\nPESSIMAL_API_KEY=k\n"
        );
        assert_eq!(merge_environment_file("", "k"), "PESSIMAL_API_KEY=k\n");
    }

    #[test]
    fn a_loaded_launch_agent_beats_homebrew() {
        // Both installed means a choice, and starting the Homebrew service beside a running
        // LaunchAgent reports the host twice.
        let facts = SystemFacts {
            launch_agent_loaded: true,
            homebrew_prefix: Some(PathBuf::from("/opt/homebrew")),
            homebrew_config_dir_exists: true,
            ..macos_facts()
        };
        assert!(matches!(
            ServicePlan::detect(&facts),
            ServicePlan::LaunchAgentLoaded { .. }
        ));
        assert_eq!(ServicePlan::detect(&facts).command(), None);
        assert!(
            ServicePlan::detect(&facts)
                .advice(Path::new("/tmp/c.toml"))
                .contains("kickstart")
        );
    }

    #[test]
    fn a_homebrew_install_starts_through_brew_services() {
        let facts = SystemFacts {
            homebrew_prefix: Some(PathBuf::from("/opt/homebrew")),
            homebrew_config_dir_exists: true,
            ..macos_facts()
        };
        assert_eq!(ServicePlan::detect(&facts), ServicePlan::BrewServices);
        assert_eq!(
            ServicePlan::detect(&facts).command().expect("a command"),
            vec!["brew", "services", "start", "pessimal-agent"]
        );
    }

    #[test]
    fn systemd_needs_sudo_unless_root() {
        let facts = SystemFacts {
            has_systemctl: true,
            systemd_unit_installed: true,
            ..SystemFacts::default()
        };
        assert_eq!(
            ServicePlan::detect(&facts)
                .command()
                .expect("a command")
                .first()
                .map(String::as_str),
            Some("sudo")
        );
        let as_root = SystemFacts {
            is_root: true,
            ..facts
        };
        assert_eq!(
            ServicePlan::detect(&as_root)
                .command()
                .expect("a command")
                .first()
                .map(String::as_str),
            Some("systemctl")
        );
    }

    #[test]
    fn systemctl_without_the_unit_installed_is_not_a_plan() {
        let facts = SystemFacts {
            has_systemctl: true,
            systemd_unit_installed: false,
            ..SystemFacts::default()
        };
        assert_eq!(ServicePlan::detect(&facts), ServicePlan::Foreground);
    }

    #[test]
    fn with_no_service_manager_the_advice_names_the_config_path() {
        let advice = ServicePlan::Foreground.advice(Path::new("/etc/pessimal/pessimal.toml"));
        assert!(advice.contains("/etc/pessimal/pessimal.toml"), "{advice}");
    }
}
