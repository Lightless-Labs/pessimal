//
//  FreshnessBannerView.swift
//  Pessimal — macOS menu bar client
//
//  How much the picture below deserves to be believed.
//
//  The verdict is core's `FreshnessRecord`, recomputed from the caller's `now` on every tick —
//  never stored. A stored verdict reads `Fresh` forever while the data underneath it rots, which
//  is the precise failure this banner exists to prevent.
//

import SwiftUI

/// The banner shown whenever the fleet on screen is not current.
///
/// Renders nothing for `.fresh`: a banner that is always there is a banner nobody reads.
struct FreshnessBannerView: View {
    let freshness: FreshnessRecord
    let now: Date

    var body: some View {
        switch freshness {
        case .fresh:
            EmptyView()

        case let .idle(lastSuccessMillis):
            banner(
                symbol: "clock.arrow.circlepath",
                tint: .secondary,
                title: "Showing cached data",
                detail: "Last updated \(MenuBarFormat.age(sinceMillis: lastSuccessMillis, at: now)). Nothing has been polled since Pessimal started.",
                failure: nil,
                consecutiveFailures: 0
            )

        case let .degraded(lastSuccessMillis, consecutiveFailures, failure):
            banner(
                symbol: "exclamationmark.triangle.fill",
                tint: .orange,
                title: "Data is stale",
                detail: "Last updated \(MenuBarFormat.age(sinceMillis: lastSuccessMillis, at: now)). The values below are real, and older than they should be.",
                failure: failure,
                consecutiveFailures: consecutiveFailures
            )

        case let .unusable(lastSuccessMillis, consecutiveFailures, failure):
            banner(
                symbol: "xmark.octagon.fill",
                tint: .red,
                title: "Not showing live data",
                detail: lastSuccessMillis.map {
                    "Last updated \(MenuBarFormat.age(sinceMillis: $0, at: now)). Nothing below can be trusted as current."
                } ?? "Pessimal has never successfully read this backend.",
                failure: failure,
                consecutiveFailures: consecutiveFailures
            )
        }
    }

    @ViewBuilder
    private func banner(
        symbol: String,
        tint: Color,
        title: String,
        detail: String,
        failure: PollFailureRecord?,
        consecutiveFailures: UInt32
    ) -> some View {
        HStack(alignment: .top, spacing: 8) {
            Image(systemName: symbol)
                .foregroundStyle(tint)
                .accessibilityHidden(true)

            VStack(alignment: .leading, spacing: 3) {
                Text(title)
                    .font(.callout.weight(.semibold))

                Text(detail)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)

                if let failure {
                    // Core's own sentence, shown and not parsed. It names the thing that failed,
                    // which no phrase composed here could do as well.
                    Text(failure.message)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .textSelection(.enabled)
                        .fixedSize(horizontal: false, vertical: true)
                }

                if consecutiveFailures > 1 {
                    // Carried by core beside the verdict so a long wait between attempts has a
                    // visible cause. It is not a number to multiply anything by here.
                    Text("\(consecutiveFailures) consecutive failures.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }

                // `isActionable` is core's predicate for "retrying cannot help, the user must
                // change something" — true only for a rejected credential. Offering the settings
                // window for anything else would send people to a screen that cannot help them.
                if failure?.isActionable == true {
                    PessimalSettingsLink("Open Settings…")
                        .font(.caption)
                        .padding(.top, 1)
                }
            }

            Spacer(minLength: 0)
        }
        .padding(8)
        .background(tint.opacity(0.12), in: RoundedRectangle(cornerRadius: 6))
        .overlay(
            RoundedRectangle(cornerRadius: 6)
                .strokeBorder(tint.opacity(0.25))
        )
    }
}
