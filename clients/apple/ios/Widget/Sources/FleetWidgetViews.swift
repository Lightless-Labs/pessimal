//
//  FleetWidgetViews.swift
//  Pessimal — iOS widget extension
//
//  What the fleet widget draws. Every decision about *what* to show — the density for the fleet
//  size, the page, which tallies, how far to believe the data — is made elsewhere (PessimalKit's
//  WidgetDensity, WidgetPaging and FleetTally, and core's freshness verdict). This file draws it.
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

struct FleetWidgetView: View {
    @Environment(\.widgetFamily) private var family
    let entry: FleetEntry

    var body: some View {
        content
            .containerBackground(.fill.tertiary, for: .widget)
    }

    @ViewBuilder
    private var content: some View {
        switch entry.content {
        case .notConnected:
            NotConnectedView(family: family)

        case let .nothingYet(failure):
            NothingYetView(family: family, failure: failure)

        case let .fleet(view, freshness):
            switch family {
            case .accessoryInline:
                InlineView(entry: entry, fleet: view)
            case .accessoryCircular:
                CircularView(entry: entry, fleet: view)
            case .accessoryRectangular:
                RectangularView(entry: entry, fleet: view, freshness: freshness)
            case .systemMedium:
                HomeView(entry: entry, fleet: view, freshness: freshness, size: .wide)
            case .systemLarge:
                HomeView(entry: entry, fleet: view, freshness: freshness, size: .large)
            default:
                HomeView(entry: entry, fleet: view, freshness: freshness, size: .square)
            }
        }
    }
}

// MARK: - Home screen

struct HomeView: View {
    let entry: FleetEntry
    let fleet: FleetViewRecord
    let freshness: FreshnessRecord
    let size: WidgetSize

    /// The hosts this widget is about: one when it is configured for a host, the fleet otherwise.
    private var hosts: [HostViewRecord] {
        guard let id = entry.scopeHostId else { return fleet.hosts }
        return fleet.hosts.filter { $0.id == id }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            page
            Spacer(minLength: 0)
            footer
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        // Past the freshness budget the values stay, dimmed: the app does the same, for the same
        // reason — the age of a value is worth more than an error that replaced it.
        .opacity(isUnusable ? 0.55 : 1)
    }

    @ViewBuilder
    private var page: some View {
        if entry.scopeHostId != nil {
            if let host = hosts.first {
                HostDetail(host: host, metric: entry.metric, size: size)
            } else {
                Text("This host is not in the last poll.").font(.caption)
            }
        } else {
            switch WidgetPaging.page(at: entry.pageIndex, size: size, hostCount: hosts.count) {
            case .start:
                start
            case let .host(index):
                HostDetail(host: hosts[index], metric: entry.metric, size: size)
            }
        }
    }

    @ViewBuilder
    private var start: some View {
        switch WidgetDensity.choose(size: size, hostCount: hosts.count) {
        case .singleHost:
            HostDetail(host: hosts[0], metric: entry.metric, size: size)
        case let .rows(limit):
            HostRows(hosts: Array(hosts.prefix(limit)), metric: entry.metric, size: size)
        case .twoLineRows:
            HostTwoLineRows(hosts: hosts, metric: entry.metric)
        case .summary:
            Summary(fleet: fleet)
        case let .summaryAndRows(limit):
            if size == .wide {
                HStack(alignment: .top, spacing: 12) {
                    Summary(fleet: fleet)
                    HostRows(hosts: Array(hosts.prefix(limit)), metric: entry.metric, size: size)
                }
            } else {
                Summary(fleet: fleet)
                HostRows(hosts: Array(hosts.prefix(limit)), metric: entry.metric, size: size)
            }
        }
    }

    /// The age of the data when it is not fresh, and the paging controls when there are pages.
    private var footer: some View {
        HStack(spacing: 8) {
            FreshnessMark(freshness: freshness)
            Spacer(minLength: 0)
            if entry.scopeHostId == nil {
                PagingControls(entry: entry, size: size, hostCount: hosts.count)
            }
        }
        .font(.caption2)
    }

    private var isUnusable: Bool {
        if case .unusable = freshness { return true }
        return false
    }
}

