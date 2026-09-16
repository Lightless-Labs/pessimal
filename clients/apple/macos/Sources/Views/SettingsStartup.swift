//
//  SettingsStartup.swift
//  Pessimal — macOS menu bar client
//
//  "Open at login", and what to do when macOS wants the user to allow it.
//

#if canImport(PessimalKit)
    import PessimalKit
#endif
import ServiceManagement
import SwiftUI

/// The login item switch.
///
/// Per device, and deliberately not part of the settings that sync through iCloud: "open at login"
/// is a statement about this Mac, and a user with two Macs will not want the same answer on both.
/// The system holds the state; this view only reads and writes it.
struct SettingsStartupSection: View {
    let controller: any LoginItemController

    /// The last state read from the system. Re-read after every write and when the window comes
    /// back to the front, because the user can turn the app off in System Settings at any time.
    @State private var state: LoginItemState

    /// What macOS said when a registration failed. Shown rather than swallowed: the usual cause is
    /// a copy of the app that is not where it was registered from, and the user can fix that.
    @State private var failure: String?

    init(controller: any LoginItemController) {
        self.controller = controller
        _state = State(initialValue: controller.state)
    }

    var body: some View {
        Section("Startup") {
            Toggle("Open Pessimal at login", isOn: Binding(
                get: { state == .enabled || state == .requiresApproval },
                set: { wanted in
                    failure = nil
                    do {
                        try controller.setEnabled(wanted)
                    } catch {
                        failure = error.localizedDescription
                    }
                    // The system's answer, not the switch's: a registration that needs approval
                    // leaves the switch on and adds the note below.
                    state = controller.state
                }
            ))
            .disabled(isUnavailable)

            switch state {
            case .requiresApproval:
                note("macOS needs you to allow this in Login Items.") {
                    Button("Open Login Items") { controller.openSystemSettings() }
                }
            case let .unavailable(reason):
                note(reason, content: { EmptyView() })
            case .enabled, .disabled:
                EmptyView()
            }

            if let failure {
                note(failure, content: { EmptyView() })
            }
        }
        .onChange(of: state) { _, _ in }
        .task {
            // The window can be opened long after launch, and Login Items may have changed since.
            state = controller.state
        }
    }

    private var isUnavailable: Bool {
        if case .unavailable = state { return true }
        return false
    }

    @ViewBuilder
    private func note(_ text: String, @ViewBuilder content: () -> some View) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(text)
                .font(.footnote)
                .foregroundStyle(.secondary)
            content()
        }
    }
}

#Preview("Off") {
    Form { SettingsStartupSection(controller: InMemoryLoginItemController(state: .disabled)) }
        .formStyle(.grouped)
}

#Preview("Needs approval") {
    Form { SettingsStartupSection(controller: InMemoryLoginItemController(state: .requiresApproval)) }
        .formStyle(.grouped)
}
