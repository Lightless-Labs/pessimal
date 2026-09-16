//
//  LoginItem.swift
//  PessimalKit
//
//  Whether the menu bar app opens when the user logs in.
//

#if os(macOS)
    import Foundation
    import ServiceManagement

    /// What macOS says about opening this app at login.
    ///
    /// This is read from the system on every look, never cached: a user can turn the app off in
    /// System Settings > General > Login Items at any time, and a remembered `true` would then show a
    /// switch that disagrees with the system.
    public enum LoginItemState: Equatable, Sendable {
        /// Registered and allowed. The app opens at the next login.
        case enabled
        /// Not registered.
        case disabled
        /// Registered, but the user has not allowed it yet. macOS asks once, in Login Items.
        case requiresApproval
        /// macOS will not answer for this copy of the app. The text says why, for the user to read.
        case unavailable(String)
    }

    /// The login item, as the settings window sees it.
    ///
    /// A protocol so a preview and the smoke test can drive the switch without registering anything
    /// with the real `launchd`.
    public protocol LoginItemController: AnyObject, Sendable {
        /// The system's answer right now.
        var state: LoginItemState { get }

        /// Registers or unregisters the app. Throws what macOS reports, for the caller to show.
        func setEnabled(_ enabled: Bool) throws

        /// Opens System Settings at Login Items, for the `requiresApproval` case.
        func openSystemSettings()
    }

    /// The real one, over `SMAppService`.
    ///
    /// `SMAppService.mainApp` registers *this bundle*, by the path it is running from. An app run
    /// from the Downloads folder therefore registers that copy, and moving it afterwards leaves a
    /// login item macOS can no longer find (`.notFound`, reported here as `unavailable`). Installing
    /// in Applications — which is what the Homebrew cask does — avoids it.
    public final class SMAppServiceLoginItem: LoginItemController, @unchecked Sendable {
        // `@unchecked`: the only state is `SMAppService`, whose own calls are the system's.
        private let service: SMAppService

        public init(service: SMAppService = .mainApp) {
            self.service = service
        }

        public var state: LoginItemState { Self.state(from: service.status) }

        /// Pure, and separate, so the mapping can be checked without a registration.
        public static func state(from status: SMAppService.Status) -> LoginItemState {
            switch status {
            case .enabled: return .enabled
            case .notRegistered: return .disabled
            case .requiresApproval: return .requiresApproval
            case .notFound:
                return .unavailable(
                    "macOS has no record of this copy of Pessimal. Move it to Applications and open it again."
                )
            @unknown default:
                return .unavailable("This version of macOS reports a login item state Pessimal does not know.")
            }
        }

        public func setEnabled(_ enabled: Bool) throws {
            if enabled {
                // `register()` on an already-registered app is not an error, so no status check first.
                try service.register()
            } else if service.status != .notRegistered {
                // `unregister()` on an app that is not registered throws, and "turn off what is
                // already off" is not a failure worth showing anybody.
                try service.unregister()
            }
        }

        public func openSystemSettings() {
            SMAppService.openSystemSettingsLoginItems()
        }
    }

    /// In memory, for previews and the smoke test. Registers nothing.
    public final class InMemoryLoginItemController: LoginItemController, @unchecked Sendable {
        private let lock = NSLock()
        private var current: LoginItemState
        /// Set to make `setEnabled` fail, which is how the settings window's error path is exercised.
        private let failure: (any Error)?

        public init(state: LoginItemState = .disabled, failure: (any Error)? = nil) {
            current = state
            self.failure = failure
        }

        public var state: LoginItemState { lock.withLock { current } }

        public func setEnabled(_ enabled: Bool) throws {
            if let failure { throw failure }
            lock.withLock { current = enabled ? .enabled : .disabled }
        }

        public func openSystemSettings() {}
    }
#endif
