import SwiftUI

/// The rule list inside the settings form: what is configured, plus add and remove.
///
/// The rules are held as records rather than as the `rules_json` string a `FleetConfigRecord`
/// carries, because that string is opaque by design — the core's docs are explicit that Swift must
/// never assemble it. So the window decodes once with `alertRulesFromJson`, edits records, and
/// re-encodes with `alertRulesToJson` when the config is built. Ids survive the round trip, which
/// is the whole reason the carriage is shaped this way: an alert that is already firing keeps its
/// identity across an unrelated edit to another rule.
struct AlertRulesSettings: View {

    /// The environment new rules are minted into. Rules already in the list keep the environment
    /// they were drafted with — their ids embed it — so this only ever affects additions.
    let environment: String

    @Binding var rules: [AlertRuleRecord]

    /// Non-nil when the stored `rules_json` could not be decoded, carrying the core's reason.
    let unreadable: String?

    /// Called when the user chooses to throw away rules that could not be decoded.
    let discardUnreadable: () -> Void

    @State private var isAdding = false

    var body: some View {
        Section {
            if let unreadable {
                unreadableNotice(unreadable)
            } else if rules.isEmpty {
                Text("No alert rules. Liveness is still watched — rules are for thresholds on top of it.")
                    .foregroundStyle(.secondary)
            } else {
                ForEach(rules, id: \.id) { rule in
                    row(rule)
                }
            }
        } header: {
            HStack {
                Text("Alert Rules")
                Spacer()
                // The sheet hangs off the button rather than off the `Section`, because a modifier
                // on a `Section` inside a `Form` is applied to something the form may or may not
                // still be treating as a section.
                Button("Add Rule…") { isAdding = true }
                    .disabled(unreadable != nil)
                    .sheet(isPresented: $isAdding) {
                        AlertRuleEditor(environment: environment) { rule in
                            rules.append(rule)
                        }
                    }
            }
        }
    }

    @ViewBuilder
    private func row(_ rule: AlertRuleRecord) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 6) {
                    Text(rule.name)
                    if !rule.enabled {
                        Text("Disabled")
                            .font(.caption)
                            .padding(.horizontal, 5)
                            .padding(.vertical, 1)
                            .background(.quaternary, in: Capsule())
                    }
                }
                Text(summary(rule))
                    .font(.callout)
                    .foregroundStyle(.secondary)
                if let complaint = complaint(about: rule) {
                    Label(complaint, systemImage: "exclamationmark.triangle")
                        .font(.callout)
                        .foregroundStyle(.orange)
                }
            }
            Spacer(minLength: 8)
            Button {
                rules.removeAll { $0.id == rule.id }
            } label: {
                Image(systemName: "trash")
            }
            .buttonStyle(.borderless)
            .help("Remove this rule")
            .accessibilityLabel("Remove \(rule.name)")
        }
        .fixedSize(horizontal: false, vertical: true)
    }

    @ViewBuilder
    private func unreadableNotice(_ reason: String) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            Label("The saved alert rules could not be read.", systemImage: "exclamationmark.octagon.fill")
                .foregroundStyle(.red)
            Text(reason)
                .font(.callout)
                .foregroundStyle(.secondary)
                .textSelection(.enabled)
            // Saving is blocked while this is showing, because a save would re-encode the empty
            // list this window is holding and quietly destroy rules that are still on disk. The
            // only way past it is to say so out loud.
            Text("Settings cannot be saved until this is resolved, so that a save does not overwrite them.")
                .font(.callout)
                .foregroundStyle(.secondary)
            Button("Discard the Unreadable Rules", role: .destructive, action: discardUnreadable)
        }
    }

    /// What the core would say if this rule were being written now.
    ///
    /// `validate_alert_rule` is stricter than the decode that loaded the list, deliberately: a rule
    /// already on disk with, say, an empty host set must still load, while a user typing one is
    /// told immediately. Shown as a caption rather than an error because the rule *is* legal to
    /// hold — the config audit is what decides whether it deserves a warning.
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
            \(label(rule.metric)) \(symbol(rule.comparator)) \(rule.threshold.formatted()) \
            \(dwell) · \(describe(rule.selector))
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

/// Authors one new rule through `draft_alert_rule`.
///
/// Nothing here decides whether a rule is acceptable. The metric picker offers all twelve kinds
/// including `agentHeartbeat`, which the core refuses — a silent host is liveness's business, not an
/// alert's — because the core's refusal explains *why*, and a picker that quietly omits the option
/// teaches the user nothing. Same for an empty host set, a zero name, a non-finite threshold: typed,
/// submitted, refused, explained.
struct AlertRuleEditor: View {

    let environment: String

    let add: (AlertRuleRecord) -> Void

    @Environment(\.dismiss) private var dismiss

