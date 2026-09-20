#!/usr/bin/env bash
# Compile and run a real Swift binary against the generated bindings and the Rust library.
#
#   ./scripts/swift-smoke.sh
#
# This exists because one claim in the FFI design cannot be checked from Rust at all: that
# `#[uniffi::export(async_runtime = "tokio")]` actually gives reqwest a reactor when the future is
# driven by *Swift's* executor rather than by a Rust test harness. The sibling projects each grew a
# hand-rolled runtime.rs because that path fails silently — it hangs, it does not error — so the
# only honest test is to drive it from Swift.
#
# It needs no backend, no credentials and no network: the session is pointed at 127.0.0.1:9, where
# nothing listens, so the one real HTTP attempt is refused by the loopback stack immediately. That
# refusal is exactly as good a reactor test as a successful GET and a great deal cheaper — reqwest
# cannot complete *or* fail a TCP connect without a tokio IO driver under it — and it doubles as the
# end-to-end check of §4.11's other load-bearing rule: a backend failure must arrive as *data* on a
# view that still renders, never as a thrown Swift error.
#
# It also runs settings sync for two devices through the real `SettingsSync` coordinator, the
# in-memory mailbox and replica stores, and the Rust merge. One write is dropped. Both replicas must
# end with the same bytes as the shared cell, and a further round must publish nothing.
#
# macOS only; needs a Swift toolchain. Not run on Linux CI.
set -euo pipefail

cd "$(dirname "$0")/.."
BINDINGS="clients/apple/PessimalFFI/Sources"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

[ -f "$BINDINGS/PessimalFFI.swift" ] || { echo "no bindings; run ./tools/uniffi/regen.sh first" >&2; exit 1; }

cargo build -p pessimal_ffi

cat > "$WORK/main.swift" <<'SWIFT'
import Foundation

struct SmokeFailure: Error, CustomStringConvertible { let description: String }

func check(_ holds: Bool, _ what: String) throws {
    guard holds else { throw SmokeFailure(description: what) }
    print("  ok  \(what)")
}

func nowMillis() -> Int64 { Int64(Date().timeIntervalSince1970 * 1000) }

/// Port 9 is discard and nothing is bound to it; loopback needs no network. A connect is therefore
/// refused at once, which `reqwest` reports as `is_connect()` and the SigNoz client maps to
/// `CoreError::Unreachable`.
let deadBackend = "http://127.0.0.1:9"

