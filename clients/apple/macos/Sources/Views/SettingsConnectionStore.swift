import Foundation

/// What the settings window needs beyond what ``FleetSettingsStore`` already offers.
///
/// `FleetSettingsStore` is deliberately read-only about the connection: the model only ever *asks*
/// where the backend is. The settings window is the one screen that answers, so it needs the two
/// writes nobody else does, plus the two facts that distinguish "nothing is configured" from
/// "something is configured and could not be read".
///
/// It refines rather than replaces `FleetSettingsStore` because the composition root already holds
/// one object that is both — the app's bridge over the Keychain, `UserDefaults` and the state file.
/// Splitting the two would mean two objects with two copies of the API key, and the platform
/// layer's contract is explicit that the key is read once and held.
///
/// Not `@MainActor`, for the same reason `FleetSettingsStore` is not: an isolated protocol cannot
/// be refined by a conformer that has to stay non-isolated. Every call arrives on the main actor in
/// practice, because the only caller is a view.
public protocol SettingsConnectionStore: FleetSettingsStore {

    /// Why the API key could not be read, as a sentence, or `nil` when there was nothing to say.
    ///
    /// This is the difference between an empty Keychain and a locked one, and the window's whole
    /// behaviour hinges on it: while it is non-nil the key field is disabled and the connection
    /// cannot be saved. Writing a key over one that could not be read destroys it, and the platform
    /// layer's contract names that as the mistake never to make.
    var credentialProblem: String? { get }

    /// Core's reason for refusing the settings already on disk, or `nil` when it has no complaint.
    ///
    /// A stored environment with `::` in it, or a stored poll interval that fails one of the
    /// tuning interlocks, yields no configuration at all — which looks exactly like a first launch
    /// while meaning the opposite. The window shows this so that the user is told what to fix
    /// rather than invited to start over.
    var settingsProblem: String? { get }

    /// Attempt the held credential read again.
    ///
    /// Worth offering because the common failure repairs itself: a login item can start before the
    /// login keychain unlocks, and the key that was unreadable a minute ago is readable now.
    func retryUnresolvedCredentials()

    /// Persist the backend address and the API key.
    ///
    /// Called only when the values differ from ``FleetSettingsStore/connection`` and only while
    /// ``credentialProblem`` is `nil`.
    func save(connection: FleetConnection) throws
}

/// A conformer that keeps everything in memory, for previews and tests.
///
/// `saveError` exists so the window's failure path can be exercised. A Keychain that refuses to
/// write is where a settings screen most easily lies about having saved.
public final class InMemorySettingsConnectionStore: SettingsConnectionStore {

    public private(set) var connection: FleetConnection?

    public var config: FleetConfigRecord?

    public private(set) var credentialProblem: String?

    public var settingsProblem: String?

    /// What ``retryUnresolvedCredentials()`` will find on its next attempt, if anything.
    public var connectionOnRetry: FleetConnection?

    public var saveError: (any Error)?

    public init(
        connection: FleetConnection? = nil,
        config: FleetConfigRecord? = nil,
        credentialProblem: String? = nil,
        settingsProblem: String? = nil
    ) {
        self.connection = connection
        self.config = config
        self.credentialProblem = credentialProblem
        self.settingsProblem = settingsProblem
    }

    public func retryUnresolvedCredentials() {
        guard let connectionOnRetry else { return }
        connection = connectionOnRetry
        credentialProblem = nil
    }

    public func save(connection: FleetConnection) throws {
        if let saveError { throw saveError }
        self.connection = connection
        credentialProblem = nil
    }

    public func saveConfig(_ config: FleetConfigRecord) throws {
        if let saveError { throw saveError }
        self.config = config
    }
}
