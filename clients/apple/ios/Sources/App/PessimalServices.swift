//
//  PessimalServices.swift
//  Pessimal — iOS client
//
//  One object graph, built once. Everything the app can reach lives here, so that a preview or a test
//  can substitute the whole of it by passing different stores, and so that the answer to "where does
//  the real Keychain get touched" is a single line rather than a search.
//
//  Deliberately the same shape as the macOS app's composition root, including the three-step
//  `refreshNow()`. The one real difference is the lifecycle: a Mac app is told it is about to quit, an
//  iOS app is not.
//

import Foundation
import Observation
#if canImport(UIKit)
    import UIKit
#endif
#if canImport(PessimalKit)
    import PessimalKit
#endif

/// The app's composition root: the platform stores, the adapter over them, and the one model the
/// views read.
@MainActor
@Observable
final class PessimalServices {
    /// The fleet, as the views see it. Owns the poll clock and the `FleetSession`.
    @ObservationIgnored let fleet: FleetModel

    /// The store adapter, kept reachable because the setup screen shows the three problems it reports
    /// and no other type knows about them.
    ///
    /// `PessimalKit.FleetStoreBridge`, shared with the macOS app rather than reimplemented here: it
    /// reads `UserDefaults`, the Keychain and a file, and neither app wants a different answer from
    /// any of the three. Its one genuinely dangerous rule — never overwrite an API key that could not
    /// be read — is not a rule worth having two copies of.
    @ObservationIgnored let stores: FleetStoreBridge

    /// - Parameter platform: `.live` in the app; `PlatformStores.inMemory(...)` in a preview, which is
    ///   what keeps a preview from reading — or overwriting — a real API key.
    init(platform: PlatformStores = .live) {
        let bridge = FleetStoreBridge(stores: platform)
        stores = bridge
        fleet = FleetModel(
            settings: bridge,
            stateCache: bridge,
            // Its own store, read afresh at every session rebuild, and deliberately not one of the
            // values `SettingsStore.removeAll()` clears — "reset connection" must not revoke an
            // opt-out. See `UsageConsentStore`.
            usageConsent: platform.usageConsent,
            // The model's own wake handling is an `NSWorkspace` notification, which does not exist
            // here — what it stands in for on iOS is the scene becoming active, and only the app
            // layer sees that. Said explicitly rather than left to the `#if` inside the model:
            // `FleetModel` documents that two owners of one behaviour is the thing to avoid, and this
            // app is the owner.
            observesSystemWake: false
        )
    }

    /// Starts polling. Called from `App.init()`, not when a view first appears.
    ///
    /// An app whose fleet is only watched while somebody is looking at it has nothing to say at the
    /// moment they look — and on iOS "looking at it" is the only state there is, so the gap between
    /// launch and first data is the whole of the user's first impression.
    func start() {
        fleet.start()
    }

    /// What pull-to-refresh does.
    ///
    /// Three steps rather than one because "refresh" means different things in the three states this
    /// app can be in, and the user pulling the list down does not know which one they are in:
    ///
    /// 1. A Keychain that was locked at launch may be open now, so an unresolved key is re-read.
    /// 2. Settings may have been saved on the screen they just came back from; `reloadSettings()` is a
    ///    no-op when nothing changed, so this is free when nothing did.
    /// 3. `refresh()` polls, or joins the poll already in flight — including from a stopped poller,
    ///    which is exactly how someone who has just fixed an API key finds out.
    ///
    /// Core may answer that the poll was *skipped* because one was already running. That is "nothing
    /// to do", not a failure, and it is handled inside `FleetModel`: nothing is surfaced here, because
    /// there is nothing for the user to do about it.
    func refreshNow() async {
        stores.retryUnresolvedCredentials()
        fleet.reloadSettings()
        await fleet.refresh()
    }

