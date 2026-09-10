//
//  FleetListView.swift
//  Pessimal — iOS client
//
//  The app's one screen: what the fleet is, how much of it to believe, and one row per host.
//
//  Everything here is a rendering of something already decided in Rust: the fleet's severity, each
//  host's liveness, the order of the rows, the counts in the header, and — recomputed on every tick
//  from the caller's `now` — how much of it deserves to be believed.
//

#if canImport(PessimalFFI)
    import PessimalFFI
#endif
#if canImport(PessimalKit)
    import PessimalKit
#endif
import SwiftUI

/// The fleet list.
struct FleetListView: View {
    @Environment(FleetModel.self) private var model
    @Environment(FleetStoreBridge.self) private var stores

    /// The services object, for the one action that is more than a model call.
    let services: PessimalServices

    var body: some View {
        // One clock for the whole screen. `freshness` is a function of `now` and has to be recomputed
        // as time passes — a stored verdict reads Fresh forever while the data rots — and sharing the
        // tick means the banner, the row ages and the countdown cannot disagree about what time it is.
        // A `TimelineView` per row would give each row its own phase, which is that same disagreement
        // in a less obvious form.
        TimelineView(.periodic(from: .now, by: 1)) { context in
            content(now: context.date)
        }
        // Declared outside the `TimelineView`, not inside it. Everything in its closure is
        // re-evaluated once a second; the navigation bar, the toolbar and the refresh action do not
        // change with the clock, and rebuilding them on every tick would put a redraw in front of a
        // pull gesture for no reason. `.refreshable` still reaches the `List` — it travels through
        // the environment, not down the view hierarchy by adjacency.
        .navigationTitle(model.fleet?.backendName ?? "Pessimal")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar { toolbar }
        // The gesture every iOS list has, wired to the model's own refresh. Core may answer that the
        // poll was skipped because one is already running; that is "nothing to do" rather than an
        // error, and `FleetModel` settles it without anything reaching this screen.
        .refreshable { await services.refreshNow() }
    }

    @ViewBuilder
    private func content(now: Date) -> some View {
        List {
            switch model.sessionState {
            case .unconfigured, .unusable:
                Section {
                    SetupNeededView(
                        sessionState: model.sessionState,
                        credentialProblem: stores.credentialProblem,
                        settingsProblem: stores.settingsProblem
                    )
                }

            case .ready:
                readyContent(now: now)
            }

            notices
        }
        .listStyle(.insetGrouped)
    }

    @ToolbarContentBuilder
    private var toolbar: some ToolbarContent {
        ToolbarItem(placement: .topBarTrailing) {
            // The same action as pulling the list down, reachable without the gesture — a pull is hard
            // to perform under VoiceOver and impossible with Switch Control.
            //
            // Never disabled while a poll is in flight: `FleetModel.refresh()` joins the running poll
            // rather than starting a second one, and a button that greys out for the duration of a
            // slow request is a button that looks broken exactly when somebody is leaning on it.
            Button {
                Task { await services.refreshNow() }
            } label: {
                Label("Refresh", systemImage: "arrow.clockwise")
            }
        }

        ToolbarItem(placement: .topBarTrailing) {
            NavigationLink(value: FleetRoute.settings) {
                Label("Settings", systemImage: "gearshape")
            }
        }
    }

    // MARK: - Ready

