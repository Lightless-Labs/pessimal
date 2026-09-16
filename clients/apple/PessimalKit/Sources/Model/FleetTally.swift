//
//  FleetTally.swift
//  PessimalKit
//
//  Which of core's fleet counts the summary shows, and what each one is called.
//

import Foundation
#if canImport(PessimalFFI)
    // Under Bazel this is a real separate module, so `FleetCountsRecord` is not in scope without it.
    // The macOS script compiles everything into one module and would not have caught a missing
    // import.
    import PessimalFFI
#endif

/// One of the seven numbers in a fleet summary.
///
/// The counts themselves stay core's: `FleetCounts` computes all seven in one pass over the same
/// host records the list beneath the summary is built from, so the header and the list cannot
/// disagree. This type only decides **which of them are worth showing**, and what the word beside a
/// number is. Both are presentation, and both must be identical in the two apps — hence one copy
/// here rather than one per app.
public enum FleetTally: String, CaseIterable, Sendable {
    case hosts
    case alive
    case stale
    case down
    case unknown
    case firingAlerts
    case pendingAlerts

    /// Shown even at zero.
    ///
    /// The total and the number of alive hosts are the two a person reads first, and a summary that
    /// can shrink to nothing says nothing. Everything else earns its place by being non-zero: on a
    /// healthy fleet the row is "12 hosts · 12 alive" rather than five numbers of which three are 0.
    public var isAlwaysShown: Bool {
        switch self {
        case .hosts, .alive: return true
        case .stale, .down, .unknown, .firingAlerts, .pendingAlerts: return false
        }
    }

    /// Whether this tally counts alerts rather than hosts.
    ///
    /// Exhaustive on purpose: the macOS summary puts the two groups on separate lines, and deriving
    /// that split from this switch is what makes an eighth count a compile error there rather than a
    /// number that silently never appears.
    public var isAlertTally: Bool {
        switch self {
        case .firingAlerts, .pendingAlerts: return true
        case .hosts, .alive, .stale, .down, .unknown: return false
        }
    }

    /// The word beside the number, singular where the count is 1.
    ///
    /// Here rather than in each app's style file because the plural rule is exactly the decision
    /// that rots when copied: both apps read "1 hosts" before this existed.
    public func word(for value: UInt32) -> String {
        switch self {
        case .hosts: return value == 1 ? "host" : "hosts"
        case .alive: return "alive"
        case .stale: return "stale"
        case .down: return "down"
        case .unknown: return "unknown"
        case .firingAlerts: return "firing"
        case .pendingAlerts: return "pending"
        }
    }

    /// This tally's value in a set of counts.
    public func value(in counts: FleetCountsRecord) -> UInt32 {
        switch self {
        case .hosts: return counts.hosts
        case .alive: return counts.alive
        case .stale: return counts.stale
        case .down: return counts.down
        case .unknown: return counts.unknown
        case .firingAlerts: return counts.firingAlerts
        case .pendingAlerts: return counts.pendingAlerts
        }
    }

    /// What the summary shows, in a fixed order, with the value beside each.
    ///
    /// Order never changes, and every entry carries its own word, so a number is never read by its
    /// position — which is what makes hiding the empty ones safe.
    public static func visible(in counts: FleetCountsRecord) -> [(tally: FleetTally, value: UInt32)] {
        allCases.compactMap { tally in
            let value = tally.value(in: counts)
            guard tally.isAlwaysShown || value > 0 else { return nil }
            return (tally, value)
        }
    }

    /// Every count, spoken, whether or not it is shown.
    ///
    /// Hiding a zero hides its cell from VoiceOver too, and unlike a sighted reader a screen reader
    /// user cannot see the gap where "0 down" would have been. So the container says all seven and
    /// the visible cells stay silent.
    public static func spokenSummary(of counts: FleetCountsRecord) -> String {
        allCases
            .map { "\($0.value(in: counts)) \($0.word(for: $0.value(in: counts)))" }
            .joined(separator: ", ")
    }
}
