//
//  FleetFormat.swift
//  Pessimal — iOS client
//
//  Numbers into strings, and nothing else. Every value here arrives already normalised, already
//  judged and already carrying its unit; the only thing left to do with it is choose how many digits
//  to show.
//

import Foundation
#if canImport(PessimalFFI)
    import PessimalFFI
#endif

/// Display formatting for the records the fleet list renders.
///
/// A deliberate mirror of the macOS app's `MenuBarFormat`, down to the rules in the comments below:
/// both apps render the same normalised numbers and must render them the same way. The rules are
/// load-bearing rather than cosmetic, so they are restated here in full rather than summarised —
/// a summary is how the two copies come to disagree. Both types are platform-neutral and ought to be
/// one shared type; see this file's entry in the handoff notes.
///
/// `@MainActor` so the cached `RelativeDateTimeFormatter` — which is not thread-safe — is confined to
/// the actor every caller is already on.
@MainActor
enum FleetFormat {
    /// Shown wherever a number would be if there were one. One glyph, used everywhere, so that "no
    /// value" never gets confused with "zero".
    static let noValue = "—"

    // MARK: - Metric values

    /// A metric's newest value, formatted for its unit.
    ///
    /// `nil` latest is a real state — a series with one cumulative sample yields no rate at all — and
    /// it renders as ``noValue`` rather than as `0`.
    static func latestValue(of metric: MetricViewRecord) -> String {
        guard let latest = metric.latest else { return noValue }
        return value(latest.value, unit: metric.unit, isRate: metric.isRate)
    }

    /// One value, formatted for its unit.
    ///
    /// The rate suffix is chosen from the **unit**, not from `isRate` alone, because core's two rate
    /// metrics are normalised differently and share the flag: `NetworkIo` becomes bytes *per second*,
    /// while `AgentCollectionFailures` becomes the count added *since the previous sample* ("per
    /// bucket, not per second"). A blanket `/s` would put a unit on the second one that it does not
    /// have, which is exactly the kind of confident wrong number this app exists to catch.
    static func value(_ value: Double, unit: MetricUnitRecord, isRate: Bool) -> String {
        guard value.isFinite else { return noValue }

        switch unit {
        case .ratio:
            // Core's contract is a `0.0...1.0` ratio; `.percent` multiplies by 100.
            return value.formatted(.percent.precision(.fractionLength(0)))

        case .bytes:
            let bytes = byteCount(value)
            return isRate ? "\(bytes)/s" : bytes

        case .seconds:
            return duration(value)

        case .count:
            let count = value.formatted(.number.precision(.fractionLength(0)))
            // A delta, so the sign carries the meaning: "3 new failures", not "3 failures".
            return isRate ? "+\(count)" : count

        case .load:
            // Not a ratio and not a percentage — a run-queue depth, which is read to two places.
            return value.formatted(.number.precision(.fractionLength(2)))
        }
    }

    // MARK: - Instants

    /// How long ago an instant was, phrased relative to `now`.
    ///
    /// `now` is a parameter rather than `Date()` so a `TimelineView` tick and everything it draws
    /// agree about the present. Two rows disagreeing by the width of a redraw is a small lie, but it
    /// is the kind this codebase does not tell.
    static func age(sinceMillis millis: Int64, at now: Date) -> String {
        relativeFormatter.localizedString(for: date(fromMillis: millis), relativeTo: now)
    }

    /// A clock time, for "as of 14:32".
    static func time(millis: Int64) -> String {
        date(fromMillis: millis).formatted(date: .omitted, time: .shortened)
    }

    /// Epoch milliseconds as a `Date`. Every `_millis` field on the boundary is epoch UTC.
    static func date(fromMillis millis: Int64) -> Date {
        Date(timeIntervalSince1970: Double(millis) / 1000)
    }

    /// A countdown to a deadline, floored at zero.
    ///
    /// A deadline that has passed reads "now" rather than a negative number: the poll is due, and the
    /// timer fires the moment the run loop gets to it.
    static func countdown(to deadline: Date, at now: Date) -> String {
        let remaining = deadline.timeIntervalSince(now)
        guard remaining > 0.5 else { return "now" }
        return duration(remaining)
    }

    /// A span of seconds as an abbreviated duration: `4h 12m`, `45s`.
    static func duration(_ seconds: Double) -> String {
        guard seconds.isFinite, seconds >= 0 else { return noValue }
        // Above a century the units formatter is doing arithmetic on a number that cannot be an
        // uptime, so say so rather than print something absurd with confidence.
        guard seconds < 3_155_760_000 else { return noValue }

        let allowed: Set<Duration.UnitsFormatStyle.Unit> =
            seconds >= 86_400 ? [.days, .hours]
            : seconds >= 3_600 ? [.hours, .minutes]
            : seconds >= 60 ? [.minutes, .seconds]
            : [.seconds]

        return Duration.seconds(seconds).formatted(
            .units(allowed: allowed, width: .narrow, maximumUnitCount: 2)
        )
    }

    // MARK: - Counts

    /// `1 host` / `2 hosts`, and the same for anything else that comes in ones.
    static func count(_ value: UInt32, singular: String, plural: String) -> String {
        "\(value) \(value == 1 ? singular : plural)"
    }

    // MARK: - Private

    /// Held rather than rebuilt because a `TimelineView` redraws this once per second per row, and
    /// `RelativeDateTimeFormatter` is expensive to construct. Safe as shared state only because the
    /// enclosing type is `@MainActor`.
    private static let relativeFormatter: RelativeDateTimeFormatter = {
        let formatter = RelativeDateTimeFormatter()
        formatter.unitsStyle = .abbreviated
        return formatter
    }()

    private static func byteCount(_ value: Double) -> String {
        // `Double(Int64.max)` rounds *up* past `Int64.max`, so converting a clamped double with it
        // traps. 2^53 is the largest integer a `Double` represents exactly, which is both a safe
        // ceiling and far past any real byte count.
        let ceiling = 9_007_199_254_740_992.0
        let clamped = min(max(value.rounded(), 0), ceiling)
        return ByteCountFormatter.string(fromByteCount: Int64(clamped), countStyle: .memory)
    }
}
