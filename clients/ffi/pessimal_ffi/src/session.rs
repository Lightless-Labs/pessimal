//! [`FleetSession`] — the one object an app actually holds.
//!
//! Everything else in this crate is a pure translation: a `*Record` with `From` impls, or a free
//! function that delegates to `pessimal_client_core`. This module is the exception only in that it
//! owns *mutable state* — the last `FleetState`, the current `FleetConfig`, the query adapter, and
//! the in-flight flag. It still makes no decisions: every judgement about liveness, dwell,
//! freshness, or backoff belongs to the client core, and the session's entire job is to hold a
//! mutex correctly and hand core's answers across the boundary.
//!
//! The concurrency rules below are the whole reason this type exists rather than Swift calling
//! `poll_once` directly. `poll_once` is a free function over `&FleetState`; nothing in it
//! serialises a menu-bar timer tick against a pull-to-refresh, and last-write-wins between two
//! folds onto the same base state silently discards one poll's HTTP. The session is where that is
//! prevented, and the rules are stated in the doc comments on each method because none of them is
//! enforced by the type system.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration as StdDuration;

use pessimal_client_core::{FleetConfig, FleetState, FleetUpdate, poll_once, probe_backend};
use pessimal_core::TelemetryQuery;
use pessimal_query_signoz::{SignozConfig, SignozQuery};
use reqwest::redirect::Policy;

use crate::config_records::{FleetConfigRecord, TuningWarningRecord};
use crate::convert::{FfiError, host_id_from_string, millis_to_datetime};
use crate::fold_records::{
    PollAdviceRecord, PollResult, RestoreReportRecord, transitions_to_records,
};
use crate::probe_records::BackendProbeRecord;
use crate::view_records::{FleetViewRecord, FreshnessRecord};

/// How long a TCP connect may take before the query is abandoned.
///
/// Separate from, and much shorter than, `SignozConfig::request_timeout`: a response can legitimately
/// take many seconds for a wide window over a large fleet, but a *connect* that has not completed in
/// five seconds is a dead network, and on a phone that distinction is the difference between a
/// progress spinner and a `CoreError::Unreachable` the banner can actually name.
const CONNECT_TIMEOUT: StdDuration = StdDuration::from_secs(5);

/// The whole client, as one handle Swift keeps for the life of the app.
///
/// Field by field, and why each is the kind of lock it is:
///
/// - `state` and `config` are **`std::sync::Mutex`**, not `tokio::sync::Mutex`, because they are
///   only ever held for a clone or a whole-value store — microseconds of uncontended work with no
///   `await` anywhere inside. An async mutex here would buy nothing and would make
///   [`FleetSession::view`] async, which a `SwiftUI` body cannot call.
/// - `query` is an `Arc<dyn TelemetryQuery>` so the port stays a port: this crate depends on the
///   SigNoz adapter to *build* one in [`FleetSession::new`], and on nothing but
///   `pessimal_core::TelemetryQuery` to use it. It needs no lock — `TelemetryQuery` is
///   `Send + Sync` and the adapter is immutable once built.
/// - `in_flight` is a **`tokio::sync::Mutex<()>`**, the one genuinely async lock, because it *is*
///   held across the `poll_once` await. It guards nothing but the right to be the poll that runs.
/// - `restore` is set once at construction and never written again, so it needs no lock at all.
///   It exists because the constructor cannot return `FleetState::restore`'s
///   `(FleetState, RestoreReport)` tuple: UniFFI carries no tuples, and an `Object` constructor
///   returns `Arc<Self>` or an error and nothing else.
#[derive(uniffi::Object)]
pub struct FleetSession {
    state: Mutex<FleetState>,
    config: Mutex<FleetConfig>,
    query: Arc<dyn TelemetryQuery>,
    /// A mutex over `()`: the unit is the point. Nothing is protected except the privilege of
    /// polling, and a mutex rather than an `AtomicBool` because the guard is RAII — see
    /// [`FleetSession::poll`].
    in_flight: tokio::sync::Mutex<()>,
    restore: Option<RestoreReportRecord>,
}

/// Recovers a `std::sync::Mutex` from poisoning instead of panicking or returning an error.
///
/// Poisoning means some thread panicked while holding the lock. That is *sound* to recover from
/// here, and not merely convenient: every write through either mutex in this module is a
/// whole-value store of a value that was fully built outside the lock (`*guard = next_state`), so
/// there is no partially-updated `FleetState` or `FleetConfig` a panic could leave behind. The
/// value inside a poisoned lock is therefore the same valid value it was before.
///
/// The alternatives are both worse at a UniFFI boundary. `.expect()` turns a recoverable condition
/// into a process abort that reaches Swift as a crash with no message. Propagating a `Result` would
/// force [`FleetSession::view`] and [`FleetSession::restore_report`] to be throwing, which is the
/// one thing a `SwiftUI` body cannot easily call — and it would make the app handle an error it can
/// do nothing about.
fn recover<'guard, T>(
    result: Result<MutexGuard<'guard, T>, PoisonError<MutexGuard<'guard, T>>>,
) -> MutexGuard<'guard, T> {
    result.unwrap_or_else(PoisonError::into_inner)
}

