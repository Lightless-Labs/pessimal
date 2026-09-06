//! Resource attributes: who is reporting.
//!
//! These are what the clients group and filter by, so they matter more than the metric values
//! themselves. `host.name` in particular is the join key between what an agent exports and what a
//! client queries — change it and a host's history splits in two.

use std::collections::BTreeMap;

use opentelemetry::KeyValue;
use opentelemetry_sdk::Resource;
use opentelemetry_semantic_conventions::attribute;
use pessimal_core::OsFamily;
use uuid::Uuid;

use crate::config::ResourceConfig;

/// Identity attached to every metric this agent exports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentIdentity {
    /// `host.name`. The clients' join key.
    pub host_name: String,
    /// `host.id`, when the platform offers a stable machine identifier.
    pub host_id: Option<String>,
    /// `host.arch`, e.g. `arm64`.
    pub host_arch: String,
    /// `os.type`.
    pub os: OsFamily,
    /// `service.name`.
    pub service_name: String,
    /// `service.version`.
    pub service_version: String,
    /// `service.instance.id`. Fresh per process, so a restart is visible in the backend.
    pub service_instance_id: Uuid,
    /// `deployment.environment.name`.
    pub environment: String,
    /// Operator-supplied extras.
    pub extra: BTreeMap<String, String>,
}

impl AgentIdentity {
    /// Builds an identity from config plus a detected hostname.
    ///
    /// `detected_host_name` is used unless the config overrides it — detection is I/O, so the
    /// caller does it and passes the result in.
    #[must_use]
    pub fn from_config(
        config: &ResourceConfig,
        detected_host_name: &str,
        service_version: &str,
    ) -> Self {
        Self {
            host_name: config
                .host_name
                .clone()
                .unwrap_or_else(|| detected_host_name.to_owned()),
            host_id: None,
            host_arch: std::env::consts::ARCH.to_owned(),
            os: OsFamily::current(),
            service_name: config.service_name.clone(),
            service_version: service_version.to_owned(),
            service_instance_id: Uuid::now_v7(),
            environment: config.environment.clone(),
            extra: config.attributes.clone(),
        }
    }

    #[must_use]
    pub fn with_host_id(mut self, host_id: impl Into<String>) -> Self {
        self.host_id = Some(host_id.into());
        self
    }

    /// The attributes as OTel key-values, operator extras last so they cannot silently shadow a
    /// semantic-convention attribute Pessimal depends on.
    #[must_use]
    pub fn attributes(&self) -> Vec<KeyValue> {
        let mut attributes = vec![
            KeyValue::new(attribute::SERVICE_NAME, self.service_name.clone()),
            KeyValue::new(attribute::SERVICE_VERSION, self.service_version.clone()),
            KeyValue::new(
                attribute::SERVICE_INSTANCE_ID,
                self.service_instance_id.to_string(),
            ),
            KeyValue::new(attribute::HOST_NAME, self.host_name.clone()),
            KeyValue::new(attribute::HOST_ARCH, self.host_arch.clone()),
            KeyValue::new(attribute::OS_TYPE, self.os.otel_value()),
            KeyValue::new(
                attribute::DEPLOYMENT_ENVIRONMENT_NAME,
                self.environment.clone(),
            ),
        ];
        if let Some(host_id) = &self.host_id {
            attributes.push(KeyValue::new(attribute::HOST_ID, host_id.clone()));
        }
        let reserved: Vec<String> = attributes
            .iter()
            .map(|kv| kv.key.as_str().to_owned())
            .collect();
        for (name, value) in &self.extra {
            if reserved.contains(name) {
                tracing::warn!(
                    attribute = %name,
                    "ignoring resource attribute: it would shadow one Pessimal relies on"
                );
                continue;
            }
            attributes.push(KeyValue::new(name.clone(), value.clone()));
        }
        attributes
    }

    /// The OTel [`Resource`] for the meter provider.
    #[must_use]
    pub fn resource(&self) -> Resource {
        Resource::builder_empty()
            .with_attributes(self.attributes())
            .build()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> ResourceConfig {
        ResourceConfig {
            service_name: "pessimal-agent".to_owned(),
            environment: "prod".to_owned(),
            host_name: None,
            attributes: BTreeMap::new(),
        }
    }

    fn value_of(attributes: &[KeyValue], key: &str) -> Option<String> {
        attributes
            .iter()
            .find(|kv| kv.key.as_str() == key)
            .map(|kv| kv.value.to_string())
    }

    #[test]
    fn uses_the_detected_hostname_by_default() {
        let identity = AgentIdentity::from_config(&config(), "web-1", "0.1.0");
        assert_eq!(identity.host_name, "web-1");
    }

    #[test]
    fn config_overrides_the_detected_hostname() {
        let mut config = config();
        config.host_name = Some("canonical-1".to_owned());
        let identity = AgentIdentity::from_config(&config, "web-1", "0.1.0");
        assert_eq!(identity.host_name, "canonical-1");
    }

    #[test]
    fn emits_the_semantic_convention_attributes() {
        let identity = AgentIdentity::from_config(&config(), "web-1", "0.1.0");
        let attributes = identity.attributes();

        assert_eq!(value_of(&attributes, "host.name").as_deref(), Some("web-1"));
        assert_eq!(
            value_of(&attributes, "service.name").as_deref(),
            Some("pessimal-agent")
        );
        assert_eq!(
            value_of(&attributes, "service.version").as_deref(),
            Some("0.1.0")
        );
        assert_eq!(
            value_of(&attributes, "deployment.environment.name").as_deref(),
            Some("prod")
        );
        assert!(value_of(&attributes, "os.type").is_some());
        assert!(value_of(&attributes, "service.instance.id").is_some());
    }

    #[test]
    fn the_instance_id_is_a_sortable_uuid_v7_and_fresh_per_process() {
        let first = AgentIdentity::from_config(&config(), "web-1", "0.1.0");
        let second = AgentIdentity::from_config(&config(), "web-1", "0.1.0");
        assert_eq!(first.service_instance_id.get_version_num(), 7);
        assert_ne!(
            first.service_instance_id, second.service_instance_id,
            "a restart must be visible in the backend"
        );
    }

    #[test]
    fn host_id_is_omitted_when_unknown_rather_than_sent_empty() {
        let identity = AgentIdentity::from_config(&config(), "web-1", "0.1.0");
        assert!(value_of(&identity.attributes(), "host.id").is_none());

        let identified = identity.with_host_id("machine-abc");
        assert_eq!(
            value_of(&identified.attributes(), "host.id").as_deref(),
            Some("machine-abc")
        );
    }

    #[test]
    fn operator_attributes_are_carried_through() {
        let mut config = config();
        config
            .attributes
            .insert("deployment.region".to_owned(), "eu-west-3".to_owned());
        let identity = AgentIdentity::from_config(&config, "web-1", "0.1.0");
        assert_eq!(
            value_of(&identity.attributes(), "deployment.region").as_deref(),
            Some("eu-west-3")
        );
    }

    #[test]
    fn an_operator_attribute_cannot_shadow_the_clients_join_key() {
        let mut config = config();
        config
            .attributes
            .insert("host.name".to_owned(), "impostor".to_owned());
        let identity = AgentIdentity::from_config(&config, "web-1", "0.1.0");

        let attributes = identity.attributes();
        assert_eq!(value_of(&attributes, "host.name").as_deref(), Some("web-1"));
        assert_eq!(
            attributes
                .iter()
                .filter(|kv| kv.key.as_str() == "host.name")
                .count(),
            1
        );
    }
}