/// One host, in as much detail as the size allows.
struct HostDetail: View {
    let host: HostViewRecord
    let metric: WidgetMetric
    let size: WidgetSize

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 4) {
                LivenessDot(liveness: host.liveness)
                Text(host.id).font(.caption.weight(.semibold)).lineLimit(1).truncationMode(.middle)
            }
            switch size {
            case .square:
                Text(WidgetFormat.value(of: metric, on: host))
                    .font(.title.weight(.semibold))
                    .minimumScaleFactor(0.6)
                    .lineLimit(1)
                Text(WidgetFormat.name(metric)).font(.caption2).foregroundStyle(.secondary)
            case .wide:
                HStack(spacing: 14) {
                    ForEach(WidgetFormat.featured(around: metric, count: 3), id: \.self) { shown in
                        MetricCell(metric: shown, host: host)
                    }
                }
            case .large:
                ForEach(WidgetFormat.featured(around: metric, count: WidgetMetric.allCases.count), id: \.self) { shown in
                    HStack {
                        Text(WidgetFormat.name(shown)).font(.caption).foregroundStyle(.secondary)
                        Spacer()
                        Text(WidgetFormat.value(of: shown, on: host)).font(.caption.weight(.medium))
                    }
                }
            }
        }
        .monospacedDigit()
    }
}

struct MetricCell: View {
    let metric: WidgetMetric
    let host: HostViewRecord

    var body: some View {
        VStack(alignment: .leading, spacing: 1) {
            Text(WidgetFormat.value(of: metric, on: host)).font(.headline).lineLimit(1)
            Text(WidgetFormat.name(metric)).font(.caption2).foregroundStyle(.secondary)
        }
    }
}

/// Two lines per host, for the wide widget with two to four hosts — the owner's design.
///
/// First the liveness and the name, then three metrics across the full width, each given an equal
/// share and aligned to the start of it so the columns line up from host to host. The chosen metric
/// comes first, then the others in their usual order: CPU, memory, disk by default.
///
/// Drawn at the largest of three type sizes that fits. A wide widget's content is 116 to 126 points
/// tall depending on the iPhone, and four hosts at two lines each need about 123 at caption size, so
/// a single size would clip the last line on the smaller phones. `ViewThatFits` measures instead of
/// guessing per device.
struct HostTwoLineRows: View {
    let hosts: [HostViewRecord]
    let metric: WidgetMetric

    var body: some View {
        ViewThatFits(in: .vertical) {
            // Three hosts or fewer try the larger type first; four start at the size that fits most
            // phones, and fall back to the smallest.
            if hosts.count < 4 {
                rows(.regular)
            }
            rows(.compact)
            rows(.tight)
        }
        .monospacedDigit()
    }

    /// The hosts spread over the widget's height rather than stacked at its top: a block of rows with
    /// an empty band under it is what the owner first pointed at. The style's spacing is the minimum
    /// gap, so `ViewThatFits` still measures each style at its tightest.
    private func rows(_ style: Style) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            ForEach(Array(hosts.enumerated()), id: \.element.id) { index, host in
                if index > 0 {
                    Spacer(minLength: style.hostSpacing)
                }
                VStack(alignment: .leading, spacing: style.lineSpacing) {
                    HStack(spacing: 5) {
                        LivenessDot(liveness: host.liveness, diameter: style.dot)
                        Text(WidgetFormat.shortName(of: host))
                            .font(style.name)
                            .lineLimit(1)
                            .truncationMode(.middle)
                    }
                    HStack(spacing: 0) {
                        ForEach(WidgetFormat.featured(around: metric, count: 3), id: \.self) { shown in
                            HStack(spacing: 3) {
                                Text(WidgetFormat.compactName(shown)).foregroundStyle(.secondary)
                                Text(WidgetFormat.value(of: shown, on: host))
                            }
                            .font(style.values)
                            .lineLimit(1)
                            .frame(maxWidth: .infinity, alignment: .leading)
                        }
                    }
                }
                .accessibilityElement(children: .combine)
            }
        }
        // Flexible height, so the spacers between hosts have room to grow. `ViewThatFits` still tests
        // each style at its ideal height, which is its tightest.
        .frame(maxHeight: .infinity, alignment: .top)
    }

    private struct Style {
        let name: Font
        let values: Font
        let hostSpacing: CGFloat
        let lineSpacing: CGFloat
        let dot: CGFloat

        static let regular = Style(
            name: .subheadline.weight(.semibold), values: .caption, hostSpacing: 7, lineSpacing: 2, dot: 8
        )
        static let compact = Style(
            name: .caption.weight(.semibold), values: .caption2, hostSpacing: 3, lineSpacing: 1, dot: 7
        )
        static let tight = Style(
            name: .caption2.weight(.semibold), values: .caption2, hostSpacing: 1, lineSpacing: 0, dot: 6
        )
    }
}

