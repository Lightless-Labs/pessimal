//! What identifies the reporting process, and nothing more.
//!
//! Deliberately absent: the deployment's URN. The convention is
//! `pessimal::$ENVIRONMENT::$SERVICE::$MODEL::$UUID`, and `$ENVIRONMENT` is a label the operator
//! chose — `acme-prod` names a customer. A URN belongs on entities in the operator's own backend,
//! not on a report to ours.

use uuid::Uuid;

use crate::attribute::{Attribute, AttributeKey, DeviceClass, Platform, Version};

/// Which Pessimal is reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Service {
    Client,
    Agent,
}

impl Service {
    /// The `service.name` value. Matches the agent's existing default so our own SigNoz groups the
    /// agent's self-reports with nothing unexpected.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Client => "pessimal-client",
            Self::Agent => "pessimal-agent",
        }
    }
}

/// The resource attached to every span from one process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageResource {
    service: Service,
    /// `None` when the host could not give a version in the accepted shape. A missing attribute is
    /// better than a guessed one.
    version: Option<Version>,
    build: Option<u32>,
    instance: Uuid,
    platform: Platform,
    device_class: DeviceClass,
    os_version: Option<Version>,
}

impl UsageResource {
    /// `version` and `os_version` are raw strings from the host and are *validated, not trusted*:
    /// anything that is not a dotted numeric version is dropped. See [`Version`].
    ///
    /// `instance` should be a freshly minted UUIDv7, per-launch and never persisted. Taking it as a
    /// parameter rather than minting it here is the point: it makes "session-scoped" a decision the
    /// caller states, visible at the call site, rather than a property buried in a constructor.
    #[must_use]
    pub fn new(
        service: Service,
        platform: Platform,
        device_class: DeviceClass,
        instance: Uuid,
        version: &str,
        build: Option<u32>,
        os_version: &str,
    ) -> Self {
        Self {
            service,
            version: Version::app(version),
            build,
            instance,
            platform,
            device_class,
            os_version: Version::os(os_version),
        }
    }

    #[must_use]
    pub fn service(&self) -> Service {
        self.service
    }

    #[must_use]
    pub fn instance(&self) -> Uuid {
        self.instance
    }

    #[must_use]
    pub fn attributes(&self) -> Vec<Attribute> {
        let mut attributes = vec![
            Attribute::enumerated(AttributeKey::ServiceName, self.service.as_str()),
            Attribute::identifier(AttributeKey::ServiceInstanceId, self.instance),
            Attribute::enumerated(AttributeKey::Platform, self.platform.as_str()),
            Attribute::enumerated(AttributeKey::DeviceClass, self.device_class.as_str()),
        ];
        if let Some(version) = &self.version {
            attributes.push(Attribute::version(
                AttributeKey::ServiceVersion,
                version.clone(),
            ));
        }
        if let Some(build) = self.build {
            attributes.push(Attribute::count(AttributeKey::AppBuild, build));
        }
        if let Some(os) = &self.os_version {
            attributes.push(Attribute::version(AttributeKey::OsVersion, os.clone()));
        }
        attributes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resource(version: &str, os: &str) -> UsageResource {
        UsageResource::new(
            Service::Client,
            Platform::Ios,
            DeviceClass::Phone,
            Uuid::now_v7(),
            version,
            Some(26),
            os,
        )
    }

    #[test]
    fn a_well_formed_resource_carries_everything() {
        let keys: Vec<_> = resource("0.1.0", "18.4.1")
            .attributes()
            .into_iter()
            .map(|a| a.key)
            .collect();
        for expected in [
            AttributeKey::ServiceName,
            AttributeKey::ServiceInstanceId,
            AttributeKey::ServiceVersion,
            AttributeKey::AppBuild,
            AttributeKey::Platform,
            AttributeKey::DeviceClass,
            AttributeKey::OsVersion,
        ] {
            assert!(keys.contains(&expected), "{expected:?} missing");
        }
    }

    #[test]
    fn an_os_version_is_trimmed_to_major_minor() {
        let os = resource("0.1.0", "18.4.1")
            .attributes()
            .into_iter()
            .find(|a| a.key == AttributeKey::OsVersion)
            .expect("present");
        assert_eq!(
            os.value,
            crate::attribute::AttributeValue::Version(Version::os("18.4").expect("valid"))
        );
    }

    #[test]
    fn a_version_string_that_is_not_a_version_is_dropped_rather_than_sent() {
        // The realistic accident: somebody wires a device name or a bundle identifier into the
        // version slot. It must not become an attribute.
        let attributes = resource("Thomas’s iPhone", "acme-prod").attributes();
        let keys: Vec<_> = attributes.iter().map(|a| a.key).collect();
        assert!(!keys.contains(&AttributeKey::ServiceVersion));
        assert!(!keys.contains(&AttributeKey::OsVersion));
        // And the rest still reports, so one bad input does not cost the whole span.
        assert!(keys.contains(&AttributeKey::ServiceName));
    }

    #[test]
    fn no_environment_label_can_reach_the_resource() {
        // There is no parameter for it, which is the assertion. This test exists so that adding one
        // breaks a test named after the reason not to.
        let keys: Vec<_> = resource("0.1.0", "18.4")
            .attributes()
            .into_iter()
            .map(|a| a.key.otel_name())
            .collect();
        assert!(!keys.iter().any(|k| k.contains("environment")));
        assert!(!keys.iter().any(|k| k.contains("deployment")));
    }
}
