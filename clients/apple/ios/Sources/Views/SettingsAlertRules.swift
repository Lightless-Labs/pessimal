//
//  SettingsAlertRules.swift
//  Pessimal — iOS client
//
//  The rule list inside the settings form, plus the sheet that writes a new one.
//
//  The rules are held as records rather than as the `rules_json` string a `FleetConfigRecord`
//  carries, because that string is opaque by design — core's docs are explicit that Swift must never
//  assemble it. So the screen decodes once with `alertRulesFromJson`, edits records, and re-encodes
//  with `alertRulesToJson` when the config is built. Ids survive the round trip, which is the whole
//  reason the carriage is shaped this way: an alert that is already firing keeps its identity, its
//  dwell and its history across an unrelated edit to another rule.
//

#if canImport(PessimalFFI)
    import PessimalFFI
#endif
#if canImport(PessimalKit)
    import PessimalKit
#endif
import SwiftUI

/// The Alert Rules section of ``SettingsView``.
struct SettingsAlertRulesSection: View {

    /// The environment new rules are minted into. Rules already in the list keep the environment they
    /// were drafted with — their ids embed it — so this only ever affects additions.
    let environment: String

    @Binding var rules: [AlertRuleRecord]

    /// Non-nil when the stored `rules_json` could not be decoded, carrying core's reason.
    let unreadable: String?

    /// Called when the user chooses to throw away rules that could not be decoded.
    let discardUnreadable: () -> Void

    @State private var isAdding = false

    var body: some View {
        Section {
            if let unreadable {
                unreadableNotice(unreadable)
            } else {
                if rules.isEmpty {
                    Text("No alert rules. Liveness is still watched — rules are thresholds on top of it.")
                        .foregroundStyle(.secondary)
                } else {
                    // Swipe to delete rather than a trash button per row: the row is already three
                    // lines tall, and a destructive control inside a `Form` row on iOS is a tap
                    // target sitting next to the one that opens the row.
                    ForEach(rules, id: \.id) { rule in
                        row(rule)
                    }
                    .onDelete { offsets in
                        rules.remove(atOffsets: offsets)
                    }
                }

                Button {
                    isAdding = true
                } label: {
                    Label("Add Rule", systemImage: "plus.circle.fill")
                }
                .sheet(isPresented: $isAdding) {
                    SettingsAlertRuleEditor(environment: environment) { rule in
                        rules.append(rule)
                    }
                }
            }
        } header: {
            Text("Alert Rules")
        } footer: {
            if unreadable == nil {
                Text("A rule fires when its metric stays past the threshold for the whole duration.")
            }
        }
    }

    @ViewBuilder
    private func row(_ rule: AlertRuleRecord) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            HStack(spacing: 6) {
                Text(rule.name)
                if !rule.enabled {
                    Text("Disabled")
                        .font(.caption2)
                        .padding(.horizontal, 5)
                        .padding(.vertical, 1)
                        .background(.quaternary, in: Capsule())
                }
            }

