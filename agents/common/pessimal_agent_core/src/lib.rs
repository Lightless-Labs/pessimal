//! Shared agent machinery.
//!
//! Everything an agent needs that is not specific to *what* it samples: configuration, backend
//! presets, the collection contract, resource identity, and OTLP export. The host agent supplies a
//! [`collector::MetricCollector`]; a future agent for something else supplies its own.

pub mod collector;
pub mod config;
pub mod error;
pub mod export;
pub mod preset;
pub mod resource;

pub use collector::{CachedCollector, MetricCollector, Observation, Snapshot};
pub use config::{AgentConfig, CollectionConfig, ExportConfig, ResourceConfig};
pub use error::{AgentError, Result};
pub use export::{RegisteredInstruments, build_meter_provider, meter, register_instruments};
pub use preset::{BackendPreset, Credentials, ExportProtocol};
pub use resource::AgentIdentity;
