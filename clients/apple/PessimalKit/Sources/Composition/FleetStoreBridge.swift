//
//  FleetStoreBridge.swift
//  Pessimal — shared by the macOS and iOS clients
//
//  Both apps' composition root adapter: the Platform layer's three stores on one side, the Model
//  layer's two protocols on the other. It exists because neither layer is allowed to know the
//  other — `FleetModel` declares what it needs (`FleetSettingsStore`, `FleetStateCache`) and the
//  platform declares what it has (`APIKeyStore`, `SettingsStore`, `FleetStateStore`), and the app
//  is the only place entitled to say that those are the same four values.
//
//  Nothing here decides anything. It reads what is on disk, asks core for the defaults core owns,
//  and hands the result over; every rejection in this file is core's, quoted.
//

import Foundation
import Observation
#if canImport(PessimalFFI)
    // Built as its own module for iOS, compiled into the app's module on macOS. See the note in
    // `FleetModel.swift`.
    import PessimalFFI
#endif

/// Assembles a `FleetConnection` and a `FleetConfigRecord` from the platform's stores, and caches
/// the exported fleet state through them.
///
/// One object rather than three conformances spread over three types, because the API key must be
/// read once and held: two objects would mean two reads, two prompts, and two answers that can
/// disagree about whether the Keychain opened.
///
/// Lives here rather than in either app because nothing in it is platform-specific — it reads
/// `UserDefaults`, the Keychain and a file, and neither app wants a different answer from any of
/// the three. A second copy on the iOS side would be two adapters that must agree about the one
/// rule in this file that is genuinely dangerous to get wrong: never overwrite an API key that
/// could not be read.
///
/// Not `@MainActor`, because ``FleetSettingsStore``, ``FleetStateCache`` and
/// ``SettingsConnectionStore`` are not: an isolated witness cannot satisfy a non-isolated
/// requirement. In practice every call arrives on the main actor — `FleetModel` says so in its own
/// documentation, and the views that read the three `*Problem` properties are main-actor views.
@Observable
public final class FleetStoreBridge: SettingsConnectionStore, FleetStateCache {
    @ObservationIgnored private let stores: PlatformStores

    // MARK: - Problems worth showing

    /// Why the API key could not be read, as a sentence, or `nil` when there was nothing to say.
    ///
    /// Distinct from "no key is stored". A Keychain that would not open is a machine that has not
    /// finished logging in or a user who dismissed a prompt, and reporting either as *unconfigured*
    /// would invite someone to retype a key that is already there — which, per the platform
    /// layer's contract, is the one thing that must never follow a failed read.
    public private(set) var credentialProblem: String?

    /// Core's sentence for stored settings it refuses — an environment name with `::` in it, a
    /// poll interval that fails one of `pollTuningValidated`'s interlocks.
    ///
    /// Without this the app would show its first-launch screen to someone whose settings exist and
    /// are wrong, and the two look identical while meaning opposite things.
    public private(set) var settingsProblem: String?

    /// Why the cached fleet state could not be read or written.
    ///
    /// A footnote, never a banner: a cache that will not load costs a cold start, and one that will
    /// not save costs the next cold start. Neither is a reason to stop polling — but an app that
    /// has silently had no history for a month is worse than one that says so.
    public private(set) var stateCacheProblem: String?

    // MARK: - Held rather than re-read

    /// The API key, read once.
    ///
    /// Every `SecItemCopyMatching` can put a prompt in front of the user, and `connection` is read
    /// on every settings reload. Reading it once is the platform layer's instruction, not an
    /// optimisation.
    @ObservationIgnored private var apiKey: String?

    /// False while a read could still usefully be retried.
    ///
    /// Set for a key that was found, a key that is genuinely absent, and a read the user cancelled
    /// — retrying that last one would only put the prompt back. Deliberately *not* set when the
    /// Keychain was unavailable, which is the login-item-before-login-keychain case the platform
    /// layer describes: that one is expected to come good on its own.
    @ObservationIgnored private var apiKeyResolved = false

    /// The last configuration core accepted, used as the base for the next one.
    ///
    /// The settings store keeps four values; a `FleetConfigRecord` has six fields. Without this,
    /// the first `reloadSettings()` after a config change would rebuild the record from core's
    /// defaults and silently revert whatever the settings screen had chosen for
    /// `overviewMetrics`, `detailMetrics` or `focus`. Within one run they survive here. Across a
    /// relaunch they do not — see ``saveConfig(_:)``.
    @ObservationIgnored private var acceptedConfig: FleetConfigRecord?

    public init(stores: PlatformStores) {
        self.stores = stores
    }

    // MARK: - FleetSettingsStore

