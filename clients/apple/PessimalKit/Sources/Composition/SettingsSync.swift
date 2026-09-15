//
//  SettingsSync.swift
//  Pessimal — shared by the macOS and iOS clients
//
//  Runs core's settings sync decisions against the mailbox, this device's replica and the settings
//  store. It decides nothing itself: what to persist, apply and publish all comes from core's
//  `settings_sync` module.
//

import Foundation
import Observation
import os
#if canImport(PessimalFFI)
    // Built as its own module for iOS, compiled into the app's module on macOS. See the note in
    // `FleetModel.swift`.
    import PessimalFFI
#endif

/// Syncs the environment, the poll interval and the alert rules with the user's other devices.
///
/// One round reads the replica, the shared value and the three stored settings, then asks core.
/// It persists the replica core returns before anything else, writes only the settings that
/// changed, and then publishes. A round is synchronous on the main actor, so nothing runs between
/// reading the replica and writing it.
@MainActor
@Observable
public final class SettingsSync {
    /// Why a round runs. Every reason except ``initialSync`` publishes.
    public enum Reason: Sendable, Equatable {
        case launch
        case serverChange
        /// The first download for this account finished. Apple advises a delay before writing, so
        /// this round does not publish, and ``initialSyncSettled`` runs after the delay.
        case initialSync
        case initialSyncSettled
        case accountChanged
        case foreground
        case refresh
        case localSave

        var publishes: Bool { self != .initialSync }
    }

    /// A Save that ``SettingsSync/recordEdits(base:edited:)`` recorded and that is not persisted yet.
    public struct Edit {
        /// The config the Save stores and applies.
        public let config: FleetConfigRecord
        /// The replica with the Save's new entries.
        fileprivate let replicaDocument: String
    }

    // MARK: - State the settings screens read

    /// Core's status for the last round. `nil` before the first round.
    public private(set) var status: SettingsSyncStatusRecord?

    /// What `synchronize()` returned in ``start()``, or `false` when there is no mailbox. `nil`
    /// before ``start()``. It is shown, never used to switch sync off.
    public private(set) var isMailboxAvailable: Bool?

    /// Set when the store reports a quota violation. An account change clears it.
    public private(set) var quotaExceeded = false

    /// Live synced rules this build cannot decode or accept.
    public private(set) var hiddenRuleCount: UInt32 = 0

    /// Core's reasons for synced values it did not apply.
    public private(set) var rejected: [String] = []

    // MARK: - Collaborators

    @ObservationIgnored private let mailbox: (any SettingsMailbox)?
    @ObservationIgnored private let replica: any SettingsReplicaStore
    @ObservationIgnored private let settings: any SettingsStore
    @ObservationIgnored private let initialSyncDelay: Duration
    @ObservationIgnored private let onSettingsChanged: @MainActor () -> Void
    @ObservationIgnored private var started = false
    @ObservationIgnored private var initialSyncRound: Task<Void, Never>?

    private static let log = Logger(subsystem: "com.lightless-labs.pessimal", category: "settings-sync")

    /// - Parameters:
    ///   - mailbox: where the document is shared. `nil` shares nothing, and rounds still keep the
    ///     replica and the settings current.
    ///   - replica: this device's merged copy of the document.
    ///   - settings: the store the three synced settings are written to.
    ///   - initialSyncDelay: how long to wait after an initial-sync change before publishing.
    ///   - onSettingsChanged: called after a round writes a setting. The apps pass
    ///     `FleetModel.reloadSettings()`.
    public init(
        mailbox: (any SettingsMailbox)?,
        replica: any SettingsReplicaStore,
        settings: any SettingsStore,
        initialSyncDelay: Duration = .seconds(10),
        onSettingsChanged: @escaping @MainActor () -> Void
    ) {
        self.mailbox = mailbox
        self.replica = replica
        self.settings = settings
        self.initialSyncDelay = initialSyncDelay
        self.onSettingsChanged = onSettingsChanged
    }

    // MARK: - Rounds