            Text(summary(rule))
                .font(.footnote)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)

            if let complaint = complaint(about: rule) {
                Label(complaint, systemImage: "exclamationmark.triangle")
                    .font(.footnote)
                    .foregroundStyle(.orange)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
    }

    @ViewBuilder
    private func unreadableNotice(_ reason: String) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            Label("The saved alert rules could not be read.", systemImage: "exclamationmark.octagon.fill")
                .foregroundStyle(.red)

            Text(reason)
                .font(.footnote)
                .foregroundStyle(.secondary)
                .textSelection(.enabled)
                .fixedSize(horizontal: false, vertical: true)

            // Saving is blocked while this is showing, because a save would re-encode the empty list
            // this screen is holding and quietly destroy rules that are still on disk. The only way
            // past it is to say so out loud.
            Text("Settings cannot be saved until this is resolved, so that a save does not overwrite them.")
                .font(.footnote)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)

            Button("Discard the Unreadable Rules", role: .destructive, action: discardUnreadable)
        }
    }

    /// What core would say if this rule were being written now.
    ///
    /// `validateAlertRule` is stricter than the decode that loaded the list, deliberately: a rule
    /// already on disk with, say, an empty host set must still load, while a user typing one is told
    /// immediately. Shown as a caption rather than an error because the rule *is* legal to hold — the
    /// config audit is what decides whether it deserves a warning.
    private func complaint(about rule: AlertRuleRecord) -> String? {
        do {
            try validateAlertRule(rule: rule)
            return nil
        } catch {
            return FleetModel.message(for: error)
        }
    }

    private func summary(_ rule: AlertRuleRecord) -> String {
        let dwell = rule.forDurationSeconds == 0
            ? "immediately"
            : "for \(rule.forDurationSeconds)s"
        return """
            \(settingsMetricLabel(rule.metric)) \(settingsComparatorSymbol(rule.comparator)) \
            \(rule.threshold.formatted()) \(dwell) · \(describe(rule.selector))
            """
    }

    private func describe(_ selector: HostSelectorRecord) -> String {
        switch selector {
        case .all:
            return "all hosts"
        case let .host(hostId):
            return hostId
        case let .anyOf(hostIds):
            return hostIds.isEmpty ? "an empty host set" : hostIds.joined(separator: ", ")
        }
    }
}

// MARK: - Editor

/// Authors one new rule through `draftAlertRule`.
///
/// Nothing here decides whether a rule is acceptable. The metric picker offers all twelve kinds
/// including the agent heartbeat, which core refuses — a silent host is liveness's business, not an
/// alert's — because core's refusal explains *why*, and a picker that quietly omits the option
/// teaches the user nothing. Same for an empty host set, a blank name, a non-finite threshold: typed,
/// submitted, refused, explained.
struct SettingsAlertRuleEditor: View {

    let environment: String

    let add: (AlertRuleRecord) -> Void

    @Environment(\.dismiss) private var dismiss

    @State private var name = ""
    @State private var metric: MetricKindRecord = .cpuUtilization
    @State private var comparator: ComparatorRecord = .greaterThan
    @State private var threshold = ""
    @State private var dwellSeconds = "300"
    @State private var scope: Scope = .all
    @State private var hostID = ""
    @State private var hostList = ""

    /// Core's refusal, or this screen's refusal to turn a text field into a number. Held rather than
    /// thrown away on the next keystroke: the user has to be able to read it while fixing the field
    /// it is about.
    @State private var rejection: String?

    private enum Scope: String, CaseIterable, Identifiable {
        case all
        case one
        case anyOf

        var id: String { rawValue }

        var label: String {
            switch self {
            case .all: return "All hosts"
            case .one: return "One host"
            case .anyOf: return "Any of…"
            }
        }
    }

