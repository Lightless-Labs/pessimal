//
//  SettingsUsageReporting.swift
//  Pessimal — iOS client
//
//  The opt-out switch, and an honest list of what is sent.
//
//  The list is not marketing copy. It is the `pessimal_usage` attribute allowlist, item for item, and
//  it is spelled out here because "anonymous diagnostics" tells a user nothing they can act on. If the
//  allowlist grows, this list grows with it — `AttributeKey::ALL` on the Rust side is the source of
//  truth and a test there asserts nothing else can be emitted.
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

/// "Help improve Pessimal", the disclosure, and — when reporting is on — what it has managed to do.
struct SettingsUsageReportingSection: View {
    let model: FleetModel

    /// Mirrors the model's value. A `@State` rather than a binding straight onto the model because the
    /// setter there has a side effect — it rebuilds the session — and a `Toggle` bound to a property
    /// that rebuilds under it redraws unpredictably.
    @State private var enabled: Bool
    @State private var showingDetail = false

    init(model: FleetModel) {
        self.model = model
        _enabled = State(initialValue: model.usageReportingEnabled)
    }

    var body: some View {
        Section {
            Toggle("Share usage diagnostics", isOn: Binding(
                get: { enabled },
                set: { shareIt in
                    enabled = shareIt
                    // One call. The model writes the store and rebuilds the session, because consent
                    // is read when a reporter is built and there is no live sink to reconfigure.
                    model.usageReportingEnabled = shareIt
                }
            ))
            .disabled(!model.usageReportingConfigured)

            Button("What gets sent") { showingDetail = true }
                .font(.footnote)
        } header: {
            Text("Diagnostics")
        } footer: {
            footer
        }
        .sheet(isPresented: $showingDetail) {
            NavigationStack {
                UsageReportingDetailView()
            }
        }
    }

    @ViewBuilder
    private var footer: some View {
        if model.usageReportingConfigured {
            // Said plainly, including the part a user would reasonably want to know and that most
            // apps leave out: where it goes, and what never leaves.
            Text(
                """
                Sends counts and outcomes about Pessimal itself to Lightless Labs, so we can see \
                what breaks. Your backend address, host names, alert rules and API key are never \
                sent, and cannot be — the app has no way to put them in a report.
                """
            )
        } else {
            Text("This build has no diagnostics destination, so nothing is sent whatever this is set to.")
        }
    }
}

/// The allowlist, in full, plus what reporting has actually managed to do.
struct UsageReportingDetailView: View {
    @Environment(\.dismiss) private var dismiss
    @Environment(FleetModel.self) private var model

    var body: some View {
        List {
            Section("Sent") {
                ForEach(Self.sent, id: \.self) { item in
                    Text(item)
                }
            }
            Section("Never sent") {
                ForEach(Self.neverSent, id: \.self) { item in
                    Label(item, systemImage: "xmark.circle")
                        .foregroundStyle(.secondary)
                }
            }
            if let diagnostics = model.usageDiagnostics() {
                Section("This install") {
                    LabeledContent("Reports delivered", value: "\(diagnostics.sent)")
                    LabeledContent("Events in them", value: "\(diagnostics.spansReported)")
                    if diagnostics.failed > 0 {
                        LabeledContent("Could not be sent", value: "\(diagnostics.failed)")
                    }
                    if diagnostics.rejected > 0 {
                        LabeledContent("Refused", value: "\(diagnostics.rejected)")
                    }
                    if diagnostics.dropped > 0 {
                        LabeledContent("Discarded", value: "\(diagnostics.dropped)")
                    }
                    if !diagnostics.enabled, let reason = diagnostics.denial {
                        // The whole reason the denial crosses the FFI boundary: a user who switched
                        // this back on and still sees nothing needs to know which switch is winning.
                        LabeledContent("Off because", value: Self.describe(reason))
                    }
                }
            }
        }
        .navigationTitle("Diagnostics")
        .toolbar {
            ToolbarItem(placement: .confirmationAction) {
                Button("Done") { dismiss() }
            }
        }
    }

    /// The allowlist, in the user's words. One line per `AttributeKey`.
    private static let sent = [
        "Pessimal's version and build number",
        "Whether this is iPhone, iPad or Mac, and the iOS or macOS version",
        "Which kind of backend you use — SigNoz, Honeycomb — but never its address",
        "How many hosts, alert rules and firing alerts there are",
        "Whether each poll succeeded, and if not, what kind of failure it was",
        "Whether the backend answered 2xx, 4xx or 5xx",
        "How long a poll took",
        "A random identifier for this run of the app, discarded when it closes",
    ]

    private static let neverSent = [
        "Your backend's address",
        "Your API key",
        "Your host names",
        "Your alert rules or their thresholds",
        "Any error message from your backend",
        "Any metric value from your fleet",
        "Anything that identifies you or your device across launches",
    ]

    private static func describe(_ reason: UsageDenialReasonRecord) -> String {
        switch reason {
        case .optedOut:
            return "you turned it off"
        case .doNotTrack:
            return "DO_NOT_TRACK is set"
        case .continuousIntegration:
            return "running under CI"
        case .noDestination:
            return "this build has no destination"
        }
    }
}