func smoke() async throws {
    // 1. A record crosses outward. These values were chosen by core's `FleetConfig::new`, not typed
    //    here, so this also checks the lift of a nested record and two enum arrays.
    let config = try fleetConfigDefaults(environment: "swift-smoke")
    try check(config.environment == "swift-smoke", "a config record keeps its environment across the boundary")
    try check(!config.overviewMetrics.isEmpty, "core chose the overview metrics")
    try check(config.rulesJson == "[]", "a default config carries no rules")
    try check(config.tuning.pollIntervalSeconds > 0, "the nested tuning record crossed too")

    // 2. The composition root. No request is made here: `new` validates the config through core and
    //    builds the timeout-carrying reqwest client.
    // Usage reporting is pointed at the same dead loopback port, for the same reason the backend is:
    // an export that cannot complete is the interesting case. A reporter built here must buffer, must
    // never throw, and must leave the poll's answer untouched.
    //
    // `optedOut: false` and a non-empty destination, so a reporter genuinely exists — unless the
    // environment denies it, which `CI=true` does, and the assertions below account for both.
    let usage = UsageReportingRecord(
        endpoint: "http://127.0.0.1:9",
        ingestionKey: "swift-smoke-not-a-real-key",
        optedOut: false,
        platform: .macos,
        deviceClass: .mac,
        appVersion: "0.1.0",
        appBuild: 1,
        osVersion: "15.0"
    )

    let session = try FleetSession(
        baseUrl: deadBackend,
        apiKey: "swift-smoke-not-a-real-key",
        config: config,
        cachedStateJson: nil,
        usage: usage
    )
    try check(session.restoreReport() == nil, "no cache was handed in, so there is no restore report")

    // 3. An exported `async fn` with no I/O in it at all, awaited on Swift's executor. This is
    //    UniFFI's async plumbing and the Rust future's completion callback and nothing else — the
    //    half of the boundary that still works when the reactor does not.
    let warnings = try await session.setConfig(config: config)
    try check(warnings.isEmpty, "core audited the default config and had nothing to warn about")

    // 4. THE REASON THIS SCRIPT EXISTS. `poll` reaches `reqwest` through
    //    `#[uniffi::export(async_runtime = "tokio")]`. With no reactor this neither returns nor
    //    throws; it hangs, and the semaphore below is what notices.
    let result = try await session.poll(nowMillis: nowMillis())
    try check(!result.skipped, "nothing else held the in-flight guard")

    // The failure is data, not an exception: the call returned normally and the view came with it.
    guard let failure = result.view.freshness.lastFailure else {
        throw SmokeFailure(description: "a refused poll must leave its failure on the view")
    }
    try check(failure.kind == .unreachable, "a refused TCP connect is unreachable, not unauthorized")
    try check(failure.source == .roster, "the roster is the request that outranks the series ones")
    try check(!failure.isActionable, "only an expired key routes the Open Settings button")
    try check(result.view.freshness.polledThisSession, "the fold ran even though every request failed")
    try check(result.view.hosts.isEmpty && result.view.counts.hosts == 0, "a failed poll never invents a fleet")
    try check(result.transitions.isEmpty, "nothing was observed, so nothing transitioned")

    guard case .retry(let afterSeconds, let consecutiveFailures) = result.advice else {
        throw SmokeFailure(description: "an unreachable backend advises a retry, got \(result.advice)")
    }
    try check(afterSeconds > 0 && consecutiveFailures == 1, "core's backoff schedule, not this script's")

    // 5. The synchronous reads a SwiftUI body makes, on the same session, without an await.
    let view = session.view()
    try check(view.asOfMillis != nil, "a failed poll still moves `as_of`")
    try check(view.backendName == result.view.backendName, "`view()` projects the state the poll stored")

    let freshness = try session.freshness(nowMillis: nowMillis())
    guard case .unusable(let lastSuccess, let failures, let carried) = freshness else {
        throw SmokeFailure(description: "a session that never saw a roster cannot claim its picture is believable, got \(freshness)")
    }
    try check(lastSuccess == nil, "there has never been a successful poll to be as of")
    try check(failures == 1, "exactly the one poll above failed")
    try check(carried?.kind == .unreachable, "the banner is handed the failure that routes its button")

    // 6. Usage reporting, over the same dead port. The claims are all negative, which is the point:
    //    reporting must be incapable of affecting the poll it reports on.
    let diagnostics = session.usageDiagnostics()
    if diagnostics.enabled {
        // A poll happened above, so a trace was buffered — but the batch threshold is higher than one,
        // so nothing has been sent and nothing has failed. Reporting that rode along with the poll
        // would mean the flush was awaited on the poll path, which is the bug this asserts against.
        try check(diagnostics.sent == 0 && diagnostics.failed == 0,
                  "one poll buffers a trace and sends nothing: a batch is not one span")
        try check(diagnostics.buffered == 1, "exactly the one poll above was recorded")
        try check(diagnostics.dropped == 0, "a single trace does not overflow the buffer")

        // And the flush itself: awaited here, against a port that refuses instantly. It must return
        // normally — `flushUsage` is not throwing, and a Swift `try` would not compile — and it must
        // record the failure as data rather than hanging on a reactor that is not there.
        await session.flushUsage()
        let afterFlush = session.usageDiagnostics()
        try check(afterFlush.failed == 1, "an unreachable ingest endpoint fails as data, once")
        try check(afterFlush.buffered == 0, "the batch was drained whether or not it landed")
        try check(afterFlush.sent == 0, "nothing was accepted by a port with nothing behind it")
    } else {
        // `CI=true` denies consent by design, so this is the expected path on a build agent. Assert
        // the *reason*, so a genuine misconfiguration cannot hide behind this branch.
        try check(diagnostics.denial == .continuousIntegration,
                  "reporting is off only because this is CI")
        try check(diagnostics.buffered == 0, "a denied session holds no reporter to buffer into")
    }

    // The poll's own answer must be untouched by any of that.
    try check(view.backendName == result.view.backendName,
              "reporting did not disturb the view the poll produced")

    // 7. The persistence round trip: the string one session exports is the string the next restores.
    let exported = try session.exportState()
    let restored = try FleetSession(
        baseUrl: deadBackend,
        apiKey: "swift-smoke-not-a-real-key",
        config: config,
        cachedStateJson: exported,
        usage: nil
    )
    guard let report = restored.restoreReport() else {
        throw SmokeFailure(description: "a session given a cache reports what it salvaged")
    }
    try check(!report.discardedUnreadable && !report.discardedIncompatible,
              "a state this very process exported restores cleanly")

    print("swift <- rust: backend \(view.backendName), \(freshness)")
}