impl FleetSession {
    /// Assembles a session over an already-built port. The seam [`FleetSession::new`] goes through.
    ///
    /// Not exported, and deliberately generic over the port rather than over SigNoz: it is what
    /// lets a test — or a second backend adapter — supply its own [`TelemetryQuery`] without a
    /// network, an HTTP client, or a base URL. `new` is then only the SigNoz-specific wiring.
    ///
    /// The backend name comes from the port rather than from an argument, so the restored state,
    /// the live state, and every view agree on what they are looking at by construction.
    pub(crate) fn assemble(
        query: Arc<dyn TelemetryQuery>,
        config: FleetConfig,
        cached_state_json: Option<&str>,
    ) -> Self {
        let backend_name = query.backend_name().to_owned();
        // `FleetState::restore` never returns `Err`: a monitoring app must not refuse to launch
        // because its cache is a schema behind. What could not be salvaged comes back in the
        // report, which is why the report is kept rather than dropped.
        let (state, restore) = match cached_state_json {
            Some(json) => {
                let (state, report) = FleetState::restore(&backend_name, json);
                (state, Some(RestoreReportRecord::from(report)))
            }
            // `None` rather than a zeroed report: "this session started with no cache" and "this
            // session restored a cache that happened to hold nothing" are different facts, and a
            // first-launch screen should not report discarded-nothing as if a restore had happened.
            None => (FleetState::new(backend_name), None),
        };

        Self {
            state: Mutex::new(state),
            config: Mutex::new(config),
            query,
            in_flight: tokio::sync::Mutex::new(()),
            restore,
        }
    }

    /// The current state, cloned, with the lock released before this returns.
    ///
    /// A separate method rather than an inline `self.state.lock()` at each use site so that no
    /// caller can accidentally keep the guard alive across an `await`. `clippy::await_holding_lock`
    /// catches that, but only where the guard is a local binding; returning an owned clone means
    /// there is nothing to catch.
    fn state_snapshot(&self) -> FleetState {
        recover(self.state.lock()).clone()
    }

    /// The current config, cloned, with the lock released before this returns.
    ///
    /// Never locked at the same time as `state`. Taking one lock at a time makes a deadlock between
    /// the two impossible without any lock-ordering convention for a future reader to violate.
    fn config_snapshot(&self) -> FleetConfig {
        recover(self.config.lock()).clone()
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl FleetSession {
    /// Builds a SigNoz-backed session.
    ///
    /// Two lines in here look optional and are not. Nothing fails if either is deleted; the
    /// symptom in both cases is a *healthy* fleet silently reading stale, which no error path will
    /// ever report:
    ///
    /// 1. **`SignozConfig::for_policy`, not `SignozConfig::new` followed by
    ///    `with_heartbeat_interval`.** The query step decides how precisely a heartbeat's age can
    ///    be measured; the [`pessimal_core::LivenessPolicy`] decides what that age *means*. Set
    ///    separately, the two drift the first time somebody edits one of them, and a fleet whose
    ///    agents beat every 30s read through a 60s step is a fleet that reads `Stale` while it is
    ///    perfectly alive. `for_policy` takes both from the same tuning, so they cannot disagree.
    /// 2. **A `reqwest::Client` with timeouts, handed over via `SignozQuery::with_client`.**
    ///    `with_client` bypasses `SignozQuery::new`'s own builder, so every property that builder
    ///    sets has to be re-supplied here or it is simply gone: `reqwest` has **no default
    ///    timeout**, so a hung SigNoz hangs the poll forever — a spinner that never resolves, and a
    ///    `CoreError::Unreachable` that a timeout can never actually produce. `redirect(none)` is
    ///    equally load-bearing: the API key travels in a custom `SIGNOZ-API-KEY` header, and
    ///    `reqwest` only strips `Authorization` across a host change, so a followed redirect would
    ///    replay the credential to wherever it points. `use_native_tls` keeps the client on
    ///    Security.framework, which is what trusts a self-hosted SigNoz behind an internal CA and
    ///    what avoids cross-compiling `aws-lc-rs` for iOS.
    ///
    /// `cached_state_json` is the string a previous session's [`FleetSession::export_state`]
    /// produced. A bad one is not an error: see [`FleetSession::restore_report`].
    ///
    /// # Errors
    /// [`FfiError::InvalidConfig`] if the environment is empty or contains `::`;
    /// [`FfiError::InvalidTuning`] if the tuning fails one of core's interlocks;
    /// [`FfiError::InvalidRule`] if `config.rules_json` is not a rule set core accepts;
    /// [`FfiError::Backend`] if the base URL has no scheme, the API key is blank, or the HTTP
    /// client cannot be built.
    #[uniffi::constructor]
    #[allow(
        clippy::needless_pass_by_value,
        reason = "UniFFI lifts a foreign string as an owned `String`; `&str` cannot cross the boundary"
    )]
    pub fn new(
        base_url: String,
        api_key: String,
        config: FleetConfigRecord,
        cached_state_json: Option<String>,
    ) -> Result<Arc<Self>, FfiError> {
        let config = FleetConfig::try_from(config)?;

        // (1) The step and the yardstick, taken from one place. See the method doc.
        let signoz = SignozConfig::for_policy(base_url, api_key, &config.tuning.liveness())?;

        // `with_client` below bypasses SignozQuery::new, which is where the adapter would have
        // installed rustls's crypto provider. reqwest's `rustls-no-provider` feature *panics* when
        // a client is built without one, and a panic here crosses UniFFI as an app crash on the
        // user's first poll, so install it before building. Idempotent.
        pessimal_query_signoz::install_crypto_provider();

        // (2) The timeouts the adapter's own constructor would have set. See the method doc.
        //
        // TLS is left to reqwest's configured default, which the manifest pins to rustls with the
        // ring provider and the platform verifier — not selected here, because this code builds for
        // macOS, iOS, Android, Linux and Windows.
        let http = reqwest::Client::builder()
            .timeout(signoz.request_timeout())
            .connect_timeout(CONNECT_TIMEOUT)
            .redirect(Policy::none())
            .build()
            .map_err(|error| FfiError::Backend {
                message: format!("could not build an HTTP client: {error}"),
            })?;

        let query = SignozQuery::with_client(signoz, http);
        Ok(Arc::new(Self::assemble(
            Arc::new(query),
            config,
            cached_state_json.as_deref(),
        )))
    }