    @ViewBuilder
    private func readyContent(now: Date) -> some View {
        if let fleet = model.fleet {
            // Everything that describes the fleet as a whole, in the order it has to be read: what the
            // worst of it is, whether to believe any of it, the tallies, and what the clock is doing.
            // The banner sits above the counts deliberately — it is the sentence that qualifies every
            // number below it.
            Section {
                summary(fleet: fleet)

                // Nothing at all while core says the data is current: a banner that is always there is
                // a banner nobody reads.
                if let freshness = model.freshness(at: now) {
                    FreshnessBannerView(freshness: freshness, now: now)
                }

                FleetCountsView(counts: fleet.counts)

                pollStatus(now: now)
            }

            Section {
                if fleet.hosts.isEmpty {
                    emptyFleet
                } else {
                    // Core's order: degraded hosts first. Not re-sorted, not filtered, not grouped — a
                    // second sort here would be a second place that order is decided, and the two would
                    // disagree the first time either changed.
                    ForEach(fleet.hosts, id: \.id) { host in
                        NavigationLink(value: FleetRoute.host(id: host.id)) {
                            HostRowView(
                                host: host,
                                overviewMetrics: model.config.map { Set($0.overviewMetrics) },
                                now: now
                            )
                        }
                    }
                }
            } header: {
                Text(FleetFormat.count(fleet.counts.hosts, singular: "host", plural: "hosts"))
            }
        } else {
            // `adopt` sets `fleet` and `sessionState` together, so this is the width of one reload
            // rather than a state anybody sits in. Said plainly all the same: a blank screen with no
            // explanation is the thing this app does not do.
            Section {
                Text("Starting up…")
                    .foregroundStyle(.secondary)
            }
        }
    }

