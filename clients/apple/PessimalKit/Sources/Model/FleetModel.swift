//
//  FleetModel.swift
//  Pessimal — macOS menu bar client
//
//  The one thing the views read, and the one thing that talks to `FleetSession`.
//
//  Its whole job is sequencing: build the session, run the clock, hold the last answer, hand
//  errors to whoever can act on them. Every *judgement* on this screen — whether a host is alive,
//  how bad the fleet is, which alert matters, how much the picture deserves to be believed, and
//  when to poll again — was made in Rust and arrives already decided. If you find yourself adding
//  a comparison against a number here, it belongs on the other side of the bridge.
//

// AppKit only exists on macOS. This file is shared with the iOS app, where waking is a scene-phase
// event the app layer reports rather than a notification this type can observe — hence the guards
// below rather than two copies of the model.
#if canImport(AppKit)
    import AppKit
#endif
import Foundation
import Observation
#if canImport(PessimalFFI)
    // Core's records arrive as their own Swift module when this directory is built as one —
    // the `//clients/apple/PessimalKit` Bazel target, which is how the iOS app consumes it.
    // The macOS app instead compiles these files *and* the generated bindings into a single
    // module (see `scripts/build-macos-app.sh`), where no module named `PessimalFFI` exists
    // and a plain import would not resolve. Hence the guard: the same sources have to build
    // both ways, and which way is in force is not something they can be told.
    import PessimalFFI
#endif

@MainActor
@Observable
public final class FleetModel {
    // MARK: - State the views read

    /// Whether there is a session at all, and if not, why not.
    public enum SessionState: Equatable {
        /// Nothing has ever been saved: no URL, no key.
        ///
        /// A distinct case rather than an empty fleet, because those two look identical on screen
        /// and mean opposite things. "You have not set this up yet" invites a click; "no hosts are
        /// reporting" is an outage. The menu must never show the second when it means the first.
        case unconfigured

        /// A session exists and is being polled. Whether its last poll *worked* is a different
        /// question, answered by ``pollingState`` and ``freshness(at:)``.
        case ready

        /// Settings exist and core refused them — no scheme on the URL, a blank key, a tuning
        /// that fails one of core's interlocks. The message is core's own sentence; it names the
        /// field, so it is shown rather than paraphrased.
        case unusable(message: String)
    }

    /// What the poll loop is doing right now.
    public enum PollingState: Equatable {
        /// No timer armed and no session to arm one for.
        case idle

        /// Sleeping until ``nextPollAt``.
        case scheduled

        /// A poll is in flight.
        case polling

        /// Deliberately paused — the popover closed, or the app asked for quiet.
        ///
        /// **Not a failure, and it must not be drawn as one.** The data on screen is as good as it
        /// was a moment ago and is still dated honestly by ``freshness(at:)``; nothing has gone
        /// wrong. It is a separate case from ``stopped(reason:message:)`` for exactly that reason.
        case suspended

        /// Core said retrying cannot help, or the bridge faulted. The timer is down and stays
        /// down until the user changes something and hits refresh.
        ///
        /// `reason` is `nil` for the one stop core does not diagnose: a thrown
        /// ``PessimalFFI/FfiError`` `.Internal`, which is a bridge fault carrying a message but no
        /// ``PessimalFFI/PollFailureKindRecord``. Every other stop is core's own verdict.
        case stopped(reason: PollFailureKindRecord?, message: String)
    }

    /// Whether the app is configured at all, and why not.
    public private(set) var sessionState: SessionState = .unconfigured

    /// What the poll loop is doing.
    public private(set) var pollingState: PollingState = .idle

    /// The fleet as of the last fold, `nil` when there is no session.
    ///
    /// `nil` means exactly one thing — *no session* — so a view can tell "not set up" from "set
    /// up, nothing found" without consulting a second property. A session that has never polled
    /// has a non-`nil` empty view here, because core answers `view()` before any poll and a first
    /// launch has a screen to draw too.
    ///
    /// Rows arrive in core's order: hosts by id, alerts by phase then host then rule. Re-sorting
    /// them is how a list animates rows that did not change.
    public private(set) var fleet: FleetViewRecord?

    /// The configuration currently in force — the one the running session was built with or last
    /// accepted, never a draft the settings screen is still editing.
    public private(set) var config: FleetConfigRecord?

