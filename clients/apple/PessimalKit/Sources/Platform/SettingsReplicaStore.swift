import Foundation

/// This device's merged copy of the settings document, and the digest of the last unreadable
/// shared value it replaced.
///
/// Both are local and never synced. The copy is core's document text, stored unchanged. Core
/// derives this device's stamp clock from it, so it must be written before anything it produced
/// is published or applied.
public protocol SettingsReplicaStore: AnyObject, Sendable {
    /// The document text, or `nil` when this device has no copy yet.
    var document: String? { get set }

    /// The digest core returned the last time this device replaced unreadable shared text.
    var replacedRemoteDigest: String? { get set }
}

/// The real store, over `UserDefaults`.
public final class UserDefaultsSettingsReplicaStore: SettingsReplicaStore, @unchecked Sendable {
    // `@unchecked` for the same reason as `UserDefaultsSettingsStore`: the only mutable state is
    // `UserDefaults`'s own.
    private let defaults: UserDefaults

    private enum Key {
        static let document = "pessimal.sync.replica"
        static let replacedRemoteDigest = "pessimal.sync.replacedRemoteDigest"
    }

    /// - Parameter defaults: `.standard` in the app. Tests pass `UserDefaults(suiteName:)`.
    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    public var document: String? {
        get { defaults.string(forKey: Key.document) }
        set { write(newValue, forKey: Key.document) }
    }

    public var replacedRemoteDigest: String? {
        get { defaults.string(forKey: Key.replacedRemoteDigest) }
        set { write(newValue, forKey: Key.replacedRemoteDigest) }
    }

    private func write(_ value: String?, forKey key: String) {
        if let value {
            defaults.set(value, forKey: key)
        } else {
            defaults.removeObject(forKey: key)
        }
    }
}

/// A store in memory, for tests and previews.
public final class InMemorySettingsReplicaStore: SettingsReplicaStore, @unchecked Sendable {
    // `@unchecked`: the two values below are guarded by this lock.
    private let lock = NSLock()
    private var storedDocument: String?
    private var storedReplacedRemoteDigest: String?

    public init(document: String? = nil, replacedRemoteDigest: String? = nil) {
        storedDocument = document
        storedReplacedRemoteDigest = replacedRemoteDigest
    }

    public var document: String? {
        get { lock.withLock { storedDocument } }
        set { lock.withLock { storedDocument = newValue } }
    }

    public var replacedRemoteDigest: String? {
        get { lock.withLock { storedReplacedRemoteDigest } }
        set { lock.withLock { storedReplacedRemoteDigest = newValue } }
    }
}
