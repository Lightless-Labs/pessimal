//
//  FleetWidget.swift
//  Pessimal — iOS widget extension
//
//  The fleet widget: how it is configured, where its data comes from, and how it pages. The views are
//  in FleetWidgetViews.swift. See docs/plans/2026-10-02-m11-ios-widgets.md.
//

import AppIntents
import SwiftUI
import WidgetKit
#if canImport(PessimalFFI)
    import PessimalFFI
#endif
#if canImport(PessimalKit)
    import PessimalKit
#endif

struct FleetWidget: Widget {
    static let kind = "com.lightless-labs.pessimal.fleet"

    var body: some WidgetConfiguration {
        AppIntentConfiguration(kind: Self.kind, intent: FleetWidgetIntent.self, provider: FleetProvider()) { entry in
            FleetWidgetView(entry: entry)
        }
        .configurationDisplayName("Fleet")
        .description("Your fleet at a glance, or one host in detail.")
        .supportedFamilies([
            .systemSmall, .systemMedium, .systemLarge,
            .accessoryInline, .accessoryCircular, .accessoryRectangular,
        ])
    }
}

// MARK: - Configuration

/// What a person chooses when they edit the widget: the whole fleet or one host, and which metric.
struct FleetWidgetIntent: WidgetConfigurationIntent {
    static let title: LocalizedStringResource = "Fleet"
    static let description = IntentDescription("Show your whole fleet, or one host in detail.")

    /// Empty is the whole fleet. A host rather than a name typed in: hosts come from the last poll,
    /// so only ones that exist can be chosen.
    @Parameter(title: "Host", description: "Leave empty for the whole fleet.")
    var host: HostEntity?

    @Parameter(title: "Metric", default: .cpu)
    var metric: WidgetMetric
}

/// A host the widget has seen, offered in the configuration editor.
struct HostEntity: AppEntity {
    static let typeDisplayRepresentation: TypeDisplayRepresentation = "Host"
    static let defaultQuery = HostQuery()

    let id: String

    var displayRepresentation: DisplayRepresentation {
        DisplayRepresentation(title: "\(id)")
    }
}

struct HostQuery: EntityQuery {
    func entities(for identifiers: [String]) async throws -> [HostEntity] {
        identifiers.map(HostEntity.init(id:))
    }

    /// The hosts from the last fleet poll this widget saved. Before the first poll there are none to
    /// offer, which the editor shows as an empty list rather than a guess.
    func suggestedEntities() async throws -> [HostEntity] {
        WidgetStateStore.shared.knownHosts().map(HostEntity.init(id:))
    }
}

/// The metrics a widget can show: the ones the app charts for every host, plus temperature.
enum WidgetMetric: String, AppEnum {
    case cpu
    case memory
    case disk
    case load
    case temperature

    static let typeDisplayRepresentation: TypeDisplayRepresentation = "Metric"
    static let caseDisplayRepresentations: [WidgetMetric: DisplayRepresentation] = [
        .cpu: "CPU",
        .memory: "Memory",
        .disk: "Disk",
        .load: "Load",
        .temperature: "Temperature",
    ]

    var kind: MetricKindRecord {
        switch self {
        case .cpu: return .cpuUtilization
        case .memory: return .memoryUtilization
        case .disk: return .filesystemUtilization
        case .load: return .loadAverage1m
        case .temperature: return .temperature
        }
    }

    /// Temperature is a detail metric — one series per sensor, so never asked for fleet-wide — and
    /// reaches a widget only when it is configured for one host, which is what makes it the focused
    /// one.
    var needsFocus: Bool { self == .temperature }
}

// MARK: - Paging

/// A page change, from a button inside the widget.
///
/// It records the page and asks for nothing else. WidgetKit reloads the timeline after it, and the
/// provider sees the pending page change and renders from the saved poll rather than fetching: a tap
/// that waited on the network would take seconds and spend the refresh budget.
struct ChangePageIntent: AppIntent {
    static let title: LocalizedStringResource = "Change page"
    static let isDiscoverable = false

    @Parameter(title: "Widget") var key: String
    @Parameter(title: "Page") var page: Int

    init() {}

    init(key: String, page: Int) {
        self.key = key
        self.page = page
    }

    func perform() async throws -> some IntentResult {
        WidgetStateStore.shared.setPage(page, for: key)
        return .result()
    }
}

// MARK: - Timeline

struct FleetEntry: TimelineEntry {
    let date: Date
    let content: Content
    let metric: WidgetMetric
    /// The host the widget is configured for, or `nil` for the whole fleet.
    let scopeHostId: String?
    /// Where this widget's page index is saved, and the index itself.
    let pageKey: String
    let pageIndex: Int

    enum Content {
        /// The app has not handed the widget a connection yet.
        case notConnected
        /// Core's view and its verdict on how far to believe it. A failing backend still lands here
        /// when there is a saved poll to show: the age of a value is information, and an error that
        /// replaced it would throw that away.
        case fleet(FleetViewRecord, FreshnessRecord)
        /// Polling failed and there has never been anything to show.
        case nothingYet(PollFailureRecord?)
    }

    static func placeholder(family: WidgetFamily) -> FleetEntry {
        FleetEntry(
            date: Date(),
            content: .nothingYet(nil),
            metric: .cpu,
            scopeHostId: nil,
            pageKey: "placeholder",
            pageIndex: 0
        )
    }
}

struct FleetProvider: AppIntentTimelineProvider {
    func placeholder(in context: Context) -> FleetEntry {
        .placeholder(family: context.family)
    }

    /// The gallery and the configuration editor. Never polls: it is shown while someone is choosing,
    /// and a network call there would make the gallery wait on a backend.
    func snapshot(for configuration: FleetWidgetIntent, in context: Context) async -> FleetEntry {
        await entry(for: configuration, family: context.family, fetch: false)
    }