    /// What this session's cold start salvaged from the cache, or `nil` if there was no cache.
    ///
    /// Worth a banner only when a `discarded_*` flag is set: the app is running on an empty state
    /// and the user's history is gone, which the first successful poll will paper over within
    /// seconds. Core's restore never fails, so this report is the only place that loss is visible.
    public private(set) var restoreReport: RestoreReportRecord?

    /// Core's audit of the configuration in force: rules that are legal and almost certainly not
    /// what their author meant. Render with `tuningWarningMessage(warning:)`; the sentence is
    /// core's.
    public private(set) var configWarnings: [TuningWarningRecord] = []

    /// Phase crossings from the last poll that produced any, kept so a "what just changed" view
    /// has something to show after the notification has been dismissed.
    ///
    /// Not cleared by a poll that crosses nothing, and never touched by a skipped poll — a
    /// skipped poll folds nothing and reports no transitions, and treating its empty list as
    /// news would erase a notification the user has not seen yet.
    public private(set) var lastTransitions: [AlertTransitionRecord] = []

    /// When the next poll is due, `nil` when none is.
    ///
    /// A wall-clock deadline rather than a countdown, so a suspension can resume core's schedule
    /// instead of restarting it: closing the popover for four minutes of a five-minute interval
    /// should leave one minute to wait, not five.
    public private(set) var nextPollAt: Date?

    /// True while polling is deliberately paused.
    ///
    /// Exposed because the menu bar icon needs it. A suspended poller means the icon's severity is
    /// frozen at whatever it was when the popover closed, and an icon that has quietly stopped
    /// tracking the fleet while still looking authoritative is the one failure this app must not
    /// ship. Draw the pause.
    public private(set) var isSuspended = false

    /// The last time saving settings or the state cache failed, as a sentence.
    ///
    /// Surfaced rather than swallowed: a cache that cannot be written means every launch is a cold
    /// one, which is invisible until somebody wonders why the app has no history. It never
    /// interrupts polling — the live session is unaffected — so it is a footnote, not a banner.
    public private(set) var lastPersistenceError: String?

    /// Called on the main actor with each poll's phase crossings, for whoever posts notifications.
    ///
    /// A hook rather than a job this type does, because "should this raise a user notification"
    /// is a platform question (authorisation, Do Not Disturb, notification grouping) and this
    /// type is deliberately ignorant of AppKit beyond the wake notification below.
    @ObservationIgnored public var onTransitions: (([AlertTransitionRecord]) -> Void)?

    // MARK: - Collaborators

    @ObservationIgnored private let settings: any FleetSettingsStore
    @ObservationIgnored private let stateCache: any FleetStateCache
    @ObservationIgnored private let jitterFraction: Double
    @ObservationIgnored private let clock: () -> Date

    /// Read afresh at every session rebuild, never cached. Toggling the switch in Settings rebuilds
    /// the session, and that is what turns reporting on or off — there is no live sink to reconfigure,
    /// because a sink only exists when consent allowed it to.
    @ObservationIgnored private let usageConsent: any UsageConsentStore

    @ObservationIgnored private var session: FleetSession?

    /// The connection the live session was built from. Held so a config change can rebuild the
    /// session without re-reading the store, and so ``reloadSettings()`` can tell a key change
    /// (keep the cache) from a URL change (drop it).
    @ObservationIgnored private var connection: FleetConnection?

    /// The poll in flight, if any. The core answers a second concurrent poll with `skipped`, which
    /// costs an HTTP round trip's worth of nothing; joining this task instead gives the caller the
    /// running poll's real answer.
    @ObservationIgnored private var pollTask: Task<Void, Never>?

    /// The armed timer. One at a time: arming cancels its predecessor, so a manual refresh and a
    /// timer tick cannot leave two clocks running.
    @ObservationIgnored private var timerTask: Task<Void, Never>?

    /// `nonisolated(unsafe)` so `deinit` — which is not main-actor isolated — can unregister it.
    /// Written only on the main actor during ``start()``, and read once when the last reference is
    /// already gone, so there is no concurrent access to be unsafe about.
    #if canImport(AppKit)
        @ObservationIgnored private nonisolated(unsafe) var wakeObserver: (any NSObjectProtocol)?

        /// Captured on the main actor at construction so `deinit`, which is not main-actor
        /// isolated, never has to touch `NSWorkspace.shared` from whichever thread released the
        /// last reference.
        @ObservationIgnored private nonisolated let workspaceNotifications: NotificationCenter
    #endif

