//! Backend presets.
//!
//! Export is plain OTLP, so every supported backend is reached the same way. The only thing that
//! genuinely differs is the authentication header, which is all a preset encodes. Anything not
//! listed here still works through [`BackendPreset::Otlp`] with explicit headers.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize};

use crate::error::AgentError;

/// Deserialises through a type's [`FromStr`], so a config file accepts exactly the spellings an
/// environment override does.
///
/// Without this, serde's derive would accept only its own `snake_case` rendering: the config file
/// would take `http_protobuf` while `PESSIMAL_PROTOCOL` took `http/protobuf`, and the OpenTelemetry
/// spelling everyone already knows would work in one place and not the other.
fn deserialize_from_str<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: FromStr<Err = AgentError>,
{
    let raw = String::deserialize(deserializer)?;
    raw.parse().map_err(serde::de::Error::custom)
}

/// The wire protocol used to reach the collector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ExportProtocol {
    /// OTLP over gRPC. The default; conventionally port 4317.
    #[default]
    Grpc,
    /// OTLP over HTTP with binary protobuf. Conventionally port 4318.
    HttpProtobuf,
}

impl<'de> Deserialize<'de> for ExportProtocol {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserialize_from_str(deserializer)
    }
}

impl fmt::Display for ExportProtocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Grpc => "grpc",
            Self::HttpProtobuf => "http/protobuf",
        })
    }
}

impl FromStr for ExportProtocol {
    type Err = AgentError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        match raw.to_ascii_lowercase().as_str() {
            "grpc" => Ok(Self::Grpc),
            "http" | "http/protobuf" | "http_protobuf" => Ok(Self::HttpProtobuf),
            other => Err(AgentError::Config(format!(
                "unknown protocol {other:?}; expected `grpc` or `http/protobuf`"
            ))),
        }
    }
}

/// Credentials a preset may need. Which fields are required depends on the preset.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Credentials {
    /// The backend's API key, ingestion key, or access token.
    pub api_key: Option<String>,
    /// The target dataset. Honeycomb only.
    pub dataset: Option<String>,
}

impl Credentials {
    #[must_use]
    pub fn new(api_key: Option<String>, dataset: Option<String>) -> Self {
        Self { api_key, dataset }
    }
}

/// A named OTLP destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum BackendPreset {
    /// A bare OTLP endpoint. No headers are added; supply your own if the endpoint needs them.
    #[default]
    Otlp,
    /// SigNoz, self-hosted or cloud. Cloud requires an access token; self-hosted does not.
    Signoz,
    /// ClickStack / HyperDX. Requires an ingestion key.
    Clickstack,
    /// Honeycomb. Requires an API key, and — for metrics specifically — a dataset.
    Honeycomb,
}

impl BackendPreset {
    /// The header names each preset owns. Used to detect a user override.
    #[must_use]
    pub fn managed_headers(self) -> &'static [&'static str] {
        match self {
            Self::Otlp => &[],
            Self::Signoz => &["signoz-access-token"],
            Self::Clickstack => &["authorization"],
            Self::Honeycomb => &["x-honeycomb-team", "x-honeycomb-dataset"],
        }
    }

    /// A short display name.
    #[must_use]
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Otlp => "OTLP",
            Self::Signoz => "SigNoz",
            Self::Clickstack => "ClickStack",
            Self::Honeycomb => "Honeycomb",
        }
    }

    /// Builds the headers this preset requires.
    ///
    /// # Errors
    /// Returns [`AgentError::Config`] when a credential the preset requires is missing, or when
    /// credentials are supplied to [`BackendPreset::Otlp`], which has no header to put them in.
    pub fn headers(
        self,
        credentials: &Credentials,
    ) -> Result<BTreeMap<String, String>, AgentError> {
        let mut headers = BTreeMap::new();
        match self {
            Self::Otlp => {
                if credentials.api_key.is_some() {
                    return Err(AgentError::Config(
                        "preset `otlp` has no authentication header to put `api_key` in; \
                         set the header explicitly under [export.headers], or choose a preset"
                            .to_owned(),
                    ));
                }
            }
            Self::Signoz => {
                // Self-hosted SigNoz accepts unauthenticated ingestion, so the token is optional.
                if let Some(key) = &credentials.api_key {
                    headers.insert("signoz-access-token".to_owned(), key.clone());
                }
            }
            Self::Clickstack => {
                let key = credentials.api_key.as_ref().ok_or_else(|| {
                    AgentError::Config(
                        "preset `clickstack` requires `api_key` (the ingestion key)".to_owned(),
                    )
                })?;
                headers.insert("authorization".to_owned(), key.clone());
            }
            Self::Honeycomb => {
                let key = credentials.api_key.as_ref().ok_or_else(|| {
                    AgentError::Config("preset `honeycomb` requires `api_key`".to_owned())
                })?;
                let dataset = credentials.dataset.as_ref().ok_or_else(|| {
                    AgentError::Config(
                        "preset `honeycomb` requires `dataset`: Honeycomb rejects OTLP metrics \
                         without an `x-honeycomb-dataset` header"
                            .to_owned(),
                    )
                })?;
                headers.insert("x-honeycomb-team".to_owned(), key.clone());
                headers.insert("x-honeycomb-dataset".to_owned(), dataset.clone());
            }
        }
        Ok(headers)
    }
}

