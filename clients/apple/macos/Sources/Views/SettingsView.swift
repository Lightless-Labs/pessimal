import SwiftUI

/// The settings window: where the backend is named, the fleet is described, and alerts are written.
///
/// The window edits a *draft* and commits it in one go rather than writing each field as it is
/// typed. That is not a stylistic preference. A `FleetConfigRecord` is judged as a whole — core's
/// `PollTuning` has ten interlocks between its fields, and a rule set is judged against the
/// environment its ids embed — so a half-typed interval or a half-named environment is not a config
/// anything can be asked about. Committing once means core is only ever shown values a user has
/// finished writing.
///
/// Every verdict on screen is core's:
///
/// - `poll_tuning_validated` decides whether an interval is usable, and says why when it is not.
/// - `fleet_config_audit` names the legal-but-probably-wrong rules *while editing*, which is what
///   core documents it for; `set_config` returns the same judgement on commit, and that is what is
///   shown once nothing is unsaved.
/// - `draft_alert_rule` and `validate_alert_rule` decide whether a rule may exist.
/// - `probe` decides what "the connection works" means — see ``BackendProbeReportView``.
///
/// This window's own contribution is trimming whitespace, turning text fields into numbers, and
/// laying the result out.
public struct SettingsView: View {

    private let model: FleetModel
    private let connectionStore: any SettingsConnectionStore

    /// Everything the user is editing. Compared against ``committed`` to know whether anything is
    /// unsaved, so it holds only values the user can change.
    private struct Draft: Equatable {
        var baseURL: String
        var apiKey: String
        var environment: String
        var pollIntervalSeconds: String
        var rules: [AlertRuleRecord]

        /// Core's reason, when the stored `rules_json` would not decode.
        ///
        /// Carried in the draft rather than beside it because it has to survive until the user
        /// resolves it: while it is set, saving is refused. A window that decoded to an empty list
        /// and then saved would silently delete every rule on disk — the kind of quiet wrong answer
        /// this codebase would rather crash than produce.
        var rulesUnreadable: String?
    }

    @State private var draft: Draft

    /// The draft as it stood after the last successful save, or the last seed from the model.
    @State private var committed: Draft

    /// The warnings `set_config` returned for the committed config — not the audit of the draft.
    /// See ``warningsSection(_:)`` for why the window shows one or the other and never both.
    @State private var committedWarnings: [TuningWarningRecord]

    /// The store's own values as of the last seed, held rather than re-read.
    ///
    /// Not an optimisation. The composition root's store computes both of these — reading
    /// `connection` can retry a Keychain load, and reading `config` rebuilds a record and records
    /// why core refused it — and both write `@Observable` properties this view also reads. A read
    /// during `body` would therefore invalidate the view that just performed it, once per pass,
    /// for ever. So they are sampled at the moments a user can cause: opening the window,
    /// reverting, retrying the Keychain, and saving.
    @State private var storedConnection: FleetConnection?
    @State private var storedConfig: FleetConfigRecord?

    @State private var probeState: BackendProbeState = .idle
    @State private var saveError: String?
    @State private var isSaving = false

    @MainActor
    public init(model: FleetModel, connectionStore: any SettingsConnectionStore) {
        self.model = model
        self.connectionStore = connectionStore
        let seed = Self.seed(model: model, connectionStore: connectionStore)
        _draft = State(initialValue: seed.draft)
        _committed = State(initialValue: seed.draft)
        _storedConnection = State(initialValue: seed.connection)
        _storedConfig = State(initialValue: seed.config)
        _committedWarnings = State(initialValue: model.configWarnings)
    }

    public var body: some View {
        // Built once per pass and threaded through the sections that need it. The audit, the Save
        // button's enablement and the footer all ask core the same question, and asking three times
        // could answer three ways.
        let candidate = Result { try makeConfig() }

        Form {
            backendSection
            fleetSection
            AlertRulesSettings(
                environment: draft.environment.trimmingCharacters(in: .whitespacesAndNewlines),
                rules: $draft.rules,
                unreadable: draft.rulesUnreadable,
                discardUnreadable: {
                    draft.rules = []
                    draft.rulesUnreadable = nil
                }
            )
            warningsSection(candidate)
            SettingsUsageReportingSection(model: model)
            statusSection
            footer(candidate)
        }
        .formStyle(.grouped)
        .frame(minWidth: 540, idealWidth: 580, minHeight: 560, idealHeight: 700)
        .onChange(of: model.config) { _, _ in
            // The config can arrive after the window is already open: the model builds its session
            // on its own schedule. Re-seeding only when nothing is unsaved keeps that from wiping
            // something half-typed.
            reseedIfUnedited()
        }
    }

