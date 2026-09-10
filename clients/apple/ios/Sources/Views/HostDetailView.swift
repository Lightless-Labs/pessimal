//
//  HostDetailView.swift
//  Pessimal — iOS client
//
//  One host, in full: what core judged it to be, every series it reports, every rule that applies
//  to it, and the one destructive thing an operator can do to it.
//
//  **The screen holds an id, not a record.** A `HostViewRecord` captured at navigation time would
//  freeze: polls keep landing while this screen is open, and a detail view showing a three-minute-old
//  snapshot under a banner that says the data is current is the precise failure this app exists to
//  prevent. So the
//  host is looked up out of `FleetModel.fleet` on every pass, and the id not being there any more is
//  a state with its own screen rather than a blank one.
//
//  `FleetModel` is read from the environment, so whoever presents this view must have injected it —
//  `.environment(services.fleet)` at the app root, exactly as the macOS menu does. That is a crash
//  if it is missing, and deliberately so: a detail screen that silently rendered nothing would be
//  indistinguishable from a host with nothing to report.
//

#if canImport(PessimalFFI)
    import PessimalFFI
#endif
#if canImport(PessimalKit)
    import PessimalKit
#endif
import SwiftUI

/// The host detail screen, pushed from the fleet list.
struct HostDetailView: View {

    /// The host's id. Core's identity for it, and the key every lookup on this screen uses.
    let hostId: String

    @Environment(FleetModel.self) private var model
    @Environment(\.dismiss) private var dismiss

    @State private var isConfirmingForget = false
    @State private var isForgetting = false

    /// Core's words when a forget did not happen. `forgetHost` is documented as never failing
    /// today, so this is the path that exists because "never" is a property of this version.
    @State private var forgetError: String?

    init(hostId: String) {
        self.hostId = hostId
    }

    var body: some View {
        // One clock for the whole screen. Freshness is a function of `now` and has to be recomputed
        // as time passes — a stored verdict reads `Fresh` for ever while the data rots — and sharing
        // the tick means the ages, the breach durations and the banner cannot disagree about what
        // time it is.
        TimelineView(.periodic(from: .now, by: 1)) { context in
            content(now: context.date)
        }
        .navigationTitle(hostId)
        .navigationBarTitleDisplayMode(.inline)
    }

    @ViewBuilder
    private func content(now: Date) -> some View {
        if let host = model.fleet?.hosts.first(where: { $0.id == hostId }) {
            List {
                freshnessNotice(now: now)
                statusSection(host: host, now: now)
                warningsSection(host: host)
                metricsSection(host: host, now: now)
                alertsSection(host: host, now: now)
                forgetSection
            }
            .listStyle(.insetGrouped)
        } else {
            // Reachable three ways, all of them real: the host was just forgotten, core dropped it
            // after `forget_host_after`, or the session was rebuilt against a different backend.
            // Saying so beats a screen of empty sections.
            ContentUnavailableView(
                "This host is no longer in the fleet",
                systemImage: "questionmark.square.dashed",
                description: Text(
                    """
                    \(hostId) is not in the current view. It was either forgotten, or dropped after \
                    being silent for longer than this fleet retains a host. If it is still exporting, \
                    it will be listed again on a future poll.
                    """
                )
            )
        }
    }

    // MARK: - Freshness

    /// How much of this screen deserves to be believed.
    ///
    /// Recomputed from `now` on every tick and never stored, which is the whole contract of
    /// `FleetSession.freshness`: a verdict computed once reads `Fresh` for ever while the data under it
    /// rots, and on a phone that is the likely case rather than the exotic one — the app was frozen in
    /// a pocket and this screen is the fleet from last night.
    ///
    /// The fleet list's banner, not a smaller one of this screen's own. The same verdict described two
    /// ways is two things to keep in step, and the banner already carries the one affordance that
    /// matters here: core's `isActionable` deciding whether Open Settings is offered at all.
    @ViewBuilder
    private func freshnessNotice(now: Date) -> some View {
        // The `.fresh` test is a layout question, not a second verdict: the banner renders nothing for
        // it, and a `Section` wrapped round nothing is a blank rounded rectangle at the top of an
        // inset-grouped list. The decision about what `.fresh` *means* stays inside the banner.
        if let freshness = model.freshness(at: now), !Self.isFresh(freshness) {
            Section {
                FreshnessBannerView(freshness: freshness, now: now)
            }
        }
    }

    private static func isFresh(_ freshness: FreshnessRecord) -> Bool {
        if case .fresh = freshness { return true }
        return false
    }

