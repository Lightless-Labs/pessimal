//
//  FleetMenuView.swift
//  Pessimal — macOS menu bar client
//
//  The dropdown. Header, freshness banner, fleet counts, the host list, whatever went quietly
//  wrong, and the three menu items.
//
//  Everything here is a rendering of something already decided in Rust: the fleet's severity, each
//  host's liveness, the order of the rows, the counts in the header, and — recomputed on every
//  tick from the caller's `now` — how much of it deserves to be believed.
//

import SwiftUI

/// The content of the menu bar window.
struct FleetMenuView: View {
    @Environment(FleetModel.self) private var model
    @Environment(FleetStoreBridge.self) private var stores

    /// The services object, for the one action that is more than a model call.
    let services: PessimalServices

    /// How wide the window is. A menu bar window has no user-resizable frame, so the width is
    /// chosen once here and every child lays out inside it.
    private static let width: CGFloat = 380

    /// The tallest the host list is allowed to get before it scrolls.
    private static let listMaxHeight: CGFloat = 420

    var body: some View {
        // One clock for the whole window. `freshness` is a function of `now` and has to be
        // recomputed as time passes — a stored verdict reads Fresh forever while the data rots —
        // and sharing the tick means the banner, the ages and the countdown cannot disagree about
        // what time it is.
        TimelineView(.periodic(from: .now, by: 1)) { context in
            content(now: context.date)
        }
        .frame(width: Self.width)
    }

    @ViewBuilder
    private func content(now: Date) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            switch model.sessionState {
            case .unconfigured, .unusable:
                SetupNeededView(
                    sessionState: model.sessionState,
                    credentialProblem: stores.credentialProblem,
                    settingsProblem: stores.settingsProblem
                )

            case .ready:
                readyContent(now: now)
            }

            notices

            Divider()

            MenuFooterView(
                pollingState: model.pollingState,
                nextPollAt: model.nextPollAt,
                now: now,
                onRefresh: { await services.refreshNow() }
            )
        }
        .padding(12)
    }

    @ViewBuilder
    private func readyContent(now: Date) -> some View {
        if let fleet = model.fleet {
            header(fleet: fleet)

            if let freshness = model.freshness(at: now) {
                FreshnessBannerView(freshness: freshness, now: now)
            }

            FleetCountsView(counts: fleet.counts)

            Divider()

            if fleet.hosts.isEmpty {
                emptyFleet
            } else {
                hostList(fleet: fleet, now: now)
            }
        } else {
            // `adopt` sets `fleet` and `sessionState` together, so this is the width of one
            // reload rather than a state anybody sits in. Said plainly all the same: a blank
            // panel with no explanation is the thing this app does not do.
            Text("Starting up…")
                .font(.callout)
                .foregroundStyle(.secondary)
        }
    }

    // MARK: - Header

    private func header(fleet: FleetViewRecord) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Image(systemName: MenuBarStyle.symbolName(for: fleet.severity))
                .foregroundStyle(MenuBarStyle.tint(for: fleet.severity))
                .accessibilityLabel("Fleet severity: \(MenuBarStyle.name(for: fleet.severity))")

            VStack(alignment: .leading, spacing: 1) {
                Text(fleet.backendName)
                    .font(.headline)
                if let environment = model.config?.environment {
                    Text(environment)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }

            Spacer(minLength: 0)

            // `asOfMillis` is `nil` until the first fold — not `0`, because a fleet nobody has
            // polled and a fleet polled at the Unix epoch must not render the same.
            if let asOf = fleet.asOfMillis {
                Text("as of \(MenuBarFormat.time(millis: asOf))")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .monospacedDigit()
            } else {
                Text("not yet polled")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
    }

    // MARK: - Hosts

    private func hostList(fleet: FleetViewRecord, now: Date) -> some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 0) {
                // Core's order: degraded hosts first. Not re-sorted, not filtered, not grouped —
                // a second sort here would be a second place that order is decided, and the two
                // would disagree the first time either changed.
                ForEach(Array(fleet.hosts.enumerated()), id: \.element.id) { index, host in
                    if index > 0 {
                        Divider()
                    }
                    HostRowView(
                        host: host,
                        overviewMetrics: model.config.map { Set($0.overviewMetrics) },
                        now: now
                    )
                }
            }
        }
        .frame(maxHeight: Self.listMaxHeight)
        .scrollBounceBehavior(.basedOnSize)
    }

    private var emptyFleet: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text("No hosts are reporting.")
                .font(.callout)
            Text(
                "The backend answered and listed nothing. Check that an agent is running and "
                    + "exporting under this environment."
            )
            .font(.caption)
            .foregroundStyle(.secondary)
            .fixedSize(horizontal: false, vertical: true)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.vertical, 6)
    }

    // MARK: - Notices

    /// The things that go wrong without stopping anything, and are therefore invisible unless
    /// something says them out loud.
    @ViewBuilder
    private var notices: some View {
        // Core's restore never fails; when it salvaged nothing, this report is the only place the
        // loss is visible at all. Worth saying only while a `discarded_*` flag is set — the first
        // successful poll papers over it within seconds.
        if let report = model.restoreReport, report.discardedUnreadable || report.discardedIncompatible {
            notice(
                symbol: "clock.badge.exclamationmark",
                text: report.discardedIncompatible
                    ? "Saved history was from an older version of Pessimal and was discarded."
                    : "Saved history could not be read and was discarded."
            )
        }

        if let problem = stores.stateCacheProblem {
            notice(symbol: "externaldrive.badge.xmark", text: "Saved history: \(problem)")
        }

        if let problem = model.lastPersistenceError {
            // A failed write costs a colder start next launch and nothing else, so it is a
            // footnote rather than a banner — and a footnote rather than silence, because an app
            // that has silently kept no history for a month is worse than one that says so.
            notice(symbol: "square.and.arrow.down.badge.xmark", text: "Could not save: \(problem)")
        }
    }

    private func notice(symbol: String, text: String) -> some View {
        Label {
            Text(text)
                .font(.caption)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
        } icon: {
            Image(systemName: symbol)
                .foregroundStyle(.secondary)
        }
        .font(.caption)
    }
}

/// The fleet tallies, straight from core.
///
/// Carried rather than counted here, and the record says why: a header and the list beneath it
/// must not be able to disagree.
struct FleetCountsView: View {
    let counts: FleetCountsRecord

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 6) {
                tally(counts.hosts, label: "hosts", tint: .primary)
                separator
                tally(counts.alive, label: "alive", tint: .green)
                separator
                tally(counts.stale, label: "stale", tint: .orange)
                separator
                tally(counts.down, label: "down", tint: .red)
                separator
                tally(counts.unknown, label: "unknown", tint: .secondary)
                Spacer(minLength: 0)
            }

            HStack(spacing: 6) {
                tally(counts.firingAlerts, label: "firing", tint: .red)
                separator
                tally(counts.pendingAlerts, label: "pending", tint: .orange)
                Spacer(minLength: 0)
            }
        }
        .font(.caption)
        // Every tally is shown even at zero. A row whose entries appear and disappear is a row
        // whose positions have to be re-read every time; a steady one can be glanced at.
        .monospacedDigit()
    }

    private var separator: some View {
        Text("·").foregroundStyle(.tertiary)
    }

    private func tally(_ value: UInt32, label: String, tint: Color) -> some View {
        HStack(spacing: 3) {
            Text(value.formatted())
                .fontWeight(.medium)
                .foregroundStyle(tint)
            Text(label)
                .foregroundStyle(.secondary)
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(value) \(label)")
    }
}