    /// Observes the mailbox, runs the launch round, then asks the store to sync. Idempotent.
    ///
    /// Call before `FleetModel.start()`, so the fleet starts from the synced settings.
    public func start() {
        guard !started else { return }
        started = true

        mailbox?.observe { [weak self] change in
            // The store may report on any thread.
            Task { @MainActor [weak self] in
                self?.handle(change)
            }
        }
        run(.launch)

        let available = mailbox?.synchronize() ?? false
        setIfChanged(\.isMailboxAvailable, available)
        Self.log.notice("settings mailbox synchronize() returned \(available, privacy: .public)")
    }

    /// Runs one round.
    public func run(_ reason: Reason) {
        let applied = storedSettings()
        let round = settingsSyncStep(
            localDocument: replica.document,
            remoteDocument: mailbox?.read(),
            applied: applied,
            lastReplacedDigest: replica.replacedRemoteDigest
        )

        // Core derives this device's stamp clock from the replica, so it is persisted first.
        if let document = round.replicaDocument {
            replica.document = document
        }

        let changed = write(round.settings, over: applied)

        if reason.publishes, let text = round.publishDocument, let mailbox {
            mailbox.write(text)
            if let digest = round.replacedDigest {
                replica.replacedRemoteDigest = digest
            }
        }

        setIfChanged(\.status, round.status)
        setIfChanged(\.hiddenRuleCount, round.hiddenRuleCount)
        setIfChanged(\.rejected, round.rejected)

        if changed {
            onSettingsChanged()
        }
    }

    /// Records a settings Save. Persists nothing: pass the result to ``persist(_:)`` once the Save
    /// goes ahead.
    ///
    /// - Parameters:
    ///   - base: the values the settings screen started from, which is its `committed` draft.
    ///     `rulesJson` is `nil` when those rules could not be read, and then no rule is deleted.
    ///   - edited: the config the screen built from its draft.
    /// - Returns: the edit. Its `config` is `edited`, with the environment, the poll interval and the
    ///   rules taken from core's projection. They differ from `edited` only where another device
    ///   changed a value this Save did not change.
    /// - Throws: core's refusal of an edited value.
    public func recordEdits(base: SyncedSettingsRecord, edited: FleetConfigRecord) throws -> Edit {
        let edit = try settingsSyncRecordEdits(
            localDocument: replica.document,
            applied: storedSettings(),
            base: base,
            edited: SyncedSettingsRecord(
                environment: edited.environment,
                pollIntervalSeconds: edited.tuning.pollIntervalSeconds,
                rulesJson: edited.rulesJson
            ),
            nowUnixMs: FleetModel.millis(Date())
        )

        // Core can leave a value unset, for example a first-run interval that was never changed.
        // The screen's own value is used then.
        let projected = edit.settings
        let config = FleetConfigRecord(
            environment: projected.environment ?? edited.environment,
            tuning: try tuning(
                edited.tuning,
                pollIntervalSeconds: projected.pollIntervalSeconds ?? edited.tuning.pollIntervalSeconds
            ),
            rulesJson: projected.rulesJson ?? edited.rulesJson,
            overviewMetrics: edited.overviewMetrics,
            detailMetrics: edited.detailMetrics,
            focus: edited.focus
        )
        return Edit(config: config, replicaDocument: edit.replicaDocument)
    }

    /// Persists a recorded Save in the replica. Call it once the Save goes ahead, then run a
    /// ``Reason/localSave`` round.
    ///
    /// A Save that never calls this leaves the replica as it was, so the next round neither applies
    /// nor publishes it.
    public func persist(_ edit: Edit) {
        // A round may have merged a remote change into the replica since `recordEdits` read it, so
        // the edit is joined with the current replica, not written over it. A step with the current
        // replica as the remote document returns that join.
        let joined = settingsSyncStep(
            localDocument: edit.replicaDocument,
            remoteDocument: replica.document,
            applied: storedSettings(),
            lastReplacedDigest: nil
        )
        replica.document = joined.replicaDocument ?? edit.replicaDocument
    }

    /// The sync line for the settings screens: the status, rules this build hides, and synced
    /// values core refused. `nil` when there is nothing to say.
    public var footnote: String? {
        var lines: [String] = []
        if let statusLine {
            lines.append(statusLine)
        }
        switch hiddenRuleCount {
        case 0:
            break
        case 1:
            lines.append("1 alert rule needs a newer Pessimal.")
        default:
            lines.append("\(hiddenRuleCount) alert rules need a newer Pessimal.")
        }
        lines += rejected.map { "A synced value was not applied: \($0)" }
        return lines.isEmpty ? nil : lines.joined(separator: "\n")
    }

