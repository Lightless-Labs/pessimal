//
//  AppVersion.swift
//  PessimalKit
//
//  What this build calls itself.
//

import Foundation

/// The version strings from the bundle, and how to show them.
///
/// Both apps show this, and a user reporting something is the reason it exists: "it does X" is not
/// actionable without knowing which build did X. The two apps also version independently — the Mac
/// app is released with the agent's tag, the iOS app ships to TestFlight on every green push — so
/// the same screen on two devices will often disagree, and that is worth being able to see.
public struct AppVersion: Equatable, Sendable {
    /// `CFBundleShortVersionString`: what a release is called. "0.5.0".
    public let marketing: String

    /// `CFBundleVersion`: the build counter. "0.5.0.74" on iOS, where the release tag and the
    /// TestFlight build number are joined; the same as `marketing` on macOS, which has no counter.
    public let build: String

    public init(marketing: String, build: String) {
        self.marketing = marketing
        self.build = build
    }

    /// Reads the running bundle. Injectable so the smoke test can assert without one.
    public static func fromBundle(_ bundle: Bundle = .main) -> AppVersion {
        AppVersion(
            marketing: bundle.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "",
            build: bundle.object(forInfoDictionaryKey: "CFBundleVersion") as? String ?? ""
        )
    }

    /// One short string: the release, and the build counter when it says something the release does
    /// not.
    ///
    /// `0.5.0` when the two agree, `0.5.0 (74)` when the build is the release plus a counter, and
    /// `0.5.0 (someone's local build)` for anything else, because a build string that does not
    /// follow the release is exactly the one worth showing whole.
    public var display: String {
        guard !marketing.isEmpty else { return build }
        guard !build.isEmpty, build != marketing else { return marketing }
        if build.hasPrefix(marketing + ".") {
            return "\(marketing) (\(String(build.dropFirst(marketing.count + 1))))"
        }
        return "\(marketing) (\(build))"
    }

    /// With the app's name in front, for a line that stands on its own.
    public var labelled: String {
        display.isEmpty ? "Pessimal" : "Pessimal \(display)"
    }
}