    var body: some View {
        // Its own `NavigationStack` because a sheet has no navigation bar of its own, and the two
        // buttons that commit or discard a modal edit belong in one.
        NavigationStack {
            Form {
                Section {
                    TextField("Name", text: $name, prompt: Text("CPU sustained above 90%"))

                    Picker("Metric", selection: $metric) {
                        ForEach(settingsOrderedMetrics, id: \.self) { kind in
                            Text(settingsMetricLabel(kind)).tag(kind)
                        }
                    }

                    Picker("Fires when", selection: $comparator) {
                        ForEach(settingsOrderedComparators, id: \.self) { comparison in
                            Text(settingsComparatorName(comparison)).tag(comparison)
                        }
                    }

                    LabeledContent("Threshold") {
                        TextField(settingsThresholdHint(metric), text: $threshold)
                            .keyboardType(.decimalPad)
                            .multilineTextAlignment(.trailing)
                    }

                    LabeledContent("Sustained for") {
                        HStack(spacing: 4) {
                            TextField("300", text: $dwellSeconds)
                                .keyboardType(.numberPad)
                                .multilineTextAlignment(.trailing)
                            Text("seconds")
                                .foregroundStyle(.secondary)
                        }
                    }
                } footer: {
                    Text(settingsThresholdFooter(metric))
                }

                Section {
                    Picker("Hosts", selection: $scope) {
                        ForEach(Scope.allCases) { option in
                            Text(option.label).tag(option)
                        }
                    }
                    .pickerStyle(.segmented)
                    .labelsHidden()

                    switch scope {
                    case .all:
                        EmptyView()
                    case .one:
                        TextField("Host ID", text: $hostID, prompt: Text("web-01"))
                            .autocorrectionDisabled()
                            .textInputAutocapitalization(.never)
                    case .anyOf:
                        TextField("Host IDs", text: $hostList, prompt: Text("web-01, web-02"))
                            .autocorrectionDisabled()
                            .textInputAutocapitalization(.never)
                    }
                } header: {
                    Text("Applies to")
                } footer: {
                    if scope == .anyOf {
                        Text("Separated by commas.")
                    }
                }

                if let rejection {
                    Section {
                        Label {
                            Text(rejection)
                                .textSelection(.enabled)
                                .fixedSize(horizontal: false, vertical: true)
                        } icon: {
                            Image(systemName: "exclamationmark.triangle.fill")
                                .foregroundStyle(.orange)
                        }
                    }
                }
            }
            .scrollDismissesKeyboard(.interactively)
            .navigationTitle("New Alert Rule")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    // Never disabled. Whether this rule may exist is core's answer, and a button
                    // greyed out by a Swift guess would withhold the explanation core would have
                    // given.
                    Button("Add") { submit() }
                }
            }
        }
    }

    /// Builds the rule core's way and shows whatever it says if it will not have it.
    private func submit() {
        rejection = nil

        // The only two judgements this screen makes, and both only because the fields are text.
        // Magnitude, sign, finiteness and the relationship between the dwell and the staleness
        // budget are core's to rule on.
        guard let thresholdValue = SettingsNumber.double(from: threshold) else {
            rejection = "The threshold must be a number."
            return
        }
        guard let dwell = SettingsNumber.int64(from: dwellSeconds) else {
            rejection = "The duration must be a whole number of seconds."
            return
        }

        do {
            let rule = try draftAlertRule(
                environment: environment,
                name: name.trimmingCharacters(in: .whitespacesAndNewlines),
                metric: metric,
                comparator: comparator,
                threshold: thresholdValue,
                forDurationSeconds: dwell,
                selector: selector()
            )
            add(rule)
            dismiss()
        } catch {
            rejection = FleetModel.message(for: error)
        }
    }

    private func selector() -> HostSelectorRecord {
        switch scope {
        case .all:
            return .all
        case .one:
            return .host(hostId: hostID.trimmingCharacters(in: .whitespacesAndNewlines))
        case .anyOf:
            // Splitting and trimming is formatting a text field into a list. Whether the resulting
            // list is acceptable — empty, in particular — is `draftAlertRule`'s call.
            let ids = hostList
                .split(separator: ",")
                .map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
                .filter { !$0.isEmpty }
            return .anyOf(hostIds: ids)
        }
    }
}

// MARK: - Metric vocabulary

/// Every metric kind core knows, in the order the picker offers them.
///
/// Listed exhaustively rather than filtered: the agent heartbeat is here even though no rule may use
/// it, so that core gets to explain the refusal.
///
/// **This table is a copy of core's, and that is a known gap rather than a choice.**
/// `MetricFactsRecord` — kind, display name, unit, `isRate`, `alertable` — exists in the bindings and
/// no exported function returns one, so a picker over *all* kinds has nowhere to read core's names
/// and units from. The macOS app carries the same table. Once `pessimal_ffi` exports the facts, both
/// copies should go.
private let settingsOrderedMetrics: [MetricKindRecord] = [
    .cpuUtilization,
    .memoryUtilization,
    .memoryUsage,
    .filesystemUtilization,
    .filesystemUsage,
    .networkIo,
    .loadAverage1m,
    .loadAverage5m,
    .loadAverage15m,
    .systemUptime,
    .agentHeartbeat,
    .agentCollectionFailures,
]

