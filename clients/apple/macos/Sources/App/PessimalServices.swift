//
//  PessimalServices.swift
//  Pessimal — macOS menu bar client
//
//  One object graph, built once. Everything the app can reach lives here, so that a preview or a
//  test can substitute the whole of it by passing different stores, and so that the answer to
//  "where does the real Keychain get touched" is a single line rather than a search.
//

import AppKit
import Foundation
import Observation

/// The app's composition root: the platform stores, the adapter over them, and the one model the
/// views read.
@MainActor
@Observable
final class PessimalServices {
    /// The fleet, as the views see it. Owns the poll clock and the `FleetSession`.
    @ObservationIgnored let fleet: FleetModel

    /// The store adapter, kept reachable because the menu shows the three problems it reports and
    /// no other type knows about them.
    @ObservationIgnored let stores: FleetStoreBridge

    /// Registered so a quit saves the fleet state one last time. Held for `deinit`.
    @ObservationIgnored private var terminationObserver: (any NSObjectProtocol)?

    /// - Parameter platform: `.live` in the app; `PlatformStores.inMemory(...)` in a preview,
    ///   which is what keeps a preview from reading — or overwriting — a real API key.
    init(platform: PlatformStores = .live) {
        let bridge = FleetStoreBridge(stores: platform)
        stores = bridge
        fleet = FleetModel(settings: bridge, stateCache: bridge)
    }

    deinit {
        if let terminationObserver {
            NotificationCenter.default.removeObserver(terminationObserver)
        }
    }

    /// Starts polling and arranges for the last state to reach disk on quit.
    ///
    /// Called at launch rather than when the menu is first opened. The menu bar icon is the
    /// product: an app whose fleet only starts being watched once somebody clicks the icon has
    /// nothing to say at the moment it is clicked.
    func start() {
        observeTermination()
        fleet.start()
    }

    /// What the Refresh Now item does.
    ///
    /// Three steps rather than one because "refresh" means different things in the three states
    /// this app can be in, and the user pressing the button does not know which one they are in:
    ///
    /// 1. A Keychain that was locked at launch may be open now, so an unresolved key is re-read.
    /// 2. Settings may have been saved from another window; `reloadSettings()` is a no-op when
    ///    nothing changed, so this is free when nothing did.
    /// 3. `refresh()` polls, or joins the poll already in flight — including from a stopped
    ///    poller, which is exactly how someone who has just fixed an API key finds out.
    func refreshNow() async {
        stores.retryUnresolvedCredentials()
        fleet.reloadSettings()
        await fleet.refresh()
    }

    /// Saves and stops. Idempotent.
    func terminate() {
        fleet.persistState()
        fleet.teardown()
    }

    private func observeTermination() {
        guard terminationObserver == nil else { return }
        terminationObserver = NotificationCenter.default.addObserver(
            forName: NSApplication.willTerminateNotification,
            object: nil,
            queue: .main
        ) { [weak self] _ in
            // `willTerminate` is delivered on the main thread; the queue above pins it there. The
            // assumption is asserted rather than hoped for.
            MainActor.assumeIsolated {
                self?.terminate()
            }
        }
    }
}
