//
//  FleetStyle.swift
//  Pessimal — iOS client
//
//  Symbols, tints and nouns for verdicts the core has already reached. Nothing in this file decides
//  whether a host is alive or a fleet is critical; it decides what shape "critical" is, which is the
//  whole of the UI layer's authority over these values.
//

#if canImport(PessimalFFI)
    import PessimalFFI
#endif
import SwiftUI

/// Presentation for the core's verdicts, in one place so that two screens cannot draw `Warning` two
/// different ways.
///
/// The words and the symbols are **deliberately identical** to the macOS app's `MenuBarStyle`. They
/// are one product's vocabulary, and an operator who has learned that a heart means liveness on the
/// Mac must not have to learn something else on the phone. Both types are platform-neutral and ought
/// to be one shared type; see this file's entry in the handoff notes.
enum FleetStyle {

    // MARK: - Severity

    /// The SF Symbol for a severity.
    ///
    /// Chosen to differ in **outline shape**, not only in colour, so that the distinction survives a
    /// monochrome rendering — a tinted widget, an accessibility setting that flattens colour, a
    /// screenshot in a bug report. A mapping that leant on red-versus-amber would make `Critical`
    /// and `Warning` identical in all three.
    static func symbolName(for severity: SeverityRecord) -> String {
        switch severity {
        case .ok: return "checkmark.circle"
        case .unknown: return "questionmark.circle"
        case .warning: return "exclamationmark.triangle.fill"
        case .critical: return "exclamationmark.octagon.fill"
        }
    }

    /// The tint for a severity.
    static func tint(for severity: SeverityRecord) -> Color {
        switch severity {
        case .ok: return .green
        case .unknown: return .secondary
        case .warning: return .orange
        case .critical: return .red
        }
    }

    /// The word for a severity. Core does not ship one — `SeverityRecord` is an enum with no message
    /// — so this is the app's copy, and the only copy.
    static func name(for severity: SeverityRecord) -> String {
        switch severity {
        case .ok: return "OK"
        case .unknown: return "Unknown"
        case .warning: return "Warning"
        case .critical: return "Critical"
        }
    }

    // MARK: - Liveness

    /// The word for a liveness verdict.
    ///
    /// `Unknown` is "No heartbeat yet" rather than "Unknown" because core is explicit that it means
    /// *never seen*, which is not the same as *not judged* and reads very differently to an operator
    /// staring at a host they just installed.
    static func name(for liveness: LivenessRecord) -> String {
        switch liveness {
        case .alive: return "Alive"
        case .stale: return "Stale"
        case .down: return "Down"
        case .unknown: return "No heartbeat yet"
        }
    }

    /// A small glyph for a liveness verdict, for the trailing edge of a host row.
    static func symbolName(for liveness: LivenessRecord) -> String {
        switch liveness {
        case .alive: return "heart.fill"
        case .stale: return "heart"
        case .down: return "heart.slash"
        case .unknown: return "heart.slash.circle"
        }
    }

    // MARK: - Platform

    /// The operating system family's name.
    static func name(for os: OsFamilyRecord) -> String {
        switch os {
        case .linux: return "Linux"
        case .darwin: return "macOS"
        case .windows: return "Windows"
        case .other: return "Unknown OS"
        }
    }

    // MARK: - Availability

    /// Why a metric has no current value, or `nil` when it has one.
    ///
    /// The three non-`present` cases mean genuinely different things and core keeps them apart for
    /// that reason: a filesystem that was unmounted, a metric the platform will never report, and a
    /// query that failed *this poll* are three different conversations to have with an operator.
    static func explanation(for availability: MetricAvailabilityRecord) -> String? {
        switch availability {
        case .present: return nil
        case .notReported: return "Not reported by this host"
        case .unsupported: return "Not available on this platform"
        case .unavailable: return "This poll could not fetch it"
        }
    }
}
