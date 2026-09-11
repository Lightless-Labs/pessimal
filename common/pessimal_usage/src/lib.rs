//! Pessimal reporting on itself.
//!
//! Opt-out usage traces from the clients and the agents, to Lightless Labs' own backend rather than
//! the operator's. Pure: span types, the attribute allowlist, the consent decision, and the
//! OTLP/JSON encoder. No I/O, no runtime — shipping them is [`pessimal_usage_otlp`]'s job.
//!
//! Two things about this crate are load-bearing rather than incidental.
//!
//! **The allowlist is the type system, not a checklist.** Spans go to a backend the operator does
//! not control, so the attributes worth having are exactly the ones that must not be sent: their
//! collector's URL, their host names, their alert thresholds, the text of a backend error. There is
//! no API here that accepts arbitrary text. See [`attribute`].
//!
//! **Consent is structural.** Reporting is opt-out, so the defaults decide. [`consent`] resolves
//! every signal — the user's switch, `DO_NOT_TRACK`, `CI`, and whether the build was given a
//! destination at all — but the real guarantee is that no transport is constructed until it answers
//! yes.
//!
//! See `docs/plans/2026-09-11-m8-usage-reporting.md`.
//!
//! [`pessimal_usage_otlp`]: https://github.com/pessimal/pessimal/tree/main/common/pessimal_usage_otlp

pub mod attribute;
pub mod consent;
pub mod encode;
pub mod resource;
pub mod span;

pub use attribute::{
    Attribute, AttributeKey, AttributeValue, BackendKind, DeviceClass, HttpStatusClass, Outcome,
    Platform, Version,
};
pub use consent::{Consent, DenialReason};
pub use encode::export_trace_request;
pub use resource::{Service, UsageResource};
pub use span::{RecordedSpan, SpanId, SpanKind, TraceId, UsageSpan, UsageTrace};
