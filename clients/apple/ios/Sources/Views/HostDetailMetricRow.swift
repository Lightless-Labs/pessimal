//
//  HostDetailMetricRow.swift
//  Pessimal — iOS client
//
//  One series of one metric on one host.
//
//  A *series*, not a metric. Several of core's metrics arrive as more than one series per host —
//  a filesystem per mount point, network I/O per interface and direction — and each carries the
//  attributes that tell it apart. Collapsing them to one row would mean picking one mount at random
//  and labelling it with the metric's name, which is the confident wrong answer this app exists to
//  avoid. So every element of `HostViewRecord.metrics` gets its own row, in core's order.
//

#if canImport(PessimalFFI)
    import PessimalFFI
#endif
import SwiftUI

/// One row of the host detail screen's metric list.
struct HostDetailMetricRow: View {

    let metric: MetricViewRecord

    /// The shared tick. Passed in rather than read from `Date()` so every age on the screen agrees
    /// about the present.
    let now: Date

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                VStack(alignment: .leading, spacing: 1) {
                    Text(title)
                        .font(.body)

                    Text(FleetStyle.unitDescription(for: metric.unit, isRate: metric.isRate))
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }

                Spacer(minLength: 8)

                Text(FleetFormat.latestValue(of: metric))
                    .font(.body.monospacedDigit())
                    // Core is explicit that an `.unavailable` tile shows *frozen* values: they are
                    // real, they are not current, and greying them is how the difference is visible
                    // without reading the line underneath.
                    .foregroundStyle(isCurrent ? AnyShapeStyle(.primary) : AnyShapeStyle(.tertiary))
            }

            if !attributeSummary.isEmpty {
                // The full attribute set, not just core's shorthand `label`. On a detail screen the
                // device and filesystem type behind a mount point are the difference between
                // "which disk is full" and "a disk is full".
                Text(attributeSummary)
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                    .textSelection(.enabled)
            }

            if let explanation = FleetStyle.explanation(for: metric.availability) {
                Label {
                    Text("\(explanation). \(fetchedDetail)")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                } icon: {
                    Image(systemName: FleetStyle.symbolName(for: metric.availability))
                        .foregroundStyle(.secondary)
                }
                .font(.caption)
            }
        }
        .accessibilityElement(children: .combine)
    }

    /// True only for `present`. The other three availabilities all mean "this number is not current",
    /// and core's own documentation is the authority on which of them applies.
    private var isCurrent: Bool {
        metric.availability == .present
    }

    /// Core's display name, plus the mount or interface when there is one.
    ///
    /// Both halves come from core. Re-deriving the label by splitting `attributes` here would put a
    /// parser in the UI layer, and core's `label` is the string its sort order already agrees with.
    private var title: String {
        guard let label = metric.label else { return metric.displayName }
        return "\(metric.displayName) \(label)"
    }

    /// The series' attributes as `key value · key value`.
    ///
    /// In core's key order, untouched. Re-sorting would animate rows that did not change, and the
    /// order is one of the things `series_id` — and therefore this list's identity — is built from.
    private var attributeSummary: String {
        metric.attributes
            .map { "\($0.key) \($0.value)" }
            .joined(separator: " · ")
    }

    /// When a non-current value was last actually fetched.
    ///
    /// `fetchedAtMillis` is older than the fleet's `asOfMillis` while a metric is unavailable, and
    /// core says that gap is the entire point of the field: it is the timestamp a greyed row shows.
    /// `nil` is its own fact — never fetched at all — and is said rather than left blank.
    private var fetchedDetail: String {
        guard let fetchedAt = metric.fetchedAtMillis else { return "Never fetched." }
        return "Fetched \(FleetFormat.age(sinceMillis: fetchedAt, at: now))."
    }
}