    /// One whole poll: plan it, issue it, fold it, keep the result.
    ///
    /// **The `state` mutex is never held across the await.** Lock, clone the state and the config,
    /// unlock, `await`, relock, store. `clippy::await_holding_lock` is denied workspace-wide and
    /// catches the *hold* half of that mistake. It does not catch the *store* half — a fold that
    /// writes its `next` state back over a change some other call made in the meantime — which is
    /// why [`FleetSession::forget_host`] and [`FleetSession::set_config`] take `in_flight` with
    /// `.lock().await` rather than `try_lock`. Holding it for their whole duration is what makes a
    /// mid-poll forget impossible to clobber, because it cannot happen mid-poll at all.
    ///
    /// **A poll already in flight is answered with `skipped`, not with an error.** A menu-bar timer
    /// tick and a pull-to-refresh *will* collide, and two folds onto the same base state would
    /// silently discard one poll's HTTP — the later store simply overwrites the earlier, transitions
    /// and all. `skipped` is a normal outcome, so it is a field on [`PollResult`] rather than a
    /// thrown error; an error here would make the app show a banner for something that went right.
    /// The view and the advice come along anyway: the screen redraws from the current view either
    /// way, and the timer has to be rescheduled from something.
    ///
    /// **The guard is RAII, which is the reason it is a mutex and not an `AtomicBool`.** UniFFI has
    /// no cancellation: when Swift drops the `Task` wrapping this call, the future is simply
    /// dropped, mid-HTTP, with no unwinding and no completion handler. A flag cleared on the way
    /// out would never be cleared, and the poller would be wedged for the life of the process. A
    /// `MutexGuard` releases when it is dropped, which a dropped future does for us. Note the
    /// binding name: `_in_flight`, never `_` — a wildcard pattern drops the guard on that very
    /// line and the whole mechanism quietly does nothing.
    ///
    /// # Errors
    /// [`FfiError::Internal`] only, and only for a programming fault: a `now_millis` no
    /// `DateTime<Utc>` can represent, or a plan core refused. Every *backend* failure — a 401, a
    /// dead network, a metric name that does not exist — arrives as `PollFailure` data inside
    /// `view.freshness`, because a poll that failed is still a poll whose last good view must
    /// survive on screen. Throwing for one would blank a screen full of perfectly good data.
    pub async fn poll(&self, now_millis: i64) -> Result<PollResult, FfiError> {
        let now = millis_to_datetime(now_millis)?;

        let Ok(_in_flight) = self.in_flight.try_lock() else {
            let state = self.state_snapshot();
            let config = self.config_snapshot();
            // Core's own advice over the unchanged state: the honest answer to "when should I come
            // back" when this call changed nothing.
            return Ok(PollResult::skipped_in_flight(
                FleetViewRecord::from(state.view(&config)),
                PollAdviceRecord::from(state.advice(&config)),
            ));
        };

        // Both guards are released inside these calls, before the await below.
        let state = self.state_snapshot();
        let config = self.config_snapshot();

        // `poll_once`'s only error is a plan core refused, which a `PollTuning` built through its
        // constructor cannot trigger — so it is a fault in this crate, not an outage, and
        // `FfiError::Internal` is what `convert`'s own documentation reserves for exactly that.
        // Re-classifying it as `InvalidTimeRange` would invite the app to show an outage banner for
        // a programming mistake, which core's own doc on `poll_once` warns against by name.
        let update = poll_once(self.query.as_ref(), &config, &state, now)
            .await
            .map_err(|error| FfiError::Internal {
                message: error.to_string(),
            })?;

        // Exhaustive destructure, no `..`: `state` goes back into the mutex and must not be the
        // field a future addition to `FleetUpdate` quietly displaces.
        let FleetUpdate {
            state: next,
            view,
            transitions,
            advice,
        } = update;

        // The store half. Still inside `_in_flight`, so no `set_config` or `forget_host` can have
        // landed between the clone above and this write.
        *recover(self.state.lock()) = next;

        Ok(PollResult::folded(
            FleetViewRecord::from(view),
            transitions_to_records(&transitions),
            PollAdviceRecord::from(advice),
        ))
    }

