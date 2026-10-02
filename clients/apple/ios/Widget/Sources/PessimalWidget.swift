//
//  PessimalWidget.swift
//  Pessimal — iOS widget extension
//
//  M11 stage 0: the memory measurement. This widget polls the backend for real, through the same
//  Rust core as the app, and prints how much memory that cost. WidgetKit kills an extension at
//  30 MB, the core brings tokio, reqwest and rustls, and nobody has measured what one poll costs in
//  this process. The simulator does not enforce the limit, so the number that decides the design
//  comes from a phone — and the only way onto a phone without anyone building locally is TestFlight,
//  with the number on screen. See docs/plans/2026-10-02-m11-ios-widgets.md.
//

import Darwin
import SwiftUI
import WidgetKit
#if canImport(PessimalFFI)
    import PessimalFFI
#endif
#if canImport(PessimalKit)
    import PessimalKit
#endif

@main
struct PessimalWidgetBundle: WidgetBundle {
    var body: some Widget {
        MemoryProbeWidget()
    }
}

struct MemoryProbeWidget: Widget {
    var body: some WidgetConfiguration {
        StaticConfiguration(kind: "com.lightless-labs.pessimal.memory-probe", provider: MemoryProbeProvider()) { entry in
            MemoryProbeView(entry: entry)
                .containerBackground(.fill.tertiary, for: .widget)
        }
        .configurationDisplayName("Pessimal (measurement)")
        .description("Polls your fleet and shows what one poll costs in memory.")
        .supportedFamilies([.systemSmall])
    }
}

// MARK: - Timeline

struct MemoryProbeEntry: TimelineEntry {
    let date: Date
    let outcome: Outcome
    /// Resident footprint after the poll, and the process's high-water mark — the peak is the one
    /// WidgetKit's limit is measured against.
    let footprint: Footprint?

    enum Outcome {
        case polled(hosts: UInt32, alive: UInt32, seconds: Double)
        case noConnection
        case failed(String)
    }
}

struct Footprint {
    let currentBytes: UInt64
    let peakBytes: UInt64
}

struct MemoryProbeProvider: TimelineProvider {
    func placeholder(in _: Context) -> MemoryProbeEntry {
        MemoryProbeEntry(date: Date(), outcome: .polled(hosts: 3, alive: 3, seconds: 0.4), footprint: nil)
    }

    func getSnapshot(in context: Context, completion: @escaping (MemoryProbeEntry) -> Void) {
        // The gallery preview must not poll: it is shown while someone is choosing a widget.
        completion(placeholder(in: context))
    }

    func getTimeline(in _: Context, completion: @escaping (Timeline<MemoryProbeEntry>) -> Void) {
        Task {
            let entry = await Self.measure()
            // Fifteen minutes is WidgetKit's usual floor; asking for less does not refresh sooner.
            let next = Date().addingTimeInterval(15 * 60)
            completion(Timeline(entries: [entry], policy: .after(next)))
        }
    }

    /// One real poll, timed, then the footprint read after it.
    private static func measure() async -> MemoryProbeEntry {
        let started = Date()
        let outcome: MemoryProbeEntry.Outcome
        do {
            guard let connection = try KeychainSharedConnectionStore().read() else {
                return MemoryProbeEntry(date: started, outcome: .noConnection, footprint: footprint())
            }
            let session = try FleetSession(
                baseUrl: connection.baseURL,
                apiKey: connection.apiKey,
                config: try fleetConfigDefaults(environment: connection.environment),
                cachedStateJson: nil,
                // Opted out: a widget is not where the app's own usage is measured, and a second
                // reporter would be memory spent on something other than the measurement.
                usage: UsageReporting.record(optedOut: true)
            )
            let result = try await session.poll(nowMillis: Int64(started.timeIntervalSince1970 * 1000))
            outcome = .polled(
                hosts: result.view.counts.hosts,
                alive: result.view.counts.alive,
                seconds: Date().timeIntervalSince(started)
            )
        } catch {
            outcome = .failed(String(describing: error))
        }
        return MemoryProbeEntry(date: started, outcome: outcome, footprint: footprint())
    }

    /// The process's own accounting, from `task_vm_info`: `phys_footprint` is what Jetsam counts, and
    /// `ledger_phys_footprint_peak` its high-water mark since launch.
    private static func footprint() -> Footprint? {
        var info = task_vm_info_data_t()
        var count = mach_msg_type_number_t(
            MemoryLayout<task_vm_info_data_t>.size / MemoryLayout<natural_t>.size
        )
        let status = withUnsafeMutablePointer(to: &info) { pointer in
            pointer.withMemoryRebound(to: integer_t.self, capacity: Int(count)) { raw in
                task_info(mach_task_self_, task_flavor_t(TASK_VM_INFO), raw, &count)
            }
        }
        guard status == KERN_SUCCESS else { return nil }
        // The peak counter is signed in the struct, the current one is not; a negative peak is not a
        // reading anyone could act on, so it is floored rather than trapped on.
        return Footprint(
            currentBytes: info.phys_footprint,
            peakBytes: UInt64(max(0, info.ledger_phys_footprint_peak))
        )
    }
}

// MARK: - View

struct MemoryProbeView: View {
    let entry: MemoryProbeEntry

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            if let footprint = entry.footprint {
                Text("peak \(megabytes(footprint.peakBytes))")
                    .font(.title3.weight(.semibold))
                    .foregroundStyle(tint(for: footprint.peakBytes))
                Text("now \(megabytes(footprint.currentBytes)) · limit 30 MB")
                    .font(.caption2)
                    .foregroundStyle(.secondary)
            } else {
                Text("footprint unavailable").font(.caption)
            }

            Spacer(minLength: 2)

            switch entry.outcome {
            case let .polled(hosts, alive, seconds):
                Text("\(alive)/\(hosts) alive").font(.caption)
                Text(String(format: "poll %.1fs", seconds)).font(.caption2).foregroundStyle(.secondary)
            case .noConnection:
                Text("Open Pessimal once to connect.").font(.caption2)
            case let .failed(message):
                Text(message).font(.caption2).lineLimit(3).foregroundStyle(.red)
            }

            Text(entry.date, style: .time).font(.caption2).foregroundStyle(.tertiary)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .monospacedDigit()
    }

    private func megabytes(_ bytes: UInt64) -> String {
        String(format: "%.1f MB", Double(bytes) / 1_048_576)
    }

    /// Green well clear of the limit, amber within ten megabytes of it, red at it. The thresholds
    /// are the plan's: under ~20 builds as designed, close to 30 needs a narrower entry point.
    private func tint(for bytes: UInt64) -> Color {
        let megabytes = Double(bytes) / 1_048_576
        if megabytes < 20 { return .green }
        if megabytes < 30 { return .orange }
        return .red
    }
}