// Awaiting from Swift's executor is the whole point: a Rust-side test would drive the future on a
// tokio worker and prove nothing about the boundary. The semaphore is the hang detector — an
// unwired reactor does not error, it simply never completes.
let finished = DispatchSemaphore(value: 0)
Task {
    do {
        try await smoke()
    } catch {
        print("FAILED: \(error)")
        exit(1)
    }
    finished.signal()
}
if finished.wait(timeout: .now() + 30) == .timedOut {
    print("TIMED OUT — the async export never completed, which is what an unwired reactor looks like")
    exit(2)
}

// MARK: - Settings sync

/// Counts how often a coordinator asked the app to reload its settings.
@MainActor
final class Counter {
    var value = 0
}

/// Runs the main run loop until `condition` holds or two seconds pass.
///
/// Mailbox notifications and the delayed initial-sync round reach the coordinator as main-actor
/// tasks. Those run only while the main thread is free.
@MainActor
func spin(until condition: () -> Bool) -> Bool {
    let deadline = Date().addingTimeInterval(2)
    while !condition() && Date() < deadline {
        RunLoop.main.run(until: Date().addingTimeInterval(0.01))
    }
    return condition()
}

/// A config as the settings screen's `makeConfig()` builds it.
func screenConfig(environment: String, interval: Int64, rules: [AlertRuleRecord]) throws -> FleetConfigRecord {
    let base = try fleetConfigDefaults(environment: environment)
    let tuning = try pollTuningValidated(
        tuning: PollTuningRecord(
            liveness: base.tuning.liveness,
            pollIntervalSeconds: interval,
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
        rulesJson: try alertRulesToJson(rules: rules),
        overviewMetrics: base.overviewMetrics,
        detailMetrics: base.detailMetrics,
        focus: base.focus
    )
}

/// What `FleetStoreBridge.saveConfig` writes once `FleetModel.applyConfig` accepts a config. A settings
/// screen then calls `SettingsSync.persist` and runs a `.localSave` round.
func store(_ config: FleetConfigRecord, in settings: any SettingsStore) {
    settings.environment = config.environment
    settings.pollIntervalSeconds = config.tuning.pollIntervalSeconds
    settings.alertRulesJSON = config.rulesJson
}

/// The base a settings screen passes to `recordEdits`: the values it was seeded with.
func screenBase(_ settings: any SettingsStore) -> SyncedSettingsRecord {
    SyncedSettingsRecord(
        environment: settings.environment,
        pollIntervalSeconds: settings.pollIntervalSeconds ?? pollTuningDefaults().pollIntervalSeconds,
        rulesJson: settings.alertRulesJSON ?? "[]"
    )
}

@MainActor
func syncSmoke() throws {
    print("settings sync:")
    let defaultInterval = pollTuningDefaults().pollIntervalSeconds
    let delay = Duration.milliseconds(250)

    // Device A was set up before sync existed. Device B is a new install. They share one cell.
    let mailboxA = InMemorySettingsMailbox()
    let mailboxB = InMemorySettingsMailbox(pairedWith: mailboxA)
    let replicaA = InMemorySettingsReplicaStore()
    let replicaB = InMemorySettingsReplicaStore()
    let settingsA = InMemorySettingsStore(
        environment: "swift-smoke",
        pollIntervalSeconds: defaultInterval,
        alertRulesJSON: "[]"
    )
    let settingsB = InMemorySettingsStore()
    let changesA = Counter()
    let changesB = Counter()
    let syncA = SettingsSync(mailbox: mailboxA, replica: replicaA, settings: settingsA, initialSyncDelay: delay) {
        changesA.value += 1
    }
    let syncB = SettingsSync(mailbox: mailboxB, replica: replicaB, settings: settingsB, initialSyncDelay: delay) {
        changesB.value += 1
    }

    syncA.start()
    try check(mailboxA.read() != nil && mailboxA.read() == replicaA.document,
              "a configured device publishes its settings at launch")
    try check(syncA.status == .upToDate && syncA.isMailboxAvailable == true, "and reports that sync is on")
    try check(changesA.value == 0, "launch changes nothing on the device that already had the settings")

    syncB.start()
    try check(settingsB.environment == "swift-smoke" && settingsB.pollIntervalSeconds == defaultInterval,
              "a new device takes the environment and interval from the cell")
    try check(changesB.value == 1, "and asks the app to reload once")
    try check(mailboxB.writeCount == 0, "and publishes nothing, because it holds nothing new")

    // A adds a rule and changes the interval. The cell drops that write.
    let rule = try draftAlertRule(
        environment: "swift-smoke",
        name: "CPU high",
        metric: .cpuUtilization,
        comparator: .greaterThan,
        threshold: 0.9,
        forDurationSeconds: 60,
        selector: .all
    )
    let editedA = try screenConfig(environment: "swift-smoke", interval: 45, rules: [rule])
    let savedA = try syncA.recordEdits(base: screenBase(settingsA), edited: editedA)
    try check(savedA.config == editedA, "a Save with no remote change in between returns what the user typed")
    store(savedA.config, in: settingsA)
    syncA.persist(savedA)
    mailboxA.writesToDrop = 1
    syncA.run(.localSave)
    try check(mailboxA.droppedWriteCount == 1 && mailboxA.read() != replicaA.document,
              "the cell dropped A's write")

    // B changes the environment meanwhile, and its write lands.
    let editedB = try screenConfig(environment: "swift-smoke-b", interval: defaultInterval, rules: [])
    let savedB = try syncB.recordEdits(base: screenBase(settingsB), edited: editedB)
    store(savedB.config, in: settingsB)
    syncB.persist(savedB)
    syncB.run(.localSave)
    try check(mailboxB.read() == replicaB.document, "B's Save reached the cell")

    // A hears about it through the mailbox notification.
    mailboxA.deliver(.serverChange)
    try check(spin { settingsA.environment == "swift-smoke-b" }, "A applies B's environment")
    try check(settingsA.pollIntervalSeconds == 45 && settingsA.alertRulesJSON == savedA.config.rulesJson,
              "and keeps its own interval and rule, although their write was dropped")
    try check(mailboxA.read() == replicaA.document, "A writes the merged copy back")

    mailboxB.deliver(.serverChange)
    try check(spin { settingsB.pollIntervalSeconds == 45 }, "B applies A's interval")
    try check(settingsB.alertRulesJSON == savedA.config.rulesJson, "and A's rule")
    try check(replicaA.document == replicaB.document && replicaA.document == mailboxA.read(),
              "both replicas and the cell hold the same bytes")

    var writes = (mailboxA.writeCount, mailboxB.writeCount)
    syncA.run(.refresh)
    syncB.run(.refresh)
    try check(mailboxA.writeCount == writes.0 && mailboxB.writeCount == writes.1,
              "a further round on each device publishes nothing")

    // A Save that fails after `recordEdits` never reaches `persist`. It leaves nothing to sync.
    let replicaBeforeFailedSave = replicaA.document
    let settingsBeforeFailedSave = (settingsA.environment, settingsA.pollIntervalSeconds, settingsA.alertRulesJSON)
    let changesBeforeFailedSave = changesA.value
    _ = try syncA.recordEdits(
        base: screenBase(settingsA),
        edited: try screenConfig(environment: "swift-smoke-failed", interval: 40, rules: [])
    )
    try check(replicaA.document == replicaBeforeFailedSave, "recording a Save does not persist it")
    syncA.run(.refresh)
    try check(mailboxA.writeCount == writes.0 && changesA.value == changesBeforeFailedSave
              && (settingsA.environment, settingsA.pollIntervalSeconds, settingsA.alertRulesJSON) == settingsBeforeFailedSave,
              "the next round after a failed Save writes nothing")

    // A round that runs between `recordEdits` and `persist`, while core judges the config, merges
    // another device's change. `persist` keeps that change in the replica.
    let mailboxC = InMemorySettingsMailbox()
    let mailboxD = InMemorySettingsMailbox(pairedWith: mailboxC)
    let replicaC = InMemorySettingsReplicaStore()
    let replicaD = InMemorySettingsReplicaStore()
    let settingsC = InMemorySettingsStore(
        environment: "swift-smoke",
        pollIntervalSeconds: defaultInterval,
        alertRulesJSON: "[]"
    )
    let settingsD = InMemorySettingsStore()
    let syncC = SettingsSync(mailbox: mailboxC, replica: replicaC, settings: settingsC) {}
    let syncD = SettingsSync(mailbox: mailboxD, replica: replicaD, settings: settingsD) {}
    syncC.start()
    syncD.start()
    let savedC = try syncC.recordEdits(
        base: screenBase(settingsC),
        edited: try screenConfig(environment: "swift-smoke", interval: 40, rules: [])
    )
    let savedD = try syncD.recordEdits(
        base: screenBase(settingsD),
        edited: try screenConfig(environment: "swift-smoke-d", interval: defaultInterval, rules: [])
    )
    store(savedD.config, in: settingsD)
    syncD.persist(savedD)
    syncD.run(.localSave)
    syncC.run(.serverChange)
    try check(settingsC.environment == "swift-smoke-d", "a round during C's Save applies D's environment")
    store(savedC.config, in: settingsC)
    syncC.persist(savedC)
    // Project C's replica alone, with no mailbox, to see what `persist` left in it.
    let projectedC = InMemorySettingsStore(
        environment: settingsC.environment,
        pollIntervalSeconds: settingsC.pollIntervalSeconds,
        alertRulesJSON: settingsC.alertRulesJSON
    )
    SettingsSync(mailbox: nil, replica: replicaC, settings: projectedC) {}.run(.refresh)
    try check(projectedC.environment == "swift-smoke-d" && projectedC.pollIntervalSeconds == 40,
              "persist joins the Save with the replica: D's environment and C's interval")
    syncC.run(.localSave)
    syncD.run(.refresh)
    try check(settingsC.environment == "swift-smoke-d" && settingsD.pollIntervalSeconds == 40
              && replicaC.document == replicaD.document && replicaC.document == mailboxC.read(),
              "C and D converge")

    // A deletes the rule and that write is dropped too. B changes the interval. An initial-sync
    // change then merges on A without publishing, and A publishes after the delay.
    let editedA2 = try screenConfig(environment: "swift-smoke-b", interval: 45, rules: [])
    let savedA2 = try syncA.recordEdits(base: screenBase(settingsA), edited: editedA2)
    store(savedA2.config, in: settingsA)
    syncA.persist(savedA2)
    mailboxA.writesToDrop = 1
    syncA.run(.localSave)
    let editedB2 = try screenConfig(environment: "swift-smoke-b", interval: 50, rules: [rule])
    let savedB2 = try syncB.recordEdits(base: screenBase(settingsB), edited: editedB2)
    store(savedB2.config, in: settingsB)
    syncB.persist(savedB2)
    syncB.run(.localSave)

    writes.0 = mailboxA.writeCount
    mailboxA.deliver(.initialSync)
    try check(spin { settingsA.pollIntervalSeconds == 50 }, "an initial-sync change merges B's interval on A")
    try check(mailboxA.writeCount == writes.0, "and A does not publish yet")
    try check(spin { mailboxA.read() == replicaA.document }, "A publishes its merged copy after the delay")

    mailboxB.deliver(.serverChange)
    try check(spin { settingsB.alertRulesJSON == "[]" }, "B removes the rule A deleted")
    try check(replicaA.document == replicaB.document && replicaB.document == mailboxB.read(),
              "both replicas and the cell hold the same bytes again")
    writes = (mailboxA.writeCount, mailboxB.writeCount)
    syncA.run(.refresh)
    syncB.run(.refresh)
    try check(mailboxA.writeCount == writes.0 && mailboxB.writeCount == writes.1,
              "and a further round publishes nothing")

    // An account change drops A's replica. An edit A never published loses to the cell.
    let editedA3 = try screenConfig(environment: "swift-smoke-a", interval: 50, rules: [])
    let savedA3 = try syncA.recordEdits(base: screenBase(settingsA), edited: editedA3)
    store(savedA3.config, in: settingsA)
    syncA.persist(savedA3)
    mailboxA.writesToDrop = 1
    syncA.run(.localSave)
    mailboxA.deliver(.accountChanged)
    try check(spin { settingsA.environment == "swift-smoke-b" }, "after an account change the cell's environment wins")
    try check(replicaA.document == mailboxA.read(), "and A's rebuilt replica equals the cell")

    // A synced interval core refuses is not applied, and the screen is told why.
    mailboxB.write(#"{"format":1,"settings":{"poll_interval_seconds":{"at":[253402300799999,0],"value":"0"}},"rules":{}}"#)
    syncA.run(.refresh)
    try check(settingsA.pollIntervalSeconds == 50 && !syncA.rejected.isEmpty,
              "a refused synced interval is kept out of the settings and listed as rejected")
    try check(syncA.footnote?.contains(syncA.rejected[0]) == true, "the footnote carries core's reason")

    // A newer format pauses sync and is never written over.
    let newer = #"{"format":2,"surprise":true}"#
    mailboxB.write(newer)
    writes.0 = mailboxA.writeCount
    syncA.run(.refresh)
    try check(syncA.status == .pausedNewerFormat(format: 2) && mailboxA.writeCount == writes.0,
              "a newer format pauses sync and publishes nothing")
    try check(mailboxA.read() == newer, "the newer document is left as it was")

    mailboxA.deliver(.quotaViolation)
    try check(spin { syncA.quotaExceeded }, "a quota violation is shown")

    // Without iCloud the round still keeps the replica and the settings screen still saves.
    let offlineSettings = InMemorySettingsStore(environment: "swift-smoke", pollIntervalSeconds: defaultInterval, alertRulesJSON: "[]")
    let offline = SettingsSync(
        mailbox: InMemorySettingsMailbox(isAvailable: false),
        replica: InMemorySettingsReplicaStore(),
        settings: offlineSettings
    ) {}
    offline.start()
    try check(offline.isMailboxAvailable == false && offline.footnote != nil, "an unavailable mailbox is reported")
    let noMailbox = SettingsSync(mailbox: nil, replica: InMemorySettingsReplicaStore(), settings: offlineSettings) {}
    noMailbox.start()
    let offlineEdit = try screenConfig(environment: "swift-smoke", interval: 40, rules: [])
    try check(try noMailbox.recordEdits(base: screenBase(offlineSettings), edited: offlineEdit).config == offlineEdit,
              "with no mailbox a Save still returns what the user typed")
    do {
        _ = try noMailbox.recordEdits(
            base: screenBase(offlineSettings),
            edited: try screenConfig(environment: "swift-smoke", interval: 40, rules: []).withEnvironment("a::b")
        )
        throw SmokeFailure(description: "recordEdits must refuse an environment core refuses")
    } catch let error as FfiError {
        guard case .InvalidConfig = error else { throw SmokeFailure(description: "expected InvalidConfig, got \(error)") }
        try check(true, "recordEdits throws core's refusal for an environment with '::'")
    }
}

extension FleetConfigRecord {
    func withEnvironment(_ environment: String) -> FleetConfigRecord {
        FleetConfigRecord(
            environment: environment,
            tuning: tuning,
            rulesJson: rulesJson,
            overviewMetrics: overviewMetrics,
            detailMetrics: detailMetrics,
            focus: focus
        )
    }
}

// The login item, the one piece of logic in the macOS "open at login" switch: what the settings
// window shows for each state macOS can report. Registering anything with the real launchd from a
// test would be a change to the machine running it, so only the mapping and the fake are driven.
func loginItemSmoke() throws {
    try check(SMAppServiceLoginItem.state(from: .enabled) == .enabled, "an enabled login item reads as enabled")
    try check(SMAppServiceLoginItem.state(from: .notRegistered) == .disabled, "an unregistered one reads as disabled")
    try check(SMAppServiceLoginItem.state(from: .requiresApproval) == .requiresApproval,
              "one waiting for the user reads as requiresApproval")
    if case .unavailable = SMAppServiceLoginItem.state(from: .notFound) {
        try check(true, "a login item macOS cannot find reads as unavailable, with a reason")
    } else {
        throw SmokeFailure(description: ".notFound must map to .unavailable")
    }

    let item = InMemoryLoginItemController(state: .disabled)
    try item.setEnabled(true)
    try check(item.state == .enabled, "turning the switch on enables the item")
    try item.setEnabled(false)
    try check(item.state == .disabled, "turning it off disables it")

    struct Refused: Error, LocalizedError { var errorDescription: String? { "refused" } }
    let failing = InMemoryLoginItemController(state: .disabled, failure: Refused())
    do {
        try failing.setEnabled(true)
        throw SmokeFailure(description: "a refused registration must throw")
    } catch is Refused {
        try check(failing.state == .disabled, "a refused registration leaves the item off")
    }
}

// Which fleet tallies the summary shows. The rule is shared by both apps (PessimalKit's
// FleetTally), and this is the only automated place either app's Swift is exercised.
func fleetTallySmoke() throws {
    func counts(
        hosts: UInt32 = 0, alive: UInt32 = 0, stale: UInt32 = 0, down: UInt32 = 0,
        unknown: UInt32 = 0, firing: UInt32 = 0, pending: UInt32 = 0
    ) -> FleetCountsRecord {
        FleetCountsRecord(
            hosts: hosts, alive: alive, stale: stale, down: down, unknown: unknown,
            firingAlerts: firing, pendingAlerts: pending
        )
    }
    func shown(_ counts: FleetCountsRecord) -> [String] {
        FleetTally.visible(in: counts).map { "\($0.value) \($0.tally.word(for: $0.value))" }
    }

    try check(shown(counts(hosts: 1, alive: 1)) == ["1 host", "1 alive"],
              "a healthy fleet shows the total and the alive count, and nothing at zero")
    try check(shown(counts(hosts: 3, alive: 1, stale: 2)) == ["3 hosts", "1 alive", "2 stale"],
              "a tally that is not zero appears, in its fixed place")
    try check(shown(counts()) == ["0 hosts", "0 alive"],
              "an empty fleet still reads as a fleet rather than as nothing")
    try check(shown(counts(hosts: 2, alive: 1, down: 1, firing: 3)).last == "3 firing",
              "alert tallies appear when they fire")
    try check(FleetTally.visible(in: counts(hosts: 9, alive: 9)).allSatisfy { !$0.tally.isAlertTally },
              "and not otherwise, so the alerts line can be dropped whole")

    // The plural is here rather than in each app, because it is the decision that already rotted:
    // both apps used to say "1 hosts".
    try check(FleetTally.hosts.word(for: 1) == "host" && FleetTally.hosts.word(for: 0) == "hosts",
              "one host is a host")

    // Every tally belongs to exactly one of the two macOS rows.
    let liveness = FleetTally.allCases.filter { !$0.isAlertTally }
    let alerts = FleetTally.allCases.filter(\.isAlertTally)
    try check(liveness.count + alerts.count == FleetTally.allCases.count && !alerts.isEmpty,
              "the liveness row and the alerts row together hold every tally")

    // Hiding a zero hides its cell from VoiceOver, so the container speaks all seven.
    let spoken = FleetTally.spokenSummary(of: counts(hosts: 1, alive: 1))
    try check(spoken.contains("0 down") && spoken.contains("0 stale") && spoken.hasPrefix("1 host,"),
              "what is not shown is still spoken")
}

// What the apps print as their version, from the two bundle strings.
func appVersionSmoke() throws {
    try check(AppVersion(marketing: "0.5.0", build: "0.5.0").display == "0.5.0",
              "a build that repeats the release shows the release alone, as the Mac app's does")
    try check(AppVersion(marketing: "0.5.0", build: "0.5.0.74").display == "0.5.0 (74)",
              "a release plus a counter shows the counter, as TestFlight builds do")
    try check(AppVersion(marketing: "0.5.0", build: "dirty").display == "0.5.0 (dirty)",
              "a build string that does not follow the release is shown whole")
    try check(AppVersion(marketing: "", build: "").display.isEmpty,
              "a bundle with neither says nothing rather than 'unknown'")
    try check(AppVersion(marketing: "0.5.0", build: "0.5.0").labelled == "Pessimal 0.5.0",
              "the labelled form names the app")
}

// Which of the iOS settings screen's two actions its toolbar offers, and whether it may be pressed.
// The rule is a pure function of the state the screen already holds — PessimalKit's
// SettingsToolbarAction — so that it can be driven from here rather than only by tapping a phone.
// This script is the only automated place either app's Swift runs.
func settingsToolbarSmoke() throws {
    func next(
        unsaved: Bool,
        answered: Bool,
        canSave: Bool = true,
        canTest: Bool = true
    ) -> SettingsToolbarAction {
        SettingsToolbarAction.next(
            hasUnsavedChanges: unsaved,
            backendAnswered: answered,
            canSave: canSave,
            canTest: canTest
        )
    }

    // A draft the backend has not answered for offers Test, and goes on offering it after a test that
    // failed — the toolbar invites the retry, and Save stays reachable in the form below, because a
    // configuration can be perfectly saveable while a backend is down.
    try check(next(unsaved: true, answered: false, canSave: true) == .test(enabled: true),
              "an untested or failed draft offers Test, saveable or not")
    try check(next(unsaved: true, answered: false, canTest: false) == .test(enabled: false),
              "greyed while a probe is in flight or the configuration is one core refuses")

    try check(next(unsaved: true, answered: true) == .save(enabled: true),
              "once the backend has answered, the outstanding step is Save")
    try check(next(unsaved: true, answered: true, canSave: false) == .save(enabled: false),
              "greyed by the Save button's own rule rather than by a second one")

    // Why the rule cannot read the probe alone: `save()` leaves `probeState` finished unless the
    // connection changed, so a draft that has just been saved would go on offering to test settings
    // that are already in force.
    try check(next(unsaved: false, answered: true, canSave: false) == .save(enabled: false),
              "a saved draft reads Save, disabled — never Test")
    try check(next(unsaved: false, answered: false, canSave: false) == .save(enabled: false),
              "and so does a screen nothing has been typed into yet")
}

do {
    try MainActor.assumeIsolated { try syncSmoke() }
    try loginItemSmoke()
    try fleetTallySmoke()
    try appVersionSmoke()
    try settingsToolbarSmoke()
} catch {
    print("FAILED: \(error)")
    exit(1)
}
exit(0)
SWIFT

# The shared PessimalKit sources compile into the same module as the bindings, as in
# scripts/build-macos-app.sh, so the settings sync case drives the real coordinator.
shared_sources=()
while IFS= read -r source; do
    shared_sources+=("$source")
done < <(find clients/apple/PessimalKit/Sources -type f -name '*.swift' | LC_ALL=C sort)

swiftc -O \
    -Xcc -fmodule-map-file="$BINDINGS/PessimalFFIFFI.modulemap" -I "$BINDINGS" \
    -L target/debug -lpessimal_ffi \
    -framework SystemConfiguration -framework CoreFoundation -framework Security \
    "$BINDINGS/PessimalFFI.swift" "${shared_sources[@]}" "$WORK/main.swift" -o "$WORK/smoke"

"$WORK/smoke"
