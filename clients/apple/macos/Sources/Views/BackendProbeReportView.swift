import SwiftUI

/// What "Test connection" is currently showing.
///
/// A separate type from the record because three of the four states are not records: nothing has
/// been asked yet, something is in flight, or the probe could not even be *started* (a base URL the
/// core refuses, say, which fails in `FleetSession.init` before any request is made).
enum BackendProbeState: Equatable {
    case idle
    case running

    /// The probe completed. Note that "completed" is not "succeeded" — see ``BackendProbeReportView``.
    case finished(BackendProbeRecord)

    /// The probe could not be run at all, with the core's message.
    case failed(String)
}

/// Renders a ``BackendProbeRecord`` without collapsing it to a tick.
///
/// The probe makes two calls — a credential handshake and a real roster listing — for one reason:
/// the four outcomes want four different repairs, and three of them look identical to a screen that
/// only asks "did it work?".
///
/// - `connected == false` — nothing answered, or the key was rejected. Fix the URL or the key.
/// - `connected == true` with a `failure` — the address and key are *fine* and the listing itself
///   errored. Fix the backend, or the query; changing the key will not help.
/// - `usable == false` with no failure — everything worked and the backend holds no Pessimal data
///   in the window. Fix the agents, or wait for them to report.
/// - `usable == true` — genuinely ready.
///
/// Every one of those verdicts is read from the record, never recomputed. `usable` in particular is
/// the core's own `connected && hosts_seen > 0`; writing that expression here would create a second
/// definition of "usable" that can drift from the one the rest of the app trusts.
struct BackendProbeReportView: View {

    let state: BackendProbeState

    var body: some View {
        switch state {
        case .idle:
            EmptyView()
        case .running:
            Label("Testing…", systemImage: "ellipsis.circle")
                .foregroundStyle(.secondary)
        case let .failed(message):
            report(
                icon: "xmark.octagon.fill",
                tint: .red,
                headline: "The test could not be run.",
                detail: message
            )
        case let .finished(record):
            finished(record)
        }
    }

    @ViewBuilder
    private func finished(_ record: BackendProbeRecord) -> some View {
        // The core writes `message` for exactly this screen, window phrase included. It is the
        // headline in all four cases; anything below it is elaboration, never a restatement.
        if !record.connected {
            report(
                icon: "bolt.horizontal.circle.fill",
                tint: .red,
                headline: record.message,
                detail: failureDetail(record.failure)
            )
        } else if let failure = record.failure {
            // The distinction the two-call probe exists to draw. Deliberately not red: nothing is
            // wrong with what the user typed, so an error styled like a typo sends them to edit
            // fields that are already correct.
            report(
                icon: "exclamationmark.triangle.fill",
                tint: .orange,
                headline: record.message,
                detail: """
                    \(record.backendName) accepted the address and the key. \
                    The request that failed was \(describe(failure.request)).
                    """,
                extra: failureDetail(failure)
            )
        } else if !record.usable {
            report(
                icon: "questionmark.circle.fill",
                tint: .orange,
                headline: record.message,
                detail: """
                    \(record.backendName) answered and reported no hosts in the window. \
                    Either no agent is exporting yet, or they are exporting somewhere else.
                    """
            )
        } else {
            report(
                icon: "checkmark.circle.fill",
                tint: .green,
                headline: record.message,
                detail: hostSummary(record)
            )
        }
    }

    @ViewBuilder
    private func report(
        icon: String,
        tint: Color,
        headline: String,
        detail: String?,
        extra: String? = nil
    ) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Image(systemName: icon)
                .foregroundStyle(tint)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 4) {
                Text(headline)
                if let detail {
                    Text(detail)
                        .font(.callout)
                        .foregroundStyle(.secondary)
                }
                if let extra {
                    Text(extra)
                        .font(.callout)
                        .foregroundStyle(.secondary)
                }
            }
            .textSelection(.enabled)
        }
        .fixedSize(horizontal: false, vertical: true)
    }

    /// The failure's own words, plus whether the core thinks the user can do anything about it.
    ///
    /// `actionable` is carried rather than inferred from the kind: the core knows, for instance,
    /// that a malformed response is the backend's problem and not the operator's, and a settings
    /// screen that guesses otherwise sends people to re-type a perfectly good key.
    private func failureDetail(_ failure: ProbeFailureRecord?) -> String? {
        guard let failure else { return nil }
        let prefix = describe(failure.kind)
        if failure.actionable {
            return "\(prefix): \(failure.message)"
        }
        return "\(prefix): \(failure.message) There is nothing to change here — this is the backend's end."
    }

    private func hostSummary(_ record: BackendProbeRecord) -> String? {
        guard !record.sampleHosts.isEmpty else { return nil }
        // Sorted by the core and not re-sorted here, so a second run of the test shows the same
        // five names in the same order.
        let names = record.sampleHosts.joined(separator: ", ")
        if record.sampleHosts.count < Int(record.hostsSeen) {
            return "Including \(names)."
        }
        return names
    }

    private func describe(_ kind: ProbeFailureKind) -> String {
        switch kind {
        case .unauthorized:
            return "The backend rejected the API key"
        case .unreachable:
            return "The backend could not be reached"
        case .backend:
            return "The backend returned an error"
        case .malformed:
            return "The backend's answer could not be read"
        }
    }

    private func describe(_ request: ProbeFailedRequest) -> String {
        switch request {
        case .roster:
            return "the host listing"
        case let .series(metricOtelName):
            return "the metric \(metricOtelName)"
        }
    }
}
