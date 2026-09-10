//
//  SetupNeededView.swift
//  Pessimal — iOS client
//
//  What the screen shows when there is no fleet to show.
//
//  "Nothing is set up" and "no hosts are reporting" look identical as an empty list and mean opposite
//  things — the first invites a tap, the second is an outage. This view exists so the app never shows
//  the second when it means the first.
//

#if canImport(PessimalKit)
    import PessimalKit
#endif
import SwiftUI

/// The first-run invitation, and the three ways a configuration can be present and unusable.
///
/// Not a `ContentUnavailableView`, though the shape is the same one. Two of the four states carry
/// core's own message verbatim and one carries a checklist, and all three have to be read left to
/// right — `ContentUnavailableView` centres its description, which turns a monospaced error message
/// and a list of three things into something that has to be deciphered rather than read. The icon,
/// title, body, action arrangement is borrowed; the alignment is not.
struct SetupNeededView: View {
    let sessionState: FleetModel.SessionState

    /// Why the API key could not be read, from `FleetStoreBridge`. Not the same as no key.
    let credentialProblem: String?

    /// Core's sentence for stored settings it refuses.
    let settingsProblem: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Image(systemName: headline.symbol)
                .font(.largeTitle)
                .foregroundStyle(headline.tint)
                .accessibilityHidden(true)

            Text(headline.title)
                .font(.title3.weight(.semibold))
                .fixedSize(horizontal: false, vertical: true)

            Text(headline.detail)
                .font(.callout)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)

            if let message = headline.message {
                Text(message)
                    .font(.footnote)
                    .monospaced()
                    // Selectable because the useful thing to do with core's complaint is paste it into
                    // a config file or a bug report.
                    .textSelection(.enabled)
                    .fixedSize(horizontal: false, vertical: true)
                    .padding(10)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(.quaternary.opacity(0.5), in: RoundedRectangle(cornerRadius: 8))
            }

            if headline.showsChecklist {
                VStack(alignment: .leading, spacing: 6) {
                    checklistItem("The base URL of your telemetry backend")
                    checklistItem("An API key with read access to it")
                    checklistItem("The environment name your agents export under")
                }
                .font(.callout)
                .foregroundStyle(.secondary)
            }

            NavigationLink(value: FleetRoute.settings) {
                Label("Open Settings", systemImage: "gearshape")
            }
            // Constrains the tap target to the button. Without it the `List` row this sits in becomes
            // one large link, and the selectable error message above stops being selectable.
            .buttonStyle(.borderedProminent)
            .controlSize(.large)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.vertical, 8)
    }

    private func checklistItem(_ text: String) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Image(systemName: "circle.fill")
                .font(.system(size: 5))
                .accessibilityHidden(true)
            Text(text)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    /// The four things this screen can be saying, in order of precedence.
    ///
    /// A configuration core has refused outranks a Keychain that would not open, which outranks a
    /// first run: the more specific the diagnosis, the more useful it is, and the vaguest of them —
    /// "you have not set this up" — is exactly the wrong thing to tell someone whose settings are
    /// sitting on disk being rejected.
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
                // The retry is a pull-to-refresh away, and this screen is inside the list that offers
                // it — which is the reason the setup state is a row rather than a screen of its own.
                detail: "Your key is stored, and this device would not hand it over. Pessimal has not "
                    + "changed it. Pull down to try again.",
                message: credentialProblem,
                showsChecklist: false
            )
        }

        return Headline(
            symbol: "circle.dashed",
            tint: .secondary,
            title: "Pessimal is not set up yet",
            detail: "Pessimal watches hosts by reading the metrics your agents already export. To "
                + "start watching, it needs three things:",
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
