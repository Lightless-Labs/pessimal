//
//  SettingsUsageReporting.swift
//  Pessimal — macOS menu bar client
//
//  The opt-out switch, and an honest list of what is sent.
//
//  The iOS app has the same section with a sheet; a Settings window has the room to say it inline, so
//  this is a disclosure group rather than a separate screen. Both lists are the `pessimal_usage`
//  attribute allowlist in the user's words — if that grows, both grow, and `AttributeKey::ALL` plus
//  the test beside it are what stop anything else being emitted in the meantime.
//

#if canImport(PessimalFFI)
    // Explicit, and the same guard every other view here carries. Under Bazel `PessimalFFI` is a real
    // separate module, so an FFI record used in a signature is not in scope without this — the macOS
    // script compiles everything into one module and would not have caught it.
    import PessimalFFI
#endif
#if canImport(PessimalKit)
    import PessimalKit
#endif
import SwiftUI

/// "Share usage diagnostics", with the full list one click away.
struct SettingsUsageReportingSection: View {
    let model: FleetModel

    /// Mirrors the model. `@State` rather than a binding onto the model because the setter there
    /// rebuilds the session, and a control bound to a property that rebuilds under it redraws
    /// unpredictably.
    @State private var enabled: Bool

    init(model: FleetModel) {
        self.model = model
        _enabled = State(initialValue: model.usageReportingEnabled)
    }

    var body: some View {
        Section("Diagnostics") {
            Toggle("Share usage diagnostics", isOn: Binding(
                get: { enabled },
                set: { shareIt in
                    enabled = shareIt
                    // One call: the model writes the store and rebuilds the session, because consent
                    // is read when a reporter is built and there is no live sink to reconfigure.
                    model.usageReportingEnabled = shareIt
                }
            ))
            .disabled(!model.usageReportingConfigured)

            if model.usageReportingConfigured {
                Text(
                    """
                    Sends counts and outcomes about Pessimal itself to Lightless Labs, so we can see \
                    what breaks. Your backend address, host names, alert rules and API key are never \
                    sent, and cannot be — the app has no way to put them in a report.
                    """
                )
                .font(.footnote)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            } else {
                Text("This build has no diagnostics destination, so nothing is sent.")
                    .font(.footnote)
                    .foregroundStyle(.secondary)
            }

            DisclosureGroup("What gets sent") {
                VStack(alignment: .leading, spacing: 6) {
                    ForEach(Self.sent, id: \.self) { item in
                        Text("• \(item)").font(.footnote)
                    }
                    Divider().padding(.vertical, 2)
                    Text("Never sent:").font(.footnote.bold())
                    ForEach(Self.neverSent, id: \.self) { item in
                        Text("• \(item)").font(.footnote).foregroundStyle(.secondary)
                    }
                    if let diagnostics = model.usageDiagnostics(), diagnostics.enabled {
                        Divider().padding(.vertical, 2)
                        Text(
                            "This install: \(diagnostics.sent) reports delivered, "
                                + "\(diagnostics.spansReported) events."
                        )
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                    } else if let reason = model.usageDiagnostics()?.denial {
                        Divider().padding(.vertical, 2)
                        Text("Currently off because \(Self.describe(reason)).")
                            .font(.footnote)
                            .foregroundStyle(.secondary)
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
    }

    /// One line per `AttributeKey`, in the user's words.
    private static let sent = [
        "Pessimal's version and build number",
        "That this is a Mac, and the macOS version",
        "Which kind of backend you use — SigNoz, Honeycomb — but never its address",
        "How many hosts, alert rules and firing alerts there are",
        "Whether each poll succeeded, and if not, what kind of failure it was",
        "Whether the backend answered 2xx, 4xx or 5xx",
        "How long a poll took",
        "A random identifier for this run of the app, discarded when it quits",
    ]

    private static let neverSent = [
        "Your backend's address",
        "Your API key",
        "Your host names",
        "Your alert rules or their thresholds",
        "Any error message from your backend",
        "Any metric value from your fleet",
        "Anything that identifies you or your Mac across launches",
    ]

    private static func describe(_ reason: UsageDenialReasonRecord) -> String {
        switch reason {
        case .optedOut: return "you turned it off"
        case .doNotTrack: return "DO_NOT_TRACK is set"
        case .continuousIntegration: return "this is running under CI"
        case .noDestination: return "this build has no destination"
        }
    }
}