    func timeline(for configuration: FleetWidgetIntent, in context: Context) async -> Timeline<FleetEntry> {
        let scope = Scope(configuration)
        let pageKey = Self.pageKey(scope: scope, family: context.family)
        // A page tap reloads the timeline too, and must not fetch. The tap left a mark; consume it.
        let fetch = !WidgetStateStore.shared.takePendingPageChange(for: pageKey)
        let entry = await entry(for: configuration, family: context.family, fetch: fetch)
        // Fifteen minutes is WidgetKit's usual floor; asking for less does not refresh sooner.
        return Timeline(entries: [entry], policy: .after(Date().addingTimeInterval(15 * 60)))
    }

    private func entry(for configuration: FleetWidgetIntent, family: WidgetFamily, fetch: Bool) async -> FleetEntry {
        let scope = Scope(configuration)
        let pageKey = Self.pageKey(scope: scope, family: family)
        let pageIndex = WidgetStateStore.shared.page(for: pageKey)
        let content = await Self.content(scope: scope, metric: configuration.metric, fetch: fetch)
        return FleetEntry(
            date: Date(),
            content: content,
            metric: configuration.metric,
            scopeHostId: scope.hostId,
            pageKey: pageKey,
            pageIndex: pageIndex
        )
    }

    /// One session, restored from the last saved poll for this scope, polled if asked to, and saved
    /// again. Core computes the freshness verdict from the restored state, so the widget has no copy
    /// of those rules.
    private static func content(scope: Scope, metric: WidgetMetric, fetch: Bool) async -> FleetEntry.Content {
        let store = WidgetStateStore.shared
        let connection: SharedConnection
        do {
            guard let found = try KeychainSharedConnectionStore().read() else { return .notConnected }
            connection = found
        } catch {
            return .notConnected
        }

        let now = Date()
        let nowMillis = Int64(now.timeIntervalSince1970 * 1000)
        do {
            let defaults = try fleetConfigDefaults(environment: connection.environment)
            let config = FleetConfigRecord(
                environment: defaults.environment,
                tuning: defaults.tuning,
                rulesJson: defaults.rulesJson,
                overviewMetrics: defaults.overviewMetrics,
                detailMetrics: defaults.detailMetrics,
                // A host-scoped widget focuses that host, so its detail metrics — temperature among
                // them — are fetched for it, and only for it.
                focus: scope.hostId
            )
            let session = try FleetSession(
                baseUrl: connection.baseURL,
                apiKey: connection.apiKey,
                config: config,
                cachedStateJson: store.state(for: scope.key),
                // A widget is not where the app's own usage is measured.
                usage: UsageReporting.record(optedOut: true)
            )
            if fetch {
                // A failed poll is not thrown away: it lands in the session's state, and the
                // freshness verdict below reports it alongside whatever the last good poll showed.
                _ = try? await session.poll(nowMillis: nowMillis)
                if let exported = try? session.exportState() {
                    store.save(state: exported, for: scope.key)
                }
            }
            let view = session.view()
            if scope.hostId == nil {
                store.saveKnownHosts(view.hosts.map(\.id))
            }
            let freshness = try session.freshness(nowMillis: nowMillis)
            if view.hosts.isEmpty, case let .unusable(lastSuccess, _, failure) = freshness, lastSuccess == nil {
                return .nothingYet(failure)
            }
            return .fleet(view, freshness)
        } catch {
            return .nothingYet(nil)
        }
    }

    /// Paging is per widget size and scope: a square and a wide widget for the same fleet keep their
    /// own pages.
    static func pageKey(scope: Scope, family: WidgetFamily) -> String {
        "\(scope.key)|\(family)"
    }
}

/// What the widget is configured to show.
struct Scope {
    let hostId: String?

    init(_ configuration: FleetWidgetIntent) {
        hostId = configuration.host?.id
    }

    /// Where this scope's saved poll lives. The metric is not part of it: every metric of a scope
    /// comes from the same poll.
    var key: String { hostId.map { "host:\($0)" } ?? "fleet" }
}

// MARK: - Saved state

/// What the widget keeps between refreshes, in its own container.
///
/// The extension's own defaults rather than an App Group: nothing here is shared with the app, so it
/// needs no capability in the portal. Core's exported state is what lets a refresh that fails show
/// the last good values with their age, and what a page tap renders from.
final class WidgetStateStore: @unchecked Sendable {
    // `@unchecked`: the only state is `UserDefaults`, which is thread-safe.
    static let shared = WidgetStateStore()

    private let defaults = UserDefaults.standard

    func state(for scope: String) -> String? {
        defaults.string(forKey: "state.\(scope)")
    }

    func save(state: String, for scope: String) {
        defaults.set(state, forKey: "state.\(scope)")
    }

    func knownHosts() -> [String] {
        defaults.stringArray(forKey: "knownHosts") ?? []
    }

    func saveKnownHosts(_ hosts: [String]) {
        defaults.set(hosts, forKey: "knownHosts")
    }

    func page(for key: String) -> Int {
        defaults.integer(forKey: "page.\(key)")
    }

    func setPage(_ page: Int, for key: String) {
        defaults.set(page, forKey: "page.\(key)")
        defaults.set(true, forKey: "pendingPageChange.\(key)")
    }

    /// Whether the reload about to happen was caused by a page tap, clearing the mark as it answers.
    func takePendingPageChange(for key: String) -> Bool {
        let pending = defaults.bool(forKey: "pendingPageChange.\(key)")
        if pending { defaults.removeObject(forKey: "pendingPageChange.\(key)") }
        return pending
    }
}
