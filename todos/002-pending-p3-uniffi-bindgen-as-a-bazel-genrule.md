---
status: pending
priority: p3
issue_id: "002"
tags: [bazel, uniffi, build, ci]
dependencies: []
---

# Run uniffi-bindgen as a Bazel genrule

## Problem Statement

The Swift bindings for `pessimal_ffi` are build outputs that are committed to
`clients/apple/PessimalFFI/Sources/`. `tools/uniffi/regen.sh` writes them, and
`scripts/ci-check-bindings.sh` fails CI when they are stale. phil-connors and kumbaya use the same
approach.

The better end state is a Bazel genrule that runs the bindgen over the built `pessimal_ffi` library
and passes the outputs to `swift_library` / `objc_library`. Then the Bazel build always uses bindings
generated from the current Rust source.

A genrule alone does not remove the committed files. The macOS menu bar app and the Swift smoke test
are built outside Bazel and read the committed files. The bindings can only stop going stale when
those builds also move to Bazel.

Not blocking.

## Findings

- **The bindgen is a Cargo binary, not a Bazel target.** It is `pessimal-uniffi-bindgen` in crate
  `pessimal_uniffi_bindgen` (`tools/uniffi/Cargo.toml`, a workspace member in `Cargo.toml`), built from
  `tools/uniffi/uniffi_bindgen_main.rs`. `tools/uniffi/` has no `BUILD.bazel`, so
  `//tools/uniffi:pessimal-uniffi-bindgen` does not exist.
- **Bazel builds no cdylib for `pessimal_ffi`.** `clients/ffi/pessimal_ffi/BUILD.bazel` defines only
  `rust_library` `pessimal_ffi` and `rust_static_library` `pessimal_ffi_static`. The cdylib that
  `regen.sh` reads comes from Cargo: `target/debug/libpessimal_ffi.dylib` on Darwin, `.so` elsewhere.
- **How `regen.sh` works.** It runs `cargo build -p pessimal_ffi` and
  `cargo build -p pessimal_uniffi_bindgen`, then
  `pessimal-uniffi-bindgen generate --library <cdylib> --language swift --out-dir clients/apple/PessimalFFI/Sources`.
  The library must be built from the current source, or the API checksums disagree and the app
  panics at its first call.
- **There are three generated files:** `PessimalFFI.swift`, `PessimalFFIFFI.h` and
  `PessimalFFIFFI.modulemap`.
- **The Bazel targets exist and read the committed files.** `clients/apple/PessimalFFI/BUILD.bazel`
  has `objc_library` `pessimal_ffiFFI` (module `PessimalFFIFFI`, `hdrs = ["Sources/PessimalFFIFFI.h"]`,
  depends on `//clients/ffi/pessimal_ffi:pessimal_ffi_static`) and `swift_library` `PessimalFFI`
  (`srcs = ["Sources/PessimalFFI.swift"]`). Its comment gives the reason for not generating them:
  "no sibling project has managed a bindgen genrule, and a stale-bindings CI check is the cheaper
  guarantee."
- **The stale-bindings check.** `scripts/ci-check-bindings.sh` runs `regen.sh` and compares the result
  with a copy of the committed files. It runs in the Buildkite `:apple: macOS menu bar app` step
  (`.buildkite/pipeline.yml`), before `scripts/swift-smoke.sh`.
- **Two builds outside Bazel read the committed files.** `scripts/build-macos-app.sh` builds the macOS
  menu bar app with plain `swiftc`. `scripts/swift-smoke.sh` runs the Swift smoke test. A genrule
  covers only the iOS build: `docs/HANDOFF.md` says "Bazel for iOS, `scripts/build-macos-app.sh` for
  macOS". The committed files and the check are still needed unless the macOS build moves to Bazel.
- **The Xcode project comes from Bazel.** It is generated with `bazel run //:xcodeproj`
  (rules_xcodeproj in `BUILD.bazel`), so it would get genrule outputs through the Bazel targets. There
  is no hand-maintained Xcode build. `MODULE.bazel` pins rules_rust 0.74.0, rules_apple 4.3.3 and
  rules_swift 3.4.1. Bazel builds the iOS app end to end to `Pessimal.ipa`.
- **Same `uniffi` version.** The bindgen and the `uniffi` runtime crate both take their version from
  `[workspace.dependencies]` (`uniffi = "0.32"`). A mismatch shows up as an API-checksum panic at app
  runtime, not at build time (`CLAUDE.md`, Cautions). A Bazel bindgen target must use the same
  version.
- **No sibling has a bindgen genrule** (checked 2026-09-14). kumbaya's genrules
  (`kumbaya/src/adapters/driving/ios/runners/App/BUILD.bazel`) write plists only. phil-connors has
  `rust_binary(name = "uniffi_bindgen")` in
  `phil-connors/phil-connors-app/tools/uniffi/BUILD.bazel`, but no genrule uses it; its only genrule
  writes `AppConfig.generated.plist`. This needs working out from scratch.
- **The monorepo's `tools/uniffi/BUILD.bazel` is still a one-line stub** (rechecked 2026-09-14;
  unchanged since 2026-02-09).
- **The revisit condition is half met.** M4 is done (25fd273, 2026-09-09). The binding surface has not
  stopped changing: the committed bindings changed in b9ccb69 (2026-09-09), 0521efb (2026-09-10),
  3aa5ad1 (2026-09-11) and 68b2271 (2026-09-12).

## Proposed Solutions

### Option A: Genrule for the Bazel build only

