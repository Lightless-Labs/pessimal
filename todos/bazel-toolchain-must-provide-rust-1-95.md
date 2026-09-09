# Bazel: unresolved, and not currently runnable on this machine

Pessimal's MSRV is 1.95, set by `sysinfo` rather than by choice, and the sibling projects all pin
`rules_rust` 0.68.1, which predates it. Copying that pin is the obvious mistake to make.

## What was tried (2026-09-09)

A minimal probe — `rules_rust` 0.74.0, `rust.toolchain(versions = ["1.95.0"])`, one `rust_binary`,
Bazel 8.2.1 via bazelisk — to answer whether the toolchain can be provided at all.

**It could not be run.** Bazel's server starts (its `jvm.out` shows only the usual deprecated-flag
warning) but the client never connects, timing out after 120s against a fresh output base, in two
different directories, with the agent sandbox both on and off. The sibling projects' `bazel-out`
symlinks show Bazel works for the owner normally, so this is specific to the agent session rather
than to the machine.

So the toolchain question is still open. What was established is narrower: `rules_rust` 0.74.0 is
current (2026-08-28), and `rust/known_shas.bzl` no longer exists at that path in either 0.68.1 or
0.74.0, so the version-resolution mechanism has changed and must be read rather than assumed.

## What was done instead

The macOS app is built the way the sibling project **Descartes** builds and ships its notarized
menu bar app: plain `swiftc`, a hand-assembled `.app` bundle, and an `Info.plist` template. No Xcode
project, no Bazel, no `rules_apple`. Verified here — a SwiftUI `MenuBarExtra` compiles that way, and
`swiftc` already links the Rust staticlib for `scripts/swift-smoke.sh`.

That is a smaller, proven path for a menu bar app, and it removes this blocker from M5 entirely.

## What still has to be decided

Bazel was an explicit requirement at the outset ("UniFFI + Bazel, you'll find docs and examples for
that in the phil-connors and kumbaya projects"). The macOS app no longer needs it. **iOS (M6) is the
real question**: App Store submission wants an Xcode project, which the siblings generate with
`rules_xcodeproj`. The options are to bring Bazel back for iOS, to check in an Xcode project, or to
generate one another way.

Do not settle this by drifting. It needs the owner, and it needs Bazel to actually run somewhere.