    @ObservationIgnored private let observesSystemWake: Bool

    /// The default jitter spread: up to 10% longer than core asked for, never shorter.
    public nonisolated static let defaultJitterFraction = 0.1

    /// - Parameters:
    ///   - settings: where the saved connection and configuration live.
    ///   - stateCache: where the previous session's exported state lives.
    ///   - jitterFraction: how much slack to add to core's intervals, as a fraction of each.
    ///     Additive only — see ``PollSchedule``.
    ///   - observesSystemWake: when true (the default) the model resumes its own schedule after
    ///     the machine wakes. Set false if the app layer wires wake handling itself; both calling
    ///     ``resume()`` is harmless, but two owners of one behaviour is not.
    ///   - clock: injected so tests can pin `now`. Everything time-shaped in this type reads it.
    public init(
        settings: any FleetSettingsStore,
        stateCache: any FleetStateCache,
        // Defaults to opted *out*, which is the opposite of the app's default and deliberately so.
        // A call site that forgets to pass this reports nothing; the failure mode of the alternative
        // is reporting from a preview, a test, or a host that never consented. Fail closed.
        usageConsent: any UsageConsentStore = InMemoryUsageConsentStore(optedOut: true),
        jitterFraction: Double = FleetModel.defaultJitterFraction,
        observesSystemWake: Bool = true,
        clock: @escaping () -> Date = { Date() }
    ) {
        self.settings = settings
        self.stateCache = stateCache
        self.usageConsent = usageConsent
        self.jitterFraction = jitterFraction
        self.observesSystemWake = observesSystemWake
        self.clock = clock
        #if canImport(AppKit)
            workspaceNotifications = NSWorkspace.shared.notificationCenter
        #endif
    }

    deinit {
        #if canImport(AppKit)
            if let wakeObserver {
                workspaceNotifications.removeObserver(wakeObserver)
            }
        #endif
    }

    // MARK: - Lifecycle

    /// Builds the session from stored settings and starts polling. Idempotent.
    ///
    /// Call once, when the app launches. There is no first sleep: a session that has not folded
    /// this session advises `poll(after: 0)`, so waiting an interval before the first request
    /// would be substituting our own cadence for the one core chose precisely to avoid it.
    public func start() {
        if observesSystemWake { observeSystemWake() }
        reloadSettings()
    }

    /// Re-reads the settings store and rebuilds the session if the connection or the environment
    /// changed. Call after the settings screen saves.
    ///
    /// The store is the single source of truth for the connection, so this is the only way a new
    /// URL or key reaches the model — there is no setter. That keeps "what is configured" in one
    /// place instead of two that can disagree.
    public func reloadSettings() {
        guard let connection = settings.connection, let config = settings.config else {
            // First launch, or settings cleared. Not an error, and not an empty fleet.
            teardown()
            sessionState = .unconfigured
            return
        }

        // Nothing changed; keep polling on the schedule already armed. Rebuilding here would
        // discard a perfectly good session — and its in-flight poll — every time the settings
        // screen closed.
        if session != nil, connection == self.connection, config == self.config { return }

        // A cache describes the fleet at the URL it was collected from. Carry it across a key
        // rotation, drop it when the URL moves: showing yesterday's hosts under a new backend
        // would be inventing a fleet, and the first poll cannot un-show them.
        let urlUnchanged = self.connection?.baseURL == connection.baseURL
        let cached: String? = urlUnchanged ? (exportedState() ?? stateCache.loadState()) : nil

        do {
            let rebuilt = try FleetSession(
                baseUrl: connection.baseURL,
                apiKey: connection.apiKey,
                config: config,
                cachedStateJson: cached,
                usage: usageReportingRecord()
            )
            adopt(rebuilt, connection: connection, config: config, warnings: try fleetConfigAudit(config: config))
            if !urlUnchanged {
                // Only now that core has accepted the new URL. Clearing before the constructor ran
                // would mean a typo in the URL field destroyed the cache, and correcting the typo
                // would not bring it back.
                record(persistence: { try self.stateCache.clearState() })
            }
            pollSoon()
        } catch {
            // The stored settings are what this app is configured to use. Carrying on against the
            // *previous* URL would poll a backend the user has just walked away from and label the
            // result as theirs, so the session goes down and the screen says why.
            teardown()
            sessionState = .unusable(message: Self.message(for: error))
        }
    }

