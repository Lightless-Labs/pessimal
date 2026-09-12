//! Agent configuration.
//!
//! Read from TOML, then overridden by environment variables so a container can be configured
//! without a config file and a secret need never be written to disk.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{AgentError, Result};
use crate::preset::{BackendPreset, Credentials, ExportProtocol};

/// Environment variable prefix for every override.
pub const ENV_PREFIX: &str = "PESSIMAL_";

/// Default seconds between exports. Also the heartbeat period, and therefore the unit the
/// liveness policy counts in.
pub const DEFAULT_INTERVAL_SECONDS: u64 = 30;

/// Default seconds before an export attempt is abandoned.
pub const DEFAULT_TIMEOUT_SECONDS: u64 = 10;

const DEFAULT_SERVICE_NAME: &str = "pessimal-agent";
const DEFAULT_ENVIRONMENT: &str = "default";

/// Where and how to export.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExportConfig {
    #[serde(default)]
    pub preset: BackendPreset,
    /// Collector URL, e.g. `http://localhost:4317`. Required; there is no sensible default.
    pub endpoint: String,
    #[serde(default)]
    pub protocol: ExportProtocol,
    #[serde(default = "default_interval")]
    pub interval_seconds: u64,
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u64,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub dataset: Option<String>,
    /// Extra headers, merged over whatever the preset produces.
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
}

fn default_interval() -> u64 {
    DEFAULT_INTERVAL_SECONDS
}

fn default_timeout() -> u64 {
    DEFAULT_TIMEOUT_SECONDS
}

impl ExportConfig {
    #[must_use]
    pub fn credentials(&self) -> Credentials {
        Credentials::new(self.api_key.clone(), self.dataset.clone())
    }

    /// Preset headers merged with the user's, the user's winning.
    ///
    /// # Errors
    /// Returns [`AgentError::Config`] if the preset's credential requirements are unmet.
    pub fn resolved_headers(&self) -> Result<BTreeMap<String, String>> {
        let mut headers = self.preset.headers(&self.credentials())?;
        for (name, value) in &self.headers {
            headers.insert(name.to_ascii_lowercase(), value.clone());
        }
        Ok(headers)
    }

    /// Header names the user has set that the preset would otherwise own. Worth warning about:
    /// the override wins, which is easy to do by accident.
    #[must_use]
    pub fn overridden_preset_headers(&self) -> Vec<String> {
        self.headers
            .keys()
            .map(|name| name.to_ascii_lowercase())
            .filter(|name| self.preset.managed_headers().contains(&name.as_str()))
            .collect()
    }
}

/// Identity attached to every exported metric.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceConfig {
    #[serde(default = "default_service_name")]
    pub service_name: String,
    #[serde(default = "default_environment")]
    pub environment: String,
    /// Overrides the detected hostname. This is the value clients group by, so changing it on a
    /// running fleet splits a host's history in two.
    #[serde(default)]
    pub host_name: Option<String>,
    /// Extra resource attributes, e.g. `deployment.region`.
    #[serde(default)]
    pub attributes: BTreeMap<String, String>,
}

fn default_service_name() -> String {
    DEFAULT_SERVICE_NAME.to_owned()
}

fn default_environment() -> String {
    DEFAULT_ENVIRONMENT.to_owned()
}

impl Default for ResourceConfig {
    fn default() -> Self {
        Self {
            service_name: default_service_name(),
            environment: default_environment(),
            host_name: None,
            attributes: BTreeMap::new(),
        }
    }
}

/// What to sample.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CollectionConfig {
    /// Mount points to report filesystem usage for. Empty means every mount `sysinfo` reports,
    /// which on a developer machine is a great many.
    #[serde(default)]
    pub filesystems: Vec<String>,
    /// Report per-interface network I/O rather than a host total.
    #[serde(default)]
    pub per_interface_network: bool,
}

impl Default for CollectionConfig {
    fn default() -> Self {
        Self {
            filesystems: vec!["/".to_owned()],
            per_interface_network: false,
        }
    }
}

/// The whole agent configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentConfig {
    pub export: ExportConfig,
    #[serde(default)]
    pub resource: ResourceConfig,
    #[serde(default)]
    pub collection: CollectionConfig,
}

