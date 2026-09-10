import Foundation

/// Everything the user configures that is not a secret.
///
/// Four values, and deliberately only four: the three that address a backend and the one opaque
/// string that carries the alert rules. Notice what is *not* here — no `AlertRule` type, no URL
/// validation, no interval bounds. The rule set round-trips through the core's
/// `alertRulesToJson`/`alertRulesFromJson`, so this layer stores a `String` it never parses; a
/// Swift model of a rule would be a second definition of a rule, and the two would disagree the
/// first time either side changed.
///
/// Every property is optional, and `nil` means *not configured*, not *empty*. The distinction
/// matters because the core supplies the defaults: `fleetConfigDefaults(environment:)` and
/// `pollTuningDefaults()` are what an unset value falls back to, and only the Model layer knows to
/// call them. A store that helpfully returned `"production"` for an unset environment would be this
/// layer picking a default the core already owns.
///
/// Class-bound so the settings can be mutated through a `let` reference held in ``PlatformStores``.
public protocol SettingsStore: AnyObject, Sendable {
    /// The telemetry backend's base URL, as the user typed it, whitespace trimmed.
    ///
    /// Not a `URL`. `URL(string:)` accepts almost anything and rejects a few things a person would
    /// call reasonable, so parsing here would either invent a validation rule or lose the user's
    /// text. The core is the judge: `FleetSession.init` throws `FfiError.Backend` for a base URL
    /// with no scheme, and that message is the one to show.
    var backendBaseURL: String? { get set }

    /// The environment name every rule URN embeds — `pessimal::$ENVIRONMENT::…`.
    ///
    /// The core requires it to be non-empty and free of `::`, and enforces that itself in
    /// `fleetConfigDefaults` and `FleetSession.init`. Stored verbatim after trimming.
    var environment: String? { get set }

    /// How often to poll, in seconds.
    ///
    /// One number out of the eight that make a `PollTuningRecord`, stored alone because it is the
    /// only one this app's settings screen offers. It is not a `PollTuningRecord` and must not be
    /// treated as one: the record's fields interlock (the metric step, the staleness budget and the
    /// liveness thresholds are all related to the interval), and the Model layer is expected to
    /// rebuild the whole record and hand it to `pollTuningValidated` before anything uses it.
    ///
    /// `nil` and `0` are different answers. `UserDefaults.integer(forKey:)` cannot tell them apart,
    /// which is why the implementation below does not use it.
    var pollIntervalSeconds: Int64? { get set }

    /// The alert rule set, as the JSON array the core produced.
    ///
    /// Opaque. Obtain it from `alertRulesToJson(rules:)` or from a `FleetConfigRecord` the core
    /// produced, and store it unchanged — not even whitespace is trimmed, because this is the
    /// core's string and the only safe thing to do with it is hand it back exactly.
    var alertRulesJSON: String? { get set }

    /// Forgets all four values.
    ///
    /// Scoped to this store's own keys rather than wiping the app's defaults domain, which would
    /// also discard window positions, the "launch at login" flag, and anything else the app comes
    /// to keep there.
    func removeAll()
}

/// Trimming applied to the two free-text fields, in one place.
///
/// Shared by the real store and the fake so that a test which passes against
/// ``InMemorySettingsStore`` describes what ``UserDefaultsSettingsStore`` actually does. Empty after
/// trimming counts as absent: a settings field the user cleared is a field that is not set, and
/// storing `""` would send an empty base URL to the core as though it were a choice.
private func normalizedSetting(_ value: String?) -> String? {
    guard let trimmed = value?.trimmingCharacters(in: .whitespacesAndNewlines), !trimmed.isEmpty else {
        return nil
    }
    return trimmed
}

/// The real store, over `UserDefaults`.
///
/// Right for these four values and wrong for a fifth one that is secret: `UserDefaults` is a plist
/// in the user's home that any process running as the user can read. The API key lives in
/// ``KeychainAPIKeyStore`` and never appears here.
public final class UserDefaultsSettingsStore: SettingsStore, @unchecked Sendable {
    // `@unchecked` because `UserDefaults` is documented as thread-safe but is not annotated
    // `Sendable` in every SDK this project may be built against. The class holds nothing else
    // mutable, so the guarantee is `UserDefaults`'s own.
    private let defaults: UserDefaults

    /// Keys are namespaced because `UserDefaults.standard` is a shared surface: AppKit,
    /// SwiftUI and the system all write into the same domain, and an unprefixed `"environment"`
    /// is exactly the sort of name that collides.
    private enum Key {
        static let backendBaseURL = "pessimal.backendBaseURL"
        static let environment = "pessimal.environment"
        static let pollIntervalSeconds = "pessimal.pollIntervalSeconds"
        static let alertRulesJSON = "pessimal.alertRulesJSON"