    /// Stops the clock and drops the session. Called for you when settings disappear; call it
    /// directly when the app is shutting down and you have already called ``persistState()``.
    public func teardown() {
        timerTask?.cancel()
        timerTask = nil
        // The in-flight poll is about a backend we are walking away from. Its answer must not
        // land on the screen after this returns; `execute(poll:)` checks identity as well, so
        // this is the belt to that brace.
        pollTask?.cancel()
        pollTask = nil
        session = nil
        connection = nil
        fleet = nil
        restoreReport = nil
        configWarnings = []
        nextPollAt = nil
        pollingState = .idle
    }

    // MARK: - Polling

    /// Polls now, or joins the poll already running. For a Refresh menu item.
    ///
    /// Works from ``PollingState/stopped(reason:message:)`` too: a stop means core sees no point
    /// in *automatic* retries, not that the user may not ask. Someone who has just fixed an API
    /// key deserves to find out without relaunching, and the poll's own advice decides whether the
    /// timer comes back.
    public func refresh() async {
        await poll(resumingAfterStop: true)
    }

    /// Pauses polling without losing the schedule. For a popover that has closed.
    ///
    /// The armed timer is cancelled but ``nextPollAt`` is kept, so ``resume()`` waits out the
    /// remainder of core's interval rather than starting a fresh one. A poll already in flight is
    /// left alone — cancelling it would throw away an HTTP round trip that is nearly paid for.
    ///
    /// Nothing here is a failure and nothing here is drawn as one.
    public func suspend() {
        guard !isSuspended else { return }
        isSuspended = true
        timerTask?.cancel()
        timerTask = nil
        if case .polling = pollingState { return } // The poll in flight settles the state itself.
        if case .stopped = pollingState { return } // A stop outranks a pause; do not hide it.
        pollingState = .suspended
    }

    /// Resumes core's schedule. For a popover that has opened, and for waking from sleep.
    ///
    /// Picks up where the schedule left off: if the deadline has passed — a lid closed for eight
    /// hours — the poll happens immediately, and if it has not, the remainder is waited out. Both
    /// are core's interval, neither is a new one.
    public func resume() {
        guard isSuspended else { return }
        isSuspended = false
        guard session != nil else { return }
        if case .stopped = pollingState { return } // Retrying is the user's call, not the lid's.
        // A poll in flight will arm the timer from its own advice when it lands; re-arming here
        // would report `.scheduled` over a poll that is still running.
        if case .polling = pollingState { return }
        arm()
    }

    /// Recomputes how much the picture on screen deserves to be believed, **as of `now`**.
    ///
    /// A function, never a stored property, and core says why: a verdict computed during a fold is
    /// correct only at the instant of the fold. A hung request folds nothing and a timer that
    /// stopped reports nothing, so a stored `Fresh` stays `Fresh` while the data underneath it
    /// rots. Call this from a `TimelineView` tick and render what comes back.
    ///
    /// - Returns: `nil` when there is no session — the same single meaning ``fleet`` carries, so
    ///   the two can never disagree about whether the app is set up.
    public func freshness(at now: Date) -> FreshnessRecord? {
        guard let session else { return nil }

        // Not decoration. Reading an observed property here registers this call as a dependency of
        // the enclosing view, so a fold that changes the inputs redraws the banner even when the
        // caller's own `now` has not ticked yet.
        _ = fleet

        do {
            return try session.freshness(nowMillis: Self.millis(now))
        } catch {
            // Core throws here only for a `now` no instant can represent, which `Date` cannot
            // produce. Reaching this line means the bridge is wrong about something, and a
            // freshness verdict computed from a fictional instant — or a `nil` that a view would
            // read as "not configured" — is precisely the silent wrong answer this whole call
            // exists to prevent.
            preconditionFailure("FleetSession.freshness rejected a Date: \(Self.message(for: error))")
        }
    }

    // MARK: - Configuration