    // MARK: - Backend

    @ViewBuilder
    private var backendSection: some View {
        Section("Backend") {
            TextField(
                "Base URL",
                text: $draft.baseURL,
                prompt: Text(verbatim: "https://your-instance.signoz.cloud")
            )
            .autocorrectionDisabled()

            SecureField("API key", text: $draft.apiKey, prompt: Text("Required"))
                .disabled(connectionStore.credentialProblem != nil)

            if let problem = connectionStore.credentialProblem {
                keychainNotice(problem)
            }

            HStack(spacing: 8) {
                Button("Test Connection") {
                    Task { await runProbe() }
                }
                .disabled(probeState == .running)

                if probeState == .running {
                    ProgressView()
                        .controlSize(.small)
                }
            }

            BackendProbeReportView(state: probeState)
        }
    }

    /// The Keychain could not be read, which is not the same as there being no key.
    ///
    /// The usual cause repairs itself — a login item that started before the login keychain
    /// unlocked — so the honest offer is to try again, rather than to invite the user to retype a
    /// key that is probably already there. Saving stays blocked meanwhile: writing over a secret
    /// that could not be read destroys it.
    @ViewBuilder
    private func keychainNotice(_ reason: String) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            Label("The saved API key could not be read.", systemImage: "key.slash.fill")
                .foregroundStyle(.orange)
            Text(reason)
                .font(.callout)
                .foregroundStyle(.secondary)
                .textSelection(.enabled)
            Text("It has not been changed. Saving the address and key is disabled until it can be read, so that a save cannot overwrite it.")
                .font(.callout)
                .foregroundStyle(.secondary)
            Button("Try Again") {
                connectionStore.retryUnresolvedCredentials()
                // A key that has just become readable is a configuration that has just become
                // usable, and nothing else would notice. Without this the fields fill in, Save
                // stays disabled because nothing is unsaved, and the fleet never starts polling.
                // `reloadSettings` is idempotent — it early-returns when nothing changed — so it
                // costs nothing when the retry found the Keychain still locked.
                model.reloadSettings()
                reseedIfUnedited()
            }
        }
        .fixedSize(horizontal: false, vertical: true)
    }

    // MARK: - Fleet

    @ViewBuilder
    private var fleetSection: some View {
        Section("Fleet") {
            TextField("Environment", text: $draft.environment, prompt: Text(verbatim: "production"))
                .autocorrectionDisabled()
            Text("The URN segment your agents report under. Every alert rule id embeds it.")
                .font(.callout)
                .foregroundStyle(.secondary)

            HStack {
                TextField("Poll interval", text: $draft.pollIntervalSeconds)
                    .frame(maxWidth: 90)
                Text("seconds")
                    .foregroundStyle(.secondary)
            }
            Text("How often the backend is asked. When the next poll actually happens is still core's call — after a failure it backs off on its own.")
                .font(.callout)
                .foregroundStyle(.secondary)
        }
    }

    // MARK: - Warnings

    /// Core's "this is legal, and probably not what you meant".
    ///
    /// Two sources, never mixed. With unsaved changes it is `fleet_config_audit` over the draft —
    /// the function core documents as being for a settings screen mid-edit, so a mistake is named
    /// before it is committed. With nothing unsaved it is what `set_config` returned, which is the
    /// judgement actually in force. Showing both at once would invite the reader to diff two lists
    /// that are usually identical and occasionally, confusingly, are not.
    @ViewBuilder
    private func warningsSection(_ candidate: Result<FleetConfigRecord, any Error>) -> some View {
        let unsaved = hasUnsavedChanges
        let warnings: [TuningWarningRecord] = unsaved
            ? ((try? candidate.get()).flatMap { try? fleetConfigAudit(config: $0) } ?? [])
            : committedWarnings

        if !warnings.isEmpty {
            Section {
                ForEach(warnings.indices, id: \.self) { index in
                    Label {
                        // The sentence is core's. Composing one here would be a second voice saying
                        // almost the same thing, which both apps would then have to reimplement.
                        Text(tuningWarningMessage(warning: warnings[index]))
                            .fixedSize(horizontal: false, vertical: true)
                    } icon: {
                        Image(systemName: "exclamationmark.triangle.fill")
                            .foregroundStyle(.orange)
                    }
                }
            } header: {
                Text(unsaved ? "Warnings for Your Unsaved Changes" : "Configuration Warnings")
            }
        }
    }

    // MARK: - Status

    /// Anything currently wrong that this window did not cause, and would otherwise go unsaid.
    ///
    /// `lastPersistenceError` matters more than it looks: `FleetModel.applyConfig` swallows a failed
    /// write into that property rather than throwing, so a config that reached core but never
    /// reached the disk would look like a clean save and quietly revert on the next launch.
    @ViewBuilder
    private var statusSection: some View {
        let sessionProblem: String? = {
            if case let .unusable(message) = model.sessionState { return message }
            return nil
        }()
        let storedProblem = connectionStore.settingsProblem
        let persistenceProblem = model.lastPersistenceError

        if sessionProblem != nil || storedProblem != nil || persistenceProblem != nil {
            Section("Status") {
                if let sessionProblem {
                    notice(
                        icon: "xmark.octagon.fill",
                        tint: .red,
                        title: "The settings in force were refused.",
                        detail: sessionProblem
                    )
                }
                if let storedProblem {
                    notice(
                        icon: "exclamationmark.triangle.fill",
                        tint: .orange,
                        title: "The saved settings could not be turned into a configuration.",
                        detail: storedProblem
                    )
                }
                if let persistenceProblem {
                    notice(
                        icon: "externaldrive.badge.exclamationmark",
                        tint: .orange,
                        title: "Something could not be written to disk.",
                        detail: persistenceProblem
                    )
                }
            }
        }
    }

    @ViewBuilder
    private func notice(icon: String, tint: Color, title: String, detail: String) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Image(systemName: icon)
                .foregroundStyle(tint)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 3) {
                Text(title)
                Text(detail)
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .textSelection(.enabled)
            }
        }
        .fixedSize(horizontal: false, vertical: true)
    }

    // MARK: - Footer

    @ViewBuilder
    private func footer(_ candidate: Result<FleetConfigRecord, any Error>) -> some View {
        Section {
            // Core's refusal of the draft, shown where the user is about to press Save rather than
            // beside the field that caused it: the interlocks run between fields, so there is often
            // no single field to blame.
            if hasUnsavedChanges, case let .failure(error) = candidate {
                notice(
                    icon: "exclamationmark.triangle.fill",
                    tint: .orange,
                    title: "These settings cannot be saved yet.",
                    detail: FleetModel.message(for: error)
                )
            }

            if let saveError {
                notice(
                    icon: "xmark.octagon.fill",
                    tint: .red,
                    title: "The save did not complete.",
                    detail: saveError
                )
            }

            HStack(spacing: 8) {
                if isSaving {
                    ProgressView()
                        .controlSize(.small)
                }
                Spacer()
                Button("Revert") { reseed() }
                    .disabled(!hasUnsavedChanges || isSaving)
                Button("Save") {
                    Task { await save() }
                }
                .keyboardShortcut(.defaultAction)
                .disabled(!canSave(candidate))
            }
        }
    }

    private var hasUnsavedChanges: Bool { draft != committed }

    private func canSave(_ candidate: Result<FleetConfigRecord, any Error>) -> Bool {
        guard hasUnsavedChanges, !isSaving else { return false }
        guard draft.rulesUnreadable == nil else { return false }
        guard connectionStore.credentialProblem == nil || !connectionChanged else { return false }
        if case .failure = candidate { return false }
        return true
    }

    private var connectionChanged: Bool {
        storedConnection != FleetConnection(baseURL: draft.baseURL, apiKey: draft.apiKey)
    }

    // MARK: - Building the config

    /// The draft as a `FleetConfigRecord`, or core's reason for refusing it.
    ///
    /// Two things are decided here, both consequences of the fields being text: an interval that is
    /// not a whole number cannot be handed to an `Int64` at all, and a config needs a starting point
    /// for the fields this window does not edit. Everything else — whether the interval is sane
    /// against the liveness policy, whether the environment can be a URN segment, whether the rules
    /// are coherent — is asked of core and reported in core's words.
    private func makeConfig() throws -> FleetConfigRecord {
        let environment = draft.environment.trimmingCharacters(in: .whitespacesAndNewlines)
        let intervalText = draft.pollIntervalSeconds.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let intervalSeconds = Int64(intervalText) else {
            throw SettingsInputError.pollIntervalNotAWholeNumber
        }

        // The fields this window does not edit — the liveness policy, the metric sets, the focused
        // host, the chart window — are carried through from the config in force, or from the one on
        // disk when no session has been built from it yet. Only when there is neither does
        // `fleet_config_defaults` supply them, and its refusal of an environment that cannot be a
        // URN segment is exactly the message a first-run user needs.
        let base = try model.config ?? storedConfig ?? fleetConfigDefaults(environment: environment)

        // `PollTuningRecord`'s fields are `let` and there is no partial update, so the whole record
        // is rebuilt with one field substituted and handed back to core to be judged.
        let tuning = try pollTuningValidated(
            tuning: PollTuningRecord(
                liveness: base.tuning.liveness,
                pollIntervalSeconds: intervalSeconds,
                metricStepSeconds: base.tuning.metricStepSeconds,
                chartWindowSeconds: base.tuning.chartWindowSeconds,
                maxStalenessSeconds: base.tuning.maxStalenessSeconds,
                backendLagAllowanceSeconds: base.tuning.backendLagAllowanceSeconds,
                forgetHostAfterSeconds: base.tuning.forgetHostAfterSeconds,
                maxRetainedHosts: base.tuning.maxRetainedHosts
            )
        )

        return FleetConfigRecord(
            environment: environment,
            tuning: tuning,
            rulesJson: try alertRulesToJson(rules: draft.rules),
            overviewMetrics: base.overviewMetrics,
            detailMetrics: base.detailMetrics,
            focus: base.focus
        )
    }

    // MARK: - Actions

    /// Runs `probe` against what is currently typed, not against what is saved.
    ///
    /// The whole point of the button is to answer "will these credentials work?" *before* committing
    /// them, so it builds a throwaway session from the draft. `FleetModel.probe(connection:config:)`
    /// does that with no cached state, so it cannot disturb the fleet already on screen.
    private func runProbe() async {
        probeState = .running
        do {
            let config = try makeConfig()
            let record = try await model.probe(
                connection: FleetConnection(baseURL: draft.baseURL, apiKey: draft.apiKey),
                config: config
            )
            probeState = .finished(record)
        } catch {
            probeState = .failed(FleetModel.message(for: error))
        }
    }

    /// Commits the draft, in the order the platform contract requires.
    ///
    /// The order matters and is not obvious. The connection has to reach the store before the model
    /// is asked to reload, because that is where the model reads it from. The config has to reach
    /// the store too, but only while there is no session: `reloadSettings` will not build one
    /// without a config, and `applyConfig` — the path that goes through `set_config` and returns the
    /// warnings this window renders — refuses to run without a session. So a first run bootstraps
    /// through the store, and every save after that goes through core.
    private func save() async {
        saveError = nil

        let config: FleetConfigRecord
        do {
            config = try makeConfig()
        } catch {
            saveError = FleetModel.message(for: error)
            return
        }

        isSaving = true
        defer { isSaving = false }

        let connection = FleetConnection(baseURL: draft.baseURL, apiKey: draft.apiKey)
        let connectionIsNew = storedConnection != connection
        let needsSession = model.sessionState != .ready

        if connectionIsNew {
            // Belt and braces: `canSave` already refuses this. The key is the one value where a
            // stale enablement check would destroy something unrecoverable.
            guard connectionStore.credentialProblem == nil else {
                saveError = "The saved API key still cannot be read, so it must not be overwritten."
                return
            }
            do {
                try connectionStore.save(connection: connection)
                storedConnection = connection
            } catch {
                saveError = FleetModel.message(for: error)
                return
            }
        }

        if needsSession {
            do {
                try connectionStore.saveConfig(config)
                storedConfig = config
            } catch {
                saveError = FleetModel.message(for: error)
                return
            }
        }

        if connectionIsNew || needsSession {
            model.reloadSettings()
        }

        guard model.sessionState == .ready else {
            saveError = notReadyMessage()
            return
        }

        do {
            // The deciding pass: `set_config` judges the whole config and hands back the warnings.
            // Nothing here decides the config is acceptable — this call is the deciding.
            committedWarnings = try await model.applyConfig(config)
            committed = draft
            // A saved connection invalidates whatever the last test said about the old one.
            if connectionIsNew { probeState = .idle }
        } catch {
            saveError = FleetModel.message(for: error)
        }
    }

    private func notReadyMessage() -> String {
        switch model.sessionState {
        case .ready:
            return ""
        case let .unusable(message):
            return message
        case .unconfigured:
            return connectionStore.settingsProblem ?? """
                The settings were written, but no session could be built from them. \
                Check the backend address and the environment name.
                """
        }
    }

    // MARK: - Seeding

    private func reseedIfUnedited() {
        guard !hasUnsavedChanges else { return }
        reseed()
    }

    private func reseed() {
        let seed = Self.seed(model: model, connectionStore: connectionStore)
        draft = seed.draft
        committed = seed.draft
        storedConnection = seed.connection
        storedConfig = seed.config
        committedWarnings = model.configWarnings
        saveError = nil
        // The last test described values that are no longer in the fields.
        probeState = .idle
    }

    /// Reads the draft's starting values out of the model and the store.
    ///
    /// Static so that it can run before `self` is fully initialised, which is what lets the window
    /// open with its fields already filled rather than flashing empty for a frame.
    ///
    /// The model's config is preferred over the store's because it is the one in force; the store's
    /// is the fallback for the case where settings exist but no session was built from them — an
    /// environment saved before a backend URL was, say, which would otherwise show as blank and
    /// invite the user to type it again.
    private struct Seed {
        var draft: Draft
        var connection: FleetConnection?
        var config: FleetConfigRecord?
    }

    @MainActor
    private static func seed(
        model: FleetModel,
        connectionStore: any SettingsConnectionStore
    ) -> Seed {
        let storedConfig = connectionStore.config
        let config = model.config ?? storedConfig
        let connection = connectionStore.connection

        var rules: [AlertRuleRecord] = []
        var rulesUnreadable: String?
        if let rulesJson = config?.rulesJson {
            do {
                rules = try alertRulesFromJson(rulesJson: rulesJson)
            } catch {
                rulesUnreadable = FleetModel.message(for: error)
            }
        }

        // `poll_tuning_defaults` is infallible by design — it is what a client falls back to when
        // nothing is stored — so even a first run opens with a real interval in the field instead of
        // a blank one the user has to guess at.
        let interval = config?.tuning.pollIntervalSeconds ?? pollTuningDefaults().pollIntervalSeconds

        return Seed(
            draft: Draft(
                baseURL: connection?.baseURL ?? "",
                apiKey: connection?.apiKey ?? "",
                environment: config?.environment ?? "",
                pollIntervalSeconds: String(interval),
                rules: rules,
                rulesUnreadable: rulesUnreadable
            ),
            connection: connection,
            config: storedConfig
        )
    }
}

/// The only refusals this window makes on its own, and it is text-field parsing.
///
/// Deliberately tiny. Anything that could be phrased as a judgement about a *value* rather than
/// about its spelling belongs to core, which says it better and says it once for both apps.
enum SettingsInputError: Error, Equatable, LocalizedError {
    case pollIntervalNotAWholeNumber

    var message: String {
        switch self {
        case .pollIntervalNotAWholeNumber:
            return "The poll interval must be a whole number of seconds."
        }
    }

    var errorDescription: String? { message }
}
