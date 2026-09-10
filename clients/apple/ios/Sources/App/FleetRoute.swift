//
//  FleetRoute.swift
//  Pessimal — iOS client
//
//  Everywhere the fleet list can send you, as one value type.
//

import Foundation

/// A destination inside the app's one `NavigationStack`.
///
/// A typed route rather than `NavigationLink(destination:)` at the call site, because the views that
/// offer "Open Settings" — the setup screen, and the freshness banner when core says the failure is
/// actionable — must not have to know how to *build* the settings screen. They know only that the
/// user wants to go there. The App is the one place that owns the mapping, which is also the one
/// place that owns the object graph the destination needs.
///
/// The macOS app has no equivalent because `SettingsLink` and a `Settings` scene do this for it. On
/// iOS there is one window and one stack, so the seam has to be drawn by hand.
///
enum FleetRoute: Hashable {
    /// The connection, environment and alert rule editor.
    case settings

    /// One host, in full.
    ///
    /// Carries core's host id and not the `HostViewRecord` the row was built from. A pushed screen
    /// outlives the poll that produced it — that is the whole point of pushing it — and a captured
    /// record would go on displaying the fleet as it was at the moment of the tap while the list
    /// behind it moved on. `HostDetailView` makes the same choice for the same reason, and looks the
    /// host up by this id on every pass.
    case host(id: String)
}
