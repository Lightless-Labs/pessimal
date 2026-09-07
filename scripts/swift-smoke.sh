#!/usr/bin/env bash
# Compile and run a real Swift binary against the generated bindings and the Rust staticlib.
#
#   ./scripts/swift-smoke.sh
#
# This exists because one claim in the FFI design cannot be checked from Rust at all: that
# `#[uniffi::export(async_runtime = "tokio")]` actually gives reqwest a reactor when the future is
# driven by *Swift's* executor rather than by a Rust test harness. The sibling projects each grew a
# hand-rolled runtime.rs because that path fails silently — it hangs, it does not error — so the
# only honest test is to drive it from Swift.
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

// Awaiting from Swift's executor is the whole point: a Rust-side test would drive the future on a
// tokio worker and prove nothing about the boundary.
let sem = DispatchSemaphore(value: 0)
var failed = false
Task {
    let probe = await pessimalSmokeProbe()
    print("swift <- rust: \(probe)")
    if probe.isEmpty { failed = true }
    sem.signal()
}
if sem.wait(timeout: .now() + 30) == .timedOut {
    print("TIMED OUT — the async export never completed, which is what an unwired reactor looks like")
    exit(2)
}
exit(failed ? 1 : 0)
SWIFT

swiftc -O \
    -Xcc -fmodule-map-file="$BINDINGS/PessimalFFIFFI.modulemap" -I "$BINDINGS" \
    -L target/debug -lpessimal_ffi \
    -framework SystemConfiguration -framework CoreFoundation -framework Security \
    "$BINDINGS/PessimalFFI.swift" "$WORK/main.swift" -o "$WORK/smoke"

"$WORK/smoke"
