import Foundation

/// Why the shared settings value changed, as the mailbox reports it.
public enum SettingsMailboxChange: Sendable, Equatable {
    /// Another device wrote a new value.
    case serverChange
    /// The first download for this account finished.
    case initialSync
    /// The store refused a write because the app is over its storage quota.
    case quotaViolation
    /// The user signed in to a different account, or signed out.
    case accountChanged
}

/// Where the settings document is shared with the user's other devices.
///
/// It moves one string and parses nothing. Core's `settings_sync` module decides what to write and
/// assumes only this: a read may return an old value, and a write may be lost.
public protocol SettingsMailbox: AnyObject, Sendable {
    /// The shared document text, or `nil` when there is none.
    func read() -> String?

    /// Replaces the shared document text. The write may be lost.
    func write(_ text: String)

    /// Asks the store to sync now. `false` means the store is not available to this app, for
    /// example when the app has no iCloud entitlement.
    @discardableResult func synchronize() -> Bool

    /// Calls `onChange` for every change the store reports, on any thread.
    func observe(_ onChange: @escaping @Sendable (SettingsMailboxChange) -> Void)
}

/// The mailbox over the iCloud key-value store, under the key `pessimal.settings`.
///
/// Without the iCloud key-value entitlement the store does not fail: `read()` returns `nil`,
/// `write(_:)` does nothing and `synchronize()` returns `false`. Measured on macOS 26 with an
/// unsigned command-line binary on 2026-09-14.
public final class UbiquitousSettingsMailbox: SettingsMailbox, @unchecked Sendable {
    // `@unchecked`: `NSUbiquitousKeyValueStore` is thread-safe, and `observers` is guarded by `lock`.

    /// The key-value store key. Changing it starts a new, empty shared value.
    public static let key = "pessimal.settings"

    private let store: NSUbiquitousKeyValueStore
    private let lock = NSLock()
    private var observers: [any NSObjectProtocol] = []

    public init(store: NSUbiquitousKeyValueStore = .default) {
        self.store = store
    }

    deinit {
        for observer in observers {
            NotificationCenter.default.removeObserver(observer)
        }
    }

    public func read() -> String? {
        store.string(forKey: Self.key)
    }

    public func write(_ text: String) {
        store.set(text, forKey: Self.key)
    }

    @discardableResult
    public func synchronize() -> Bool {
        store.synchronize()
    }

    public func observe(_ onChange: @escaping @Sendable (SettingsMailboxChange) -> Void) {
        let observer = NotificationCenter.default.addObserver(
            forName: NSUbiquitousKeyValueStore.didChangeExternallyNotification,
            object: store,
            queue: nil
        ) { notification in
            guard let change = Self.change(from: notification.userInfo) else { return }
            onChange(change)
        }
        lock.withLock { observers.append(observer) }
    }

    /// Maps `NSUbiquitousKeyValueStoreChangeReasonKey` to a change. `nil` for a reason this build
    /// does not know.
    static func change(from userInfo: [AnyHashable: Any]?) -> SettingsMailboxChange? {
        guard let reason = userInfo?[NSUbiquitousKeyValueStoreChangeReasonKey] as? Int else {
            return nil
        }
        switch reason {
        case NSUbiquitousKeyValueStoreServerChange:
            return .serverChange
        case NSUbiquitousKeyValueStoreInitialSyncChange:
            return .initialSync
        case NSUbiquitousKeyValueStoreQuotaViolationChange:
            return .quotaViolation
        case NSUbiquitousKeyValueStoreAccountChange:
            return .accountChanged
        default:
            return nil
        }
    }
}

/// A mailbox in memory, for tests and previews.
///
/// Mailboxes made with ``init(pairedWith:)`` share one value, like two devices on one iCloud
/// account. Nothing is delivered on its own: a test calls ``deliver(_:)`` to report a change.
public final class InMemorySettingsMailbox: SettingsMailbox, @unchecked Sendable {
    // `@unchecked`: every mutable property below is guarded by `lock`, and the shared value by the
    // cell's own lock.

    /// The one value that paired mailboxes share.
    private final class Cell: @unchecked Sendable {
        private let lock = NSLock()
        private var stored: String?

        var text: String? {
            get { lock.withLock { stored } }
            set { lock.withLock { stored = newValue } }
        }
    }

    private let cell: Cell
    private let lock = NSLock()
    private var storedWritesToDrop = 0
    private var storedWriteCount = 0
    private var storedDroppedWriteCount = 0
    private var handlers: [@Sendable (SettingsMailboxChange) -> Void] = []

    /// When `false`, the mailbox behaves like the iCloud store without its entitlement: reads return
    /// `nil`, writes are lost and ``synchronize()`` returns `false`.
    public let isAvailable: Bool

    /// A mailbox with its own, empty value.
    public init(isAvailable: Bool = true) {
        cell = Cell()
        self.isAvailable = isAvailable
    }

    /// A mailbox that shares `other`'s value.
    public init(pairedWith other: InMemorySettingsMailbox, isAvailable: Bool = true) {
        cell = other.cell
        self.isAvailable = isAvailable
    }

    /// How many of the next writes are lost.
    public var writesToDrop: Int {
        get { lock.withLock { storedWritesToDrop } }
        set { lock.withLock { storedWritesToDrop = newValue } }
    }

    /// Every call to ``write(_:)``, kept or lost.
    public var writeCount: Int {
        lock.withLock { storedWriteCount }
    }

    /// The writes that were lost.
    public var droppedWriteCount: Int {
        lock.withLock { storedDroppedWriteCount }
    }

    public func read() -> String? {
        isAvailable ? cell.text : nil
    }

    public func write(_ text: String) {
        let kept: Bool = lock.withLock {
            storedWriteCount += 1
            if storedWritesToDrop > 0 {
                storedWritesToDrop -= 1
                storedDroppedWriteCount += 1
                return false
            }
            return isAvailable
        }
        if kept { cell.text = text }
    }

    @discardableResult
    public func synchronize() -> Bool {
        isAvailable
    }

    public func observe(_ onChange: @escaping @Sendable (SettingsMailboxChange) -> Void) {
        lock.withLock { handlers.append(onChange) }
    }

    /// Reports `change` to every observer of this mailbox, on the calling thread.
    public func deliver(_ change: SettingsMailboxChange) {
        let handlers = lock.withLock { self.handlers }
        for handler in handlers {
            handler(change)
        }
    }
}