    // MARK: - Status

    /// What core judged this host to be.
    ///
    /// Every row is read, never derived. `severity` in particular is the worse of the host's liveness
    /// severity and its worst alert severity — a host beating perfectly with a firing rule is
    /// `Critical` — so colouring this screen from `liveness` would contradict the fleet list.
    @ViewBuilder
    private func statusSection(host: HostViewRecord, now: Date) -> some View {
        Section {
            LabeledContent("Severity") {
                Label(
                    FleetStyle.name(for: host.severity),
                    systemImage: FleetStyle.symbolName(for: host.severity)
                )
                .foregroundStyle(FleetStyle.tint(for: host.severity))
            }

            LabeledContent("Liveness") {
                Label(
                    FleetStyle.name(for: host.liveness),
                    systemImage: FleetStyle.symbolName(for: host.liveness)
                )
            }

            LabeledContent("Last heartbeat") {
                if let heartbeat = host.lastHeartbeatMillis {
                    Text(FleetFormat.age(sinceMillis: heartbeat, at: now))
                } else {
                    // Not the same as "a long time ago". Core's `Unknown` liveness means never seen.
                    Text("Never seen")
                }
            }

            LabeledContent("Verdict computed") {
                Text(FleetFormat.age(sinceMillis: host.livenessAtMillis, at: now))
            }

            LabeledContent("Operating system", value: FleetStyle.name(for: host.os))

            LabeledContent("Agent version", value: host.agentVersion ?? FleetFormat.noValue)

            LabeledContent("Alerts") {
                Text(
                    "\(FleetFormat.count(host.firingAlerts, singular: "firing", plural: "firing"))"
                        + " · "
                        + "\(FleetFormat.count(host.pendingAlerts, singular: "pending", plural: "pending"))"
                )
                .monospacedDigit()
            }
        } header: {
            Text("Status")
        } footer: {
            Text(
                """
                Severity is the worse of this host's liveness and its worst alert. The liveness \
                verdict's timestamp freezes while the roster query is failing, so its age is shown \
                rather than the verdict being recomputed from it.
                """
            )
        }
    }

    // MARK: - Warnings

    /// The facts about this host that no other row can show, and that core computes for that reason.
    ///
    /// Rendered as a section that is absent when there is nothing to say, rather than as a row
    /// reading "no problems": the three states below are all rare, and a section that appears is
    /// easier to notice than a word that changes.
    @ViewBuilder
    private func warningsSection(host: HostViewRecord) -> some View {
        let hasWarnings = !host.inCurrentRoster
            || host.idIsPlaceholder
            || isFailing(host.collection)

        if hasWarnings {
            Section {
                if !host.inCurrentRoster {
                    warning(
                        symbol: "eye.slash",
                        tint: .orange,
                        text: """
                            Not in the last listed roster. Core keeps such a host visible as a \
                            positive Down rather than letting the row vanish at the moment somebody \
                            needed it.
                            """
                    )
                }

                if host.idIsPlaceholder {
                    warning(
                        symbol: "questionmark.square.dashed",
                        tint: .orange,
                        text: """
                            This is the agent's placeholder host id. Several unrelated machines can \
                            collide into it, and an alert rule's host selector would then treat them \
                            as one machine. Set a host id on the agents reporting under it.
                            """
                    )
                }

                if case let .failing(added, overSeconds) = host.collection {
                    warning(
                        symbol: "exclamationmark.arrow.triangle.2.circlepath",
                        tint: .red,
                        text: """
                            \(FleetFormat.value(added, unit: .count, isRate: false)) collection \
                            failures in \(FleetFormat.duration(Double(overSeconds))). The agent \
                            is beating happily while collecting nothing, which is invisible in every \
                            other field on this screen.
                            """
                    )
                }
            }
        }
    }

    /// `CollectionHealthRecord.unknown` is explicitly *unjudged, not a problem* — fewer than two
    /// samples, or the metric was not fetched — so it earns no warning.
    private func isFailing(_ collection: CollectionHealthRecord) -> Bool {
        if case .failing = collection { return true }
        return false
    }

    private func warning(symbol: String, tint: Color, text: String) -> some View {
        Label {
            Text(text)
                .font(.footnote)
                .fixedSize(horizontal: false, vertical: true)
        } icon: {
            Image(systemName: symbol)
                .foregroundStyle(tint)
        }
    }

    // MARK: - Metrics