    /// Severity, environment, and when the picture was taken.
    private func summary(fleet: FleetViewRecord) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 12) {
            Image(systemName: FleetStyle.symbolName(for: fleet.severity))
                .font(.title3)
                .foregroundStyle(FleetStyle.tint(for: fleet.severity))
                .accessibilityHidden(true)

            VStack(alignment: .leading, spacing: 2) {
                Text(FleetStyle.name(for: fleet.severity))
                    .font(.headline)
                if let environment = model.config?.environment {
                    Text(environment)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }

            Spacer(minLength: 8)

            // `asOfMillis` is `nil` until the first fold — not `0`, because a fleet nobody has polled
            // and a fleet polled at the Unix epoch must not render the same.
            if let asOf = fleet.asOfMillis {
                Text("as of \(FleetFormat.time(millis: asOf))")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .monospacedDigit()
            } else {
                Text("not yet polled")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel("Fleet severity: \(FleetStyle.name(for: fleet.severity))")
    }

    private var emptyFleet: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text("No hosts are reporting.")
            Text(
                "The backend answered and listed nothing. Check that an agent is running and "
                    + "exporting under this environment."
            )
            .font(.footnote)
            .foregroundStyle(.secondary)
            .fixedSize(horizontal: false, vertical: true)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    // MARK: - Poll status

    /// What the clock is doing. Drawn at all times, including when it is doing nothing.
    ///
    /// Separate from the freshness banner on purpose: the banner is core's verdict on how old the
    /// *data* is, and this line is the platform's report on the *timer*. A stop that core diagnosed a
    /// moment ago has not yet aged the data, so without this line the app would look healthy while
    /// having given up.
    @ViewBuilder
    private func pollStatus(now: Date) -> some View {
        switch model.pollingState {
        case .idle:
            footnote("Idle.", symbol: "pause.circle", tint: .secondary)

        case .scheduled:
            if let nextPollAt = model.nextPollAt {
                footnote(
                    "Next poll \(FleetFormat.countdown(to: nextPollAt, at: now)).",
                    symbol: "clock",
                    tint: .secondary
                )
            } else {
                footnote("Scheduled.", symbol: "clock", tint: .secondary)
            }

        case .polling:
            HStack(spacing: 8) {
                ProgressView()
                    .controlSize(.small)
                Text("Polling…")
                    .font(.footnote)
                    .foregroundStyle(.secondary)
            }

        case .suspended:
            // Deliberately paused, and not a failure. Drawn plainly rather than in a warning colour,
            // and drawn at all rather than hidden: an app that has quietly stopped watching must say
            // so, and on iOS it stops watching every time it is put away.
            footnote("Polling is paused.", symbol: "pause.circle", tint: .secondary)

        case let .stopped(_, message):
            // Core's verdict that retrying cannot help, in core's words. Refresh still works — a stop
            // is a reason not to retry automatically, not a refusal to be asked.
            //
            // No Settings link here even though one stop kind deserves it: "the user must change
            // something" is `PollFailureRecord.isActionable`, which this case does not carry, and
            // guessing it from the failure kind would be re-deciding in Swift what core already
            // decided. The freshness banner has the record, and offers the link.
            footnote(message, symbol: "xmark.octagon.fill", tint: .red)
        }
    }

    // MARK: - Notices

    /// The things that go wrong without stopping anything, and are therefore invisible unless
    /// something says them out loud.
    @ViewBuilder
    private var notices: some View {
        if hasNotices {
            Section {
                // Core's restore never fails; when it salvaged nothing, this report is the only place
                // the loss is visible at all. Worth saying only while a `discarded_*` flag is set — the
                // first successful poll papers over it within seconds.
                if let report = model.restoreReport,
                   report.discardedUnreadable || report.discardedIncompatible {
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
                    // footnote rather than a banner — and a footnote rather than silence, because an
                    // app that has silently kept no history for a month is worse than one that says so.
                    notice(
                        symbol: "square.and.arrow.down.badge.xmark",
                        text: "Could not save: \(problem)"
                    )
                }
            }
        }
    }

    /// Whether `notices` has anything to draw.
    ///
    /// Asked separately so an empty `Section` — which `List` still draws as a gap — never appears.
    private var hasNotices: Bool {
        if let report = model.restoreReport, report.discardedUnreadable || report.discardedIncompatible {
            return true
        }
        return stores.stateCacheProblem != nil || model.lastPersistenceError != nil
    }

    // MARK: - Shared bits

    private func notice(symbol: String, text: String) -> some View {
        Label {
            Text(text)
                .font(.footnote)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
        } icon: {
            Image(systemName: symbol)
                .foregroundStyle(.secondary)
        }
    }

    private func footnote(_ text: String, symbol: String, tint: Color) -> some View {
        Label {
            Text(text)
                .font(.footnote)
                .foregroundStyle(tint == .secondary ? AnyShapeStyle(.secondary) : AnyShapeStyle(tint))
                .fixedSize(horizontal: false, vertical: true)
                .textSelection(.enabled)
        } icon: {
            Image(systemName: symbol)
                .foregroundStyle(tint)
        }
    }
}

/// The fleet tallies, straight from core.
///
/// Carried rather than counted here, and the record says why: a header and the list beneath it must
/// not be able to disagree.
struct FleetCountsView: View {
    let counts: FleetCountsRecord

    var body: some View {
        // A wrapping grid rather than one row: five tallies and their words do not fit across a phone,
        // and abbreviating the words to make them fit is how "stale" and "down" become guesses.
        LazyVGrid(
            columns: [GridItem(.adaptive(minimum: 88), spacing: 12, alignment: .leading)],
            alignment: .leading,
            spacing: 10
        ) {
            tally(counts.hosts, label: "hosts", tint: .primary)
            tally(counts.alive, label: "alive", tint: .green)
            tally(counts.stale, label: "stale", tint: .orange)
            tally(counts.down, label: "down", tint: .red)
            tally(counts.unknown, label: "unknown", tint: .secondary)
            tally(counts.firingAlerts, label: "firing", tint: .red)
            tally(counts.pendingAlerts, label: "pending", tint: .orange)
        }
        // Every tally is shown even at zero. A grid whose cells appear and disappear is a grid whose
        // positions have to be re-read every time; a steady one can be glanced at.
        .monospacedDigit()
    }

    private func tally(_ value: UInt32, label: String, tint: Color) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            Text(value.formatted())
                .font(.title3.weight(.medium))
                .foregroundStyle(tint)
            Text(label)
                .font(.caption)
                .foregroundStyle(.secondary)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(value) \(label)")
    }
}
