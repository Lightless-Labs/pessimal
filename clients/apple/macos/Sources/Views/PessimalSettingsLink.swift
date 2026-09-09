//
//  PessimalSettingsLink.swift
//  Pessimal — macOS menu bar client
//
//  Opening the settings window, in the one way that works from a menu bar app.
//

import AppKit
import SwiftUI

/// A button that opens the app's `Settings` scene.
///
/// Wraps `SettingsLink` rather than sending an action selector by name. The selector approach —
/// `showSettingsWindow:`, `showPreferencesWindow:` before it — was renamed by Apple between macOS
/// releases and fails *silently* when it is wrong, which for the one control that repairs a broken
/// configuration is the worst possible failure. `SettingsLink` is the supported route from macOS
/// 14, and it is wired to the scene by the framework rather than by a string.
///
/// The gesture is the other half. `LSUIElement` is true, so Pessimal is not an active application
/// and the window it opens can arrive behind whatever the user was looking at. Activating first
/// puts it in front. `simultaneousGesture` rather than an action, because `SettingsLink` owns its
/// own tap and replacing it would replace the thing that makes the link work.
struct PessimalSettingsLink<Label: View>: View {
    @ViewBuilder let label: Label

    var body: some View {
        SettingsLink {
            label
        }
        .simultaneousGesture(
            TapGesture().onEnded {
                NSApp.activate(ignoringOtherApps: true)
            }
        )
    }
}

extension PessimalSettingsLink where Label == Text {
    /// The plain-text form, for menu-shaped rows.
    init(_ title: String) {
        self.init { Text(title) }
    }
}
