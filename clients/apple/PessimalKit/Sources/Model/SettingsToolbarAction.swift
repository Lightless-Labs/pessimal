//
//  SettingsToolbarAction.swift
//  PessimalKit
//
//  Which of the settings screen's two actions its toolbar offers, and whether it may be pressed.
//

import Foundation

/// The one action the iOS settings toolbar shows.
///
/// The screen has two: Test Connection, in the backend section, and Save, in the last section below
/// the alert rules. A successful test is the loudest thing on that screen and it is not what makes
/// the settings take effect, so a reader can leave believing they are done. The toolbar carries
/// whichever of the two is the outstanding step, at the top of the screen where the test result is
/// read.
///
/// It lives in PessimalKit rather than beside the screen for one reason: `scripts/swift-smoke.sh`
/// compiles the bindings and PessimalKit's sources and nothing else, so this is the only place an
/// automated test can reach Swift that the apps run. `LoginItem.swift` is here for the same reason
/// and is macOS-only.
///
/// The macOS settings window has the same two actions and does not use this. Its Save carries
/// `.keyboardShortcut(.defaultAction)`, so Return commits from anywhere in the window — an
/// affordance iOS has no equivalent of — and a `Settings {}` scene has no navigation bar to put a
/// trailing item in, so the same idea there would mean a different container and a different
/// change.
public enum SettingsToolbarAction: Equatable, Sendable {
    /// Offer the probe. The values in the fields have not reached the backend.
    case test(enabled: Bool)

    /// Offer the commit. The backend answered for the values in the fields.
    case save(enabled: Bool)

    /// Which action is outstanding, from state the screen already holds.
    ///
    /// Nothing here is stored. "Has this been tested?" has one answer on the screen — the probe's
    /// state — and a second copy of it would be a second thing that can disagree with it.
    ///
    /// - Parameters:
    ///   - hasUnsavedChanges: the draft differs from what was last saved.
    ///   - backendAnswered: a probe completed for the values now in the fields, and reached the
    ///     backend. Reaching it is the test the toolbar cares about, not liking what came back: a
    ///     listing that errored and a backend with no hosts in it yet are both configurations worth
    ///     saving, and neither is fixed by testing again.
    ///   - canSave: the Save button's own enablement, passed in rather than reproduced here.
    ///   - canTest: the Test button's own enablement, likewise.
    public static func next(
        hasUnsavedChanges: Bool,
        backendAnswered: Bool,
        canSave: Bool,
        canTest: Bool
    ) -> SettingsToolbarAction {
        // Nothing unsaved: the sequence is over and the toolbar says so by going grey on the step it
        // finished. This is the line that matters after a Save. `probeState` survives a save unless
        // the connection changed, so a rule that looked only at the probe would leave the toolbar
        // offering to test settings that are already in force — an invitation to do something with
        // no effect, on a screen whose whole bug was an action that looked like it had one.
        guard hasUnsavedChanges else { return .save(enabled: false) }

        if backendAnswered { return .save(enabled: canSave) }

        return .test(enabled: canTest)
    }
}
