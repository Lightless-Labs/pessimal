//
//  HostDetailVocabulary.swift
//  Pessimal — iOS client
//
//  The words and numbers the host detail screen needs and the fleet list did not.
//
//  Extensions on `FleetStyle` and `FleetFormat` rather than a second pair of types: there is one
//  vocabulary for this product, and two of them is how `Warning` comes to be an octagon on one screen
//  and a triangle on the next. Everything here is about records the fleet list never renders — an
//  alert's phase, a metric's unit as a noun, an instant that may be in the future — so it belongs
//  beside that vocabulary rather than inside it.
//

import Foundation
#if canImport(PessimalFFI)
    import PessimalFFI
#endif
import SwiftUI

extension FleetStyle {

    // MARK: - Alert phase

    /// The word for an alert's phase.
    ///
    /// `noData` is "No data" rather than "Unknown": core sorts it *above* `Ok` in the alert list on the
    /// grounds that an unjudged rule is worth looking at, and a word that sounded like an absence of
    /// news would hide that.
    static func name(for phase: AlertPhaseRecord) -> String {
        switch phase {
        case .ok: return "OK"
        case .pending: return "Pending"
        case .firing: return "Firing"
        case .noData: return "No data"
        }
    }

    /// A glyph for an alert's phase.
    ///
    /// Differs in shape and not only in fill, for the same reason the severity symbols do: the tint
    /// beside it comes from `severity`, and a phase that is only distinguishable by colour is a phase
    /// that vanishes in a screenshot.
    static func symbolName(for phase: AlertPhaseRecord) -> String {
        switch phase {
        case .ok: return "bell.slash"
        case .pending: return "bell.badge"
        case .firing: return "bell.fill"
        case .noData: return "bell.slash.circle"
        }
    }

    // MARK: - Availability

    /// A glyph for why a metric has no current value.
    ///
    /// The fleet list greys a chip and leaves the reason to the detail screen, which has room for a
    /// sentence. This is the icon that sentence hangs off.
    static func symbolName(for availability: MetricAvailabilityRecord) -> String {
        switch availability {
        case .present: return "checkmark.circle"
        case .notReported: return "minus.circle"
        case .unsupported: return "slash.circle"
        case .unavailable: return "exclamationmark.arrow.triangle.2.circlepath"
        }
    }

    // MARK: - Units

    /// What kind of number a metric's value is.
    ///
    /// Shown beside the value because the formatted value does not always carry its own unit: `45%` and
    /// `3.2 GB` speak for themselves, while `1.35` and `+3` do not.
    ///
    /// The rate wording is chosen from the **unit** and not from `isRate` alone, because core's two rate
    /// metrics are normalised differently and share the flag: `NetworkIo` becomes bytes *per second*,
    /// while `AgentCollectionFailures` becomes the count added *since the previous sample* (core's
    /// `normalize::delta` — "per bucket, not per second"). A blanket "per second" here would put a unit
    /// on the second one that it does not have, which is the same mistake `FleetFormat.value` exists to
    /// avoid and has to be avoided twice because the noun and the number are written separately.
    static func unitDescription(for unit: MetricUnitRecord, isRate: Bool) -> String {
        switch unit {
        case .ratio:
            return "ratio, shown as a percentage"
        case .bytes:
            return isRate ? "bytes per second" : "bytes"
        case .seconds:
            return "seconds"
        case .count:
            return isRate ? "count added since the previous sample" : "count"
        case .load:
            return "load average, a run-queue depth"
        }
    }
}

extension FleetFormat {

    /// When an instant is, in either direction — "in 2 min" for a deadline, "2 min ago" for something
    /// past.
    ///
    /// Distinct from `age(sinceMillis:at:)` by intent rather than by arithmetic: an alert's
    /// `firesAtMillis` is `breaching_since + for_duration` and core says it is already in the past once
    /// the phase is `Firing`, so the one field on this screen that legitimately points forwards needs a
    /// name that does not claim otherwise.
    static func when(millis: Int64, at now: Date) -> String {
        age(sinceMillis: millis, at: now)
    }

    /// A value for which core has not told us a unit.
    ///
    /// Reached for an alert's value or threshold when no series of that metric is on screen to supply
    /// one. Printing the bare number is the honest outcome: a unit guessed from the metric kind in
    /// Swift would be a second copy of core's `MetricFacts` table, free to disagree with it.
    static func unitlessValue(_ value: Double) -> String {
        guard value.isFinite else { return noValue }
        return value.formatted(.number)
    }
}
