import Foundation
import Security

/// Where the telemetry backend's API key lives.
///
/// A protocol rather than a concrete type for one reason: the real implementation talks to the
/// login keychain, which on an ad-hoc-signed build shows a modal prompt. A view model that must
/// exercise "no key configured", "key present", and "the read failed" cannot do any of that against
/// a store that blocks on a dialog, so the view model takes `any APIKeyStore` and tests hand it
/// ``InMemoryAPIKeyStore``.
///
/// Synchronous on purpose. Every call is a single `SecItem*` round trip against a local database;
/// the one that can be slow is the first read after launch, when macOS may prompt. That is a reason
/// to read the key *once* at startup and hold it, not a reason to make three methods `async` and
/// give every caller an actor hop it does not need.
public protocol APIKeyStore: Sendable {
    /// The stored key, or `nil` when the user has never configured one.
    ///
    /// `nil` means exactly one thing: no item exists. Every other outcome — a dismissed prompt, a
    /// locked keychain, a denied ACL — throws. The distinction is the whole point: a menu bar app
    /// that read a cancelled prompt as "no key configured" would silently offer to set up a backend
    /// the user already configured, and on save would overwrite the key it could not read.
    ///
    /// - Throws: ``KeychainError`` for any failure that is not "no such item".
    func load() throws -> String?

    /// Stores the key, replacing whatever was there.
    ///
    /// Leading and trailing whitespace is trimmed — a pasted key routinely carries a trailing
    /// newline, and that is presentation noise, not content. Nothing else is inspected: whether the
    /// key is *acceptable* is the backend's answer, delivered as `FfiError.Unauthorized` on the
    /// first poll, and a length or charset check here would be this app inventing a rule the
    /// backend never stated.
    ///
    /// - Throws: ``KeychainError`` if the item can be neither added nor updated.
    func save(_ apiKey: String) throws

    /// Removes the key. Succeeds when there was nothing to remove.
    ///
    /// - Throws: ``KeychainError`` for any failure other than "no such item".
    func delete() throws
}

/// A keychain operation, named for the error message.
///
/// The four `SecItem*` calls report through the same `OSStatus` space, so "errSecAuthFailed" alone
/// does not say whether the user was denied a read or a write. This is that word.
public enum KeychainOperation: String, Sendable, Equatable {
    case read
    case add
    case update
    case delete
}

/// What the Security framework said, in a form that can be shown to a person.
///
/// Carries the raw `OSStatus` alongside the rendered message so a bug report can quote a number
/// that means the same thing in every locale. It carries nothing else — not the key, not its
/// length, not a prefix of it. There is no diagnostic worth printing a secret's shape for.
public enum KeychainError: Error, Equatable, CustomStringConvertible, LocalizedError {
    /// The Security framework refused, and the refusal is not one this app knows how to absorb.
    case unhandled(status: OSStatus, operation: KeychainOperation)
    /// An item exists but its bytes are not UTF-8, so it was not written by this app.
    case malformedSecret

    public var description: String {
        switch self {
        case let .unhandled(status, operation):
            let detail = SecCopyErrorMessageString(status, nil) as String? ?? "no description available"
            return "keychain \(operation.rawValue) failed: \(detail) (OSStatus \(status))"
        case .malformedSecret:
            return "the stored API key is not valid UTF-8, so it was not written by this app"
        }
    }

    public var errorDescription: String? { description }

    /// True when the user dismissed the keychain prompt.
    ///
    /// Exposed because the response is different in kind: nothing is wrong, the user simply
    /// declined, and an alert saying "keychain read failed" for a button the user just pressed
    /// reads as a bug. The decision of what to *do* is the view model's; this is the fact it needs.
    public var isUserCancelled: Bool {
        if case let .unhandled(status, _) = self { return status == errSecUserCanceled }
        return false
    }

    /// True when the keychain could not be consulted at all, rather than consulted and refused.
    ///
    /// The case that matters for a menu bar app: launched as a login item, this process can be
    /// running before the login keychain is unlocked, and a read then fails with
    /// `errSecInteractionNotAllowed` — a *timing* failure that succeeds on retry a moment later.
    /// Treating it as "no key configured" would greet the user with the setup screen after every
    /// reboot, and worse, a subsequent save would replace a key that was there all along.
    public var isKeychainUnavailable: Bool {
        if case let .unhandled(status, _) = self {
            return status == errSecInteractionNotAllowed || status == errSecNotAvailable
        }
        return false
    }
}

/// The real store: a `kSecClassGenericPassword` item in the login keychain.
///
/// **Why the file-based keychain and not the data-protection keychain.** Setting
/// `kSecUseDataProtectionKeychain` would buy `kSecAttrAccessible`, access groups, and iCloud sync —
/// and would fail outright with `errSecMissingEntitlement` (-34018) on any build that is not signed
/// with an Apple-issued application identifier. That is every developer build and every CI build
/// this repo currently produces; there is no `.entitlements` file in the tree yet. One code path
/// that works everywhere beats two that differ by signing identity, and `kSecAttrAccessible` is
/// therefore also *not* set here: outside the data-protection keychain it is ignored, and writing
/// an attribute that does nothing is how a later reader concludes the item is protected when it is
/// not. When the app gains a signed identity and an entitlements file, moving to the
/// data-protection keychain is a deliberate migration — items do not move by themselves — not a
/// flag flip.
///
/// **The consequence to expect while developing.** An ad-hoc-signed binary's designated requirement
/// changes on every rebuild, so macOS treats each build as a different application and prompts for
/// access to an item the previous build created. That prompt is the system working; it is not a bug
/// in this type, and it disappears once builds are signed with a stable identity.
///
/// This type never logs. There is no message it could emit about a secret that is both useful and
/// safe, and a log line is the one place a secret leaks without anyone reading the code that put it
/// there.
public final class KeychainAPIKeyStore: APIKeyStore {
    /// The keychain service the key is filed under.
    ///
    /// Deliberately a literal rather than `Bundle.main.bundleIdentifier`. The service name *is* the
    /// address of the secret, and the bundle identifier is `nil` in a command-line test harness and
    /// something else again under a preview agent — a store whose address depends on how the
    /// process was launched writes the key in one place and reads `nil` from another.
    public static let defaultService = "com.lightless-labs.pessimal.macos"