    @State private var name = ""
    @State private var metric: MetricKindRecord = .cpuUtilization
    @State private var comparator: ComparatorRecord = .greaterThan
    @State private var threshold = ""
    @State private var dwellSeconds = "300"
    @State private var scope: SelectorScope = .all
    @State private var hostID = ""
    @State private var hostList = ""
    @State private var rejection: String?

    private enum SelectorScope: String, CaseIterable, Identifiable {
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
        VStack(alignment: .leading, spacing: 0) {
            Form {
                Section {
                    TextField("Name", text: $name, prompt: Text("CPU sustained above 90%"))

                    Picker("Metric", selection: $metric) {
                        ForEach(orderedMetrics, id: \.self) { kind in
                            Text(label(kind)).tag(kind)
                        }
                    }

                    HStack {
                        Picker("Fires when", selection: $comparator) {
                            ForEach(orderedComparators, id: \.self) { comparison in
                                Text(symbol(comparison)).tag(comparison)
                            }
                        }
                        .labelsHidden()
                        .frame(width: 70)
                        TextField("Threshold", text: $threshold, prompt: Text("0.9"))
                            .frame(width: 100)
                        Text(unitHint(metric))
                            .foregroundStyle(.secondary)
                    }

                    HStack {
                        TextField("Sustained for", text: $dwellSeconds)
                            .frame(width: 100)
                        Text("seconds")
                            .foregroundStyle(.secondary)
                    }
                }

                Section("Applies to") {
                    Picker("Hosts", selection: $scope) {
                        ForEach(SelectorScope.allCases) { option in
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
                    case .anyOf:
                        TextField(
                            "Host IDs",
                            text: $hostList,
                            prompt: Text("web-01, web-02")
                        )
                        Text("Separated by commas.")
                            .font(.callout)
                            .foregroundStyle(.secondary)
                    }
                }

                if let rejection {
                    Section {
                        Label(rejection, systemImage: "exclamationmark.triangle.fill")
                            .foregroundStyle(.orange)
                            .textSelection(.enabled)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
            }
            .formStyle(.grouped)

            Divider()

            HStack {
                Spacer()
                Button("Cancel", role: .cancel) { dismiss() }
                    .keyboardShortcut(.cancelAction)
                Button("Add Rule", action: submit)
                    .keyboardShortcut(.defaultAction)
            }
            .padding(12)
        }
        .frame(width: 460)
        .frame(minHeight: 420)
    }

    /// Builds the rule the core's way and shows whatever it says if it will not have it.
    private func submit() {
        rejection = nil

        // The one judgement Swift makes here, and only because the field is text: a threshold that
        // is not a number cannot be handed to a `Double` parameter at all. Magnitude, sign and
        // finiteness are the core's to rule on.
        guard let thresholdValue = Double(threshold.trimmingCharacters(in: .whitespaces)) else {
            rejection = "The threshold must be a number, using a period for the decimal point."
            return
        }
        guard let dwell = Int64(dwellSeconds.trimmingCharacters(in: .whitespaces)) else {
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
            // list is acceptable — empty, in particular — is `draft_alert_rule`'s call.
            let ids = hostList
                .split(separator: ",")
                .map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
                .filter { !$0.isEmpty }
            return .anyOf(hostIds: ids)
        }
    }
}

/// Every metric kind the core knows, in the order the picker offers them.
///
/// Listed exhaustively rather than filtered: `agentHeartbeat` is here even though no rule may use
/// it, so that the core gets to explain the refusal.
private let orderedMetrics: [MetricKindRecord] = [
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

private let orderedComparators: [ComparatorRecord] = [
    .greaterThan,
    .greaterThanOrEqual,
    .lessThan,
    .lessThanOrEqual,
]

private func label(_ metric: MetricKindRecord) -> String {
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

/// A hint about the number the threshold field wants, for the kinds where the unit is not obvious.
///
/// Presentation only: it says what a utilisation looks like, never what an acceptable one is.
private func unitHint(_ metric: MetricKindRecord) -> String {
    switch metric {
    case .cpuUtilization, .memoryUtilization, .filesystemUtilization:
        return "a fraction, 0–1"
    case .memoryUsage, .filesystemUsage:
        return "bytes"
    case .networkIo:
        return "bytes"
    case .loadAverage1m, .loadAverage5m, .loadAverage15m:
        return "processes"
    case .systemUptime:
        return "seconds"
    case .agentHeartbeat, .agentCollectionFailures:
        return "count"
    }
}

private func symbol(_ comparator: ComparatorRecord) -> String {
    switch comparator {
    case .greaterThan: return ">"
    case .greaterThanOrEqual: return "≥"
    case .lessThan: return "<"
    case .lessThanOrEqual: return "≤"
    }
}
