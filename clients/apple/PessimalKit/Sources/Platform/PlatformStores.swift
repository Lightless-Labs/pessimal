import Foundation

/// The three places this app keeps things, handed to the Model layer as one value.
///
/// Exists so a view model takes a single parameter with a single test substitute rather than three
/// of each, and so there is one obvious place to see that the secret goes somewhere different from
/// everything else. It holds three references and adds no behaviour: any logic that appears here —
/// deciding what to do when the environment changes, choosing defaults for unset settings — belongs
/// in the Model layer, which is the layer that can ask the core.
public struct PlatformStores: Sendable {
    /// The backend API key. Keychain, never `UserDefaults`, never a log.
    public let apiKey: any APIKeyStore
    /// Base URL, environment, poll interval, alert rules JSON.
    public let settings: any SettingsStore
    /// The cached fleet state a relaunch restores from.
    public let fleetState: any FleetStateStore

    public init(apiKey: any APIKeyStore, settings: any SettingsStore, fleetState: any FleetStateStore) {
        self.apiKey = apiKey
        self.settings = settings
        self.fleetState = fleetState
    }

    /// What the app runs on: the login keychain, `UserDefaults.standard`, and a file under
    /// Application Support.
    ///
    /// One shared value rather than a computed property that builds a new set on every access. The
    /// three stores are stateless proxies onto shared system state, so duplicates would behave
    /// identically today — but "the app has one of these" is the property worth being able to rely
    /// on the day one of them grows a cache or a debounce.
    public static let live = PlatformStores(
        apiKey: KeychainAPIKeyStore(),
        settings: UserDefaultsSettingsStore(),
        fleetState: FileFleetStateStore()
    )

    /// What tests and previews run on: nothing outside the process.
    ///
    /// The parameters take pre-seeded fakes so a test can start from a configured install rather
    /// than driving three setters first.
    public static func inMemory(
        apiKey: InMemoryAPIKeyStore = InMemoryAPIKeyStore(),
        settings: InMemorySettingsStore = InMemorySettingsStore(),
        fleetState: InMemoryFleetStateStore = InMemoryFleetStateStore()
    ) -> PlatformStores {
        PlatformStores(apiKey: apiKey, settings: settings, fleetState: fleetState)
    }
}
