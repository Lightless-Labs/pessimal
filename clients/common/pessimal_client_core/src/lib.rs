//! Fleet polling, liveness, and alert evaluation for the Pessimal clients.
//!
//! This crate turns a telemetry backend into a fleet picture the macOS and iOS apps can render
//! without deciding anything. It is three stages, and the split is the whole design:
//!
//! 1. **Plan** — [`plan_poll`] is a pure function of `(&FleetConfig, now)`. It decides every
//!    request for one poll from configuration and the clock alone, never from accumulated state,
//!    so the same config at the same instant produces the same [`PollPlan`] byte for byte.
//! 2. **Gather** — [`execute_plan`] is the only `async` function in the crate. It issues the plan
//!    and records what came back as a [`PollObservation`]. It makes no decisions and **never
//!    returns `Err`**: a failed request becomes a [`PollFailure`] *inside* the observation.
//! 3. **Fold** — [`FleetState::apply`] is a pure function of `(&FleetConfig, &FleetState,
//!    &PollObservation)`. It returns a [`FleetUpdate`]: the next state, the [`FleetView`] the apps
//!    draw, the [`AlertTransition`]s worth a notification, and the [`PollAdvice`] that says when to
//!    come back. [`poll_once`] is the ten-line composition of all three.
//!
//! Every decision the product makes — liveness, alert dwell, counter normalisation, per-mount
//! reduction, evaluation lifecycle, freshness, backoff — is therefore a synchronous pure function
//! over three plain values. A test constructs its input literally: no mock port, no runtime, no
//! `Clock`, no `Arc<Mutex<_>>`.
//!
//! # The platform drives the clock
//!
//! There is no timer, no task, no runtime and no clock trait here. `now` is an explicit parameter
//! on [`plan_poll`], [`poll_once`], [`probe_backend`] and [`freshness_at`], because only the
//! platform sees `scenePhase`, popover-open, and sleep/wake, and only the platform knows the
//! cadence those imply. Core owns the *policy* — every fold returns a [`PollAdvice`] — and the
//! platform owns the *timer*; it may add jitter, core stays deterministic so a backoff schedule is
//! exact in a test.
//!
//! # Failure freezes; it never fabricates
//!
//! The crate's one load-bearing promise. A backend we cannot reach must never be rendered as a
//! fleet that has gone down.
//!
//! - A failed poll mutates only the metadata. The roster, every [`pessimal_core::Liveness`]
//!   verdict, every `liveness_at` and every stored series are left exactly as they were; only
//!   `as_of`, `consecutive_failures`, `last_failure` and the advised next poll move. Re-running
//!   liveness against an advanced clock would walk a healthy fleet Alive → Stale → Down on the
//!   strength of a dead Wi-Fi connection. "I cannot see" and "your fleet is down" must not share a
//!   code path.
//! - `Ok(vec![])` and `Err` are different absences, and [`Coverage`] keeps them apart structurally:
//!   `observe` is called on [`Coverage::Fresh`] and [`Coverage::Empty`], never on
//!   [`Coverage::Unavailable`]. An empty series legitimately clears dwell; a failed fetch is not
//!   evidence of anything and must not reset every alert in the fleet after one transient 500.
//! - The last good view is never blanked, and never claims to be current. [`FleetView`] carries
//!   [`FreshnessInputs`] rather than a [`Freshness`], and [`freshness_at`] is a function of `now` —
//!   the only shape that greys the screen when a poll hangs or the platform's timer stops, with no
//!   error having arrived at all.
//!
//! # Dependencies
//!
//! [`pessimal_core`] and its two ports, and nothing else: no HTTP, no `reqwest`, no `tokio`, no
//! query adapter, no `uniffi`. The hexagon boundary is enforced by the manifest.
//!
//! # Re-exports
//!
//! The root re-exports this crate's own items only. [`pessimal_core`] is deliberately *not*
//! glob-re-exported: its `Result<T>` alias would shadow [`crate::Result`] and silently change what
//! every `-> Result<_>` in this crate means. Name core's types through `pessimal_core::`.

pub mod config;
pub mod error;
pub mod fold;
pub mod gather;
pub mod normalize;
pub mod observation;
pub mod plan;
pub mod rules;
pub mod view;

#[cfg(any(test, feature = "testing"))]
pub mod testing;

pub use crate::config::{
    DEFAULT_DETAIL_METRICS, DEFAULT_OVERVIEW_METRICS, FleetConfig, PollTuning, TuningWarning,
};
pub use crate::error::{
    ClientError, FailureSource, PollFailure, PollFailureKind, Result, worst_failure,
};
pub use crate::fold::{
    AlertTransition, FLEET_STATE_SCHEMA_VERSION, FleetState, FleetUpdate, PollAdvice, RestoreReport,
};
pub use crate::gather::{BackendProbe, execute_plan, poll_once, probe_backend};
pub use crate::normalize::{series_id, series_label};
pub use crate::observation::{
    Coverage, PollObservation, RosterOutcome, SeriesOutcome, SeriesResult,
};
pub use crate::plan::{PollPlan, QuerySpec, plan_poll};
pub use crate::rules::{
    delete_rule, draft_rule, load_rules, parse_rule_id, save_rule, validate_rule,
};
pub use crate::view::{
    AlertEvidence, AlertPhase, AlertView, CollectionHealth, FleetCounts, FleetView, Freshness,
    FreshnessInputs, HostView, MetricAvailability, MetricFacts, MetricView, PLACEHOLDER_HOST_ID,
    Severity, alertable, describe_metric, expected_on, freshness_at, metric_facts,
};
