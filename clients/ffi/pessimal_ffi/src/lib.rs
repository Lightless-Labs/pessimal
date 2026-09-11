//! The UniFFI boundary: `pessimal_client_core` in the shapes Swift can hold.
//!
//! **This layer translates. It does not decide.** Every judgement the product makes — liveness,
//! alert dwell, counter normalisation, per-mount reduction, freshness, backoff, what "usable"
//! means, which failure is the worst one — is made by a synchronous pure function in
//! `pessimal_client_core` and is merely *restated* here. The only things this crate owns are the
//! four shape changes UniFFI forces (`DateTime` to epoch milliseconds, `Duration` to seconds,
//! newtype ids to `String`, `BTreeMap` to a key-sorted `Vec`), the composition root that builds a
//! SigNoz-backed port, and the mutexes that keep two concurrent calls from overwriting each other.
//! A threshold, an interlock, a comparison or an English sentence that appears *only* in this crate
//! is a bug: it is a second copy of a rule, it will drift the first time core's changes, and
//! nothing in either test suite would notice.
//!
//! Three consequences of that, which the module docs expand on:
//!
//! - A conversion fails only when the *representation* cannot hold the value
//!   ([`FfiError::Internal`] for an instant no `DateTime<Utc>` can express), never because the
//!   value is judged wrong. Judging is core's.
//! - A failed backend is **data**, not a thrown error: [`FfiError`] is the crate's one
//!   `uniffi::Error` and appears in return position only, while [`PollFailureRecord`] appears in
//!   field position only, inside a view that still renders. A poll that failed is still a poll
//!   whose last good view must survive on screen.
//! - Every `From` impl destructures its source exhaustively, with no `..`, so a new field in core
//!   is a compile error here rather than a value silently dropped on the floor.
//!
//! # Layout
//!
//! | Module | What crosses |
//! |---|---|
//! | [`convert`] | [`FfiError`], and the scalar shape changes every other module uses |
//! | [`config_records`] | what the user configures: tuning, fleet config, alert rules |
//! | [`view_records`] | what the screens draw: fleet, host, metric, alert, freshness |
//! | [`fold_records`] | what one poll produced: result, transitions, advice, failures |
//! | [`probe_records`] | the "test connection" answer |
//! | [`session`] | [`FleetSession`], the one object an app holds, and the only mutable state |
//!
//! # Why the crate root re-exports everything
//!
//! Records, enums and objects share **one flat Swift namespace regardless of their Rust module**,
//! and so do exported free functions. §4.11 of the M3 design states the rule and the cost of
//! breaking it: a duplicate name is a bindgen-time failure that surfaces as an unrelated-looking
//! Swift redeclaration error, a long way from the two files that disagreed.
//!
//! Glob-re-exporting all six modules here reproduces that one flat namespace in Rust, where the
//! compiler checks it on every build instead of a human checking it with `grep` at generation time:
//! two modules declaring the same name make `ambiguous_glob_reexports` fire on the `pub use` lines
//! below. Re-exporting the *same* item through two globs — `view_records` deliberately re-exports
//! `config_records`' [`ComparatorRecord`] and [`MetricKindRecord`], because it is the module
//! `fold_records` resolves them through — does not, so the check costs the legitimate case nothing.
//!
//! The lint is warn-by-default, which under `cargo build` would print a warning and generate
//! colliding bindings anyway. Denied here, a collision stops the build at the file that can explain
//! why — which is the point, and the reason this is not left to CI's `RUSTFLAGS`.
#![deny(
    ambiguous_glob_reexports,
    reason = "the Rust-side enforcement of §4.11's one-flat-Swift-namespace rule; see above"
)]

// Exactly one call, in the crate root, for the whole library. It emits the per-crate scaffolding
// (checksums, the `uniffi_` initialiser, the FFI metadata symbols) that `uniffi-bindgen` reads back
// out of the built `cdylib`; the `#[uniffi::export]` and `#[derive(uniffi::…)]` items in the modules
// below register themselves into it from wherever they are declared.
uniffi::setup_scaffolding!();

pub mod config_records;
pub mod convert;
pub mod fold_records;
pub mod probe_records;
pub mod session;
pub mod usage;
pub mod view_records;

pub use config_records::*;
pub use convert::*;
pub use fold_records::*;
pub use probe_records::*;
pub use session::*;
pub use view_records::*;