    /// The saved URL and key, or `nil` when either is missing.
    ///
    /// `nil` from a *failed* key read is not the same fact as `nil` from an empty Keychain, and the
    /// difference is carried on ``credentialProblem`` rather than folded in here: this protocol has
    /// no error channel, and inventing one by returning a blank key would send core a credential
    /// the user never typed.
    public var connection: FleetConnection? {
        loadAPIKeyIfNeeded()
        guard let baseURL = stores.settings.backendBaseURL, let apiKey else { return nil }
        return FleetConnection(baseURL: baseURL, apiKey: apiKey)
    }

    /// The configuration in force, rebuilt from core's defaults plus the values this app stores.
    ///
    /// `nil` when no environment has ever been saved — first launch, which the menu renders as an
    /// invitation rather than as an empty fleet. `nil` *also* when core refuses what is stored, and
    /// ``settingsProblem`` is what tells those two apart.
    public var config: FleetConfigRecord? {
        guard let environment = stores.settings.environment else {
            setSettingsProblem(nil)
            return nil
        }

        do {
            // `fleetConfigDefaults` is the only source of a default config: it is core's own
            // answer, and a record assembled here would be a second one to keep in step.
            let base: FleetConfigRecord
            if let acceptedConfig, acceptedConfig.environment == environment {
                base = acceptedConfig
            } else {
                base = try fleetConfigDefaults(environment: environment)
            }

            let config = FleetConfigRecord(
                environment: base.environment,
                tuning: try tuning(basedOn: base.tuning),
                // Untouched and unparsed. Core wrote this string and core reads it back; a stored
                // rule set core cannot parse is refused by `FleetSession.init`, with core's
                // message, which is a better error than anything a check here could produce.
                rulesJson: stores.settings.alertRulesJSON ?? base.rulesJson,
                overviewMetrics: base.overviewMetrics,
                detailMetrics: base.detailMetrics,
                focus: base.focus
            )
            setSettingsProblem(nil)
            return config
        } catch {
            setSettingsProblem(message(for: error))
            return nil
        }
    }

    /// Persists the three fields this app's settings store keeps, from a config core has accepted.
    ///
    /// `overviewMetrics`, `detailMetrics` and `focus` are **not** written: `SettingsStore` has no
    /// place for them. They survive the run in ``acceptedConfig`` and fall back to core's defaults
    /// on the next launch. That is a real limitation of the four-value store rather than a
    /// decision made here, and it is written down so that nobody discovers it as a bug.
    public func saveConfig(_ config: FleetConfigRecord) throws {
        stores.settings.environment = config.environment
        stores.settings.pollIntervalSeconds = config.tuning.pollIntervalSeconds
        stores.settings.alertRulesJSON = config.rulesJson
        acceptedConfig = config
    }

    // MARK: - FleetStateCache

    public func loadState() -> String? {
        switch stores.fleetState.load() {
        case .absent:
            setStateCacheProblem(nil)
            return nil
        case let .restored(json):
            setStateCacheProblem(nil)
            return json
        case let .unreadable(reason):
            // A first launch and a corrupt cache both start from nothing, and only one of them is
            // a defect. Core's `restoreReport` cannot see this one — the string never reached it.
            setStateCacheProblem(reason)
            return nil
        }
    }

    public func saveState(_ json: String) throws {
        try stores.fleetState.save(json)
        setStateCacheProblem(nil)
    }

    public func clearState() throws {
        try stores.fleetState.clear()
        setStateCacheProblem(nil)
    }

    // MARK: - Credentials

    /// Asks the Keychain again, when the last attempt did not settle the question.
    ///
    /// Only ever reached by an explicit gesture — Refresh Now, or the settings window's own retry
    /// button — which is what makes re-prompting the right thing to do rather than a nuisance: a
    /// person pressing it is a person saying they will answer the panel this time. A read that
    /// already succeeded, or that found nothing to read, is settled and costs nothing here.
    public func retryUnresolvedCredentials() {
        guard credentialProblem != nil else { return }
        apiKeyResolved = false
        loadAPIKeyIfNeeded()
    }

    /// Writes the backend address and the API key.
    ///
    /// The key goes to the Keychain and the URL to `UserDefaults`, in that order: a URL saved
    /// beside a key that would not store is a configuration that points somewhere with no way in,
    /// and the next launch would report it as a credential failure rather than as a save that did
    /// not happen.
    ///
    /// Refuses outright while the stored key could not be *read*. The platform layer's contract
    /// names overwriting an unreadable key as the one unrecoverable mistake in this file, and
    /// while ``SettingsView`` checks the same thing before calling, an enablement check evaluated
    /// one render ago is not a guarantee. The read is retried first, so a Keychain that has since
    /// unlocked settles the question instead of blocking a legitimate save.
    public func save(connection: FleetConnection) throws {
        retryUnresolvedCredentials()
        if let credentialProblem {
            throw FleetStoreBridgeError.credentialUnreadable(credentialProblem)
        }

        try stores.apiKey.save(connection.apiKey)
        stores.settings.backendBaseURL = connection.baseURL

        // The held copy is now known-good without another prompt.
        apiKey = connection.apiKey
        apiKeyResolved = true
        setCredentialProblem(nil)
    }

