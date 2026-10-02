//
//  WidgetLayout.swift
//  PessimalKit
//
//  How much a home screen widget shows, given its size and the size of the fleet, and which page of
//  it is on screen. Pure, so the thresholds are tested rather than scattered across views.
//

import Foundation

/// The three home screen sizes the widget offers. WidgetKit's own family type is not used here so
/// this stays testable without WidgetKit, and so the lock screen families — which never page and
/// have their own layouts — cannot be passed by mistake.
public enum WidgetSize: Sendable, CaseIterable {
    case square
    case wide
    case large
}

/// What a fleet widget's first page shows.
///
/// The owner's rule (docs/plans/2026-10-02-m11-ios-widgets.md): a fleet of two has room to be shown
/// in full, a fleet of thirty has room for a summary and the hosts that need attention. The limits
/// are the rows that fit at the default text size.
public enum WidgetDensity: Equatable, Sendable {
    /// One host, in as much detail as the size allows.
    case singleHost
    /// One row per host, up to `limit`.
    case rows(limit: Int)
    /// The tallies alone.
    case summary
    /// The tallies, then the first `limit` hosts in core's order — which puts degraded hosts first,
    /// so the first few in a large fleet are the ones worth a glance.
    case summaryAndRows(limit: Int)

    public static func choose(size: WidgetSize, hostCount: Int) -> WidgetDensity {
        // No hosts is a summary that says so — "0 hosts · 0 alive" — rather than an empty frame.
        guard hostCount > 0 else { return .summary }
        if hostCount == 1 { return .singleHost }
        switch size {
        case .square:
            return hostCount <= 4 ? .rows(limit: 4) : .summary
        case .wide:
            return hostCount <= 4 ? .rows(limit: 4) : .summaryAndRows(limit: 3)
        case .large:
            return hostCount <= 8 ? .rows(limit: 8) : .summaryAndRows(limit: 6)
        }
    }
}

/// One screen of a paging widget.
public enum WidgetPage: Equatable, Sendable {
    /// The first page: whatever ``WidgetDensity`` chose.
    case start
    /// One host in full, by its position in core's order.
    case host(index: Int)
}

/// Back and forth through the hosts of a fleet widget, one page each.
///
/// Only the square and the wide widget page, and only when there is more than one host: the large
/// one has room for the list, and a fleet of one already shows that host. The pages do not wrap —
/// back and forth, and a way home, is the whole of the interaction.
public enum WidgetPaging {
    public static func pages(size: WidgetSize, hostCount: Int) -> Int {
        pages(applyTo: size, hostCount: hostCount) ? hostCount + 1 : 1
    }

    /// The page an index lands on, clamped into range. The index is saved between refreshes and the
    /// fleet can shrink in the meantime, so an index past the end shows the last host rather than
    /// nothing.
    public static func page(at index: Int, size: WidgetSize, hostCount: Int) -> WidgetPage {
        let count = pages(size: size, hostCount: hostCount)
        let clamped = min(max(index, 0), count - 1)
        return clamped == 0 ? .start : .host(index: clamped - 1)
    }

    public static func next(after index: Int, size: WidgetSize, hostCount: Int) -> Int {
        min(index + 1, pages(size: size, hostCount: hostCount) - 1)
    }

    public static func previous(before index: Int) -> Int {
        max(index - 1, 0)
    }

    private static func pages(applyTo size: WidgetSize, hostCount: Int) -> Bool {
        size != .large && hostCount > 1
    }
}
