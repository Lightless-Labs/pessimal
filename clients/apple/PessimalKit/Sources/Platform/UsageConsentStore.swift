import Foundation

/// Whether the user has opted out of usage reporting, and the build's own destination.
///
/// A **separate store from ``SettingsStore``, on purpose.** `SettingsStore.removeAll()` is what
/// "reset connection" calls, and an opt-out living there would be silently revoked by a user who
/// cleared a mistyped backend URL. Opting out of telemetry is not a connection setting, it is a
/// standing instruction, and it has to survive every other thing this app forgets.
///
/// Reporting is opt-*out*, so the absent case is the loud one: a key that was never written means
/// **reporting is allowed**. That inverts ``SettingsStore``'s "nil means not configured" convention,
/// which is exactly why this is its own type with its own documentation rather than a fifth property
/// over there.
public protocol UsageConsentStore: AnyObject, Sendable {
    /// `true` when the user has asked not to report. Defaults to `false` — see the type's note on
    /// why the absent case means allowed.
    var optedOut: Bool { get set }
}

/// The real store, over `UserDefaults`.
///
/// Not the Keychain: an opt-out is not a secret, and a Keychain read can fail on a locked device —
/// which here would mean failing *open* and reporting from a user who had opted out. `UserDefaults`
/// is readable whenever the app is running.
public final class UserDefaultsUsageConsentStore: UsageConsentStore, @unchecked Sendable {
    // `@unchecked` for the same reason as `UserDefaultsSettingsStore`: the only mutable state is
    // `UserDefaults`'s own, which is thread-safe but not annotated `Sendable` on every SDK.
    private let defaults: UserDefaults

    /// Deliberately **not** in ``UserDefaultsSettingsStore``'s `Key.all`, which is what `removeAll()`
    /// iterates. Keeping the name here rather than there is the mechanism, not a style choice.
    private static let key = "pessimal.usageReportingOptedOut"

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    public var optedOut: Bool {
        // `bool(forKey:)` answers `false` for a key that was never written, and here that is the
        // behaviour we want rather than an ambiguity to work around: unset means not opted out.
        get { defaults.bool(forKey: Self.key) }
        set { defaults.set(newValue, forKey: Self.key) }
    }
}

/// In memory, for tests and previews.
public final class InMemoryUsageConsentStore: UsageConsentStore, @unchecked Sendable {
    private let lock = NSLock()
    private var stored: Bool

    public init(optedOut: Bool = false) {
        stored = optedOut
    }

    public var optedOut: Bool {
        get { lock.withLock { stored } }
        set { lock.withLock { stored = newValue } }
    }
}

/// Where this build reports, as read from its own bundle.
///
/// Empty strings are the normal answer. A `genrule` writes the plist with `$${VAR:-}`, so a build
/// that was not given a destination — every local build, every simulator build, every CI build —
/// gets empty values and reports nothing. The Rust side treats empty as "no destination" rather than
/// as a destination that fails, so there is nothing to check here.
public struct UsageDestination: Sendable, Equatable {
    public let endpoint: String
    public let ingestionKey: String

    public init(endpoint: String, ingestionKey: String) {
        self.endpoint = endpoint
        self.ingestionKey = ingestionKey
    }

    /// Reads the two keys the `usage_plist` genrule writes.
    ///
    /// Mirrors kumbaya's `Config.swift` and phil-connors' equivalent, including the `?? ""` — the
    /// pattern both ship, and the one the Rust side is written to expect.
    public static func fromBundle(_ bundle: Bundle = .main) -> UsageDestination {
        UsageDestination(
            endpoint: bundle.object(forInfoDictionaryKey: "SIGNOZ_OTLP_ENDPOINT") as? String ?? "",
            ingestionKey: bundle.object(forInfoDictionaryKey: "SIGNOZ_OTLP_INGESTION_KEY") as? String ?? ""
        )
    }

    /// Whether this build could report at all, consent aside. Shown on the settings screen so a
    /// developer running a local build is told why the toggle has no effect.
    public var isConfigured: Bool {
        !endpoint.isEmpty && !ingestionKey.isEmpty
    }
}

#if canImport(UIKit)
    import UIKit
#endif
#if canImport(PessimalFFI)
    import PessimalFFI
#endif

/// Assembles the record `FleetSession.init` wants, from the bundle, the platform, and the user's
/// switch.
///
/// Kept out of ``FleetModel`` because every value here is an environmental fact rather than a
/// decision: which OS this is, what the bundle says its version is, whether the user opted out. The
/// model's job is to carry the record to the core, not to discover it.
///
/// Note what this does **not** assemble: any identifier. `service.instance.id` is minted inside Rust
/// and never accepted from here, so no amount of editing this file can attach a device id or a host
/// id to a span.
public enum UsageReporting {
    /// The record for this build, or `nil` if the FFI layer is not linked in.
    ///
    /// - Parameters:
    ///   - optedOut: read from ``UsageConsentStore`` at the moment a session is built, so toggling
    ///     the switch and letting the session rebuild is what turns reporting on or off.
    ///   - destination: normally ``UsageDestination/fromBundle(_:)``. Injectable so a test can assert
    ///     the empty case without a bundle.
    public static func record(
        optedOut: Bool,
        destination: UsageDestination = .fromBundle(),
        bundle: Bundle = .main
    ) -> UsageReportingRecord {
        UsageReportingRecord(
            endpoint: destination.endpoint,
            ingestionKey: destination.ingestionKey,
            optedOut: optedOut,
            platform: currentPlatform,
            deviceClass: currentDeviceClass,
            // `CFBundleShortVersionString` is the marketing version; the Rust side drops it unless it
            // parses as a dotted numeric version, so a placeholder cannot become an attribute.
            appVersion: bundle.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "",
            appBuild: (bundle.object(forInfoDictionaryKey: "CFBundleVersion") as? String)
                .flatMap { UInt32($0.split(separator: ".").last.map(String.init) ?? $0) },
            osVersion: currentOSVersion
        )
    }

    private static var currentPlatform: UsagePlatformRecord {
        #if os(iOS)
            return .ios
        #elseif os(macOS)
            return .macos
        #elseif os(Linux)
            return .linux
        #elseif os(Windows)
            return .windows
        #else
            // No variant for "something else", and inventing one would widen the allowlist for a
            // platform this app does not ship on. macOS is the closest true statement for any other
            // Apple target.
            return .macos
        #endif
    }

    /// Coarse on purpose: phone, tablet, Mac. An exact model would be a narrower cohort than anything
    /// we would act on, and on a small install base a narrow cohort is an identifier.
    private static var currentDeviceClass: UsageDeviceClassRecord {
        #if canImport(UIKit) && !os(watchOS)
            switch UIDevice.current.userInterfaceIdiom {
            case .pad: return .tablet
            case .mac: return .mac
            default: return .phone
            }
        #elseif os(macOS)
            return .mac
        #else
            return .server
        #endif
    }

    /// Major and minor only — the Rust side trims a patch level anyway, so sending one would be
    /// pointless rather than merely unwise.
    private static var currentOSVersion: String {
        let version = ProcessInfo.processInfo.operatingSystemVersion
        return "\(version.majorVersion).\(version.minorVersion)"
    }
}