private let settingsOrderedComparators: [ComparatorRecord] = [
    .greaterThan,
    .greaterThanOrEqual,
    .lessThan,
    .lessThanOrEqual,
]

/// Word for word the macOS app's labels. Two apps naming the same metric differently is a support
/// conversation nobody can win.
private func settingsMetricLabel(_ metric: MetricKindRecord) -> String {
    switch metric {
    case .cpuUtilization: return "CPU utilisation"
    case .memoryUtilization: return "Memory utilisation"
    case .memoryUsage: return "Memory used"
    case .filesystemUtilization: return "Filesystem utilisation"
    case .filesystemUsage: return "Filesystem used"
    case .networkIo: return "Network I/O"
    case .loadAverage1m: return "Load average (1m)"
    case .loadAverage5m: return "Load average (5m)"
    case .loadAverage15m: return "Load average (15m)"
    case .systemUptime: return "Uptime"
    case .agentHeartbeat: return "Agent heartbeat"
    case .agentCollectionFailures: return "Agent collection failures"
    }
}

private func settingsComparatorSymbol(_ comparator: ComparatorRecord) -> String {
    switch comparator {
    case .greaterThan: return ">"
    case .greaterThanOrEqual: return "≥"
    case .lessThan: return "<"
    case .lessThanOrEqual: return "≤"
    }
}

/// Spelled out rather than symbolic in the picker: a segmented `>` and `≥` are two glyphs that differ
/// by three pixels on a phone.
private func settingsComparatorName(_ comparator: ComparatorRecord) -> String {
    switch comparator {
    case .greaterThan: return "is above"
    case .greaterThanOrEqual: return "is at or above"
    case .lessThan: return "is below"
    case .lessThanOrEqual: return "is at or below"
    }
}

/// The placeholder in the threshold field: what a value of this metric looks like.
///
/// Presentation only. It says what shape the number has, never what an acceptable one is.
private func settingsThresholdHint(_ metric: MetricKindRecord) -> String {
    switch metric {
    case .cpuUtilization, .memoryUtilization, .filesystemUtilization:
        return "0.9"
    case .memoryUsage, .filesystemUsage, .networkIo:
        return "bytes"
    case .loadAverage1m, .loadAverage5m, .loadAverage15m:
        return "4"
    case .systemUptime:
        return "seconds"
    case .agentHeartbeat, .agentCollectionFailures:
        return "1"
    }
}

/// What unit the threshold is in, and — for the two counters — what it is compared against.
///
/// The rate sentence is core's contract, not advice: a rule on a counter compares the threshold
/// against the normalised rate or per-bucket delta, never the cumulative total the backend returns,
/// and core's own documentation says the editor must label the field accordingly.
private func settingsThresholdFooter(_ metric: MetricKindRecord) -> String {
    switch metric {
    case .cpuUtilization, .memoryUtilization, .filesystemUtilization:
        return "A fraction between 0 and 1. 0.9 is ninety percent."
    case .memoryUsage, .filesystemUsage:
        return "In bytes."
    case .networkIo:
        return "In bytes per second — the normalised rate, not the counter's cumulative total."
    case .loadAverage1m, .loadAverage5m, .loadAverage15m:
        return "A run-queue depth, not a percentage."
    case .systemUptime:
        return "In seconds."
    case .agentHeartbeat:
        return "Core refuses rules on the heartbeat: a silent host is liveness's business."
    case .agentCollectionFailures:
        return "The count added since the previous sample, not the cumulative total."
    }
}
