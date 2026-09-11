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
exit(0)
SWIFT

swiftc -O \
    -Xcc -fmodule-map-file="$BINDINGS/PessimalFFIFFI.modulemap" -I "$BINDINGS" \
    -L target/debug -lpessimal_ffi \
    -framework SystemConfiguration -framework CoreFoundation -framework Security \
    "$BINDINGS/PessimalFFI.swift" "$WORK/main.swift" -o "$WORK/smoke"

"$WORK/smoke"
