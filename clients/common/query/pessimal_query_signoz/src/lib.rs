//! SigNoz implementation of [`pessimal_core::TelemetryQuery`].
//!
//! Talks to `/api/v5/query_range`. The request shape is documented by SigNoz; the response shape is
//! not, and is taken from their Go types instead — see [`wire`] for the details that a reasonable
//! guess gets wrong.

pub mod client;
pub mod naming;
pub mod wire;

pub use client::{SignozConfig, SignozQuery};
pub use naming::MetricNaming;