    /// The account the key is filed under. One backend, one key, one account.
    public static let defaultAccount = "backend-api-key"

    private let service: String
    private let account: String
    private let label: String

    /// - Parameters:
    ///   - service: Overridden only by tests, which must use a throwaway name so a test run cannot
    ///     touch the key a real install put there.
    ///   - account: As above.
    ///   - label: What Keychain Access shows in its list. Purely for the human who goes looking.
    public init(
        service: String = KeychainAPIKeyStore.defaultService,
        account: String = KeychainAPIKeyStore.defaultAccount,
        label: String = "Pessimal telemetry backend API key"
    ) {
        self.service = service
        self.account = account
        self.label = label
    }

    /// The three attributes that identify the item, and nothing else.
    ///
    /// Reused verbatim by read, update, and delete so the four operations cannot drift into
    /// addressing different items — the failure mode being a save that writes one item and a load
    /// that finds another, which looks exactly like a key that will not stick.
    private var identity: [String: Any] {
        [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
        ]
    }

    public func load() throws -> String? {
        var query = identity
        query[kSecReturnData as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne

        var item: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &item)

        switch status {
        case errSecSuccess:
            guard let data = item as? Data, let key = String(data: data, encoding: .utf8) else {
                throw KeychainError.malformedSecret
            }
            return key
        case errSecItemNotFound:
            return nil
        default:
            throw KeychainError.unhandled(status: status, operation: .read)
        }
    }

    public func save(_ apiKey: String) throws {
        let data = Data(apiKey.trimmingCharacters(in: .whitespacesAndNewlines).utf8)

        var attributes = identity
        attributes[kSecValueData as String] = data
        attributes[kSecAttrLabel as String] = label

        let addStatus = SecItemAdd(attributes as CFDictionary, nil)
        switch addStatus {
        case errSecSuccess:
            return
        case errSecDuplicateItem:
            // The expected path on every save after the first. `SecItemUpdate` takes the identity as
            // the query and *only* the changed attributes as the update — passing `kSecClass` in the
            // second dictionary is rejected with errSecParam, which reads like a malformed key and
            // is not.
            let updateStatus = SecItemUpdate(
                identity as CFDictionary,
                [kSecValueData as String: data] as CFDictionary
            )
            guard updateStatus == errSecSuccess else {
                throw KeychainError.unhandled(status: updateStatus, operation: .update)
            }
        default:
            throw KeychainError.unhandled(status: addStatus, operation: .add)
        }
    }

    public func delete() throws {
        let status = SecItemDelete(identity as CFDictionary)
        guard status == errSecSuccess || status == errSecItemNotFound else {
            throw KeychainError.unhandled(status: status, operation: .delete)
        }
    }
}

/// A store that keeps the key in memory, for tests and SwiftUI previews.
///
/// Shipped in the app rather than hidden behind `#if DEBUG` because previews build in release
/// configurations too, and a preview that touches the login keychain is a preview that shows a
/// modal prompt inside Xcode's canvas.
public final class InMemoryAPIKeyStore: APIKeyStore, @unchecked Sendable {
    // `@unchecked` because the mutable state below is guarded by this lock rather than by the
    // compiler. `NSLock` is the right primitive over an actor: the protocol is synchronous, and it
    // is synchronous because the real implementation is.
    private let lock = NSLock()
    private var apiKey: String?
    private var failure: (any Error)?

    public init(apiKey: String? = nil) {
        self.apiKey = apiKey
    }

    /// Makes every subsequent call throw, so a view model's failure branch can be driven.
    ///
    /// Worth having because the interesting keychain bugs are all on the error path — a cancelled
    /// prompt and a locked keychain are the two states a happy-path fake can never produce.
    public func failEveryOperation(with error: any Error) {
        lock.withLock { failure = error }
    }

    /// Undoes ``failEveryOperation(with:)``.
    public func stopFailing() {
        lock.withLock { failure = nil }
    }

    public func load() throws -> String? {
        try lock.withLock {
            if let failure { throw failure }
            return apiKey
        }
    }

    public func save(_ apiKey: String) throws {
        try lock.withLock {
            if let failure { throw failure }
            // Trims exactly as the keychain store does. A fake with looser rules is a test that
            // passes on behaviour the app does not have.
            self.apiKey = apiKey.trimmingCharacters(in: .whitespacesAndNewlines)
        }
    }

    public func delete() throws {
        try lock.withLock {
            if let failure { throw failure }
            apiKey = nil
        }
    }
}