impl AgentConfig {
    /// Parses TOML.
    ///
    /// # Errors
    /// Returns [`AgentError::ConfigParse`] for malformed TOML or an unknown key, and
    /// [`AgentError::Config`] if the result does not validate.
    pub fn from_toml(source: &str) -> Result<Self> {
        let config: Self = toml::from_str(source)?;
        config.validate()?;
        Ok(config)
    }

    /// Reads and parses a config file.
    ///
    /// # Errors
    /// Returns [`AgentError::ConfigRead`] if the file cannot be read, plus anything
    /// [`AgentConfig::from_toml`] returns.
    pub fn from_file(path: &Path) -> Result<Self> {
        let source = std::fs::read_to_string(path).map_err(|source| AgentError::ConfigRead {
            path: path.display().to_string(),
            source,
        })?;
        Self::from_toml(&source)
    }

    /// Applies `PESSIMAL_*` overrides from an environment map.
    ///
    /// Takes a map rather than reading the process environment so this stays a pure function.
    ///
    /// # Errors
    /// Returns [`AgentError::Config`] if a value does not parse, or if the result does not
    /// validate.
    pub fn apply_env(&mut self, env: &BTreeMap<String, String>) -> Result<()> {
        // An empty value counts as unset. Every system that injects environment variables --
        // launchd, compose, a CI matrix -- expresses "no value here" as an empty one, and reading
        // that literally turns `PESSIMAL_API_KEY=` into a refusal to start.
        let get = |suffix: &str| {
            env.get(&format!("{ENV_PREFIX}{suffix}"))
                .filter(|value| !value.is_empty())
        };

        if let Some(value) = get("ENDPOINT") {
            self.export.endpoint.clone_from(value);
        }
        if let Some(value) = get("PRESET") {
            self.export.preset = value.parse()?;
        }
        if let Some(value) = get("PROTOCOL") {
            self.export.protocol = value.parse()?;
        }
        if let Some(value) = get("API_KEY") {
            self.export.api_key = Some(value.clone());
        }
        if let Some(value) = get("DATASET") {
            self.export.dataset = Some(value.clone());
        }
        if let Some(value) = get("INTERVAL_SECONDS") {
            self.export.interval_seconds = value.parse().map_err(|_| {
                AgentError::Config(format!(
                    "{ENV_PREFIX}INTERVAL_SECONDS: {value:?} is not a number"
                ))
            })?;
        }
        if let Some(value) = get("SERVICE_NAME") {
            self.resource.service_name.clone_from(value);
        }
        if let Some(value) = get("ENVIRONMENT") {
            self.resource.environment.clone_from(value);
        }
        if let Some(value) = get("HOST_NAME") {
            self.resource.host_name = Some(value.clone());
        }
        self.validate()
    }