    /// The fleet as it currently stands, projected from the state this session holds.
    ///
    /// Synchronous and infallible so a `SwiftUI` body can call it directly: a view that has to
    /// `await` or `try` to draw itself needs a state machine around it, and there is nothing here
    /// to await — the projection is pure and the lock is held only for a clone. Callable before any
    /// poll, where it answers with an empty fleet rather than refusing: a first launch has a screen
    /// to draw too.
    ///
    /// Takes no `now`. Ages are presentation's business, and the one thing on the view that *is* a
    /// function of `now` — how much the picture deserves to be believed — is
    /// [`FleetSession::freshness`], which is why it is a separate call.
    #[must_use]
    pub fn view(&self) -> FleetViewRecord {
        let state = self.state_snapshot();
        let config = self.config_snapshot();
        FleetViewRecord::from(state.view(&config))
    }

    /// How much the picture on screen deserves to be believed, **as of `now`**.
    ///
    /// A separate call, recomputed per `TimelineView` tick, because freshness is a function of
    /// `now` and not a field frozen at fold time. A `Freshness` computed during the fold is only
    /// correct at the instant it was computed: between polls it does not move, so a hung
    /// `execute_plan` — no error, nothing to fold — or a Swift timer that stopped when the app was
    /// suspended would leave the banner reading `Fresh` indefinitely while the data underneath it
    /// rots. Recomputing from `now` is the only shape that greys the screen when *nothing is
    /// calling us at all*, which is precisely the failure no error path can report. Do not cache
    /// the answer.
    ///
    /// # Errors
    /// [`FfiError::Internal`] if `now_millis` is not a representable instant. The alternative — an
    /// infallible signature with a clamp or a fallback to a second clock read — would compute
    /// freshness against a fictional instant and report it as fact, which is the one outcome this
    /// function exists to prevent. `Date` on the Swift side cannot produce such a value, so this
    /// is a bridge fault rather than anything a user did.
    pub fn freshness(&self, now_millis: i64) -> Result<FreshnessRecord, FfiError> {
        let now = millis_to_datetime(now_millis)?;
        let state = self.state_snapshot();
        let config = self.config_snapshot();
        Ok(FreshnessRecord::from(state.view(&config).freshness_at(now)))
    }

    /// Replaces the configuration and returns core's audit of it.
    ///
    /// **Async because it takes `in_flight` with `.lock().await`.** A config swapped in between a
    /// running poll's clone and its store would be clobbered the moment the fold wrote back:
    /// `poll` cloned the *old* config, folded against it, and stores a state computed from it.
    /// `try_lock` would be wrong here in a way `try_lock` in `poll` is not — a skipped poll loses
    /// one HTTP round trip, a skipped config change loses the user's edit with no sign it happened.
    ///
    /// Validation happens *before* the lock is taken, so a config core refuses fails immediately
    /// rather than after waiting out a poll that was going to reject it anyway.
    ///
    /// The warnings come back rather than being thrown: each describes a rule that is legal and
    /// almost certainly not what its author meant, so the config is stored and the settings screen
    /// is told. Refusing the save would be refusing a configuration core accepts.
    ///
    /// One limitation, stated rather than hidden: the SigNoz query's step was baked from the
    /// *constructor's* [`pessimal_core::LivenessPolicy`] by `SignozConfig::for_policy`, and the
    /// query behind an `Arc<dyn TelemetryQuery>` cannot be rebuilt from here. A config whose
    /// liveness policy differs from the one this session was built with therefore reintroduces
    /// exactly the step/yardstick disagreement `for_policy` exists to prevent — so a liveness
    /// change requires a new session, not a `set_config`.
    ///
    /// # Errors
    /// [`FfiError::InvalidConfig`], [`FfiError::InvalidTuning`], or [`FfiError::InvalidRule`],
    /// carrying core's own message, for a configuration core refuses. Nothing is stored in that
    /// case.
    pub async fn set_config(
        &self,
        config: FleetConfigRecord,
    ) -> Result<Vec<TuningWarningRecord>, FfiError> {
        let config = FleetConfig::try_from(config)?;
        let warnings = config.audit();

        let _in_flight = self.in_flight.lock().await;
        *recover(self.config.lock()) = config;

        Ok(warnings
            .into_iter()
            .map(TuningWarningRecord::from)
            .collect::<Vec<TuningWarningRecord>>())
    }

