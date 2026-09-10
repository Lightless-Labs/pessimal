//
//  PessimalApp.swift
//  Pessimal — iOS client
//
//  The whole application: one window, one navigation stack, one fleet.
//
//  The macOS app is a status item that is always there; this one is a screen the operating system
//  freezes the moment it is put away. That single difference is what this file is about: the poll
//  clock has to be handed back to the OS on the way out and taken up again on the way in, and nobody
//  but the app layer can see either event happen.
//

#if canImport(PessimalKit)
    import PessimalKit
#endif
import SwiftUI

@main
struct PessimalApp: App {
    /// Built once, for the life of the process. `@State` rather than a plain `let` so SwiftUI owns the
    /// storage and the object survives every re-evaluation of `body`.
    @State private var services: PessimalServices

    init() {
        // `App.init()` runs on the main thread before the run loop starts. `assumeIsolated` asserts
        // that rather than assuming it, and compiles whether or not the SDK in use annotates `App` as
        // main-actor isolated.
        let services = MainActor.assumeIsolated { PessimalServices() }
        _services = State(initialValue: services)

        // Polling starts now, not when the first view appears. The fleet list is the product: if the
        // first poll waited for `onAppear`, the screen would have to draw itself before it had
        // anything to draw, every single launch.
        MainActor.assumeIsolated { services.start() }
    }

    var body: some Scene {
        WindowGroup {
            PessimalRootView(services: services)
        }
    }
}

/// The scene's root: the navigation stack, the environment, and the lifecycle wiring.
///
/// A view rather than code inside `PessimalApp.body` because `scenePhase` has to be *observed*, and
/// `onChange` is a view modifier. Reading the phase here — inside the window, where the environment
/// value is scoped to the scene the user is actually looking at — is also the reading that stays
/// correct if this app ever gains a second scene.
struct PessimalRootView: View {
    let services: PessimalServices

    /// Whether the app is on screen, per the OS. The only signal that distinguishes "the user put this
    /// away" from "the user is still here": an iOS app is suspended by the system, not by the machine
    /// going to sleep, and `FleetModel` deliberately does not watch for it.
    @Environment(\.scenePhase) private var scenePhase

    var body: some View {
        NavigationStack {
            FleetListView(services: services)
                // One table, here, rather than a `NavigationLink(destination:)` at each call site.
                // The two views that offer "Open Settings" — the setup screen and the freshness
                // banner — are nested deep inside the list and have no business knowing how to build
                // a settings screen; they know only where the user wants to go. Keeping the answer
                // here also keeps it next to the object graph the answer needs.
                .navigationDestination(for: FleetRoute.self) { route in
                    switch route {
                    case .settings:
                        // Takes its two collaborators by hand rather than through the environment, so
                        // a screen that cannot be built is a compile error rather than a crash the
                        // first time somebody opens it. Same signature as the macOS app's.
                        SettingsView(model: services.fleet, connectionStore: services.stores)

                    case let .host(id):
                        // By id, not by record: see ``FleetRoute/host(id:)``. The model reaches it
                        // through the environment below.
                        HostDetailView(hostId: id)
                    }
                }
        }
        // Handed to the whole stack, so a pushed screen reads the same model instance the list does.
        .environment(services.fleet)
        .environment(services.stores)
        .onChange(of: scenePhase) { _, phase in
            switch phase {
            case .active:
                services.enterForeground()

            case .background:
                // The last callback iOS guarantees. Everything that must outlive the process happens
                // here; see `PessimalServices.enterBackground()`.
                services.enterBackground()

            case .inactive:
                // Transitional and frequent — the app switcher, a notification banner, an incoming
                // call, the moment between `.background` and `.active`. Suspending here would stop
                // polling every time a banner appeared, and persisting here would write the same
                // bytes several times per backgrounding. Doing nothing is the correct response.
                break

            @unknown default:
                // A phase this build has never heard of. Leaving the clock running is the safer of
                // the two guesses: a poll too many costs one request, and a poll too few means an app
                // that has quietly stopped watching.
                break
            }
        }
    }
}
