import Foundation

/// The places this app keeps things, handed to the Model layer as one value.
///
/// Exists so a view model takes a single parameter with a single test substitute rather than three
/// of each, and so there is one obvious place to see that the secret goes somewhere different from
/// everything else. It holds references and adds no behaviour: any logic that appears here —
/// deciding what to do when the environment changes, choosing defaults for unset settings — belongs
/// in the Model layer, which is the layer that can ask the core.
public struct PlatformStores: Sendable {
    /// The backend API key. Keychain, never `UserDefaults`, never a log.
    public let apiKey: any APIKeyStore
    /// Base URL, environment, poll interval, alert rules JSON.
    public let settings: any SettingsStore
    /// The cached fleet state a relaunch restores from.
    public let fleetState: any FleetStateStore
    /// Whether the user has opted out of usage reporting.
    ///
    /// Its own store, not a fifth property on ``SettingsStore``, because `SettingsStore.removeAll()`
    /// is what "reset connection" calls and an opt-out must not be collateral damage of clearing a
    /// mistyped URL. See ``UsageConsentStore``.
    public let usageConsent: any UsageConsentStore
    /// Where the settings document is shared with the user's other devices. `nil` shares nothing.
    public let settingsMailbox: (any SettingsMailbox)?
    /// This device's merged copy of the settings document. See ``SettingsReplicaStore``.
    public let settingsReplica: any SettingsReplicaStore

    public init(
        apiKey: any APIKeyStore,
        settings: any SettingsStore,
        fleetState: any FleetStateStore,
        usageConsent: any UsageConsentStore,
        settingsMailbox: (any SettingsMailbox)?,
        settingsReplica: any SettingsReplicaStore
    ) {
        self.apiKey = apiKey
        self.settings = settings
        self.fleetState = fleetState
        self.usageConsent = usageConsent
        self.settingsMailbox = settingsMailbox
        self.settingsReplica = settingsReplica
    }

    /// What the app runs on: the login keychain, `UserDefaults.standard`, a file under
    /// Application Support, and the iCloud key-value store.
    ///
    /// One shared value rather than a computed property that builds a new set on every access. The
    /// stores are proxies onto shared system state, so duplicates would behave
    /// identically today — but "the app has one of these" is the property worth being able to rely
    /// on the day one of them grows a cache or a debounce.
    public static let live = PlatformStores(
        apiKey: KeychainAPIKeyStore(),
        settings: UserDefaultsSettingsStore(),
        fleetState: FileFleetStateStore(),
        usageConsent: UserDefaultsUsageConsentStore(),
        settingsMailbox: UbiquitousSettingsMailbox(),
        settingsReplica: UserDefaultsSettingsReplicaStore()
    )

    /// What tests and previews run on: nothing outside the process.
    ///
    /// The parameters take pre-seeded fakes so a test can start from a configured install rather
    /// than driving three setters first.
    public static func inMemory(
        apiKey: InMemoryAPIKeyStore = InMemoryAPIKeyStore(),
        settings: InMemorySettingsStore = InMemorySettingsStore(),
        fleetState: InMemoryFleetStateStore = InMemoryFleetStateStore(),
        // Opted out by default in tests and previews, which is the opposite of the app's default and
        // deliberately so: a preview must never be able to report, and a test asserting the opt-out
        // path should not need a line of setup to get there.
        usageConsent: InMemoryUsageConsentStore = InMemoryUsageConsentStore(optedOut: true),
        // No mailbox by default, so a preview never shares settings.
        settingsMailbox: InMemorySettingsMailbox? = nil,
        settingsReplica: InMemorySettingsReplicaStore = InMemorySettingsReplicaStore()
    ) -> PlatformStores {
        PlatformStores(
            apiKey: apiKey,
            settings: settings,
            fleetState: fleetState,
            usageConsent: usageConsent,
            settingsMailbox: settingsMailbox,
            settingsReplica: settingsReplica
        )
    }
}
