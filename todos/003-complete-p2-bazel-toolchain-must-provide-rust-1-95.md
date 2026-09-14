---
status: complete
priority: p2
issue_id: "003"
tags: [bazel, toolchain, msrv, ios]
dependencies: []
---

# Bazel toolchain must provide Rust 1.95

## Problem Statement

Pessimal's MSRV is 1.95, set by `sysinfo` 0.39, not by choice (`Cargo.toml`: `rust-version = "1.95"`,
`sysinfo = "0.39"`). The Bazel toolchain must provide Rust 1.95 or newer.

The sibling projects pin `rules_rust` 0.68.1, which predates Rust 1.95. Copying that pin would not
work.

Bazel is pinned to 8.2.1 because `rules_apple` is not Bazel 9 ready. A newer `rules_rust` must still
work with Bazel 8.2.1 and `rules_apple` 4.3.3.

## Findings

- `MODULE.bazel` pins `rules_rust` 0.74.0 and calls `rust.toolchain` with `edition = "2024"`,
  `versions = ["1.95.0"]` and `extra_target_triples = ["aarch64-apple-ios", "aarch64-apple-ios-sim"]`.
  It needs no `sha256s` override.
- The same file pins `rules_apple` 4.3.3, `rules_swift` 3.4.1, `rules_xcodeproj` 3.0.0,
  `apple_support` 2.1.0, `rules_cc` 0.2.14, `platforms` 1.1.0 and `bazel_skylib` 1.8.2. crate_universe
  reads the same `Cargo.lock` that cargo uses. `MODULE.bazel.lock` is committed.
- `.bazelversion` is 8.2.1.
- The toolchain Bazel fetches reports `rustc 1.95.0 (59807616e 2026-04-14)`.
- Bazel builds the whole iOS chain to `Pessimal.ipa`:
  - The Buildkite step `:bazel: iOS app` (`.buildkite/pipeline.yml:169-223`) runs
    `bazelisk build --config=ci //clients/apple/ios:Pessimal`.
  - `scripts/release-ios-testflight-buildkite.sh` builds the IPA with `bazelisk` (line 278).
  - `BUILD.bazel` defines the `xcodeproj` target (`bazel run //:xcodeproj`).
  - Build 0.1.0.26 reached TestFlight on 2026-09-11. Every push to `main` that passes the three
    verification steps now ships to TestFlight.
- The development machine is an 11 GB VM. On 2026-09-09 it had about 2.8 GB free and 5.3 of 6 GB of
  swap in use. Under that load the Bazel server starts, but it cannot finish starting inside the
  client's default 120s connect window.
- `.bazelrc` handles this for every build:
  - `startup --host_jvm_args=-Xmx1500m`
  - `startup --connect_timeout_secs=900`
  - `build --local_resources=memory=HOST_RAM*.4`

  `--config=ci` raises the memory share to `HOST_RAM*.8`. The JVM heap stays at 1500m in CI, because
  startup flags cannot be config-specific. There is no `--jobs` limit.
- Builds on the development VM are slow. A one-`rust_binary` probe took 727s for 168 actions.
- The macOS menu bar app does not use Bazel. It builds with plain `swiftc` and a hand-assembled `.app`
  bundle, as the sibling project Descartes does, in `scripts/build-macos-app.sh`. The Buildkite step
  `:apple: macOS menu bar app` (`.buildkite/pipeline.yml:225`) builds it. Developer ID signing,
  notarisation and stapling are scripted in `scripts/release-macos-artifacts.sh` (through
  `scripts/notarize-macos-app.sh`), which runs in the `release-macos` step, but have not run yet.
  They first run at the first release tag.
- The CI guest images ship an older default rustc (1.88 on the macOS image). Bazel is not affected,
  because `MODULE.bazel` pins its own 1.95.0 toolchain. The cargo steps install a new enough toolchain
  (`.buildkite/pipeline.yml`, comment at line 82).

## Proposed Solutions

### Option A: Pin `rules_rust` 0.74.0 with Rust 1.95.0

- **Pros:** provides rustc 1.95.0 under Bazel 8.2.1 with no `sha256s` override. Resolves with
  `rules_apple` 4.3.3, `rules_swift` 3.4.1 and `rules_xcodeproj` 3.0.0.
- **Cons:** differs from the siblings' pin, so their `MODULE.bazel` cannot be copied unchanged.

### Option B: Copy the siblings' `rules_rust` 0.68.1

- **Pros:** the dependency set is already proven in kumbaya, phil-connors and bande-a-bonnot.
- **Cons:** 0.68.1 predates Rust 1.95, so it cannot provide the MSRV.

### Option C: Build the Apple apps without Bazel

Use plain `swiftc` and a hand-assembled bundle, as Descartes does.

- **Pros:** no Bazel on a memory-constrained machine. Proven for a notarized menu bar app in Descartes.
- **Cons:** App Store submission wants an Xcode project, and the siblings generate one with
  `rules_xcodeproj`. This path suits the macOS app only.

## Recommended Action

Done. Option A for Bazel, and Option C for the macOS app only.

- 06ec246 (2026-09-10) added `MODULE.bazel` with `rules_rust` 0.74.0 and Rust 1.95.0, and `.bazelrc`.
- 1332e4f committed `MODULE.bazel.lock`.
- 1b62635 and 96aeb88 added the iOS targets, the Xcode project and the CI step that builds the app.
- 59869ca marked this todo resolved. `docs/HANDOFF.md:469` marks the item done.