    /// Every series this host reports, in core's order.
    ///
    /// Not filtered and not grouped. `metrics` arrives sorted by `(kind, id)` with one entry per
    /// attribute set, and a second sort here would be a second place that order is decided — which
    /// is also how a `ForEach` comes to animate rows that did not change.
    @ViewBuilder
    private func metricsSection(host: HostViewRecord, now: Date) -> some View {
        Section {
            if host.metrics.isEmpty {
                Text("No series were gathered for this host.")
                    .foregroundStyle(.secondary)
            } else {
                ForEach(host.metrics, id: \.id) { metric in
                    HostDetailMetricRow(metric: metric, now: now)
                }
            }
        } header: {
            Text("Metrics")
        } footer: {
            Text(
                """
                One row per series: a filesystem per mount point, network I/O per interface and \
                direction. These are the series the last poll gathered, not a list of everything \
                this host could report.
                """
            )
        }
    }

    // MARK: - Alerts

    /// The rules that apply to this host, with the phase core judged each one to be in.
    ///
    /// Filtered out of the fleet's alert list rather than re-derived, and filtering preserves core's
    /// order: phase rank first, so anything firing or pending is at the top and `Ok` rules sit below
    /// — which is also why the `Ok` ones are shown at all. A list of only the problems cannot answer
    /// "is anything watching this host?".
    @ViewBuilder
    private func alertsSection(host: HostViewRecord, now: Date) -> some View {
        let alerts = model.fleet?.alerts.filter { $0.host == host.id } ?? []

        Section {
            if alerts.isEmpty {
                Text("No alert rule applies to this host.")
                    .foregroundStyle(.secondary)
            } else {
                ForEach(alerts, id: \.id) { alert in
                    HostDetailAlertRow(
                        alert: alert,
                        unit: unit(for: alert, on: host),
                        now: now
                    )
                }
            }
        } header: {
            Text("Alerts")
        } footer: {
            Text("Liveness is watched whatever the rules say — rules are thresholds on top of it.")
        }
    }

    /// The unit for an alert's metric, taken from a series core produced for the same kind.
    ///
    /// Core's `MetricFacts` maps a kind to exactly one unit, so any series of that kind carries the
    /// right one; matching on the series label as well would only narrow the lookup without changing
    /// the answer. `nil` when this host reports no such series, and the row then prints the value
    /// bare rather than labelling it with a unit nothing told us.
    private func unit(for alert: AlertViewRecord, on host: HostViewRecord) -> MetricUnitRecord? {
        host.metrics.first { $0.kind == alert.metric }?.unit
    }

    // MARK: - Forget

    @ViewBuilder
    private var forgetSection: some View {
        Section {
            if let forgetError {
                Label {
                    Text(forgetError)
                        .font(.footnote)
                        .textSelection(.enabled)
                        .fixedSize(horizontal: false, vertical: true)
                } icon: {
                    Image(systemName: "xmark.octagon.fill")
                        .foregroundStyle(.red)
                }
            }

            Button(role: .destructive) {
                isConfirmingForget = true
            } label: {
                HStack {
                    Text("Forget This Host")
                    if isForgetting {
                        Spacer()
                        ProgressView()
                    }
                }
            }
            .disabled(isForgetting)
            .confirmationDialog(
                "Forget \(hostId)?",
                isPresented: $isConfirmingForget,
                titleVisibility: .visible
            ) {
                Button("Forget This Host", role: .destructive) {
                    Task { await forget() }
                }
                Button("Cancel", role: .cancel) {}
            } message: {
                // What actually happens, because it is not what "forget" suggests. Core's
                // `forget_host` drops the host from the *stored state* — its series and its alert
                // evaluations — and touches nothing at the backend.
                Text(
                    """
                    This drops the stored history and alert evaluations for this host. If it is still \
                    exporting, the next poll will list it again, starting from nothing.
                    """
                )
            }
        } footer: {
            Text("For a machine that has been decommissioned, so its row stops being a false Down.")
        }
    }

    /// Drops the host, then leaves — the screen it was showing no longer describes anything.
    ///
    /// Dismissing on success rather than falling through to the "no longer in the fleet" screen: the
    /// operator asked for the row to be gone, and being told that the thing they just removed is
    /// missing is a worse answer than being put back where they came from.
    private func forget() async {
        forgetError = nil
        isForgetting = true
        defer { isForgetting = false }

        do {
            try await model.forgetHost(hostId)
            dismiss()
        } catch {
            forgetError = FleetModel.message(for: error)
        }
    }
}