    /// The app is going off screen: stop the clock, then write the state down.
    ///
    /// This is the iOS replacement for the macOS app's `willTerminate` observer, and it runs at a
    /// different moment for a good reason — **iOS never promises to tell you about termination.** A
    /// suspended app can be killed to reclaim memory with no further callback, so the last safe
    /// moment to persist is the one we are in now. Backgrounding happens often and the write is small;
    /// paying for it every time is the only way the history survives a kill.
    ///
    /// Suspending first, persisting second: cancelling the armed timer means nothing new starts
    /// during the few seconds iOS gives us here. A poll already in flight is left alone — `suspend()`
    /// does not cancel it, and the state it folds will be written by the next background transition.
    func enterBackground() {
        fleet.suspend()
        fleet.persistState()
        flushUsageReporting()
    }

    /// Sends the buffered usage batch before the process is suspended.
    ///
    /// Wrapped in `beginBackgroundTask` because that is the whole reason this is not fire-and-forget:
    /// a bare `Task` started as the app is suspended is very likely never scheduled, so the batch
    /// would sit in memory until the next launch rebuilt the session and dropped it. The expiry
    /// handler ends the assertion whatever happens — an unbalanced `beginBackgroundTask` is a
    /// watchdog termination, which would be a far worse bug than a lost batch.
    ///
    /// Nothing is awaited by the caller and nothing is reported. Diagnostics that could not be
    /// delivered are not an event the user needs to hear about, and `enterBackground()` has a few
    /// seconds of wall clock to spend, not a result to act on.
    private func flushUsageReporting() {
        #if canImport(UIKit)
            var identifier = UIBackgroundTaskIdentifier.invalid
            identifier = UIApplication.shared.beginBackgroundTask(withName: "pessimal.usage.flush") {
                // Expired: iOS wants the assertion back now. Ending it is mandatory; the flush below
                // is simply abandoned.
                if identifier != .invalid {
                    UIApplication.shared.endBackgroundTask(identifier)
                    identifier = .invalid
                }
            }
            guard identifier != .invalid else { return }

            Task {
                // A ceiling of our own, well inside the ~30s iOS grants, because the sink's own
                // request timeout is 10s and a hung DNS lookup should not hold an assertion open.
                await withTaskGroup(of: Void.self) { group in
                    group.addTask { await self.fleet.flushUsageReporting() }
                    group.addTask { try? await Task.sleep(for: .seconds(3)) }
                    await group.next()
                    group.cancelAll()
                }
                UIApplication.shared.endBackgroundTask(identifier)
                identifier = .invalid
            }
        #endif
    }

    /// The app is on screen again: settle anything that could not be settled while it was not, then
    /// resume core's schedule.
    ///
    /// The resume is the obvious part, and not a new interval: `FleetModel.resume()` picks up the
    /// deadline it had, so a phone in a pocket overnight polls immediately and one glanced at twice in
    /// a minute waits out the remainder. Both are core's cadence.
    ///
    /// The two calls before it are the load-bearing part, and they are here because of something the
    /// macOS app never meets. **iOS starts processes before the device is unlocked.** The system
    /// prewarms an app, `App.init()` runs, `start()` reads the Keychain — and on a locked device that
    /// read fails with "keychain unavailable", which the platform layer deliberately leaves
    /// *unresolved* because it is the failure that repairs itself. The scene then connects straight to
    /// `.active` with no `.background` before it, so `resume()` alone returns at its own guard and the
    /// app sits there telling a user with a perfectly good key that it could not be read.
    ///
    /// So opening the app counts as the explicit gesture the bridge asks for before re-prompting. Both
    /// calls cost nothing in the steady state: the retry returns immediately unless a problem is
    /// outstanding, and `reloadSettings()` is documented as a no-op when neither the connection nor the
    /// configuration has changed. The retry goes first, because the settings reload is what turns a key
    /// that has become readable into a live session.
    func enterForeground() {
        stores.retryUnresolvedCredentials()
        fleet.reloadSettings()
        fleet.resume()
    }
}