Moving the macOS app from `swiftc` to `rules_apple` is not part of this todo. It is a choice, not a
constraint. `docs/HANDOFF.md:470` says a `macos_application` target is still new ground. Open a new
todo if that work is wanted.

## Technical Details

- `MODULE.bazel`: `rules_rust` pin and `rust.toolchain`
- `MODULE.bazel.lock`
- `.bazelversion`: Bazel 8.2.1
- `.bazelrc`: JVM cap, connect timeout, memory share
- `BUILD.bazel`: `xcodeproj` target
- `clients/apple/ios/BUILD.bazel`: the iOS app
- `.buildkite/pipeline.yml`: `:bazel: iOS app` (169-223), `:apple: macOS menu bar app` (225)
- `scripts/release-ios-testflight-buildkite.sh`: IPA build (line 278)
- `scripts/build-macos-app.sh`: macOS app bundle
- `scripts/release-macos-artifacts.sh`: macOS signing, notarisation and stapling (`release-macos` step)
- `Cargo.toml`: `rust-version`, `sysinfo` version

## Acceptance Criteria

- [x] How current `rules_rust` resolves a toolchain version is known:
  `rust.toolchain(versions = ["1.95.0"])` needs no `sha256s` override.
- [x] `MODULE.bazel` pins `rules_rust` 0.74.0, not 0.68.1.
- [x] The Bazel toolchain provides rustc 1.95.0.
- [x] `rules_rust` 0.74.0 works with Bazel 8.2.1 and `rules_apple` 4.3.3: the iOS app builds in CI and
  reaches TestFlight.

## Work Log

### 2026-09-06

- 0ee9804: the scaffold pinned Bazel 8.2.1 in `.bazelversion`, because `rules_apple` is not Bazel 9
  ready.
- Checked `rules_rust`: 0.74.0 is the current release (2026-08-28); the monorepo pins 0.68.1.
  `rust/known_shas.bzl` no longer exists at that path in either release, so it was not possible to
  confirm which toolchain versions either release knows.

### 2026-09-07

- b65136c: todo created. Steps for M4: read how current `rules_rust` resolves a toolchain version
  (the static SHA list is gone, so `versions = ["1.95.0"]` might need an integrity override); pin
  0.74.0, not 0.68.1; check 0.74.0 with Bazel 8.2.1 and `rules_apple` 4.3.3.

### 2026-09-09

- e1602ed: recorded that Bazel could not run. A minimal probe (`rules_rust` 0.74.0, Rust 1.95.0, one
  `rust_binary`, Bazel 8.2.1 through bazelisk) started the server, but the client timed out after 120s,
  against a fresh output base, in two directories, with the agent sandbox on and off.
- 880b3af: the macOS menu bar app was built with plain `swiftc`, a hand-assembled `.app` bundle and an
  `Info.plist` template, as Descartes does, in `scripts/build-macos-app.sh`. It uses `swiftc` because
  Bazel was thought not to run at the time (e1602ed).
- e1867ac: GitHub Actions CI built the macOS app bundle.
- 887cc20: Bazel works. The e1602ed probe had not been retried after clearing stale Bazel servers.
  The cause was memory: 11 GB VM, about 2.8 GB free, 5.3 of 6 GB swap in use. This command worked:

  ```bash
  bazelisk --host_jvm_args=-Xmx1500m --connect_timeout_secs=900 \
    build --jobs=1 "--local_resources=memory=HOST_RAM*.3" //target
  ```

  The probe built 168 actions in 727s (about 12 minutes), and the binary ran. The toolchain reported
  `rustc 1.95.0 (59807616e 2026-04-14)`. No `sha256s` override was needed. Advice at the time: keep the
  JVM capped and `--jobs` low so Bazel does not compete with cargo for memory.

### 2026-09-10

- 06ec246: added `MODULE.bazel` (`rules_rust` 0.74.0, Rust 1.95.0, `rules_apple` 4.3.3,
  `rules_swift` 3.4.1, `rules_xcodeproj` 3.0.0, crate_universe on `Cargo.lock`) and `.bazelrc` (JVM cap,
  connect timeout, `HOST_RAM*.4`). The full set resolves and analyses a `rust_library` and a
  `swift_library` together.
- 1b62635: Bazel targets for PessimalKit, the iOS app and the Xcode project.
- 59869ca: todo marked resolved in full; Bazel builds the whole iOS chain to `Pessimal.ipa`. Corrected
  `docs/HANDOFF.md`, which still said Bazel could not run.
- 96aeb88: CI step that builds the iOS app and checks that the Rust FFI symbols are linked into it.
- 1332e4f: committed `MODULE.bazel.lock`.
- 74af91d: `docs/HANDOFF.md` strikes the item through as done.
- afb2af6: `--config=ci` raises the memory share to `HOST_RAM*.8`.
- 7910028: CI installs a toolchain that meets the MSRV on the Tart guests for the cargo steps.

### 2026-09-11

- bc8ee4f: the Bazel-built iOS app reached TestFlight as build 0.1.0.26.

### 2026-09-14

- 32a24dc: GitHub Actions removed. The macOS app bundle now builds in the Buildkite step
  `:apple: macOS menu bar app`.
- Rewritten into this format.

## Resources

- `docs/HANDOFF.md`
- `docs/plans/2026-09-06-pessimal-roadmap.md` (M4, M5, M6)
- [MSRV note in AGENTS.md](../AGENTS.md)
- `rules_rust` releases: 0.74.0 (2026-08-28), 0.68.1 (the siblings' pin)
