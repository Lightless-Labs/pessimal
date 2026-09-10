//
//  HostDetailAlertRow.swift
//  Pessimal — iOS client
//
//  One alert row: a rule against this host, with the evidence its phase was judged on.
//
//  Every verdict here is core's. The phase, the severity, the value the phase was judged on, the
//  instant the breach began, the instant the rule fires, whether the stored rule is broken — all of
//  it arrives decided. This row shows it, and the one thing it is allowed to decide is which
//  sentence goes where.
//

#if canImport(PessimalFFI)
    import PessimalFFI
#endif
import SwiftUI

/// One row of the host detail screen's alert list.
struct HostDetailAlertRow: View {

    let alert: AlertViewRecord

    /// The unit core gave this metric on this host, when one of its series is on screen to say.
    ///
    /// `AlertViewRecord` carries a `MetricKindRecord` but no unit, and no exported function maps a
    /// kind to its `MetricFacts`. So the unit is *looked up* from a `MetricViewRecord` core produced
    /// for the same kind on the same host — a fact core stated, not a mapping reinvented here — and
    /// is `nil` when this host reports no series of that kind, in which case the value prints bare.
    /// A kind-to-unit table written in Swift would be a second copy of core's, free to drift.
    let unit: MetricUnitRecord?

    let now: Date

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Label {
                    Text(FleetStyle.name(for: alert.phase))
                        .font(.subheadline.weight(.medium))
                } icon: {
                    Image(systemName: FleetStyle.symbolName(for: alert.phase))
                }
                // Tinted from the record's `severity`, not from the phase: severity is the field
                // core folds for exactly this purpose, and a colour chosen from the phase here
                // would be a second opinion about how much a firing rule matters.
                .foregroundStyle(FleetStyle.tint(for: alert.severity))
                .font(.subheadline)

                Spacer(minLength: 8)

                Text(alert.ruleName)
                    .font(.subheadline)
                    .multilineTextAlignment(.trailing)
            }

            // Core's `AlertRule::describe()`. Shown, and parsed on neither side of the boundary.
            Text(alert.description)
                .font(.caption)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)

            if let evidence = evidenceLine {
                Text(evidence)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }

            if let timing = timingLine {
                Text(timing)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }

            if alert.isRate {
                // Core's contract, verbatim in substance: the threshold is compared against the
                // normalised rate, not against the cumulative total the backend returns. Saying so
                // is the difference between a threshold that looks absurd and one that is read
                // correctly.
                Text("Judged against the normalised rate, not the cumulative total.")
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }

            if let invalidReason = alert.invalidReason {
                // Core shows such a rule and does **not** evaluate it: a `NaN` threshold makes every
                // comparator return false, so without this badge the rule sits in the list silently
                // never firing. This label is the only way an operator ever learns it is broken.
                Label {
                    Text("This rule is not being evaluated. \(invalidReason)")
                        .font(.caption)
                        .fixedSize(horizontal: false, vertical: true)
                } icon: {
                    Image(systemName: "exclamationmark.octagon.fill")
                }
                .font(.caption)
                .foregroundStyle(.red)
                .textSelection(.enabled)
            }
        }
        .accessibilityElement(children: .combine)
    }

    /// What the phase was judged on: the reduced value, the threshold, and which series held it.
    private var evidenceLine: String? {
        var parts: [String] = []

        if let latest = alert.latestValue {
            parts.append("Latest \(format(latest))")
        } else {
            // `nil` is not zero. A rule with nothing to judge on is why `noData` exists as a phase.
            parts.append("No value to judge")
        }

        parts.append("threshold \(format(alert.threshold))")

        if let seriesLabel = alert.seriesLabel {
            // Which mount or interface held it. Core names the series; this row does not guess.
            parts.append("on \(seriesLabel)")
        }

        if case let .frozen(sinceMillis) = alert.evidence {
            // The query for this metric is failing and core is holding the phase rather than
            // re-judging it. Without this the row would look like a live verdict.
            parts.append(
                "held since \(FleetFormat.age(sinceMillis: sinceMillis, at: now)) — this metric's query is failing"
            )
        }

        return parts.joined(separator: " · ")
    }

    /// When the breach started, and when the rule fires or fired.
    ///
    /// `breachingSinceMillis` is the start of the **breach**, not of the alert, and core says so
    /// explicitly. `firesAtMillis` is `breaching_since + for_duration` and is already in the past
    /// once the phase is `Firing`, which is why it is phrased relatively rather than as a countdown.
    private var timingLine: String? {
        var parts: [String] = []

        if let breachingSince = alert.breachingSinceMillis {
            parts.append("Breaching since \(FleetFormat.age(sinceMillis: breachingSince, at: now))")
        }

        if let firesAt = alert.firesAtMillis {
            parts.append("fires \(FleetFormat.when(millis: firesAt, at: now))")
        }

        return parts.isEmpty ? nil : parts.joined(separator: " · ")
    }

    /// A value in core's unit when a series on this host supplied one, and bare when none did.
    private func format(_ value: Double) -> String {
        guard let unit else { return FleetFormat.unitlessValue(value) }
        return FleetFormat.value(value, unit: unit, isRate: alert.isRate)
    }
}
