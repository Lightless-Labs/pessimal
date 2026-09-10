//
//  HostRowView.swift
//  Pessimal — iOS client
//
//  One host, as the core folded it. Everything on this row arrived decided: the severity is the worse
//  of the host's liveness and its worst alert, the liveness is a verdict against a policy, the metric
//  values are normalised and the metric list is sorted. The row draws them.
//

#if canImport(PessimalFFI)
    import PessimalFFI
#endif
import SwiftUI

/// A single host in the fleet list.
struct HostRowView: View {
    let host: HostViewRecord

    /// The metric kinds core's configuration nominates for the overview, as a set for lookup.
    ///
    /// This is `FleetConfigRecord.overviewMetrics` — "charted for every host". The host's own `metrics`
    /// array is deliberately wider than it: core also fetches whatever an enabled alert rule needs, so
    /// that a rule can never sit on a metric the poll does not gather. Those extra series belong to the
    /// alert that asked for them, not to this row.
    ///
    /// `nil` means the configuration was not available to say, and every reported metric is shown.
    /// Showing too many is an untidy row; showing none would be a row claiming a host reports nothing,
    /// which is a different and false statement.
    let overviewMetrics: Set<MetricKindRecord>?

    let now: Date

    private var headlineMetrics: [MetricViewRecord] {
        // A filter, never a sort. `metrics` arrives ordered by `(kind, id)` and preserving that order
        // is what keeps a `ForEach` from animating rows that did not change.
        guard let overviewMetrics else { return host.metrics }
        return host.metrics.filter { overviewMetrics.contains($0.kind) }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Image(systemName: FleetStyle.symbolName(for: host.severity))
                    .foregroundStyle(FleetStyle.tint(for: host.severity))
                    .accessibilityLabel(FleetStyle.name(for: host.severity))

                Text(host.id)
                    .font(.headline)
                    .lineLimit(1)
                    // The middle, not the tail: host names differ in their suffix far more often than
                    // in their prefix, and `web-prod-01`/`web-prod-02` truncated at the tail are the
                    // same string.
                    .truncationMode(.middle)

                Spacer(minLength: 6)

                Label(
                    FleetStyle.name(for: host.liveness),
                    systemImage: FleetStyle.symbolName(for: host.liveness)
                )
                .font(.subheadline)
                .foregroundStyle(.secondary)
                .labelStyle(.titleAndIcon)
                .lineLimit(1)
                .layoutPriority(1)
            }

            if !badges.isEmpty {
                // A wrapping grid rather than an `HStack`, so a host carrying four badges does not
                // squeeze them all into unreadable slivers on a phone.
                LazyVGrid(
                    columns: [GridItem(.adaptive(minimum: 130), spacing: 6, alignment: .leading)],
                    alignment: .leading,
                    spacing: 6
                ) {
                    ForEach(badges) { badge in
                        HostBadgeView(badge: badge)
                    }
                }
            }

            if headlineMetrics.isEmpty {
                Text("No overview metrics for this host yet.")
                    .font(.footnote)
                    .foregroundStyle(.secondary)
            } else {
                LazyVGrid(
                    columns: [GridItem(.adaptive(minimum: 104), spacing: 12, alignment: .leading)],
                    alignment: .leading,
                    spacing: 8
                ) {
                    ForEach(headlineMetrics, id: \.id) { metric in
                        MetricChipView(metric: metric, now: now)
                    }
                }
            }

