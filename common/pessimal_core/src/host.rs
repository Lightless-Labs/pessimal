//! Host identity and selection.

use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A host's stable identifier.
///
/// This is the value the agent sets as the `host.name` resource attribute and the value a query
/// adapter groups by. It is deliberately the operator-visible hostname rather than a Pessimal
/// UUID: it has to match what already exists in the telemetry backend, which Pessimal does not
/// control.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct HostId(String);

impl HostId {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for HostId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for HostId {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl From<String> for HostId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

/// The operating system family a host runs, as reported in the `os.type` resource attribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OsFamily {
    Linux,
    Darwin,
    Windows,
    Other,
}

impl OsFamily {
    /// The `os.type` semantic-convention value.
    #[must_use]
    pub fn otel_value(self) -> &'static str {
        match self {
            Self::Linux => "linux",
            Self::Darwin => "darwin",
            Self::Windows => "windows",
            Self::Other => "other",
        }
    }

    /// Parses an `os.type` value, falling back to [`OsFamily::Other`] rather than failing — an
    /// unrecognised OS is still a host worth showing.
    #[must_use]
    pub fn from_otel_value(value: &str) -> Self {
        match value.to_ascii_lowercase().as_str() {
            "linux" => Self::Linux,
            "darwin" | "macos" => Self::Darwin,
            "windows" => Self::Windows,
            _ => Self::Other,
        }
    }

    /// The family of the platform this build targets.
    #[must_use]
    pub fn current() -> Self {
        match std::env::consts::OS {
            "linux" => Self::Linux,
            "macos" | "ios" => Self::Darwin,
            "windows" => Self::Windows,
            _ => Self::Other,
        }
    }
}

impl fmt::Display for OsFamily {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.otel_value())
    }
}

/// A monitored host as the clients know it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Host {
    pub id: HostId,
    pub os: OsFamily,
    /// Timestamp of the most recent heartbeat seen for this host, if any. `None` means the
    /// backend returned no heartbeat in the queried window.
    pub last_heartbeat: Option<DateTime<Utc>>,
    /// Agent version, from the `service.version` resource attribute.
    pub agent_version: Option<String>,
}

impl Host {
    #[must_use]
    pub fn new(id: HostId, os: OsFamily) -> Self {
        Self {
            id,
            os,
            last_heartbeat: None,
            agent_version: None,
        }
    }

    #[must_use]
    pub fn with_last_heartbeat(mut self, at: DateTime<Utc>) -> Self {
        self.last_heartbeat = Some(at);
        self
    }

    #[must_use]
    pub fn with_agent_version(mut self, version: impl Into<String>) -> Self {
        self.agent_version = Some(version.into());
        self
    }
}

/// Which hosts a rule or query applies to.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum HostSelector {
    /// Every host the backend reports.
    #[default]
    All,
    /// Exactly one host.
    Host(HostId),
    /// Any of a set. An empty set matches nothing.
    AnyOf(Vec<HostId>),
}

impl HostSelector {
    #[must_use]
    pub fn matches(&self, host: &HostId) -> bool {
        match self {
            Self::All => true,
            Self::Host(id) => id == host,
            Self::AnyOf(ids) => ids.contains(host),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn os_family_round_trips_through_its_otel_value() {
        for family in [
            OsFamily::Linux,
            OsFamily::Darwin,
            OsFamily::Windows,
            OsFamily::Other,
        ] {
            assert_eq!(OsFamily::from_otel_value(family.otel_value()), family);
        }
    }

    #[test]
    fn os_family_parsing_is_lenient() {
        assert_eq!(OsFamily::from_otel_value("MacOS"), OsFamily::Darwin);
        assert_eq!(OsFamily::from_otel_value("Linux"), OsFamily::Linux);
        assert_eq!(OsFamily::from_otel_value("plan9"), OsFamily::Other);
    }

    #[test]
    fn all_selector_matches_every_host() {
        assert!(HostSelector::All.matches(&HostId::new("anything")));
    }

    #[test]
    fn host_selector_matches_only_its_host() {
        let selector = HostSelector::Host(HostId::new("web-1"));
        assert!(selector.matches(&HostId::new("web-1")));
        assert!(!selector.matches(&HostId::new("web-2")));
    }

    #[test]
    fn any_of_matches_members_only() {
        let selector = HostSelector::AnyOf(vec![HostId::new("web-1"), HostId::new("db-1")]);
        assert!(selector.matches(&HostId::new("db-1")));
        assert!(!selector.matches(&HostId::new("web-2")));
    }

    #[test]
    fn an_empty_any_of_matches_nothing() {
        assert!(!HostSelector::AnyOf(vec![]).matches(&HostId::new("web-1")));
    }
}