/// A row per host: liveness and name, and the chosen metric where there is room for it.
struct HostRows: View {
    let hosts: [HostViewRecord]
    let metric: WidgetMetric
    let size: WidgetSize

    var body: some View {
        VStack(alignment: .leading, spacing: 3) {
            ForEach(hosts, id: \.id) { host in
                HStack(spacing: 4) {
                    LivenessDot(liveness: host.liveness)
                    Text(host.id).font(.caption2).lineLimit(1).truncationMode(.middle)
                    if size != .square {
                        Spacer(minLength: 4)
                        Text(WidgetFormat.value(of: metric, on: host)).font(.caption2.weight(.medium))
                        if size == .large {
                            Text(WidgetFormat.value(of: WidgetFormat.second(after: metric), on: host))
                                .font(.caption2)
                                .foregroundStyle(.secondary)
                        }
                    }
                }
            }
        }
        .monospacedDigit()
    }
}

/// The fleet's worst severity and the tallies `FleetTally` says are worth showing.
struct Summary: View {
    let fleet: FleetViewRecord

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            Image(systemName: WidgetFormat.symbol(for: fleet.severity))
                .foregroundStyle(WidgetFormat.tint(for: fleet.severity))
                .font(.title3)
            ForEach(FleetTally.visible(in: fleet.counts), id: \.tally) { entry in
                Text("\(entry.value) \(entry.tally.word(for: entry.value))")
                    .font(.caption)
            }
        }
        .monospacedDigit()
        // Every count spoken, including the hidden zeroes, as the apps do.
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(FleetTally.spokenSummary(of: fleet.counts))
    }
}

/// Back, forward, and home. Present only when there are pages.
struct PagingControls: View {
    let entry: FleetEntry
    let size: WidgetSize
    let hostCount: Int

    var body: some View {
        let pages = WidgetPaging.pages(size: size, hostCount: hostCount)
        if pages > 1 {
            let index = min(max(entry.pageIndex, 0), pages - 1)
            HStack(spacing: 10) {
                if index > 0 {
                    Button(intent: ChangePageIntent(key: entry.pageKey, page: 0)) {
                        Image(systemName: "arrow.uturn.backward")
                    }
                    .accessibilityLabel("Back to start")

                    Button(intent: ChangePageIntent(key: entry.pageKey, page: WidgetPaging.previous(before: index))) {
                        Image(systemName: "chevron.left")
                    }
                    .accessibilityLabel(index == 1 ? "Back to start" : "Previous host")
                }
                if index < pages - 1 {
                    Button(intent: ChangePageIntent(
                        key: entry.pageKey,
                        page: WidgetPaging.next(after: index, size: size, hostCount: hostCount)
                    )) {
                        Image(systemName: "chevron.right")
                    }
                    .accessibilityLabel("Next host")
                }
            }
            .buttonStyle(.plain)
            .foregroundStyle(.secondary)
        }
    }
}

// MARK: - Lock screen

struct InlineView: View {
    let entry: FleetEntry
    let fleet: FleetViewRecord

    var body: some View {
        if let id = entry.scopeHostId, let host = fleet.hosts.first(where: { $0.id == id }) {
            Text("\(host.id) \(WidgetFormat.value(of: entry.metric, on: host))")
        } else {
            Text("\(fleet.counts.alive)/\(fleet.counts.hosts) alive")
        }
    }
}

struct CircularView: View {
    let entry: FleetEntry
    let fleet: FleetViewRecord

    var body: some View {
        if let id = entry.scopeHostId, let host = fleet.hosts.first(where: { $0.id == id }),
           let ratio = WidgetFormat.ratio(of: entry.metric, on: host)
        {
            Gauge(value: ratio) {
                Text(WidgetFormat.shortName(entry.metric))
            } currentValueLabel: {
                Text(WidgetFormat.value(of: entry.metric, on: host))
            }
            .gaugeStyle(.accessoryCircular)
        } else {
            let total = max(Double(fleet.counts.hosts), 1)
            Gauge(value: Double(fleet.counts.alive) / total) {
                Text("alive")
            } currentValueLabel: {
                Text("\(fleet.counts.alive)")
            }
            .gaugeStyle(.accessoryCircular)
        }
    }
}

