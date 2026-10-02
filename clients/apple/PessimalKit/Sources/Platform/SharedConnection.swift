//
//  SharedConnection.swift
//  PessimalKit
//
//  What the iOS widget needs to poll on its own: the backend, the key, and the environment.
//

import Foundation
import Security

/// The connection the widget extension polls with.
///
/// A widget runs in its own process, and the app is rarely running when one refreshes, so the widget
/// polls the backend itself rather than reading a snapshot the app wrote — a snapshot is stale exactly
/// when the widget is the only thing anyone is looking at. To poll it needs these three values.
public struct SharedConnection: Codable, Equatable, Sendable {
    public let baseURL: String
    public let apiKey: String
    public let environment: String

    public init(baseURL: String, apiKey: String, environment: String) {
        self.baseURL = baseURL
        self.apiKey = apiKey
        self.environment = environment
    }
}

/// Where the app leaves the connection and the widget finds it.
public protocol SharedConnectionStore: AnyObject, Sendable {
    /// The stored connection, or `nil` when the app has not written one yet.
    func read() throws -> SharedConnection?
    /// Replaces the stored connection.
    func write(_ connection: SharedConnection) throws
}

/// The real store: one keychain item in a group the app and the widget both claim.
///
/// The keychain rather than an App Group, because every explicit App ID's profile already allows any
/// access group under the team's prefix, so this needs no capability in the developer portal — and
/// the API key is a secret that belongs in the keychain anyway. See
/// `docs/plans/2026-10-02-m11-ios-widgets.md`.
///
/// A **mirror**, not the app's source of truth. The app keeps its own key and settings where they
/// were; this copy exists for the widget. Nothing reads it back into the app, so a widget that cannot
/// see it fails on its own without taking the app's configuration with it.
public final class KeychainSharedConnectionStore: SharedConnectionStore, @unchecked Sendable {
    // `@unchecked`: the only state is two immutable strings; every call goes to the keychain.

    /// The group both targets list in `keychain-access-groups`. Team-prefixed, which is what lets the
    /// existing profiles allow it without a portal change.
    ///
    /// Named explicitly on every call. Once an app's entitlements list a keychain group, the first one
    /// listed becomes its default for items written without a group, so an unnamed write here could
    /// land somewhere a later version of the entitlements no longer puts it.
    public static let defaultAccessGroup = "PKPPLFK854.com.lightless-labs.pessimal.shared"

    /// One item, so one service name.
    public static let defaultService = "com.lightless-labs.pessimal.shared"

    private static let account = "connection"

    private let accessGroup: String
    private let service: String

    public init(
        accessGroup: String = KeychainSharedConnectionStore.defaultAccessGroup,
        service: String = KeychainSharedConnectionStore.defaultService
    ) {
        self.accessGroup = accessGroup
        self.service = service
    }

    private var query: [String: Any] {
        [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: Self.account,
            kSecAttrAccessGroup as String: accessGroup,
        ]
    }

    public func read() throws -> SharedConnection? {
        var lookup = query
        lookup[kSecReturnData as String] = true
        lookup[kSecMatchLimit as String] = kSecMatchLimitOne

        var result: CFTypeRef?
        let status = SecItemCopyMatching(lookup as CFDictionary, &result)
        switch status {
        case errSecSuccess:
            guard let data = result as? Data else { return nil }
            return try JSONDecoder().decode(SharedConnection.self, from: data)
        case errSecItemNotFound:
            return nil
        default:
            throw SharedConnectionError.keychain(status)
        }
    }

    public func write(_ connection: SharedConnection) throws {
        let data = try JSONEncoder().encode(connection)
        let update: [String: Any] = [
            kSecValueData as String: data,
            // After first unlock, not when unlocked: a lock screen widget refreshes while the phone is
            // locked, and an item it could only read while unlocked would leave it blank exactly
            // where it is most visible.
            kSecAttrAccessible as String: kSecAttrAccessibleAfterFirstUnlock,
        ]
        let status = SecItemUpdate(query as CFDictionary, update as CFDictionary)
        if status == errSecItemNotFound {
            var item = query
            item.merge(update) { _, new in new }
            let added = SecItemAdd(item as CFDictionary, nil)
            guard added == errSecSuccess else { throw SharedConnectionError.keychain(added) }
            return
        }
        guard status == errSecSuccess else { throw SharedConnectionError.keychain(status) }
    }
}

/// In memory, for tests and previews.
public final class InMemorySharedConnectionStore: SharedConnectionStore, @unchecked Sendable {
    private let lock = NSLock()
    private var stored: SharedConnection?

    public init(_ connection: SharedConnection? = nil) {
        stored = connection
    }

    public func read() throws -> SharedConnection? {
        lock.withLock { stored }
    }

    public func write(_ connection: SharedConnection) throws {
        lock.withLock { stored = connection }
    }
}

public enum SharedConnectionError: Error, Equatable, LocalizedError {
    /// The keychain refused, with its own status. `-34018` (`errSecMissingEntitlement`) means the
    /// running binary does not claim the access group — a build without the entitlements file.
    case keychain(OSStatus)

    public var errorDescription: String? {
        switch self {
        case let .keychain(status):
            let message = SecCopyErrorMessageString(status, nil) as String? ?? "unknown"
            return "The shared keychain item could not be used (\(status): \(message))."
        }
    }
}