            // The facts the macOS app hides behind a tooltip. There is no hover on a phone, so they are
            // either on the row or they are nowhere — and "judged 40m ago" is how an operator notices
            // that a liveness verdict is frozen rather than current.
            Text(provenance)
                .font(.caption2)
                .foregroundStyle(.secondary)
                .lineLimit(2)
        }
        .padding(.vertical, 4)
        .accessibilityElement(children: .combine)
    }

    /// Where this row's verdict came from and how old it is.
    private var provenance: String {
        var parts = [FleetStyle.name(for: host.os)]
        if let agentVersion = host.agentVersion {
            parts.append("agent \(agentVersion)")
        }
        if let heartbeat = host.lastHeartbeatMillis {
            parts.append("heartbeat \(FleetFormat.age(sinceMillis: heartbeat, at: now))")
        } else {
            parts.append("no heartbeat seen")
        }
        // Core freezes this instant while the roster query is failing, and says so: render its age,
        // never re-judge liveness from it.
        parts.append("judged \(FleetFormat.age(sinceMillis: host.livenessAtMillis, at: now))")
        return parts.joined(separator: " · ")
    }

    private var badges: [HostBadge] {
        var badges: [HostBadge] = []

        if host.firingAlerts > 0 {
            badges.append(
                HostBadge(
                    id: "firing",
                    symbol: "bell.fill",
                    text: FleetFormat.count(host.firingAlerts, singular: "firing", plural: "firing"),
                    tint: .red
                )
            )
        }
        if host.pendingAlerts > 0 {
            badges.append(
                HostBadge(
                    id: "pending",
                    symbol: "bell.badge",
                    text: FleetFormat.count(host.pendingAlerts, singular: "pending", plural: "pending"),
                    tint: .orange
                )
            )
        }
        if !host.inCurrentRoster {
            // Core keeps such a host visible on purpose: a positive `Down` with a badge, rather than a
            // row that vanished at the moment somebody needed it.
            badges.append(
                HostBadge(id: "roster", symbol: "eye.slash", text: "Not in roster", tint: .orange)
            )
        }
        if host.idIsPlaceholder {
            // The agent's `unknown-host` fallback. Several unrelated machines collide into it and a
            // host selector then treats them as one; nothing above this flag can detect that.
            badges.append(
                HostBadge(
                    id: "placeholder",
                    symbol: "questionmark.square.dashed",
                    text: "Placeholder id",
                    tint: .orange
                )
            )
        }
        if case let .failing(added, overSeconds) = host.collection {
            // An agent beating happily while collecting nothing. Invisible in every other field on this
            // row, which is the whole reason core computes it.
            badges.append(
                HostBadge(
                    id: "collection",
                    symbol: "exclamationmark.arrow.triangle.2.circlepath",
                    text: "\(FleetFormat.value(added, unit: .count, isRate: false)) collection failures"
                        + " in \(FleetFormat.duration(Double(overSeconds)))",
                    tint: .red
                )
            )
        }

        return badges
    }
}

/// One badge on a host row.
struct HostBadge: Identifiable, Equatable {
    let id: String
    let symbol: String
    let text: String
    let tint: Color
}

struct HostBadgeView: View {
    let badge: HostBadge

    var body: some View {
        Label(badge.text, systemImage: badge.symbol)
            .font(.caption)
            .lineLimit(1)
            .truncationMode(.tail)
            .padding(.horizontal, 7)
            .padding(.vertical, 3)
            .background(badge.tint.opacity(0.15), in: Capsule())
            .foregroundStyle(badge.tint)
    }
}

/// One metric's newest value.
struct MetricChipView: View {
    let metric: MetricViewRecord
    let now: Date

    var body: some View {
        VStack(alignment: .leading, spacing: 1) {
            Text(title)
                .font(.caption2)
                .foregroundStyle(.secondary)
                .lineLimit(1)
                .truncationMode(.tail)

            Text(FleetFormat.latestValue(of: metric))
                .font(.body.monospacedDigit())
                // Core is explicit that a non-`present` tile shows *frozen* values: they are real, they
                // are not current, and greying them is how the difference is visible at a glance.
                .foregroundStyle(isStale ? AnyShapeStyle(.tertiary) : AnyShapeStyle(.primary))

            if let note = FleetStyle.explanation(for: metric.availability) {
                // The macOS app puts this in a tooltip. A phone has no tooltip, and a greyed number
                // with no explanation is a number the reader has to guess about.
                Text(note)
                    .font(.caption2)
                    .foregroundStyle(.tertiary)
                    .lineLimit(2)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityElement(children: .combine)
        .accessibilityLabel(accessibilityDescription)
    }

    private var isStale: Bool {
        metric.availability != .present
    }

    /// Core's display name, plus the mount or interface when there is one. Both come from core;
    /// re-splitting an attribute string here would put a parser in the UI layer.
    private var title: String {
        guard let label = metric.label else { return metric.displayName }
        return "\(metric.displayName) \(label)"
    }

    private var accessibilityDescription: String {
        var parts = ["\(title), \(FleetFormat.latestValue(of: metric))"]
        if let note = FleetStyle.explanation(for: metric.availability) {
            parts.append(note)
        }
        if let fetchedAt = metric.fetchedAtMillis {
            // Older than the fleet's `asOfMillis` while the metric is unavailable. That gap is the
            // entire point of the field: it is the timestamp a greyed tile shows.
            parts.append("fetched \(FleetFormat.age(sinceMillis: fetchedAt, at: now))")
        } else {
            parts.append("never fetched")
        }
        return parts.joined(separator: ", ")
    }
}