impl FromStr for BackendPreset {
    type Err = AgentError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        match raw.to_ascii_lowercase().as_str() {
            "otlp" => Ok(Self::Otlp),
            "signoz" => Ok(Self::Signoz),
            "clickstack" | "hyperdx" => Ok(Self::Clickstack),
            "honeycomb" => Ok(Self::Honeycomb),
            other => Err(AgentError::Config(format!(
                "unknown preset {other:?}; expected one of `otlp`, `signoz`, `clickstack`, `honeycomb`"
            ))),
        }
    }
}

impl<'de> Deserialize<'de> for BackendPreset {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserialize_from_str(deserializer)
    }
}

impl fmt::Display for BackendPreset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.display_name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Serialize, Deserialize)]
    struct Wrapper {
        value: ExportProtocol,
    }

    #[derive(Debug, Serialize, Deserialize)]
    struct PresetWrapper {
        value: BackendPreset,
    }

    fn key(value: &str) -> Credentials {
        Credentials::new(Some(value.to_owned()), None)
    }

    #[test]
    fn otlp_adds_no_headers() {
        let headers = BackendPreset::Otlp
            .headers(&Credentials::default())
            .expect("no credentials needed");
        assert!(headers.is_empty());
    }

    #[test]
    fn otlp_rejects_an_api_key_it_has_nowhere_to_put() {
        let error = BackendPreset::Otlp
            .headers(&key("secret"))
            .expect_err("otlp has no auth header");
        assert!(error.to_string().contains("export.headers"), "{error}");
    }

    #[test]
    fn signoz_works_without_a_token_for_self_hosted() {
        let headers = BackendPreset::Signoz
            .headers(&Credentials::default())
            .expect("self-hosted needs no token");
        assert!(headers.is_empty());
    }

    #[test]
    fn signoz_sets_its_access_token_when_given_one() {
        let headers = BackendPreset::Signoz.headers(&key("tok")).expect("valid");
        assert_eq!(
            headers.get("signoz-access-token").map(String::as_str),
            Some("tok")
        );
    }

    #[test]
    fn clickstack_requires_an_ingestion_key() {
        assert!(
            BackendPreset::Clickstack
                .headers(&Credentials::default())
                .is_err()
        );
        let headers = BackendPreset::Clickstack
            .headers(&key("ing"))
            .expect("valid");
        assert_eq!(
            headers.get("authorization").map(String::as_str),
            Some("ing")
        );
    }

    #[test]
    fn honeycomb_requires_both_a_key_and_a_dataset() {
        assert!(
            BackendPreset::Honeycomb
                .headers(&Credentials::default())
                .is_err()
        );
        assert!(
            BackendPreset::Honeycomb.headers(&key("hcaik")).is_err(),
            "metrics need a dataset header"
        );

        let credentials = Credentials::new(Some("hcaik".to_owned()), Some("pessimal".to_owned()));
        let headers = BackendPreset::Honeycomb
            .headers(&credentials)
            .expect("valid");
        assert_eq!(
            headers.get("x-honeycomb-team").map(String::as_str),
            Some("hcaik")
        );
        assert_eq!(
            headers.get("x-honeycomb-dataset").map(String::as_str),
            Some("pessimal")
        );
    }

    #[test]
    fn honeycomb_explains_why_the_dataset_is_required() {
        let error = BackendPreset::Honeycomb
            .headers(&key("hcaik"))
            .expect_err("dataset missing");
        assert!(error.to_string().contains("x-honeycomb-dataset"), "{error}");
    }

    #[test]
    fn every_header_a_preset_emits_is_declared_as_managed() {
        let credentials = Credentials::new(Some("k".to_owned()), Some("d".to_owned()));
        for preset in [
            BackendPreset::Signoz,
            BackendPreset::Clickstack,
            BackendPreset::Honeycomb,
        ] {
            for name in preset.headers(&credentials).expect("valid").keys() {
                assert!(
                    preset.managed_headers().contains(&name.as_str()),
                    "{preset} emits undeclared header {name}"
                );
            }
        }
    }

    #[test]
    fn presets_parse_from_their_config_spelling() {
        assert_eq!(
            "signoz".parse::<BackendPreset>().expect("known"),
            BackendPreset::Signoz
        );
        assert_eq!(
            "SigNoz".parse::<BackendPreset>().expect("known"),
            BackendPreset::Signoz
        );
        assert_eq!(
            "hyperdx".parse::<BackendPreset>().expect("alias"),
            BackendPreset::Clickstack
        );
        assert!("datadog".parse::<BackendPreset>().is_err());
    }

    #[test]
    fn protocols_parse_from_their_config_spelling() {
        assert_eq!(
            "grpc".parse::<ExportProtocol>().expect("known"),
            ExportProtocol::Grpc
        );
        assert_eq!(
            "http/protobuf".parse::<ExportProtocol>().expect("known"),
            ExportProtocol::HttpProtobuf
        );
        assert!("thrift".parse::<ExportProtocol>().is_err());
    }

    #[test]
    fn a_config_file_accepts_every_spelling_an_env_override_does() {
        // The OpenTelemetry spelling is `http/protobuf`. Left to serde's derive, that would work
        // in PESSIMAL_PROTOCOL and fail in the config file.
        for spelling in ["grpc", "GRPC"] {
            let parsed: Wrapper = toml::from_str(&format!("value = {spelling:?}")).expect("known");
            assert_eq!(parsed.value, ExportProtocol::Grpc, "{spelling}");
        }
        for spelling in ["http", "http/protobuf", "http_protobuf"] {
            let parsed: Wrapper = toml::from_str(&format!("value = {spelling:?}")).expect("known");
            assert_eq!(parsed.value, ExportProtocol::HttpProtobuf, "{spelling}");
        }
    }

    #[test]
    fn a_config_file_accepts_every_preset_alias() {
        for (spelling, expected) in [
            ("otlp", BackendPreset::Otlp),
            ("signoz", BackendPreset::Signoz),
            ("SigNoz", BackendPreset::Signoz),
            ("clickstack", BackendPreset::Clickstack),
            ("hyperdx", BackendPreset::Clickstack),
            ("honeycomb", BackendPreset::Honeycomb),
        ] {
            let parsed: PresetWrapper =
                toml::from_str(&format!("value = {spelling:?}")).expect("known");
            assert_eq!(parsed.value, expected, "{spelling}");
        }
    }

    #[test]
    fn a_config_file_rejects_an_unknown_spelling_with_a_useful_message() {
        let error = toml::from_str::<Wrapper>(r#"value = "thrift""#).expect_err("unknown");
        assert!(error.to_string().contains("grpc"), "{error}");
    }

    #[test]
    fn what_is_serialized_can_be_read_back() {
        for protocol in [ExportProtocol::Grpc, ExportProtocol::HttpProtobuf] {
            let rendered = toml::to_string(&Wrapper { value: protocol }).expect("serialisable");
            let parsed: Wrapper = toml::from_str(&rendered).expect("round trip");
            assert_eq!(parsed.value, protocol);
        }
        for preset in [
            BackendPreset::Otlp,
            BackendPreset::Signoz,
            BackendPreset::Clickstack,
            BackendPreset::Honeycomb,
        ] {
            let rendered = toml::to_string(&PresetWrapper { value: preset }).expect("serialisable");
            let parsed: PresetWrapper = toml::from_str(&rendered).expect("round trip");
            assert_eq!(parsed.value, preset);
        }
    }
}
