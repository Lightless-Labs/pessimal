//
//  SetupNeededView.swift
//  Pessimal — macOS menu bar client
//
//  What the dropdown shows when there is no fleet to show.
//
//  "Nothing is set up" and "no hosts are reporting" look identical as an empty list and mean
//  opposite things — the first invites a click, the second is an outage. This view exists so the
//  app never shows the second when it means the first.
//

import SwiftUI

/// The first-run invitation, and the three ways a configuration can be present and unusable.
struct SetupNeededView: View {
    let sessionState: FleetModel.SessionState

    /// Why the API key could not be read, from ``FleetStoreBridge``. Not the same as no key.
    let credentialProblem: String?

    /// Core's sentence for stored settings it refuses.
    let settingsProblem: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Image(systemName: headline.symbol)
                    .foregroundStyle(headline.tint)
                    .accessibilityHidden(true)
                Text(headline.title)
                    .font(.headline)
            }

            Text(headline.detail)
                .font(.callout)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)

            if let message = headline.message {
                Text(message)
                    .font(.callout)
                    .monospaced()
                    .textSelection(.enabled)
                    .fixedSize(horizontal: false, vertical: true)
                    .padding(8)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(.quaternary.opacity(0.5), in: RoundedRectangle(cornerRadius: 6))
            }

            if headline.showsChecklist {
                VStack(alignment: .leading, spacing: 4) {
                    checklistItem("The base URL of your telemetry backend")
                    checklistItem("An API key with read access to it")
                    checklistItem("The environment name your agents export under")
                }
                .font(.callout)
                .foregroundStyle(.secondary)
            }

            PessimalSettingsLink {
                Label("Open Settings…", systemImage: "gearshape")
            }
            .buttonStyle(.borderedProminent)
            .controlSize(.regular)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func checklistItem(_ text: String) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 6) {
            Image(systemName: "circle.fill")
                .font(.system(size: 4))
                .accessibilityHidden(true)
            Text(text)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    /// The four things this screen can be saying, in order of precedence.
    ///
    /// A configuration core has refused outranks a Keychain that would not open, which outranks a
    /// first run: the more specific the diagnosis, the more useful it is, and the vaguest of them
    /// — "you have not set this up" — is exactly the wrong thing to tell someone whose settings
    /// are sitting on disk being rejected.
    private var headline: Headline {
        if case let .unusable(message) = sessionState {
            return Headline(
                symbol: "exclamationmark.triangle.fill",
                tint: .orange,
                title: "These settings cannot be used",
                // Core's message names the field it refused. It is shown rather than paraphrased
                // because a paraphrase would be a second opinion about what core rejected.
                detail: "Pessimal read your settings and the core refused them:",
                message: message,
                showsChecklist: false
            )
        }

        if let settingsProblem {
            return Headline(
                symbol: "exclamationmark.triangle.fill",
                tint: .orange,
                title: "These settings cannot be used",
                detail: "Pessimal read your settings and the core refused them:",
                message: settingsProblem,
                showsChecklist: false
            )
        }

        if let credentialProblem {
            return Headline(
                symbol: "key.slash",
                tint: .orange,
                title: "The API key could not be read",
                detail: "Your key is stored, and this Mac would not hand it over. Pessimal has not "
                    + "changed it and will try again on the next refresh.",
                message: credentialProblem,
                showsChecklist: false
            )
        }

        return Headline(
            symbol: "circle.dashed",
            tint: .secondary,
            title: "Pessimal is not set up yet",
            detail: "Pessimal watches hosts by reading the metrics your agents already export. "
                + "To start watching, it needs three things:",
            message: nil,
            showsChecklist: true
        )
    }

    private struct Headline {
        let symbol: String
        let tint: Color
        let title: String
        let detail: String
        let message: String?
        let showsChecklist: Bool
    }
}
