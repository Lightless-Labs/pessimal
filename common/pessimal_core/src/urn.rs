//! Stable resource names.
//!
//! Format: `pessimal::{environment}::{service}::{model}::{uuid}`
//!
//! The `pessimal` prefix is configurable so a deployment that forked its identifiers early can
//! keep them. Everything Pessimal persists or exposes over FFI is addressed by a URN.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::CoreError;

/// The default URN namespace prefix.
pub const DEFAULT_URN_PREFIX: &str = "pessimal";

const SEPARATOR: &str = "::";
const SEGMENT_COUNT: usize = 5;

/// A parsed Pessimal URN.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Urn {
    prefix: String,
    environment: String,
    service: String,
    model: String,
    id: Uuid,
}

impl Urn {
    /// Builds a URN with the default prefix and a freshly minted UUIDv7.
    ///
    /// # Errors
    /// Returns [`CoreError::InvalidUrn`] if any segment is empty or contains the `::` separator.
    pub fn new(environment: &str, service: &str, model: &str) -> Result<Self, CoreError> {
        Self::with_id(environment, service, model, Uuid::now_v7())
    }

    /// Builds a URN with the default prefix around an existing id.
    ///
    /// # Errors
    /// Returns [`CoreError::InvalidUrn`] if any segment is empty or contains the `::` separator.
    pub fn with_id(
        environment: &str,
        service: &str,
        model: &str,
        id: Uuid,
    ) -> Result<Self, CoreError> {
        Self::with_prefix(DEFAULT_URN_PREFIX, environment, service, model, id)
    }

    /// Builds a URN with an explicit namespace prefix.
    ///
    /// # Errors
    /// Returns [`CoreError::InvalidUrn`] if any segment is empty or contains the `::` separator.
    pub fn with_prefix(
        prefix: &str,
        environment: &str,
        service: &str,
        model: &str,
        id: Uuid,
    ) -> Result<Self, CoreError> {
        for segment in [prefix, environment, service, model] {
            if segment.is_empty() || segment.contains(SEPARATOR) {
                return Err(CoreError::InvalidUrn(format!(
                    "segment {segment:?} must be non-empty and free of `{SEPARATOR}`"
                )));
            }
        }
        Ok(Self {
            prefix: prefix.to_owned(),
            environment: environment.to_owned(),
            service: service.to_owned(),
            model: model.to_owned(),
            id,
        })
    }

    #[must_use]
    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    #[must_use]
    pub fn environment(&self) -> &str {
        &self.environment
    }

    #[must_use]
    pub fn service(&self) -> &str {
        &self.service
    }

    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }

    #[must_use]
    pub fn id(&self) -> Uuid {
        self.id
    }
}

impl fmt::Display for Urn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}{SEPARATOR}{}{SEPARATOR}{}{SEPARATOR}{}{SEPARATOR}{}",
            self.prefix, self.environment, self.service, self.model, self.id
        )
    }
}

impl FromStr for Urn {
    type Err = CoreError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        let segments: Vec<&str> = raw.split(SEPARATOR).collect();
        if segments.len() != SEGMENT_COUNT {
            return Err(CoreError::InvalidUrn(format!(
                "expected {SEGMENT_COUNT} `{SEPARATOR}`-separated segments, got {}",
                segments.len()
            )));
        }
        let id = Uuid::parse_str(segments[4])
            .map_err(|e| CoreError::InvalidUrn(format!("trailing segment is not a UUID: {e}")))?;
        Self::with_prefix(segments[0], segments[1], segments[2], segments[3], id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_string() {
        let urn = Urn::new("prod", "agent", "host").expect("valid segments");
        let parsed: Urn = urn.to_string().parse().expect("round trip");
        assert_eq!(urn, parsed);
    }

    #[test]
    fn renders_all_five_segments_in_order() {
        let id = Uuid::now_v7();
        let urn = Urn::with_id("prod", "agent", "host", id).expect("valid segments");
        assert_eq!(
            urn.to_string(),
            format!("pessimal::prod::agent::host::{id}")
        );
    }

    #[test]
    fn mints_sortable_uuid_v7() {
        let urn = Urn::new("prod", "agent", "host").expect("valid segments");
        assert_eq!(urn.id().get_version_num(), 7);
    }

    #[test]
    fn honours_a_custom_prefix() {
        let urn = Urn::with_prefix("acme", "prod", "agent", "host", Uuid::now_v7())
            .expect("valid segments");
        assert!(urn.to_string().starts_with("acme::prod::"));
        assert_eq!(urn.prefix(), "acme");
    }

    #[test]
    fn rejects_empty_segments() {
        assert!(Urn::new("", "agent", "host").is_err());
        assert!(Urn::new("prod", "", "host").is_err());
        assert!(Urn::new("prod", "agent", "").is_err());
    }

    #[test]
    fn rejects_segments_containing_the_separator() {
        assert!(Urn::new("pr::od", "agent", "host").is_err());
    }

    #[test]
    fn rejects_wrong_segment_count() {
        assert!("pessimal::prod::agent".parse::<Urn>().is_err());
    }

    #[test]
    fn rejects_a_non_uuid_tail() {
        assert!(
            "pessimal::prod::agent::host::not-a-uuid"
                .parse::<Urn>()
                .is_err()
        );
    }
}