    // MARK: - Private

    private var statusLine: String? {
        if isMailboxAvailable == false {
            return "iCloud is unavailable, so these settings stay on this device."
        }
        if quotaExceeded {
            return "iCloud has no room for these settings, so changes may stay on this device."
        }
        switch status {
        case nil:
            return nil
        case .upToDate:
            return "The environment, poll interval and alert rules sync through iCloud."
        case let .pausedNewerFormat(format):
            return "iCloud holds settings from a newer Pessimal (format \(format)). Sync is paused until this app is updated."
        case .remoteReplaced:
            return "The settings in iCloud could not be read, so this device's settings replaced them."
        case .remoteUnreadable:
            return "The settings in iCloud cannot be read. Sync is paused until they change."
        case let .tooLarge(bytes):
            return "The settings are too large for iCloud (\(bytes) bytes), so changes stay on this device."
        case let .bootstrapDeferred(message):
            return "Sync is off until the unreadable alert rules are discarded and saved: \(message)"
        }
    }

    private func handle(_ change: SettingsMailboxChange) {
        switch change {
        case .serverChange:
            run(.serverChange)

        case .initialSync:
            run(.initialSync)
            initialSyncRound?.cancel()
            let delay = initialSyncDelay
            initialSyncRound = Task { [weak self] in
                try? await Task.sleep(for: delay)
                guard !Task.isCancelled else { return }
                self?.run(.initialSyncSettled)
            }

        case .accountChanged:
            // The replica is rebuilt from the stored settings at the bootstrap stamp, so the new
            // account's values win.
            Self.log.notice("iCloud account changed; rebuilding the settings replica")
            replica.document = nil
            replica.replacedRemoteDigest = nil
            setIfChanged(\.quotaExceeded, false)
            run(.accountChanged)

        case .quotaViolation:
            // Not retried here.
            Self.log.error("the settings mailbox reported a quota violation")
            setIfChanged(\.quotaExceeded, true)
        }
    }

    private func storedSettings() -> SyncedSettingsRecord {
        SyncedSettingsRecord(
            environment: settings.environment,
            pollIntervalSeconds: settings.pollIntervalSeconds,
            rulesJson: settings.alertRulesJSON
        )
    }

    /// Writes each setting that differs from `applied`. Returns whether any was written.
    private func write(_ projected: SyncedSettingsRecord, over applied: SyncedSettingsRecord) -> Bool {
        var changed = false
        if projected.environment != applied.environment {
            settings.environment = projected.environment
            changed = true
        }
        if projected.pollIntervalSeconds != applied.pollIntervalSeconds {
            settings.pollIntervalSeconds = projected.pollIntervalSeconds
            changed = true
        }
        if projected.rulesJson != applied.rulesJson {
            settings.alertRulesJSON = projected.rulesJson
            changed = true
        }
        return changed
    }

    /// `base` with its poll interval replaced, checked by core. `PollTuningRecord` has no partial
    /// update, so the whole record is rebuilt.
    private func tuning(_ base: PollTuningRecord, pollIntervalSeconds: Int64) throws -> PollTuningRecord {
        guard pollIntervalSeconds != base.pollIntervalSeconds else { return base }
        return try pollTuningValidated(
            tuning: PollTuningRecord(
                liveness: base.liveness,
                pollIntervalSeconds: pollIntervalSeconds,
                metricStepSeconds: base.metricStepSeconds,
                chartWindowSeconds: base.chartWindowSeconds,
                maxStalenessSeconds: base.maxStalenessSeconds,
                backendLagAllowanceSeconds: base.backendLagAllowanceSeconds,
                forgetHostAfterSeconds: base.forgetHostAfterSeconds,
                maxRetainedHosts: base.maxRetainedHosts
            )
        )
    }

    /// Assigns only a new value. `@Observable` reports every assignment, equal or not.
    private func setIfChanged<Value: Equatable>(
        _ keyPath: ReferenceWritableKeyPath<SettingsSync, Value>,
        _ value: Value
    ) {
        guard self[keyPath: keyPath] != value else { return }
        self[keyPath: keyPath] = value
    }
}