    /// # Errors
    /// Returns [`AgentError::Config`] describing the first problem found.
    pub fn validate(&self) -> Result<()> {
        if self.export.endpoint.trim().is_empty() {
            return Err(AgentError::Config(
                "export.endpoint is required, e.g. `http://localhost:4317`".to_owned(),
            ));
        }
        if !self.export.endpoint.starts_with("http://")
            && !self.export.endpoint.starts_with("https://")
        {
            return Err(AgentError::Config(format!(
                "export.endpoint {:?} must start with http:// or https://",
                self.export.endpoint
            )));
        }
        if self.export.interval_seconds == 0 {
            return Err(AgentError::Config(
                "export.interval_seconds must be at least 1".to_owned(),
            ));
        }
        if self.export.timeout_seconds == 0 {
            return Err(AgentError::Config(
                "export.timeout_seconds must be at least 1".to_owned(),
            ));
        }
        if self.export.timeout_seconds > self.export.interval_seconds {
            return Err(AgentError::Config(format!(
                "export.timeout_seconds ({}) exceeds export.interval_seconds ({}); \
                 exports would overlap",
                self.export.timeout_seconds, self.export.interval_seconds
            )));
        }
        if self.resource.service_name.trim().is_empty() {
            return Err(AgentError::Config(
                "resource.service_name must not be empty".to_owned(),
            ));
        }
        if self.resource.environment.contains("::") {
            return Err(AgentError::Config(
                "resource.environment must not contain `::`, the URN separator".to_owned(),
            ));
        }
        // Surfaces a missing credential at startup rather than on the first export.
        self.export.resolved_headers()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = r#"
        [export]
        endpoint = "http://localhost:4317"
    "#;

    fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn a_minimal_config_takes_sensible_defaults() {
        let config = AgentConfig::from_toml(MINIMAL).expect("valid");
        assert_eq!(config.export.preset, BackendPreset::Otlp);
        assert_eq!(config.export.protocol, ExportProtocol::Grpc);
        assert_eq!(config.export.interval_seconds, DEFAULT_INTERVAL_SECONDS);
        assert_eq!(config.resource.service_name, "pessimal-agent");
        assert_eq!(config.collection.filesystems, vec!["/".to_owned()]);
    }

    #[test]
    fn parses_a_full_config() {
        let config = AgentConfig::from_toml(
            r#"
            [export]
            preset = "signoz"
            endpoint = "https://ingest.eu.signoz.cloud:443"
            protocol = "http_protobuf"
            interval_seconds = 15
            timeout_seconds = 5
            api_key = "tok"

            [export.headers]
            "x-tenant" = "acme"

            [resource]
            service_name = "pessimal-agent"
            environment = "prod"
            host_name = "web-1"

            [resource.attributes]
            "deployment.region" = "eu-west-3"

            [collection]
            filesystems = ["/", "/data"]
            per_interface_network = true
        "#,
        )
        .expect("valid");

        assert_eq!(config.export.preset, BackendPreset::Signoz);
        assert_eq!(config.export.protocol, ExportProtocol::HttpProtobuf);
        assert_eq!(config.export.interval_seconds, 15);
        assert_eq!(config.resource.host_name.as_deref(), Some("web-1"));
        assert_eq!(
            config
                .resource
                .attributes
                .get("deployment.region")
                .map(String::as_str),
            Some("eu-west-3")
        );
        assert!(config.collection.per_interface_network);
    }

    #[test]
    fn rejects_an_unknown_key_rather_than_ignoring_a_typo() {
        let error = AgentConfig::from_toml(
            r#"
            [export]
            endpoint = "http://localhost:4317"
            intervall_seconds = 30
        "#,
        )
        .expect_err("typo must not be silently dropped");
        assert!(error.to_string().contains("intervall_seconds"), "{error}");
    }

    #[test]
    fn requires_an_endpoint() {
        assert!(AgentConfig::from_toml("[export]\n").is_err());
    }

    #[test]
    fn requires_an_endpoint_scheme() {
        assert!(
            AgentConfig::from_toml("[export]\nendpoint = \"localhost:4317\"\n").is_err(),
            "a bare host:port is ambiguous between http and https"
        );
    }

    #[test]
    fn rejects_a_timeout_longer_than_the_interval() {
        let error = AgentConfig::from_toml(
            r#"
            [export]
            endpoint = "http://localhost:4317"
            interval_seconds = 5
            timeout_seconds = 30
        "#,
        )
        .expect_err("overlapping exports");
        assert!(error.to_string().contains("overlap"), "{error}");
    }

    #[test]
    fn rejects_a_zero_interval() {
        assert!(
            AgentConfig::from_toml(
                "[export]\nendpoint = \"http://x.test\"\ninterval_seconds = 0\n"
            )
            .is_err()
        );
    }

    #[test]
    fn rejects_an_environment_containing_the_urn_separator() {
        assert!(
            AgentConfig::from_toml(
                "[export]\nendpoint = \"http://x.test\"\n[resource]\nenvironment = \"a::b\"\n"
            )
            .is_err()
        );
    }

    #[test]
    fn validation_catches_a_missing_preset_credential_at_startup() {
        let error = AgentConfig::from_toml(
            r#"
            [export]
            preset = "honeycomb"
            endpoint = "https://api.honeycomb.io:443"
            api_key = "hcaik"
        "#,
        )
        .expect_err("dataset missing");
        assert!(error.to_string().contains("dataset"), "{error}");
    }

    #[test]
    fn user_headers_merge_over_preset_headers() {
        let config = AgentConfig::from_toml(
            r#"
            [export]
            preset = "signoz"
            endpoint = "http://localhost:4317"
            api_key = "tok"

            [export.headers]
            "X-Tenant" = "acme"
        "#,
        )
        .expect("valid");

        let headers = config.export.resolved_headers().expect("valid");
        assert_eq!(
            headers.get("signoz-access-token").map(String::as_str),
            Some("tok")
        );
        assert_eq!(
            headers.get("x-tenant").map(String::as_str),
            Some("acme"),
            "header names are lowercased"
        );
    }

    #[test]
    fn an_explicit_header_overrides_the_preset_and_is_reported() {
        let config = AgentConfig::from_toml(
            r#"
            [export]
            preset = "signoz"
            endpoint = "http://localhost:4317"
            api_key = "tok"

            [export.headers]
            "signoz-access-token" = "override"
        "#,
        )
        .expect("valid");

        assert_eq!(
            config
                .export
                .resolved_headers()
                .expect("valid")
                .get("signoz-access-token"),
            Some(&"override".to_owned())
        );
        assert_eq!(
            config.export.overridden_preset_headers(),
            vec!["signoz-access-token"]
        );
    }

    #[test]
    fn env_overrides_the_file() {
        let mut config = AgentConfig::from_toml(MINIMAL).expect("valid");
        config
            .apply_env(&env(&[
                ("PESSIMAL_ENDPOINT", "https://ingest.eu.signoz.cloud:443"),
                ("PESSIMAL_PRESET", "signoz"),
                ("PESSIMAL_API_KEY", "tok"),
                ("PESSIMAL_INTERVAL_SECONDS", "15"),
                ("PESSIMAL_ENVIRONMENT", "prod"),
            ]))
            .expect("valid overrides");

        assert_eq!(config.export.endpoint, "https://ingest.eu.signoz.cloud:443");
        assert_eq!(config.export.preset, BackendPreset::Signoz);
        assert_eq!(config.export.api_key.as_deref(), Some("tok"));
        assert_eq!(config.export.interval_seconds, 15);
        assert_eq!(config.resource.environment, "prod");
    }

    #[test]
    fn an_empty_override_counts_as_unset() {
        // Every orchestrator that passes variables -- launchd, compose, CI -- spells "no value" as
        // an empty one. Taking that literally made `PESSIMAL_API_KEY=` a startup failure: an empty
        // key with a preset that has nowhere to put one does not validate.
        let mut config = AgentConfig::from_toml(MINIMAL).expect("valid");
        let before = config.clone();
        config
            .apply_env(&env(&[
                ("PESSIMAL_API_KEY", ""),
                ("PESSIMAL_DATASET", ""),
                ("PESSIMAL_HOST_NAME", ""),
                ("PESSIMAL_ENDPOINT", ""),
                ("PESSIMAL_ENVIRONMENT", ""),
            ]))
            .expect("an empty override must not fail validation");

        assert_eq!(config.export.api_key, before.export.api_key);
        assert_eq!(config.export.dataset, before.export.dataset);
        assert_eq!(config.resource.host_name, before.resource.host_name);
        assert_eq!(config.export.endpoint, before.export.endpoint);
        assert_eq!(config.resource.environment, before.resource.environment);
    }

    #[test]
    fn env_ignores_unrelated_variables() {
        let mut config = AgentConfig::from_toml(MINIMAL).expect("valid");
        let before = config.clone();
        config
            .apply_env(&env(&[("PATH", "/usr/bin"), ("HOME", "/root")]))
            .expect("valid");
        assert_eq!(config, before);
    }

    #[test]
    fn env_rejects_an_unparseable_value() {
        let mut config = AgentConfig::from_toml(MINIMAL).expect("valid");
        assert!(
            config
                .apply_env(&env(&[("PESSIMAL_INTERVAL_SECONDS", "soon")]))
                .is_err()
        );
        assert!(
            config
                .apply_env(&env(&[("PESSIMAL_PRESET", "datadog")]))
                .is_err()
        );
    }

    #[test]
    fn env_overrides_are_validated_together_not_in_isolation() {
        let mut config = AgentConfig::from_toml(MINIMAL).expect("valid");
        let error = config
            .apply_env(&env(&[
                ("PESSIMAL_PRESET", "honeycomb"),
                ("PESSIMAL_API_KEY", "k"),
            ]))
            .expect_err("honeycomb still needs a dataset");
        assert!(error.to_string().contains("dataset"), "{error}");
    }
}