    /// Applies a configuration and returns core's warnings about it.
    ///
    /// Two paths, and the split is not an optimisation:
    ///
    /// - Ordinarily, `setConfig` — core validates, stores, and audits in one call while holding
    ///   the poll lock, so a config change cannot be clobbered by a fold landing on top of it.
    /// - When the **liveness policy** changes, a whole new session. The SigNoz query's step was
    ///   baked from the constructor's policy by `SignozConfig::for_policy` and cannot be rebuilt
    ///   through `setConfig`; a session whose step and yardstick disagree reports a healthy fleet
    ///   as stale, with no error anywhere to say so. Core's documentation states the limitation
    ///   and this is where it is honoured.
    ///
    /// The new config reaches disk only after core has accepted it, and the running session is
    /// replaced only after the new one has been built, so a rejected config leaves both the screen
    /// and the poller exactly as they were.
    ///
    /// - Throws: ``PessimalFFI/FfiError`` `.InvalidConfig`, `.InvalidTuning` or `.InvalidRule`
    ///   with core's message, or ``FleetModelError/notConfigured`` when there is no connection to
    ///   validate a config against.
    @discardableResult
    public func applyConfig(_ config: FleetConfigRecord) async throws -> [TuningWarningRecord] {
        guard let connection else { throw FleetModelError.notConfigured }

        let warnings: [TuningWarningRecord]
        if let session, config.tuning.liveness == self.config?.tuning.liveness {
            warnings = try await session.setConfig(config: config)
            self.config = config
            configWarnings = warnings
        } else {
            let rebuilt = try FleetSession(
                baseUrl: connection.baseURL,
                apiKey: connection.apiKey,
                config: config,
                cachedStateJson: exportedState() ?? stateCache.loadState(),
                usage: usageReportingRecord()
            )
            // `setConfig` returns the audit; a constructor cannot, so ask for it by name. Same
            // function core calls, so the settings screen sees the same warnings either way.
            warnings = try fleetConfigAudit(config: config)
            adopt(rebuilt, connection: connection, config: config, warnings: warnings)
        }

        record(persistence: { try self.settings.saveConfig(config) })
        persistState()

        // The plan changed: which metrics are fetched, which rules are evaluated. Poll rather than
        // wait out an interval computed for the old one — and do it without blocking the Save
        // button on an HTTP round trip.
        pollSoon()
        return warnings
    }

    /// Drops a host and everything remembered about it — an operator decommissioning a machine.
    ///
    /// The view comes back from core rather than being patched locally, so the row disappears from
    /// the same projection the next poll will agree with.
    public func forgetHost(_ hostId: String) async throws {
        guard let session else { throw FleetModelError.notConfigured }
        fleet = try await session.forgetHost(hostId: hostId)
        persistState()
    }

    // MARK: - Probing

    /// Tests the live session's backend the way a poll uses it.
    ///
    /// Safe to call while a poll is running — core deliberately does not take the poll lock here,
    /// because a Test Connection button whose whole purpose is to be pressable when things are
    /// going wrong must not answer with a silent skip.
    public func probe() async throws -> BackendProbeRecord {
        guard let session else { throw FleetModelError.notConfigured }
        return try await session.probe(nowMillis: Self.millis(clock()))
    }

    /// Tests credentials the user has typed but not saved.
    ///
    /// The settings screen needs this: ``probe()`` asks the *live* session, which is still built
    /// from the old URL and key, so it can only ever confirm what already worked. This builds a
    /// throwaway session — no cache, nothing stored, nothing on the live session disturbed — and
    /// asks that.
    ///
    /// A URL with no scheme or a blank key makes the *constructor* throw rather than producing a
    /// probe record. That is core's judgement of the credentials and is shown as such; do not
    /// pre-screen the fields in Swift to avoid it.
    ///
    /// - Parameter config: the configuration to probe under. The screen has one in hand — either
    ///   the edited draft or `fleetConfigDefaults(environment:)` on a first run — and the
    ///   environment inside it decides which hosts the roster query looks for, so guessing one
    ///   here would test a different backend than the one about to be saved.
    public func probe(
        connection: FleetConnection,
        config: FleetConfigRecord
    ) async throws -> BackendProbeRecord {
        let candidate = try FleetSession(
            baseUrl: connection.baseURL,
            apiKey: connection.apiKey,
            config: config,
            cachedStateJson: nil,
            // A probe's session is a throwaway built to answer one question and then dropped. Giving
            // it a reporter would buffer a span into an object about to be discarded, and a probe
            // against a URL the user is still typing is not a fact worth reporting.
            usage: nil
        )
        return try await candidate.probe(nowMillis: Self.millis(clock()))
    }

    // MARK: - Usage reporting