struct RectangularView: View {
    let entry: FleetEntry
    let fleet: FleetViewRecord
    let freshness: FreshnessRecord

    var body: some View {
        VStack(alignment: .leading, spacing: 1) {
            if let id = entry.scopeHostId, let host = fleet.hosts.first(where: { $0.id == id }) {
                Text(host.id).font(.headline).lineLimit(1)
                Text("\(WidgetFormat.name(entry.metric)) \(WidgetFormat.value(of: entry.metric, on: host))")
                Text(WidgetFormat.livenessName(host.liveness)).foregroundStyle(.secondary)
            } else {
                Text("\(fleet.counts.alive)/\(fleet.counts.hosts) alive").font(.headline)
                if let worst = fleet.hosts.first, worst.liveness != .alive {
                    Text("\(worst.id) \(WidgetFormat.livenessName(worst.liveness))").lineLimit(1)
                }
                FreshnessMark(freshness: freshness)
            }
        }
        .monospacedDigit()
    }
}

// MARK: - Fallbacks

struct NotConnectedView: View {
    let family: WidgetFamily

    var body: some View {
        if family == .accessoryInline {
            Text("Open Pessimal to connect")
        } else {
            VStack(alignment: .leading, spacing: 4) {
                Image(systemName: "link.badge.plus").font(.title3)
                Text("Open Pessimal to connect").font(.caption)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        }
    }
}

/// Polling failed and there has never been anything to show. A refused key says so, because the
/// fix is in the app's settings; anything else is "can't reach", because to a widget no network, a
/// backend that is down and a DNS failure are the same state.
struct NothingYetView: View {
    let family: WidgetFamily
    let failure: PollFailureRecord?

    private var message: String {
        switch failure?.kind {
        case .some(.unauthorized): return "Your key was refused — open Pessimal"
        case .some: return "Can't reach your backend"
        case .none: return "Waiting for the first poll…"
        }
    }

    var body: some View {
        if family == .accessoryInline {
            Text(message)
        } else {
            VStack(alignment: .leading, spacing: 4) {
                Image(systemName: failure == nil ? "clock" : "wifi.exclamationmark").font(.title3)
                Text(message).font(.caption)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        }
    }
}

/// When the data is from, if it is not current. Nothing when it is: a mark that is always there is a
/// mark nobody reads.
struct FreshnessMark: View {
    let freshness: FreshnessRecord

    var body: some View {
        switch freshness {
        case .fresh, .unattempted:
            EmptyView()
        case let .idle(lastSuccessMillis):
            Label(WidgetFormat.time(millis: lastSuccessMillis), systemImage: "clock")
                .foregroundStyle(.secondary)
        case let .degraded(lastSuccessMillis, _, _):
            Label(WidgetFormat.time(millis: lastSuccessMillis), systemImage: "exclamationmark.triangle.fill")
                .foregroundStyle(.orange)
        case let .unusable(lastSuccessMillis, _, _):
            Label(lastSuccessMillis.map(WidgetFormat.time(millis:)) ?? "no data", systemImage: "xmark.octagon.fill")
                .foregroundStyle(.red)
        }
    }
}

struct LivenessDot: View {
    let liveness: LivenessRecord
    var diameter: CGFloat = 6

    var body: some View {
        Circle()
            .fill(WidgetFormat.tint(for: liveness))
            .frame(width: diameter, height: diameter)
            .accessibilityLabel(WidgetFormat.livenessName(liveness))
    }
}

// MARK: - Formatting

/// The widget's own wording and number formats. Each app has its own (`FleetFormat` on iOS,
/// `MenuBarFormat` on macOS) because presentation is the UI layer's; this is the widget's.
enum WidgetFormat {
    /// The value of a metric on a host, or a dash when the host does not report it. Temperature is
    /// the hottest sensor, because one number has to stand for many and the hottest is the one that
    /// matters; a filesystem is the root mount when there is one.
    static func value(of metric: WidgetMetric, on host: HostViewRecord) -> String {
        guard let series = series(of: metric, on: host), let value = series.latest?.value else { return "—" }
        switch series.unit {
        case .ratio: return "\(Int((value * 100).rounded()))%"
        case .celsius: return "\(Int(value.rounded()))°"
        case .load: return String(format: "%.2f", value)
        case .bytes: return ByteCountFormatter.string(fromByteCount: Int64(value), countStyle: .binary)
        case .seconds: return "\(Int(value / 86400))d"
        case .count: return "\(Int(value))"
        }
    }