    private func loadAPIKeyIfNeeded() {
        guard !apiKeyResolved else { return }

        do {
            // `nil` here is the honest "nothing is stored": the read worked and found no key.
            apiKey = try stores.apiKey.load()
            apiKeyResolved = true
            setCredentialProblem(nil)
        } catch let error as KeychainError where error.isKeychainUnavailable {
            // Left unresolved on purpose: this is the one failure that repairs itself, when the
            // login keychain a login item started ahead of finally unlocks.
            apiKey = nil
            setCredentialProblem(error.description)
        } catch let error as KeychainError where error.isUserCancelled {
            // Marked resolved so nothing re-raises the panel on its own — but the *problem stays
            // set*, and that is the load-bearing half. A throw is not "not configured": a key the
            // user declined to reveal is a key that is still there, and clearing the problem here
            // would leave the app showing its first-run screen, invite a fresh key, and let
            // `save(connection:)` overwrite the one it was never allowed to read. That is the one
            // unrecoverable mistake this file can make.
            apiKey = nil
            apiKeyResolved = true
            setCredentialProblem("Reading the saved API key was cancelled.")
        } catch {
            apiKey = nil
            apiKeyResolved = true
            setCredentialProblem(message(for: error))
        }
    }

    // MARK: - Reporting

    // The three `*Problem` properties are written from *getters*, and getters are called while
    // SwiftUI is evaluating a view body — `SettingsView` seeds its draft from `connection` as the
    // settings scene is built. `@Observable` fires its mutation callback on every assignment,
    // equal value or not, so an unconditional write there is a state change during a view update.
    // Comparing first makes the steady state free and the noisy state quiet.

    private func setCredentialProblem(_ newValue: String?) {
        guard credentialProblem != newValue else { return }
        credentialProblem = newValue
    }

    private func setSettingsProblem(_ newValue: String?) {
        guard settingsProblem != newValue else { return }
        settingsProblem = newValue
    }

    private func setStateCacheProblem(_ newValue: String?) {
        guard stateCacheProblem != newValue else { return }
        stateCacheProblem = newValue
    }

    // MARK: - Errors

    /// Core's own sentence for an error.
    ///
    /// `FleetModel.message(for:)` is `@MainActor` because its enclosing type is, and this adapter
    /// cannot be: ``FleetSettingsStore`` is not an isolated protocol, and an isolated witness
    /// cannot satisfy a non-isolated requirement. Every call into this type does arrive on the
    /// main actor — `FleetModel` documents that it only ever touches its stores there, and the
    /// settings window is a view — so the isolation is *asserted* rather than worked around. The
    /// alternative, a second copy of that switch over `FfiError`, is how two places come to
    /// disagree about what core said.
    private func message(for error: any Error) -> String {
        MainActor.assumeIsolated { FleetModel.message(for: error) }
    }

    // MARK: - Tuning

    /// Substitutes the one tuning field this app stores, then hands the whole record to core.
    ///
    /// The eight fields interlock — the metric step, the staleness budget and the liveness
    /// thresholds are all related to the interval — so a stored interval is not a tuning until
    /// `pollTuningValidated` has said so. Rebuilding the whole record is not a style choice
    /// either: `PollTuningRecord`'s fields are `let`, and there is no partial update.
    private func tuning(basedOn base: PollTuningRecord) throws -> PollTuningRecord {
        guard let interval = stores.settings.pollIntervalSeconds,
              interval != base.pollIntervalSeconds
        else {
            return base
        }

        return try pollTuningValidated(
            tuning: PollTuningRecord(
                liveness: base.liveness,
                pollIntervalSeconds: interval,
                metricStepSeconds: base.metricStepSeconds,
                chartWindowSeconds: base.chartWindowSeconds,
                maxStalenessSeconds: base.maxStalenessSeconds,
                backendLagAllowanceSeconds: base.backendLagAllowanceSeconds,
                forgetHostAfterSeconds: base.forgetHostAfterSeconds,
                maxRetainedHosts: base.maxRetainedHosts
            )
        )
    }
}

/// The one failure this adapter raises on its own behalf. Everything else is core's or the
/// platform's, passed through untouched.
public enum FleetStoreBridgeError: Error, Equatable, LocalizedError {
    /// Asked to save a key over one that could not be read.
    case credentialUnreadable(String)

    public var errorDescription: String? {
        switch self {
        case let .credentialUnreadable(reason):
            return "The saved API key could not be read, so it must not be overwritten: \(reason)"
        }
    }
}