    /// Whether the user lets Pessimal report on itself.
    ///
    /// The setter writes the store **and rebuilds the session**, which is the whole mechanism rather
    /// than a convenience: consent is read when a reporter is constructed, so there is no live sink to
    /// reconfigure. Turning it off drops the object that could report; turning it on builds one.
    ///
    /// Exposed here rather than on ``FleetStoreBridge`` so that the write and the rebuild cannot be
    /// separated by a caller who forgets the second half.
    public var usageReportingEnabled: Bool {
        get { !usageConsent.optedOut }
        set {
            // Correct as `newValue == usageConsent.optedOut`, and unreadable that way. Spelled against
            // the getter so the condition says what it means: do nothing if this is already the case.
            guard newValue != usageReportingEnabled else { return }
            usageConsent.optedOut = !newValue
            rebuildSessionForConsentChange()
        }
    }

    /// Whether this build has a destination at all, consent aside.
    ///
    /// Shown in Settings so someone on a local build is told why the switch appears inert, rather than
    /// concluding the feature is broken.
    public nonisolated var usageReportingConfigured: Bool {
        UsageDestination.fromBundle().isConfigured
    }

    /// Rebuilds the live session so a consent change takes effect, and does nothing if there is none.
    ///
    /// Goes through ``reloadSettings()`` rather than constructing a session here: that function already
    /// knows how to carry the cache across a rebuild, when to drop it, and how to report a failure, and
    /// a second copy of that reasoning is the thing to avoid. Its early-out compares the connection and
    /// the config, neither of which has changed — so the session is dropped first, which is what makes
    /// the rebuild happen.
    private func rebuildSessionForConsentChange() {
        guard session != nil else { return }
        session = nil
        reloadSettings()
    }

    /// The record a session is built with: this build's destination plus the user's current switch.
    ///
    /// Read at rebuild time rather than stored, so the switch takes effect the moment the session is
    /// rebuilt. Nothing here can fail: a build with no destination produces a record with empty
    /// strings, which the core reads as "nowhere to report".
    private func usageReportingRecord() -> UsageReportingRecord {
        UsageReporting.record(optedOut: usageConsent.optedOut)
    }

    /// Sends anything buffered, now. Call when the app is about to stop running.
    ///
    /// Awaiting is right *here* and nowhere else: the alternative at this moment is losing the batch
    /// when the process is suspended. Everywhere else on the poll path, reporting is fire-and-forget.
    ///
    /// Returns normally whatever happened. A diagnostics batch that could not be sent is not
    /// something the app, or the user, can act on.
    public func flushUsageReporting() async {
        await session?.flushUsage()
    }

    /// What usage reporting has managed to do, or why it is not doing it.
    ///
    /// `nil` before a session exists — a first launch with no backend configured has no reporter
    /// either, and the settings screen shows the setup prompt rather than diagnostics.
    public func usageDiagnostics() -> UsageDiagnosticsRecord? {
        session?.usageDiagnostics()
    }

    // MARK: - Persistence

    /// Writes the session's state to the cache. Called after every fold; call it again on quit.
    ///
    /// Cheap and non-blocking on the core side: `exportState` takes no poll lock, so an app being
    /// backgrounded never has to wait out a hung request to save.
    public func persistState() {
        guard let json = exportedState() else { return }
        record(persistence: { try self.stateCache.saveState(json) })
    }

    // MARK: - Display helpers

    /// The core's own sentence for an error, without the type name Swift would otherwise print.
    ///
    /// `FfiError.errorDescription` is `String(reflecting:)`, which renders
    /// `PessimalFFI.FfiError.Unauthorized(message: "…")` — accurate, and not something to show an
    /// operator. Every variant carries a `message` that core wrote for exactly this purpose, and
    /// unwrapping it is formatting, not judgement: the routing decision stays on the variant.
    public static func message(for error: any Error) -> String {
        switch error {
        case let ffi as FfiError:
            switch ffi {
            case let .InvalidTuning(message), let .InvalidConfig(message), let .InvalidRule(message),
                 let .InvalidUrn(message), let .InvalidTimeRange(message), let .UnknownMetric(message),
                 let .RuleNotFound(message), let .UnknownHost(message), let .Backend(message),
                 let .Unauthorized(message), let .Unreachable(message), let .NotInitialized(message),
                 let .Internal(message):
                return message
            }
        case let model as FleetModelError:
            return model.message
        case let localized as any LocalizedError:
            return localized.errorDescription ?? String(describing: error)
        default:
            return String(describing: error)
        }
    }

    /// Epoch milliseconds, the unit every `FleetSession` call takes.
    static func millis(_ date: Date) -> Int64 {
        Int64((date.timeIntervalSince1970 * 1000).rounded())
    }

