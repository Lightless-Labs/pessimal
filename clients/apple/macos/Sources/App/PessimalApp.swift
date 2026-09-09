//
//  PessimalApp.swift
//  Pessimal — macOS menu bar client
//
//  The whole application: one status item and one settings window.
//
//  `LSUIElement` is true in the Info.plist, so there is no Dock icon, no app switcher entry and no
//  main window — the menu bar icon is the entire user interface, which is why it is started at
//  launch rather than when somebody first clicks it.
//

import SwiftUI

@main
struct PessimalApp: App {
    /// Built once, for the life of the process. `@State` rather than a plain `let` so SwiftUI owns
    /// the storage and the object survives every re-evaluation of `body`.
    @State private var services: PessimalServices

    init() {
        // `App.init()` runs on the main thread before the run loop starts. `assumeIsolated`
        // asserts that rather than assuming it, and compiles whether or not the SDK in use
        // annotates `App` as main-actor isolated.
        let services = MainActor.assumeIsolated { PessimalServices() }
        _services = State(initialValue: services)

        // Polling starts now, not when the dropdown first opens. An app whose fleet is only
        // watched while somebody is looking at it has nothing to say at the moment they look.
        MainActor.assumeIsolated { services.start() }
    }

    var body: some Scene {
        MenuBarExtra {
            FleetMenuView(services: services)
                .environment(services.fleet)
                .environment(services.stores)
        } label: {
            // The label is built outside the content's environment, so the model is handed to it
            // directly. See ``MenuBarLabelView``.
            MenuBarLabelView(model: services.fleet)
        }
        // `.window`, not `.menu`: the dropdown is a real SwiftUI view — a banner, a scrolling host
        // list, progress — and `.menu` would flatten all of it into menu items.
        .menuBarExtraStyle(.window)

        // Declared here because `SettingsLink` — the only supported way to open this window from a
        // menu bar app — does nothing at all when no `Settings` scene exists, and does it
        // silently. `SettingsView` takes its two collaborators by hand rather than through the
        // environment, so that a window which cannot be built is a compile error rather than a
        // crash the first time somebody opens it.
        Settings {
            SettingsView(model: services.fleet, connectionStore: services.stores)
        }
    }
}
