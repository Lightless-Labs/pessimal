//! Agent-side errors.

use thiserror::Error;

pub type Result<T> = std::result::Result<T, AgentError>;

#[derive(Debug, Error)]
pub enum AgentError {
    #[error("invalid configuration: {0}")]
    Config(String),

    #[error("could not read configuration from {path}: {source}")]
    ConfigRead {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("could not parse configuration: {0}")]
    ConfigParse(#[from] toml::de::Error),

    #[error("metric collection failed: {0}")]
    Collection(String),

    #[error("could not build the OTLP exporter: {0}")]
    Exporter(String),

    #[error("export failed: {0}")]
    Export(String),
}