    // MARK: - The poll loop

    /// One whole poll, or a join onto the one already running.
    ///
    /// The in-flight check and the assignment that satisfies it both happen before the first
    /// `await`. `@MainActor` is *reentrant*: it is released across `session.poll`, so a timer tick
    /// landing mid-poll would sail past a guard set any later and start a second one. Core would
    /// answer that second call with `skipped`, which is correct but wasteful; joining the running
    /// task gives the caller the real answer instead of a shrug.
    private func poll(resumingAfterStop: Bool = false) async {
        if let running = pollTask {
            await running.value
            return
        }
        guard let session else { return }
        if case .stopped = pollingState, !resumingAfterStop { return }

        let task: Task<Void, Never> = Task { @MainActor [weak self] in
            guard let self else { return }
            await self.execute(poll: session)
        }
        pollTask = task
        await task.value
        // Only the creator clears it, and only while it is still its own: an older poll finishing
        // after the session was replaced would otherwise nil out the *current* task and let the
        // next tick start a duplicate.
        if pollTask == task { pollTask = nil }
    }

    private func execute(poll session: FleetSession) async {
        pollingState = .polling

        let outcome: Result<PollResult, any Error>
        do {
            outcome = .success(try await session.poll(nowMillis: Self.millis(clock())))
        } catch {
            outcome = .failure(error)
        }

        // The session can be replaced across that await — a new URL saved, a liveness policy
        // changed — and this poll's answer is then about a backend the app has left. Writing it
        // would put the old fleet's hosts on screen under the new connection, and nothing would
        // say so until the next interval. Identity, not equality: `FleetSession` is the handle.
        guard self.session === session else { return }

        do {
            let result = try outcome.get()

            if result.skipped {
                // Another poll folded while this one waited. Its view is the current view and its
                // transitions are empty by construction — assigning either would either redraw
                // rows for nothing or wipe a phase crossing nobody has seen yet. The advice still
                // counts: core hands it back on a skip precisely because the timer has to be
                // rescheduled from something honest.
                apply(advice: result.advice)
                return
            }

            fleet = result.view
            configWarnings = result.view.warnings
            if !result.transitions.isEmpty {
                lastTransitions = result.transitions
                onTransitions?(result.transitions)
            }
            persistState()
            apply(advice: result.advice)
        } catch is CancellationError {
            // A cancelled poll is a teardown or a suspension — never a failure, and never a
            // banner. Leave the last good view alone and re-derive the state from the schedule
            // that was already standing; `arm` is what knows whether that means suspended,
            // scheduled, or nothing at all. Whoever cancelled has usually set the state already,
            // in which case this is a no-op.
            if case .polling = pollingState { arm() }
        } catch {
            // Core throws from `poll` only for a bridge fault: every backend failure — a 401, a
            // dead network, a metric that does not exist — arrives as data inside the view, so
            // that a poll which failed still leaves the last good view on screen. This path has no
            // advice attached, and inventing a retry interval for it is exactly the thing this
            // model does not do. Stop, say why, and let a manual refresh be the retry.
            timerTask?.cancel()
            timerTask = nil
            nextPollAt = nil
            pollingState = .stopped(reason: nil, message: Self.message(for: error))
        }
    }

    /// Obeys core's advice: schedule, or stop.
    private func apply(advice: PollAdviceRecord) {
        switch PollSchedule.from(advice, jitterFraction: jitterFraction) {
        case let .wait(seconds):
            nextPollAt = clock().addingTimeInterval(seconds)
            // Core has advised a next poll, which retires any earlier stop. A manual refresh that
            // succeeded is exactly how a stopped poller is meant to come back to life, and the
            // advice in front of us is core's word that it should.
            if case .stopped = pollingState { pollingState = .idle }
            arm()
        case let .halt(reason, message):
            timerTask?.cancel()
            timerTask = nil
            nextPollAt = nil
            pollingState = .stopped(reason: reason, message: message)
        }
    }