    /// The state as JSON, for the app to persist.
    ///
    /// `FleetState::to_json` exactly — no envelope of this crate's own, because the string a
    /// session exports is the string [`FleetSession::new`] restores, and a second format in between
    /// would be a second thing to keep in step. `FleetState` is opaque by design: private fields
    /// over nested maps, crossing the boundary only as this string.
    ///
    /// Synchronous, and deliberately *not* holding `in_flight`: it writes nothing, so whichever
    /// snapshot it catches mid-poll — before the fold or after it — is a whole, valid state. Making
    /// it wait on a poll would mean an app being backgrounded could not save until a hung request
    /// timed out.
    ///
    /// # Errors
    /// [`FfiError::Backend`] if the state cannot be serialised, carrying core's message.
    pub fn export_state(&self) -> Result<String, FfiError> {
        self.state_snapshot().to_json().map_err(FfiError::from)
    }

    /// What this session's cold start could and could not salvage, if it had a cache to start from.
    ///
    /// This method exists because `FleetState::restore` returns `(FleetState, RestoreReport)` and a
    /// tuple cannot cross UniFFI — nor can an `Object` constructor return anything but `Arc<Self>`.
    /// The session keeps the report and hands it over on request.
    ///
    /// `None` means there was no cache to restore, which is not the same fact as a report with no
    /// `discarded_*` flags set. `Some` with `discarded_unreadable` or `discarded_incompatible` is
    /// the case worth surfacing: the app is running on an empty state and the user's history is
    /// gone, which a first poll will hide within seconds. A restore never *fails* — refusing to
    /// launch because the cache is a schema behind would be the wrong answer for a monitoring app —
    /// so this report is the only place that loss is visible.
    #[must_use]
    pub fn restore_report(&self) -> Option<RestoreReportRecord> {
        self.restore
    }

    /// Drops a host, its series, and its evaluations — an operator decommissioning a machine.
    ///
    /// **Async because it takes `in_flight` with `.lock().await`**, for the reason spelled out on
    /// [`FleetSession::poll`]: a forget applied between a running poll's clone and its store would
    /// be erased by the fold writing back, and the host would reappear with no sign anything was
    /// lost. `clippy::await_holding_lock` cannot see that mistake, so the lock is what prevents it.
    ///
    /// Returns the view the drop produced, so the caller redraws from the same projection the next
    /// poll will agree with rather than deleting a row locally and hoping.
    ///
    /// An unknown host id is not an error. Core's `FleetState::forget_host` is infallible, and
    /// restating a contract core does not have would make the two diverge the moment core's
    /// changes; "forget a host that was already forgotten" is also idempotent, which is what a
    /// retried call from a flaky UI needs.
    ///
    /// # Errors
    /// Never, today. The `Result` is mandated rather than earned: a non-`Result` exported `async
    /// fn` generates Swift `try!`, which aborts the process if Rust panics, so every async export
    /// here is fallible by declaration even where nothing can fail.
    pub async fn forget_host(&self, host_id: String) -> Result<FleetViewRecord, FfiError> {
        let host = host_id_from_string(host_id);

        let _in_flight = self.in_flight.lock().await;
        let next = self.state_snapshot().forget_host(&host);
        let config = self.config_snapshot();
        let view = next.view(&config);
        *recover(self.state.lock()) = next;

        Ok(FleetViewRecord::from(view))
    }

