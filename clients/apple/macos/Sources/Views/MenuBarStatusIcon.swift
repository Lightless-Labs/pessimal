//
//  MenuBarStatusIcon.swift
//  Pessimal — macOS menu bar client
//
//  The one glyph in the menu bar, and the rule that picks it.
//
//  The rule is written as a pure function over four already-decided values so that it can be read
//  in one screenful and argued with. Its precedence is the only opinion in this file, and it is
//  the app's central promise: **an icon that has stopped tracking the fleet never looks like an
//  icon that is tracking a healthy one.**
//

import SwiftUI

/// What the menu bar shows: one symbol, and the words for the people who cannot see it.
struct MenuBarStatusIcon: Equatable {
    let symbolName: String
    let accessibilityLabel: String

    /// Resolves the icon from the model's states.
    ///
    /// Precedence, strongest first:
    ///
    /// 1. **Not set up**, and **set up wrong** — there is no fleet to have a severity.
    /// 2. **Stopped**: core has said retrying cannot help. The clock is down; the severity on
    ///    screen is a photograph of whenever it stopped.
    /// 3. **Suspended**: polling is deliberately paused. Same problem, different cause.
    /// 4. The fleet's own severity, which is what the icon is *for*.
    ///
    /// Steps 2 and 3 outrank severity, which does mean a critical fleet behind a stopped poller
    /// shows the stopped glyph rather than the critical one. That trade is deliberate: neither
    /// glyph reads as "fine", and the alternative — a confident `Critical` that is quietly four
    /// hours old — is the failure `FleetModel` names in its own documentation. The dropdown says
    /// which of the two it is, in words, the moment the icon is clicked.
    static func resolve(
        session: FleetModel.SessionState,
        polling: FleetModel.PollingState,
        isSuspended: Bool,
        severity: SeverityRecord?
    ) -> MenuBarStatusIcon {
        switch session {
        case .unconfigured:
            return MenuBarStatusIcon(
                symbolName: "circle.dashed",
                accessibilityLabel: "Pessimal: not set up"
            )
        case .unusable:
            return MenuBarStatusIcon(
                symbolName: "exclamationmark.circle",
                accessibilityLabel: "Pessimal: settings cannot be used"
            )
        case .ready:
            break
        }

        if case .stopped = polling {
            return MenuBarStatusIcon(
                symbolName: "xmark.circle",
                accessibilityLabel: "Pessimal: polling stopped"
            )
        }

        if isSuspended {
            return MenuBarStatusIcon(
                symbolName: "pause.circle",
                accessibilityLabel: "Pessimal: polling paused"
            )
        }

        // A session with no view is a session that has not been adopted yet, which lasts for the
        // width of one `reloadSettings`. `Unknown` is core's own word for "nobody could judge
        // this", so borrowing it here says the true thing rather than a reassuring one.
        let severity = severity ?? .unknown
        return MenuBarStatusIcon(
            symbolName: MenuBarStyle.symbolName(for: severity),
            accessibilityLabel: "Pessimal: \(MenuBarStyle.name(for: severity))"
        )
    }
}

/// The status item's label.
///
/// Reads the model directly rather than through the environment: a `MenuBarExtra`'s label is built
/// outside the content's environment, so anything injected for the dropdown is not in scope here.
/// `@Observable` needs no property wrapper to track a read made inside `body`.
struct MenuBarLabelView: View {
    let model: FleetModel

    var body: some View {
        let icon = MenuBarStatusIcon.resolve(
            session: model.sessionState,
            polling: model.pollingState,
            isSuspended: model.isSuspended,
            severity: model.fleet?.severity
        )

        // `.template` is what makes the glyph follow the menu bar: black on a light bar, white on
        // a dark one, and correctly inverted while the bar is highlighted. A rendered-as-original
        // symbol would be invisible in one of those three states.
        Image(systemName: icon.symbolName)
            .renderingMode(.template)
            .accessibilityLabel(icon.accessibilityLabel)
    }
}
