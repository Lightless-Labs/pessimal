//
//  FleetModelDependencies.swift
//  Pessimal — macOS menu bar client
//
//  The two things ``FleetModel`` cannot invent for itself: where the backend is, and what the
//  last session knew. Both are declared here, by the consumer, rather than imported from the
//  platform layer — the model states what it needs and the platform satisfies it, so a change of
//  storage (UserDefaults to a plist, Keychain to something else) cannot reach up into the model.
//

import Foundation
#if canImport(PessimalFFI)
    // Built as its own module for iOS, compiled into the app's module on macOS. See the note in
    // `FleetModel.swift`.
    import PessimalFFI
#endif

// MARK: - Connection

/// A backend URL and the key that opens it.
///
/// Kept apart from ``PessimalFFI/FleetConfigRecord`` because the two have different lifetimes and
/// different homes: the config is a document the user edits and core validates, while these two
/// strings are a credential — the key belongs in the Keychain and must never be written next to
/// the rest of the settings. Separating them here is what lets the platform layer store them in
/// two different places without the model knowing or caring.
public struct FleetConnection: Equatable, Sendable {
    /// The backend's base URL, e.g. `https://signoz.example.com`.
    ///
    /// Not validated here. A URL with no scheme is rejected by `FleetSession.init` with core's
    /// own message, and a second opinion in Swift would be a second thing to keep in step — one
    /// that will disagree the first time core learns to accept something new.
    public let baseURL: String

    /// The backend API key.
    ///
    /// Blank is likewise core's call, not ours: the constructor throws ``PessimalFFI/FfiError``
    /// `.Backend` for an empty key and the settings screen shows that sentence.
    public let apiKey: String

    /// Trims both fields, and only trims them.
    ///
    /// A key pasted from a password manager or a URL pasted from a browser very often arrives
    /// with a trailing newline, and neither is a different credential for having one. This is the
    /// whole of the cleanup the model is entitled to do: whether what remains is *usable* is a
    /// question only core answers.
    public init(baseURL: String, apiKey: String) {
        self.baseURL = baseURL.trimmingCharacters(in: .whitespacesAndNewlines)
        self.apiKey = apiKey.trimmingCharacters(in: .whitespacesAndNewlines)
    }
}

// MARK: - Settings

/// Where the user's saved settings live.
///
/// Read-mostly on purpose. Connection changes travel one way — the settings screen writes the
/// store, then calls ``FleetModel/reloadSettings()`` — so there is exactly one copy of "what is
/// currently configured" and it is not in the model. The config is the exception: only the model
/// knows whether core *accepted* a config, and a config core rejected must never reach disk, so
/// ``saveConfig(_:)`` is called by the model after acceptance rather than by the screen before it.
///
/// Only ever touched from the main actor by ``FleetModel``.
public protocol FleetSettingsStore: AnyObject {
    /// The saved connection, or `nil` when nothing has been saved yet.
    ///
    /// `nil` means *first launch* and must be reported as such. A `FleetConnection` holding two
    /// empty strings is a different fact — settings that exist and are wrong — and the model
    /// renders the two differently, so do not collapse them here.
    var connection: FleetConnection? { get }

    /// The saved configuration, or `nil` when the user has never saved one.
    ///
    /// The model does not manufacture a default: `fleetConfigDefaults(environment:)` needs an
    /// environment name, and inventing one would embed a guess in every rule URN the user later
    /// creates. The settings screen asks for it; this store remembers the answer.
    var config: FleetConfigRecord? { get }

    /// Persists a configuration core has already accepted.
    ///
    /// - Throws: whatever the storage layer throws. The model surfaces the failure on
    ///   ``FleetModel/lastPersistenceError`` rather than rolling back: the config is already live
    ///   in the session, and pretending otherwise would put the screen and the poller out of step.
    func saveConfig(_ config: FleetConfigRecord) throws
}

// MARK: - State cache

/// Where a session's exported state sleeps between launches.
///
/// The string is opaque — it is `FleetSession.exportState()`'s output and `FleetSession.init`'s
/// `cachedStateJson` input, and nothing in Swift may parse, merge, or repair it. A cache that
/// cannot be read is not an error either: core reports what it salvaged through
/// ``FleetModel/restoreReport``, because a monitoring app that refuses to launch over a stale
/// cache has failed at the one job it had.
public protocol FleetStateCache: AnyObject {
    /// The last exported state, or `nil` if there is none.
    func loadState() -> String?

    /// Stores the exported state.
    ///
    /// - Throws: whatever the storage layer throws; the model records it on
    ///   ``FleetModel/lastPersistenceError``. A failed cache write costs a colder start next
    ///   launch and nothing else, so it must never interrupt polling.
    func saveState(_ json: String) throws

    /// Discards the cached state.
    ///
    /// Called when the backend URL changes: the hosts in a cache describe the fleet at the old
    /// URL, and showing them under a new one would be inventing a fleet.
    func clearState() throws
}

// MARK: - Test and preview doubles

/// An in-memory ``FleetSettingsStore``, for previews and tests.
///
/// Deliberately not the app's real store: nothing here touches UserDefaults or the Keychain, so a
/// preview cannot read — or worse, overwrite — the operator's actual credentials.
public final class InMemoryFleetSettingsStore: FleetSettingsStore {
    public var connection: FleetConnection?
    public var config: FleetConfigRecord?

    /// Set to have ``saveConfig(_:)`` throw, to exercise the persistence-failure path.
    public var saveError: (any Error)?

    public init(connection: FleetConnection? = nil, config: FleetConfigRecord? = nil) {
        self.connection = connection
        self.config = config
    }

    public func saveConfig(_ config: FleetConfigRecord) throws {
        if let saveError { throw saveError }
        self.config = config
    }
}

/// An in-memory ``FleetStateCache``, for previews and tests.
public final class InMemoryFleetStateCache: FleetStateCache {
    public private(set) var state: String?

    /// Set to have ``saveState(_:)`` throw, to exercise the persistence-failure path.
    public var saveError: (any Error)?

    public init(state: String? = nil) { self.state = state }

    public func loadState() -> String? { state }

    public func saveState(_ json: String) throws {
        if let saveError { throw saveError }
        state = json
    }

    public func clearState() throws { state = nil }
}