    /// Arms one timer for ``nextPollAt``, cancelling any predecessor.
    ///
    /// The wait is recomputed from the wall-clock deadline every time, which is what makes a
    /// resume pick up core's schedule instead of restarting it, and what makes a deadline that
    /// passed while the machine slept fire at once rather than a full interval late.
    private func arm() {
        // A stop is core's verdict that retrying cannot help. Only a poll whose advice says
        // otherwise retires it — never a lid opening, never a config save.
        if case .stopped = pollingState { return }

        timerTask?.cancel()
        timerTask = nil

        guard session != nil else {
            pollingState = .idle
            return
        }
        guard !isSuspended else {
            pollingState = .suspended
            return
        }
        guard let nextPollAt else {
            pollingState = .idle
            return
        }

        let wait = max(0, nextPollAt.timeIntervalSince(clock()))
        pollingState = .scheduled
        timerTask = Task { @MainActor [weak self] in
            try? await Task.sleep(for: .seconds(wait))
            guard !Task.isCancelled else { return }
            await self?.poll()
        }
    }

    /// Polls at the next opportunity without making the caller wait for the round trip.
    ///
    /// Used where core's own answer would be `poll(after: 0)` anyway — a session just built, a
    /// config just changed — so the zero is core's, not ours.
    private func pollSoon() {
        if case .stopped = pollingState { return }
        nextPollAt = clock()
        arm()
    }

    // MARK: - Session plumbing

    private func adopt(
        _ session: FleetSession,
        connection: FleetConnection,
        config: FleetConfigRecord,
        warnings: [TuningWarningRecord]
    ) {
        timerTask?.cancel()
        timerTask = nil

        // Letting the outgoing session's poll stay registered would wedge the loop, not just
        // waste a request: the next tick would *join* it, and a poll about a session we have
        // replaced returns without advice, so nothing would rearm the timer. `execute(poll:)`
        // refuses its writes on identity; this is what keeps the clock running.
        pollTask?.cancel()
        pollTask = nil

        self.session = session
        self.connection = connection
        self.config = config
        configWarnings = warnings
        // Safe before any poll, and the reason a first launch has something to draw: core answers
        // with an empty fleet rather than refusing.
        fleet = session.view()
        restoreReport = session.restoreReport()
        sessionState = .ready
        pollingState = .idle
        lastTransitions = []
    }

    private func exportedState() -> String? {
        guard let session else { return nil }
        do {
            return try session.exportState()
        } catch {
            lastPersistenceError = Self.message(for: error)
            return nil
        }
    }

    private func record(persistence work: () throws -> Void) {
        do {
            try work()
            lastPersistenceError = nil
        } catch {
            // Never fatal. A cache or a preference that will not write costs a colder start, and
            // stopping the poller over it would trade a footnote for an outage.
            lastPersistenceError = Self.message(for: error)
        }
    }

    // MARK: - Waking

    /// Resumes after the machine wakes.
    ///
    /// Registered here rather than left to the app layer because the guarantee is worth owning: a
    /// laptop that slept through the night must not open onto a poller that quietly stopped, and
    /// ``resume()`` is idempotent, so a second owner wiring the same notification costs nothing.
    /// Pass `observesSystemWake: false` if the app layer would rather own it outright.
    private func observeSystemWake() {
        #if canImport(AppKit)
            guard wakeObserver == nil else { return }
            wakeObserver = workspaceNotifications.addObserver(
                forName: NSWorkspace.didWakeNotification,
                object: nil,
                queue: .main
            ) { [weak self] _ in
                MainActor.assumeIsolated {
                    guard let self else { return }
                    if self.isSuspended {
                        // Suspended on purpose — the popover is closed. Waking is not a reason to
                        // start polling behind a closed menu.
                        return
                    }
                    // Re-arm from the stored deadline. If it passed while the machine slept, the wait
                    // computes to zero and the poll happens now.
                    self.arm()
                }
            }
        #else
            // On iOS there is no wake notification to observe: the app is suspended rather than the
            // machine, and the app layer reports it through `resume()` on a scene-phase change.
            // Doing nothing here is correct, not a gap.
        #endif
    }
}

/// The failures this model raises on its own behalf. Everything else comes from core, verbatim.
public enum FleetModelError: Error, Equatable, LocalizedError {
    /// Asked to do something that needs a backend, before one has been configured.
    ///
    /// Raised rather than silently ignored, and in particular raised by
    /// ``FleetModel/applyConfig(_:)``: a configuration cannot be validated without a session, and
    /// writing one core has not accepted to disk would leave the app to fail at the next launch
    /// instead of at the Save button.
    case notConfigured

    var message: String {
        switch self {
        case .notConfigured:
            return "No backend is configured yet."
        }
    }

    public var errorDescription: String? { message }
}