        static let all = [backendBaseURL, environment, pollIntervalSeconds, alertRulesJSON]
    }

    /// - Parameter defaults: `.standard` in the app. Tests pass `UserDefaults(suiteName:)` so a run
    ///   cannot disturb a real install's settings.
    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    public var backendBaseURL: String? {
        get { normalizedSetting(defaults.string(forKey: Key.backendBaseURL)) }
        set { write(normalizedSetting(newValue), forKey: Key.backendBaseURL) }
    }

    public var environment: String? {
        get { normalizedSetting(defaults.string(forKey: Key.environment)) }
        set { write(normalizedSetting(newValue), forKey: Key.environment) }
    }

    public var pollIntervalSeconds: Int64? {
        get {
            // `object(forKey:)`, not `integer(forKey:)`: the latter answers `0` for a key that was
            // never written, and `0` is a value a user could conceivably have stored. Reading it as
            // "unset" would silently replace their setting with the core's default; reading unset as
            // `0` would send an impossible interval to `pollTuningValidated`. Only the presence of
            // the object distinguishes them.
            //
            // A key holding something that is not a number — a hand-edited plist, an older schema —
            // reads as absent rather than crashing on a forced cast.
            guard let number = defaults.object(forKey: Key.pollIntervalSeconds) as? NSNumber else {
                return nil
            }
            return number.int64Value
        }
        set { write(newValue.map { NSNumber(value: $0) }, forKey: Key.pollIntervalSeconds) }
    }

    public var alertRulesJSON: String? {
        // Untrimmed and unparsed on the way in and on the way out. The core wrote this string; the
        // core reads it back. An empty string is not turned into `nil` here either — that would be
        // this layer deciding an empty rule set looks like no rule set, when the core spells the
        // empty rule set `"[]"`.
        get { defaults.string(forKey: Key.alertRulesJSON) }
        set { write(newValue, forKey: Key.alertRulesJSON) }
    }

    public func removeAll() {
        for key in Key.all { defaults.removeObject(forKey: key) }
    }

    /// Writes a value, or removes the key when there is no value.
    ///
    /// Removing rather than storing a null keeps one representation of "unset", so a value written
    /// and then cleared is indistinguishable from one never written.
    private func write(_ value: Any?, forKey key: String) {
        if let value {
            defaults.set(value, forKey: key)
        } else {
            defaults.removeObject(forKey: key)
        }
    }
}

/// A store that keeps the settings in memory, for tests and SwiftUI previews.
///
/// Applies the same trimming and the same absent-versus-empty rules as
/// ``UserDefaultsSettingsStore``, because a fake that is more permissive than the real thing turns
/// a test into a description of a codebase that does not exist.
public final class InMemorySettingsStore: SettingsStore, @unchecked Sendable {
    // `@unchecked`: the four values below are guarded by this lock.
    private let lock = NSLock()
    private var storedBackendBaseURL: String?
    private var storedEnvironment: String?
    private var storedPollIntervalSeconds: Int64?
    private var storedAlertRulesJSON: String?

    public init(
        backendBaseURL: String? = nil,
        environment: String? = nil,
        pollIntervalSeconds: Int64? = nil,
        alertRulesJSON: String? = nil
    ) {
        storedBackendBaseURL = normalizedSetting(backendBaseURL)
        storedEnvironment = normalizedSetting(environment)
        storedPollIntervalSeconds = pollIntervalSeconds
        storedAlertRulesJSON = alertRulesJSON
    }

    public var backendBaseURL: String? {
        get { lock.withLock { storedBackendBaseURL } }
        set { lock.withLock { storedBackendBaseURL = normalizedSetting(newValue) } }
    }

    public var environment: String? {
        get { lock.withLock { storedEnvironment } }
        set { lock.withLock { storedEnvironment = normalizedSetting(newValue) } }
    }

    public var pollIntervalSeconds: Int64? {
        get { lock.withLock { storedPollIntervalSeconds } }
        set { lock.withLock { storedPollIntervalSeconds = newValue } }
    }

    public var alertRulesJSON: String? {
        get { lock.withLock { storedAlertRulesJSON } }
        set { lock.withLock { storedAlertRulesJSON = newValue } }
    }

    public func removeAll() {
        lock.withLock {
            storedBackendBaseURL = nil
            storedEnvironment = nil
            storedPollIntervalSeconds = nil
            storedAlertRulesJSON = nil
        }
    }
}