    /// Tests the backend the way a poll uses it — "test connection" on a settings screen.
    ///
    /// Deliberately does **not** take `in_flight`. It mutates nothing, and a user tapping Test
    /// Connection while the timer happens to be polling must get an answer rather than a silent
    /// skip; the whole point of the button is to be pressable when things are going wrong.
    ///
    /// Infallible in substance — the outcome *is* the return value, including "connected, but no
    /// hosts reported in the last 210s", which is the misconfiguration a bare health check cannot
    /// detect. A settings screen's job is to report a misconfiguration, never to raise one.
    ///
    /// # Errors
    /// [`FfiError::Internal`] if `now_millis` is not a representable instant. A bridge fault, not a
    /// connection problem: a connection problem arrives inside [`BackendProbeRecord`].
    pub async fn probe(&self, now_millis: i64) -> Result<BackendProbeRecord, FfiError> {
        let now = millis_to_datetime(now_millis)?;
        let tuning = self.config_snapshot().tuning;
        Ok(BackendProbeRecord::from(
            probe_backend(self.query.as_ref(), &tuning, now).await,
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use chrono::Duration;
    use pessimal_client_core::{FleetConfig, PollTuning, draft_rule};
    use pessimal_core::{
        AlertRule, Comparator, Host, HostSelector, MetricKind, MetricSeries, Result as CoreResult,
        SeriesRequest, TelemetryQuery, TimeRange,
    };
    use tokio::sync::Notify;
    use tokio::task::JoinHandle;

    use super::FleetSession;
    use crate::config_records::{FleetConfigRecord, TuningWarningRecord};
    use crate::convert::FfiError;
    use crate::fold_records::PollResult;
    use crate::view_records::FreshnessRecord;

    const NOW_MILLIS: i64 = 1_760_000_000_000;

    /// A base URL that parses and is never connected to. The tests that use it either fail their
    /// `now` conversion first or touch nothing but the state and config mutexes, so the real SigNoz
    /// adapter is built and never used — which is the point: they exercise [`FleetSession::new`]'s
    /// own wiring rather than going around it.
    const BASE_URL: &str = "https://signoz.invalid";

    /// A [`TelemetryQuery`] whose roster call parks until the test lets it go.
    ///
    /// `pessimal_client_core::testing::ScriptedQuery` would be the natural double, but it sits behind
    /// that crate's `testing` feature and this crate's `Cargo.toml` is not this module's to change.
    /// Hence a local stub — and one written in `async_trait`'s *desugared* shape
    /// (`Pin<Box<dyn Future + Send>>`), because [`TelemetryQuery`] is an `#[async_trait]` trait and
    /// this crate does not depend on the macro. The desugaring is mechanical: one lifetime per
    /// reference argument, one for the returned future, each outlived by the future's.
    ///
    /// Parking inside `list_hosts` is what makes a poll *genuinely* in flight, which is the only way
    /// to test the in-flight guard for real: a test that takes `in_flight` by hand would also pass
    /// against a `poll` that dropped the guard on the line it acquired it — the exact mistake the
    /// `_in_flight` binding name exists to prevent.
    struct GatedQuery {
        /// Fired when the first `list_hosts` has been entered: from there the poll holding
        /// `in_flight` is genuinely in flight.
        entered: Arc<Notify>,
        /// Awaited by the first `list_hosts`, so the test decides when that poll may finish.
        release: Arc<Notify>,
        /// Only the *first* roster call parks. Later ones answer at once so that a regression in
        /// `poll`'s guard — a `_` binding that drops it on the line it was taken, say — surfaces as
        /// the second poll folding instead of skipping, which fails an assertion. Were every call
        /// gated, the same regression would hang the suite instead of naming itself.
        roster_calls: AtomicUsize,
    }

    /// The shape `#[async_trait]` gives every method of [`TelemetryQuery`].
    type Parked<'fut, T> = Pin<Box<dyn Future<Output = CoreResult<T>> + Send + 'fut>>;

    impl TelemetryQuery for GatedQuery {
        fn list_hosts<'this, 'fut>(&'this self, _range: TimeRange) -> Parked<'fut, Vec<Host>>
        where
            'this: 'fut,
            Self: 'fut,
        {
            Box::pin(async move {
                if self.roster_calls.fetch_add(1, Ordering::Relaxed) == 0 {
                    self.entered.notify_one();
                    self.release.notified().await;
                }
                // An empty roster rather than a failure: these tests are about the guard, and a
                // failing poll would also fold — just with a `PollFailure` in the view.
                Ok(Vec::new())
            })
        }

        fn query_series<'this, 'request, 'fut>(
            &'this self,
            _request: &'request SeriesRequest,
        ) -> Parked<'fut, Vec<MetricSeries>>
        where
            'this: 'fut,
            'request: 'fut,
            Self: 'fut,
        {
            Box::pin(async move { Ok(Vec::new()) })
        }

        fn check_connection<'this, 'fut>(&'this self) -> Parked<'fut, ()>
        where
            'this: 'fut,
            Self: 'fut,
        {
            Box::pin(async move { Ok(()) })
        }

        fn backend_name(&self) -> &'static str {
            "Gated"
        }
    }

    fn config() -> FleetConfig {
        FleetConfig::new("prod", PollTuning::default()).expect("`prod` is a usable environment")
    }

    /// A rule whose dwell is shorter than the default evidence horizon (330 s), which is exactly
    /// what `FleetConfig::audit` reports as `SpikeCanFire`.
    fn spiky_rule() -> AlertRule {
        draft_rule(
            "prod",
            "cpu hot",
            MetricKind::CpuUtilization,
            Comparator::GreaterThan,
            0.9,
            Duration::seconds(10),
            HostSelector::All,
        )
        .expect("a 10 s dwell on an alertable metric is a rule core accepts")
    }

    fn record(config: &FleetConfig) -> FleetConfigRecord {
        FleetConfigRecord::try_from(config.clone())
            .expect("a config core built renders as a record")
    }

    /// A session built the way an app builds one, through the exported constructor.
    fn session(cached_state_json: Option<&str>) -> Arc<FleetSession> {
        FleetSession::new(
            BASE_URL.to_owned(),
            "a-key".to_owned(),
            record(&config()),
            cached_state_json.map(str::to_owned),
        )
        .expect("a parseable URL, a non-blank key, and a config core accepts")
    }

    /// A session over [`GatedQuery`], through the `assemble` seam: no URL, no HTTP client, no
    /// network, and a poll that stops exactly where the test wants it.
    fn gated() -> (Arc<FleetSession>, Arc<Notify>, Arc<Notify>) {
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let query = GatedQuery {
            entered: Arc::clone(&entered),
            release: Arc::clone(&release),
            roster_calls: AtomicUsize::new(0),
        };
        let session = Arc::new(FleetSession::assemble(Arc::new(query), config(), None));
        (session, entered, release)
    }

    fn spawn_poll(session: &Arc<FleetSession>) -> JoinHandle<Result<PollResult, FfiError>> {
        let session = Arc::clone(session);
        tokio::spawn(async move { session.poll(NOW_MILLIS).await })
    }

    /// Lets every other task on this runtime run until it parks.
    ///
    /// `#[tokio::test]` is a current-thread runtime, so yielding hands the scheduler to the spawned
    /// writer, which then runs as far as it can get — to `in_flight.lock().await` — before control
    /// comes back. A `sleep` would be the usual reflex and is the wrong tool twice over: the virtual
    /// clock that would make it instant needs tokio's `test-util` feature, which this crate does not
    /// enable, and a real one would trade determinism for wall-clock time.
    async fn settle() {
        for _ in 0..4 {
            tokio::task::yield_now().await;
        }
    }

    /// The collision `try_lock` exists for, with a poll genuinely parked mid-request.
    #[tokio::test]
    async fn a_second_poll_while_one_is_in_flight_is_skipped() {
        let (session, entered, release) = gated();
        let first = spawn_poll(&session);
        entered.notified().await;

        // Neither call may block: a `state` lock held across `poll_once`'s await would deadlock this
        // current-thread runtime here, which is the failure `clippy::await_holding_lock` prevents and
        // this line proves.
        assert!(session.view().hosts.is_empty());
        session
            .export_state()
            .expect("a state can be persisted while a poll is in flight");

        let second = session
            .poll(NOW_MILLIS)
            .await
            .expect("a skipped poll is a normal outcome, not an error");
        assert!(
            second.skipped,
            "the loser of the race reports `skipped` rather than folding onto the same base state"
        );
        assert!(
            second.transitions.is_empty(),
            "nothing was folded, so there is no crossing to notify about"
        );
        assert_eq!(
            second.view.backend_name, "Gated",
            "the view comes along anyway: the screen redraws from it either way"
        );

        release.notify_one();
        let first = first
            .await
            .expect("the polling task did not panic")
            .expect("a roster that answers folds");
        assert!(
            !first.skipped,
            "the poll that held the guard is the one that ran"
        );
        assert!(
            session.in_flight.try_lock().is_ok(),
            "the guard is RAII: completing the poll released it"
        );
    }

    /// The dropped-`Task` case, which is why the guard is a mutex and not an `AtomicBool`.
    ///
    /// `abort` drops the future mid-request with no unwinding and no completion handler — exactly
    /// what UniFFI's lack of cancellation leaves behind when Swift drops the `Task` wrapping a poll.
    /// A flag cleared on the way out would never be cleared and the poller would be wedged for the
    /// life of the process.
    #[tokio::test]
    async fn a_poll_dropped_mid_flight_releases_the_guard() {
        let (session, entered, _release) = gated();
        let dropped = spawn_poll(&session);
        entered.notified().await;

        dropped.abort();
        assert!(
            dropped
                .await
                .expect_err("an aborted task never returns a value")
                .is_cancelled(),
            "the future was dropped where Swift would have dropped it: mid-request"
        );

        let next = session
            .poll(NOW_MILLIS)
            .await
            .expect("a poll after a dropped poll");
        assert!(
            !next.skipped,
            "an `AtomicBool` cleared on the way out would have wedged the poller here forever"
        );
    }

    /// The store half of the rule, which `clippy::await_holding_lock` cannot see.
    #[tokio::test]
    async fn set_config_waits_for_a_poll_in_flight_rather_than_landing_mid_fold() {
        let (session, entered, release) = gated();
        let poll = spawn_poll(&session);
        entered.notified().await;

        let spiky = record(&config().with_rules(vec![spiky_rule()]));
        let writer = {
            let session = Arc::clone(&session);
            tokio::spawn(async move { session.set_config(spiky).await })
        };
        settle().await;

        assert!(
            !writer.is_finished(),
            "`try_lock` here would have stored the config mid-poll"
        );
        assert!(
            session.view().warnings.is_empty(),
            "the config the running fold was computed from is still the live one; a config swapped \
             in now would be the one the fold's write-back silently contradicts"
        );

        release.notify_one();
        let warnings = writer
            .await
            .expect("the writing task did not panic")
            .expect("a legal config with a questionable rule is stored");
        assert!(
            !poll
                .await
                .expect("the polling task did not panic")
                .expect("a roster that answers folds")
                .skipped
        );
        assert_eq!(
            session.view().warnings,
            warnings,
            "the edit landed once the poll released the guard, not before"
        );
    }

    /// Same rule, the other writer. A forget applied mid-poll would be erased by the fold's
    /// write-back, and the host would reappear with nothing to say it had been dropped.
    #[tokio::test]
    async fn forget_host_waits_for_a_poll_in_flight_rather_than_being_erased_by_its_fold() {
        let (session, entered, release) = gated();
        let poll = spawn_poll(&session);
        entered.notified().await;

        let forget = {
            let session = Arc::clone(&session);
            tokio::spawn(async move { session.forget_host("retired-01".to_owned()).await })
        };
        settle().await;
        assert!(
            !forget.is_finished(),
            "a forget must wait out the poll whose fold would otherwise overwrite it"
        );

        release.notify_one();
        assert!(
            !poll
                .await
                .expect("the polling task did not panic")
                .expect("a roster that answers folds")
                .skipped
        );
        forget
            .await
            .expect("the forgetting task did not panic")
            .expect("core's `forget_host` is infallible and this crate adds no contract");
    }

    #[tokio::test]
    async fn set_config_stores_the_config_and_returns_core_s_warnings() {
        let session = session(None);
        let spiky = config().with_rules(vec![spiky_rule()]);

        let warnings = session
            .set_config(record(&spiky))
            .await
            .expect("a legal config with a questionable rule is stored, not refused");

        assert!(
            matches!(
                warnings.as_slice(),
                [TuningWarningRecord::SpikeCanFire { rule_name, .. }] if rule_name == "cpu hot"
            ),
            "a dwell shorter than the evidence horizon is core's `SpikeCanFire`, got {warnings:?}"
        );
        assert_eq!(
            session.view().warnings,
            warnings,
            "the stored config is the one that was audited, so the view carries the same warnings"
        );
    }

    #[tokio::test]
    async fn set_config_refuses_a_config_core_refuses_and_stores_nothing() {
        let session = session(None);
        let mut bad = record(&config());
        bad.environment = "pro::d".to_owned();

        let error = session
            .set_config(bad)
            .await
            .expect_err("`::` separates URN segments, so core refuses it");

        assert!(
            matches!(error, FfiError::InvalidConfig { .. }),
            "core's classification crosses unchanged, got {error:?}"
        );
        assert!(
            session.view().warnings.is_empty(),
            "a refused config must not have been stored"
        );
    }

    #[test]
    fn export_state_round_trips_through_a_new_session() {
        let saved = session(None);
        assert!(
            saved.restore_report().is_none(),
            "no cache means no report, which is not the same fact as a report of nothing discarded"
        );

        let exported = saved.export_state().expect("a fresh state serialises");

        let restored = session(Some(&exported));
        let report = restored
            .restore_report()
            .expect("a session given a cache has a report");
        assert!(
            !report.discarded_unreadable && !report.discarded_incompatible,
            "a string this crate itself exported must restore, got {report:?}"
        );
        assert_eq!(
            restored
                .export_state()
                .expect("the restored state serialises"),
            exported,
            "export -> restore -> export is the identity; the state is the only thing persisted"
        );
    }

    #[test]
    fn an_unreadable_cache_is_reported_rather_than_refused() {
        let restored = session(Some("{ not json"));
        let report = restored.restore_report().expect("a cache was offered");
        assert!(
            report.discarded_unreadable,
            "a monitoring app launches on an empty state and says so, got {report:?}"
        );
    }

    /// `view` and `freshness` are the two calls a first launch makes before any poll has run, so
    /// neither may depend on one having run — and neither is async, so a `SwiftUI` body can call
    /// them.
    #[test]
    fn view_and_freshness_are_callable_before_any_poll() {
        let session = session(None);

        let view = session.view();
        assert!(view.hosts.is_empty(), "nothing has been observed yet");
        assert!(view.alerts.is_empty());
        assert_eq!(
            view.as_of_millis, None,
            "there has been no fold to be as of"
        );
        assert_eq!(view.counts.hosts, 0);

        let freshness = session
            .freshness(NOW_MILLIS)
            .expect("a representable instant");
        assert!(
            matches!(freshness, FreshnessRecord::Unusable { .. }),
            "a session that has never seen a roster cannot claim its picture is worth believing, \
             got {freshness:?}"
        );
    }

    #[tokio::test]
    async fn forget_host_is_idempotent_for_a_host_that_was_never_seen() {
        let session = session(None);
        let view = session
            .forget_host("never-existed".to_owned())
            .await
            .expect("core's `forget_host` is infallible and this crate adds no contract");
        assert!(view.hosts.is_empty());
    }

    #[tokio::test]
    async fn an_unrepresentable_instant_is_an_internal_fault_not_an_outage() {
        let session = session(None);
        for error in [
            session.poll(i64::MAX).await.expect_err("not an instant"),
            session.freshness(i64::MAX).expect_err("not an instant"),
            session.probe(i64::MAX).await.expect_err("not an instant"),
        ] {
            assert!(
                matches!(error, FfiError::Internal { .. }),
                "Swift's `Date` cannot produce this, so it is a bridge fault, got {error:?}"
            );
        }
    }
}
