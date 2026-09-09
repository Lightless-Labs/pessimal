//
//  MenuFooterView.swift
//  Pessimal — macOS menu bar client
//
//  The three things a menu bar app must always offer, and one line saying what the clock is doing.
//

import AppKit
import SwiftUI

/// Poll status, then Refresh Now / Settings… / Quit.
struct MenuFooterView: View {
    let pollingState: FleetModel.PollingState

    /// When the next poll is due. Core's deadline, jittered by `PollSchedule` and nothing else —
    /// this view counts down to it and never decides it.
    let nextPollAt: Date?

    let now: Date
    let onRefresh: () async -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            status

            HStack(spacing: 8) {
                Button {
                    Task { await onRefresh() }
                } label: {
                    Label("Refresh Now", systemImage: "arrow.clockwise")
                }
                // Never disabled while a poll is in flight. `FleetModel.refresh()` joins the
                // running poll rather than starting a second one, and a button that greys out for
                // the duration of a slow request is a button that looks broken exactly when
                // somebody is leaning on it.
                .keyboardShortcut("r")

                PessimalSettingsLink {
                    Label("Settings…", systemImage: "gearshape")
                }
                .keyboardShortcut(",")

                Spacer(minLength: 0)

                Button {
                    // Through `NSApplication` rather than `exit`, so the app's own
                    // `willTerminate` observer gets to write the fleet state to disk first.
                    NSApplication.shared.terminate(nil)
                } label: {
                    Label("Quit", systemImage: "power")
                }
                .keyboardShortcut("q")
            }
            .labelStyle(.titleAndIcon)
        }
    }

    @ViewBuilder
    private var status: some View {
        switch pollingState {
        case .idle:
            footnote("Idle.", symbol: "pause.circle", tint: .secondary)

        case .scheduled:
            if let nextPollAt {
                footnote(
                    "Next poll \(MenuBarFormat.countdown(to: nextPollAt, at: now)).",
                    symbol: "clock",
                    tint: .secondary
                )
            } else {
                footnote("Scheduled.", symbol: "clock", tint: .secondary)
            }

        case .polling:
            HStack(spacing: 6) {
                ProgressView()
                    .controlSize(.small)
                Text("Polling…")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }

        case .suspended:
            // Deliberately paused, and not a failure. Drawn plainly rather than in a warning
            // colour, and drawn at all rather than hidden: an app that has quietly stopped
            // watching must say so.
            footnote("Polling is paused.", symbol: "pause.circle", tint: .secondary)

        case let .stopped(_, message):
            // Core's verdict that retrying cannot help, in core's words. Refresh Now still works
            // — a stop is a reason not to retry automatically, not a refusal to be asked.
            footnote(message, symbol: "xmark.octagon.fill", tint: .red)
        }
    }

    private func footnote(_ text: String, symbol: String, tint: Color) -> some View {
        Label {
            Text(text)
                .font(.caption)
                .foregroundStyle(tint == .secondary ? AnyShapeStyle(.secondary) : AnyShapeStyle(tint))
                .fixedSize(horizontal: false, vertical: true)
        } icon: {
            Image(systemName: symbol)
                .foregroundStyle(tint)
        }
        .font(.caption)
    }
}
