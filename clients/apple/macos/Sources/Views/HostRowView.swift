//
//  HostRowView.swift
//  Pessimal — macOS menu bar client
//
//  One host, as the core folded it. Everything on this row arrived decided: the severity is the
//  worse of the host's liveness and its worst alert, the liveness is a verdict against a policy,
//  the metric values are normalised and the metric list is sorted. The row draws them.
//

import SwiftUI

/// A single host in the dropdown.
struct HostRowView: View {
    let host: HostViewRecord

    /// The metric kinds core's configuration nominates for the overview, as a set for lookup.
    ///
    /// This is `FleetConfigRecord.overviewMetrics` — "charted for every host". The host's own
    /// `metrics` array is deliberately wider than it: core also fetches whatever an enabled alert
    /// rule needs, so that a rule can never sit on a metric the poll does not gather. Those extra
    /// series belong to the alert that asked for them, not to this row.
    ///
    /// `nil` means the configuration was not available to say, and every reported metric is shown.
    /// Showing too many is an untidy row; showing none would be a row claiming a host reports
    /// nothing, which is a different and false statement.
    let overviewMetrics: Set<MetricKindRecord>?

    let now: Date

    private var headlineMetrics: [MetricViewRecord] {
        // A filter, never a sort. `metrics` arrives ordered by `(kind, id)` and preserving that
        // order is what keeps a `ForEach` from animating rows that did not change.
        guard let overviewMetrics else { return host.metrics }
        return host.metrics.filter { overviewMetrics.contains($0.kind) }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 5) {
            HStack(alignment: .firstTextBaseline, spacing: 6) {
                Image(systemName: MenuBarStyle.symbolName(for: host.severity))
                    .foregroundStyle(MenuBarStyle.tint(for: host.severity))
                    .accessibilityLabel(MenuBarStyle.name(for: host.severity))

                Text(host.id)
                    .font(.callout.weight(.medium))
                    .lineLimit(1)
                    .truncationMode(.middle)

                Spacer(minLength: 4)

                Label(
                    MenuBarStyle.name(for: host.liveness),
                    systemImage: MenuBarStyle.symbolName(for: host.liveness)
                )
                .font(.caption)
                .foregroundStyle(.secondary)
                .labelStyle(.titleAndIcon)
                .layoutPriority(1)
            }

            if !badges.isEmpty {
                // Wrapped in a grid rather than an HStack so a host carrying four badges does not
                // push its own name off the row.
                LazyVGrid(
                    columns: [GridItem(.adaptive(minimum: 108), spacing: 4, alignment: .leading)],
                    alignment: .leading,
                    spacing: 4
                ) {
                    ForEach(badges) { badge in
                        HostBadgeView(badge: badge)
                    }
                }
            }

            if headlineMetrics.isEmpty {
                Text("No overview metrics for this host yet.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            } else {
                LazyVGrid(
                    columns: [GridItem(.adaptive(minimum: 96), spacing: 8, alignment: .leading)],
                    alignment: .leading,
                    spacing: 6
                ) {
                    ForEach(headlineMetrics, id: \.id) { metric in
                        MetricChipView(metric: metric, now: now)
                    }
                }
            }
        }
        .padding(.vertical, 5)
        .help(tooltip)
    }

    /// The facts that do not earn a line of their own but are worth having on hover.
    private var tooltip: String {
        var parts = [MenuBarStyle.name(for: host.os)]
        if let agentVersion = host.agentVersion {
            parts.append("agent \(agentVersion)")
        }
        if let heartbeat = host.lastHeartbeatMillis {
            parts.append("last heartbeat \(MenuBarFormat.age(sinceMillis: heartbeat, at: now))")
        } else {
            parts.append("no heartbeat seen")
        }
        // Core freezes this instant while the roster query is failing, and says so: render its
        // age, never re-judge liveness from it.
        parts.append("judged \(MenuBarFormat.age(sinceMillis: host.livenessAtMillis, at: now))")
        return parts.joined(separator: " · ")
    }

    private var badges: [HostBadge] {
        var badges: [HostBadge] = []

        if host.firingAlerts > 0 {
            badges.append(
                HostBadge(
                    id: "firing",
                    symbol: "bell.fill",
                    text: MenuBarFormat.count(host.firingAlerts, singular: "firing", plural: "firing"),
                    tint: .red
                )
            )
        }
        if host.pendingAlerts > 0 {
            badges.append(
                HostBadge(
                    id: "pending",
                    symbol: "bell.badge",
                    text: MenuBarFormat.count(host.pendingAlerts, singular: "pending", plural: "pending"),
                    tint: .orange
                )
            )
        }
        if !host.inCurrentRoster {
            // Core keeps such a host visible on purpose: a positive `Down` with a badge, rather
            // than a row that vanished at the moment somebody needed it.
            badges.append(
                HostBadge(
                    id: "roster",
                    symbol: "eye.slash",
                    text: "Not in roster",
                    tint: .orange
                )
            )
        }
        if host.idIsPlaceholder {
            // The agent's `unknown-host` fallback. Several unrelated machines collide into it and
            // a host selector then treats them as one; nothing above this flag can detect that.
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
            // An agent beating happily while collecting nothing. Invisible in every other field
            // on this row, which is the whole reason core computes it.
            badges.append(
                HostBadge(
                    id: "collection",
                    symbol: "exclamationmark.arrow.triangle.2.circlepath",
                    text: "\(MenuBarFormat.value(added, unit: .count, isRate: false)) collection failures in \(MenuBarFormat.duration(Double(overSeconds)))",
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
            .font(.caption2)
            .lineLimit(1)
            .truncationMode(.tail)
            .padding(.horizontal, 5)
            .padding(.vertical, 2)
            .background(badge.tint.opacity(0.15), in: Capsule())
            .foregroundStyle(badge.tint)
            .help(badge.text)
    }
}

/// One metric's newest value.
struct MetricChipView: View {
    let metric: MetricViewRecord
    let now: Date

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Text(title)
                .font(.caption2)
                .foregroundStyle(.secondary)
                .lineLimit(1)
                .truncationMode(.tail)

            Text(MenuBarFormat.latestValue(of: metric))
                .font(.callout.monospacedDigit())
                // Core is explicit that a `.unavailable` tile shows *frozen* values: they are
                // real, they are not current, and greying them is how the difference is visible.
                .foregroundStyle(isStale ? AnyShapeStyle(.tertiary) : AnyShapeStyle(.primary))
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .help(tooltip)
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

    private var tooltip: String {
        var parts = [title]
        if let explanation = MenuBarStyle.explanation(for: metric.availability) {
            parts.append(explanation)
        }
        if let fetchedAt = metric.fetchedAtMillis {
            // Older than the fleet's `asOfMillis` while the metric is unavailable. That gap is the
            // entire point of the field: it is the timestamp a greyed tile shows.
            parts.append("fetched \(MenuBarFormat.age(sinceMillis: fetchedAt, at: now))")
        } else {
            parts.append("never fetched")
        }
        return parts.joined(separator: " · ")
    }
}