Add `tools/uniffi/BUILD.bazel` with a `rust_binary` for the bindgen, as in
`phil-connors/phil-connors-app/tools/uniffi/BUILD.bazel`. Add a cdylib target for `pessimal_ffi`. Add a
genrule that runs the bindgen over it and outputs the three files. Point `pessimal_ffiFFI` `hdrs` and
`PessimalFFI` `srcs` at the genrule outputs.

- **Pros:** the iOS build and the generated Xcode project always use bindings generated from the
  current source.
- **Cons:** the committed files, `regen.sh` and `scripts/ci-check-bindings.sh` all stay, for the macOS
  build and the Swift smoke test. There are then two sources of bindings. No sibling has done it.

### Option B: Genrule, and move the macOS build to Bazel

Do Option A. Then build the macOS menu bar app and the Swift smoke test with Bazel, delete the
committed files, and delete the stale-bindings check.

- **Pros:** the bindings cannot go stale at all. One source of bindings.
- **Cons:** the largest change. `scripts/build-macos-app.sh` and `scripts/swift-smoke.sh` must be
  replaced.

### Option C: Keep the committed files and the check

The current state.

- **Pros:** works today. CI fails on stale bindings.
- **Cons:** a developer must run `regen.sh` after every change to `pessimal_ffi`'s signatures. Stale
  bindings are found in CI, not at build time.

## Recommended Action

Keep Option C for now. Revisit when the binding surface stops changing. When this is picked up, aim
for Option B: only that removes the committed files. Option A is a step towards it.

## Technical Details

- `tools/uniffi/Cargo.toml`, `tools/uniffi/uniffi_bindgen_main.rs`: the bindgen binary
- `tools/uniffi/regen.sh`: generates the bindings with Cargo
- `tools/uniffi/BUILD.bazel`: does not exist; a Bazel bindgen target would go here
- `clients/ffi/pessimal_ffi/BUILD.bazel`: `pessimal_ffi` and `pessimal_ffi_static`; no cdylib
- `clients/apple/PessimalFFI/BUILD.bazel`: `pessimal_ffiFFI` and `PessimalFFI`, reading the committed
  files
- `clients/apple/PessimalFFI/Sources/`: `PessimalFFI.swift`, `PessimalFFIFFI.h`,
  `PessimalFFIFFI.modulemap`
- `scripts/ci-check-bindings.sh` and `.buildkite/pipeline.yml` (`:apple: macOS menu bar app`): the
  stale-bindings check
- `scripts/build-macos-app.sh`, `scripts/swift-smoke.sh`: builds outside Bazel that read the committed
  files
- `BUILD.bazel` (`//:xcodeproj`), `MODULE.bazel`: the Xcode project target and the pinned rules
  versions
- `docs/HANDOFF.md`: describes the committed bindings and the CI check

## Acceptance Criteria

- [ ] A Bazel target builds the bindgen, on the same `uniffi` version as the runtime crate.
- [ ] A genrule generates `PessimalFFI.swift`, `PessimalFFIFFI.h` and `PessimalFFIFFI.modulemap` from
  `pessimal_ffi` as Bazel builds it.
- [ ] `PessimalFFI` and `pessimal_ffiFFI` read the genrule outputs, and the iOS app still builds to
  `Pessimal.ipa` with uniffi symbols linked.
- [ ] Every build that reads the bindings (iOS, macOS menu bar app, Swift smoke test) uses generated
  bindings, or is still covered by `scripts/ci-check-bindings.sh`.
- [ ] The comment in `clients/apple/PessimalFFI/BUILD.bazel` and `docs/HANDOFF.md` describe the new
  state.

## Work Log

### 2026-09-06

- Todo created in the scaffold commit 0ee9804. The same commit added `tools/uniffi/Cargo.toml` and
  `tools/uniffi/uniffi_bindgen_main.rs`.
- Proposed a genrule running `//tools/uniffi:pessimal-uniffi-bindgen` over the built cdylib. Reason
  given for committed bindings: they keep the Xcode-side build simple. Revisit once M4 lands and the
  binding surface stops changing. `regen.sh`, the committed bindings and the CI check did not exist
  yet.

### 2026-09-07

- 2bc8c68 added `tools/uniffi/regen.sh` and `scripts/swift-smoke.sh`.

### 2026-09-09

- 25fd273 implemented `pessimal_ffi` (M4) and committed `PessimalFFI.swift`, `PessimalFFIFFI.h` and
  `PessimalFFIFFI.modulemap`.
- 3f5eddc added the stale-bindings check to `.github/workflows/ci.yml`. Before it, `regen.sh` said CI
  ran this check.
- b9ccb69 changed the bindings.

### 2026-09-10

- 06ec246 added the Bazel workspace and `clients/apple/PessimalFFI/BUILD.bazel`. That file reads the
  committed bindings and keeps the CI check instead of a genrule.
- 0521efb changed the bindings.

### 2026-09-11

- 3aa5ad1 changed the bindings.

### 2026-09-12

- 68b2271 changed the bindings.

### 2026-09-14

- 32a24dc deleted the GitHub Actions workflow. The check moved to `scripts/ci-check-bindings.sh` in the
  Buildkite `:apple: macOS menu bar app` step.
- Rechecked: there is still no genrule, and no sibling has a bindgen genrule. The monorepo's
  `tools/uniffi/BUILD.bazel` is still a stub. Moved to this format.

## Resources

- `tools/uniffi/regen.sh`
- `scripts/ci-check-bindings.sh`
- `phil-connors/phil-connors-app/tools/uniffi/BUILD.bazel`: a `rust_binary` for the bindgen
- <https://bazel.build/reference/be/general#genrule>
- <https://mozilla.github.io/uniffi-rs/latest/>