    /// The metric as a fraction of its whole, for a gauge, when it has one.
    static func ratio(of metric: WidgetMetric, on host: HostViewRecord) -> Double? {
        guard let series = series(of: metric, on: host), let value = series.latest?.value else { return nil }
        switch series.unit {
        case .ratio: return min(max(value, 0), 1)
        // A gauge needs a whole, and a temperature has none of its own; 100° is a ceiling no sensor
        // in a working machine reaches, so the needle sits where a reader expects.
        case .celsius: return min(max(value / 100, 0), 1)
        default: return nil
        }
    }

    private static func series(of metric: WidgetMetric, on host: HostViewRecord) -> MetricViewRecord? {
        let matching = host.metrics.filter { $0.kind == metric.kind && $0.latest != nil }
        switch metric {
        case .temperature:
            // A fold, not `max(by:)`: `>` over a NaN is false both ways, which is not an ordering.
            return matching.reduce(nil) { hottest, candidate in
                guard let value = candidate.latest?.value, value.isFinite else { return hottest }
                guard let current = hottest?.latest?.value else { return candidate }
                return value > current ? candidate : hottest
            }
        case .disk:
            return matching.first { $0.label == "/" } ?? matching.first
        default:
            return matching.first
        }
    }

    /// A host's name without its domain: "bad-blintz-mini-m4-16gb-2024.home" is
    /// "bad-blintz-mini-m4-16gb-2024". A column is narrow, and the suffix is the same on every host
    /// of a home or office network, so it says the least. An address is left whole.
    static func shortName(of host: HostViewRecord) -> String {
        let id = host.id
        if id.split(separator: ".").allSatisfy({ Int($0) != nil }) { return id }
        return id.split(separator: ".", maxSplits: 1).first.map(String.init) ?? id
    }

    static func name(_ metric: WidgetMetric) -> String {
        switch metric {
        case .cpu: return "CPU"
        case .memory: return "Memory"
        case .disk: return "Disk"
        case .load: return "Load"
        case .temperature: return "Temperature"
        }
    }

    /// The labels on the second line of a two-line row, as the owner wrote them: "CPU", "MEM", "Disk".
    static func compactName(_ metric: WidgetMetric) -> String {
        switch metric {
        case .cpu: return "CPU"
        case .memory: return "MEM"
        case .disk: return "Disk"
        case .load: return "Load"
        case .temperature: return "Temp"
        }
    }

    static func shortName(_ metric: WidgetMetric) -> String {
        switch metric {
        case .cpu: return "CPU"
        case .memory: return "MEM"
        case .disk: return "DSK"
        case .load: return "LD"
        case .temperature: return "°C"
        }
    }

    /// The chosen metric first, then the others in their usual order, for the layouts that show
    /// several.
    static func featured(around metric: WidgetMetric, count: Int) -> [WidgetMetric] {
        Array(([metric] + WidgetMetric.allCases.filter { $0 != metric }).prefix(count))
    }

    /// The metric shown second in the large widget's rows.
    static func second(after metric: WidgetMetric) -> WidgetMetric {
        metric == .memory ? .cpu : .memory
    }

    static func time(millis: Int64) -> String {
        Date(timeIntervalSince1970: Double(millis) / 1000).formatted(date: .omitted, time: .shortened)
    }

    static func livenessName(_ liveness: LivenessRecord) -> String {
        switch liveness {
        case .alive: return "Alive"
        case .stale: return "Stale"
        case .down: return "Down"
        case .unknown: return "No heartbeat yet"
        }
    }

    static func tint(for liveness: LivenessRecord) -> Color {
        switch liveness {
        case .alive: return .green
        case .stale: return .orange
        case .down: return .red
        case .unknown: return .secondary
        }
    }

    static func symbol(for severity: SeverityRecord) -> String {
        switch severity {
        case .ok: return "checkmark.circle"
        case .unknown: return "questionmark.circle"
        case .warning: return "exclamationmark.triangle.fill"
        case .critical: return "exclamationmark.octagon.fill"
        }
    }

    static func tint(for severity: SeverityRecord) -> Color {
        switch severity {
        case .ok: return .green
        case .unknown: return .secondary
        case .warning: return .orange
        case .critical: return .red
        }
    }
}

