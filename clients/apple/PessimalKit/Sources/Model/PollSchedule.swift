//
//  PollSchedule.swift
//  Pessimal — macOS menu bar client
//
//  The only file in the app that turns core's advice into a number of seconds. If you are looking
//  for an invented interval, a hardcoded retry, or a reimplemented backoff, this is the one place
//  it could hide — which is exactly why it is separated from the model and made pure.
//

import Foundation
#if canImport(PessimalFFI)
    // Built as its own module for iOS, compiled into the app's module on macOS. See the note in
    // `FleetModel.swift`.
    import PessimalFFI
#endif

/// What the poll loop does next, derived from ``PessimalFFI/PollAdviceRecord`` and nothing else.
enum PollSchedule: Equatable {
    /// Come back after this many seconds.
    ///
    /// The number is core's. Swift's only contribution is jitter, and jitter can only ever make
    /// the wait longer — see ``PollSchedule/from(_:jitterFraction:randomUnitInterval:)``.
    case wait(TimeInterval)

    /// Do not come back. Core has decided that retrying cannot help.
    case halt(reason: PollFailureKindRecord, message: String)
}

extension PollSchedule {
    /// Translates one poll's advice into a delay, or into a stop.
    ///
    /// Jitter is **multiplicative and additive-only**: `seconds × (1 + fraction × u)`, `u` drawn
    /// from `0...1`. Both properties are load-bearing.
    ///
    /// - *Multiplicative* keeps `poll(afterSeconds: 0)` at zero. That zero is not a degenerate
    ///   case: core emits it for a state that has not folded this session — a cold start, a
    ///   relaunch, a session just rebuilt for a config change — so the app shows data instead of
    ///   sleeping an interval first. Additive jitter would push it off zero and quietly restore
    ///   the delay the zero exists to remove.
    /// - *Additive-only* means the wait is never shorter than core asked for. Jitter is a
    ///   courtesy to the backend, and one that could subtract would be a Swift-side decision to
    ///   poll more often than core sanctioned.
    ///
    /// Jitter belongs here rather than in core for the reason core's own documentation gives: its
    /// backoff is deterministic and unjittered so that a schedule is exact in a test. Spreading a
    /// fleet of clients that all launch at login is the caller's job.
    ///
    /// - Parameter randomUnitInterval: injected so a test can pin the draw. Values outside
    ///   `0...1` are clamped rather than trusted.
    static func from(
        _ advice: PollAdviceRecord,
        jitterFraction: Double,
        randomUnitInterval: () -> Double = { Double.random(in: 0 ... 1) }
    ) -> PollSchedule {
        switch advice {
        case let .poll(afterSeconds):
            return .wait(jittered(afterSeconds, fraction: jitterFraction, draw: randomUnitInterval))

        case let .retry(afterSeconds, _):
            // `consecutiveFailures` rides along so the UI can explain a long wait ("4th attempt").
            // It is emphatically not a multiplier for Swift to apply: the curve is already in
            // `afterSeconds`, and raising it here would be backing off twice from one failure.
            return .wait(jittered(afterSeconds, fraction: jitterFraction, draw: randomUnitInterval))

        case let .stop(reason, message):
            // No delay to compute. A wrong API key does not become a right one by being asked
            // again, and core has said so.
            return .halt(reason: reason, message: message)
        }
    }

    private static func jittered(
        _ seconds: Int64,
        fraction: Double,
        draw: () -> Double
    ) -> TimeInterval {
        // A negative interval is not something core produces. The floor guards against a corrupt
        // lift, not against a cadence we disagree with.
        let base = TimeInterval(max(0, seconds))
        let fraction = fraction.clamped(to: 0 ... 1)
        return base * (1 + fraction * draw().clamped(to: 0 ... 1))
    }
}

private extension Double {
    func clamped(to range: ClosedRange<Double>) -> Double {
        // `isNaN` would sail through `min`/`max` and produce a NaN deadline, which `Date` turns
        // into a distant-future poll that never happens.
        guard !isNaN else { return range.lowerBound }
        return Swift.min(Swift.max(self, range.lowerBound), range.upperBound)
    }
}
