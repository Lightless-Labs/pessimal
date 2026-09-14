---
status: pending
priority: p3
issue_id: "005"
tags: [clients, ffi, uniffi, platforms]
dependencies: []
---

# Support client platforms beyond Apple

## Problem Statement

The owner confirmed this platform split:

- **Agents**: Linux, macOS and Windows, more later. Shared code is in `agents/common/pessimal_agent_core`.
- **Clients**: macOS and iOS now. Android, Linux and Windows later. Shared code is in
  `clients/common/pessimal_client_core`.

The Rust layout matches that split. `common/` holds what both sides share: `pessimal_core`,
`pessimal_usage` and `pessimal_usage_otlp`.

The client side does not yet match the list. `pessimal_ffi` generates Swift only. Nobody has decided
what a Linux or Windows client is. The usage `Platform` enum has no Android. The roadmap names only iOS
and macOS clients.

## Findings

**Bindings**

- UniFFI generates Kotlin as well as Swift. For Android, `pessimal_ffi` needs a new bindings target, not
  a rewrite. The crate's Rust code needs no change.
- `tools/uniffi/regen.sh` reads no arguments. It hardcodes `--language swift` and
  `OUT="clients/apple/PessimalFFI/Sources"`. The `--language` option belongs to the bindgen binary,
  `pessimal-uniffi-bindgen generate` (`tools/uniffi/`, uniffi 0.32 with the `cli` feature). uniffi_bindgen
  0.32.0 has a Kotlin generator (`src/bindings/kotlin/`).
- `clients/ffi/pessimal_ffi/uniffi.toml` has only `[bindings.swift]`. A `[bindings.kotlin]` section is
  likely needed to set the package name.
- `scripts/ci-check-bindings.sh` checks only the Swift bindings.
- The `clients/ffi/pessimal_ffi/Cargo.toml` description says "UniFFI bridge exposing pessimal_client_core
  to Swift".
- The FFI surface (the Rust sources in `clients/ffi/pessimal_ffi/src`) grew between 213b98a and 3dbd7e1:

  | | Files | Lines | Records | Enums | Errors | Objects |
  |---|---|---|---|---|---|---|
  | 213b98a (2026-09-09) | 7 | 6,031 | 19 | 18 | 4 | 1 |
  | 3dbd7e1 (2026-09-14) | 8 | 6,843 | 23 | 21 | 4 | 1 |

  Each Record added must be right in Kotlin as well as in Swift.
- Shared Swift for both Apple apps is in `clients/apple/PessimalKit/`: `FleetModel`, the platform stores,
  and the composition root. An Android client needs the same layer in Kotlin.

**Linux and Windows clients**

- UniFFI has no obvious target language for a Linux or Windows client.
- A native Rust client (egui, or a TUI) can use `pessimal_client_core` directly, with no bridge. The crate
  already supports that: `FleetState::apply` (`clients/common/pessimal_client_core/src/fold.rs:252`) is a
  pure fold, and `poll_once` (`clients/common/pessimal_client_core/src/gather.rs:96`) is one async call.
- Such a client needs no Swift or Kotlin code. It still needs, in Rust, what
  PessimalKit and `FleetSession` (`clients/ffi/pessimal_ffi/src/session.rs`) provide: the poll timer and
  jitter, session sequencing, usage reporting, state export and restore, and stores for the API key,
  settings, cached fleet state and usage consent. `pessimal_client_core` has no timer, task, runtime or
  clock trait. Core owns the poll policy and the platform owns the timer
  (`clients/common/pessimal_client_core/src/lib.rs`).
- That is a different kind of client from the Apple ones, not the same client ported. Decide it on
  purpose.
- No decision exists. The code, `docs/plans/` and `docs/HANDOFF.md` have no egui, ratatui, TUI, Kotlin or
  Android client. The roadmap goal (`docs/plans/2026-09-06-pessimal-roadmap.md` lines 8-11) says "iOS +
  macOS menu bar clients" and "native Swift over a UniFFI bridge". No milestone (M0-M8) names an Android,
  Linux or Windows client.
- Windows is built and tested nowhere. `cargo test --workspace` ran on GitHub Actions `windows-latest`
  until 32a24dc (2026-09-14). `docs/HANDOFF.md` says "cargo test on Windows: Dropped. The owner does not
  want Windows tested".
- The Windows agent release is deferred (`docs/plans/2026-09-12-distribution.md` lines 24-26). A
  `cargo zigbuild` probe ran over twenty minutes, and no machine on the cluster can run the result.
- Given the two points above, the owner should confirm that Windows is still on the client list.

**Usage platform list**

- `pessimal_usage::attribute::Platform` (`common/pessimal_usage/src/attribute.rs:78`) has `Ios`, `Macos`,
  `Linux` and `Windows`. It has no Android.
- `UsagePlatformRecord` (`clients/ffi/pessimal_ffi/src/usage.rs:69`) has the same four values.
- The `pessimal.platform` row of `docs/plans/2026-09-11-m8-usage-reporting.md` (line 57) lists the same
  four values.

**TLS**

- The client crates first used `native-tls`, because the clients were assumed to be Apple-only (2747c41).
- Since ab5193f (2026-09-09) they use rustls with the `ring` provider and `rustls-platform-verifier`:
  `clients/common/query/pessimal_query_signoz/Cargo.toml` and `clients/ffi/pessimal_ffi/Cargo.toml`. This
  needs no system OpenSSL. It reads the platform trust store, so a self-hosted SigNoz behind an internal
  CA works.
- ab5193f checked a handshake against live SigNoz Cloud and checked that `aws-lc-rs` is not in the client
  dependency graphs. It did not test cross-compiling.
- Which of the five client targets are built today: Linux and macOS, by `cargo test --workspace --locked`
  in `.buildkite/pipeline.yml` (lines 107 and 294). iOS, through Bazel. Windows, nowhere since 32a24dc.
  Android, never.
- `common/pessimal_usage_otlp` uses the same `rustls-no-provider` and `ring` setup, with its own
  `install_crypto_provider()` in `src/lib.rs`. Today only `pessimal_ffi` links it. It is meant to be
  linked into the agent too (M8 phase 2, not yet wired). It installs `ring` explicitly because the
  agent's dependency graph enables both `ring` and `aws-lc-rs`, and rustls panics there if the provider is
  left to crate features. That was measured on 2026-09-11 with a throwaway bin in `pessimal_agent_host`
  (`docs/plans/2026-09-11-m8-usage-reporting.md` step 4, `CLAUDE.md` Cautions).
- Rule: code in `clients/common` that uses a platform API must work on the whole client list, not only on
  macOS and iOS.

## Proposed Solutions

These are two parts of the work, not alternatives. They can be done in either order.

### Option A: Kotlin bindings for Android

Make `tools/uniffi/regen.sh` take a language and an output directory. Add `[bindings.kotlin]` to
`uniffi.toml`. Make `scripts/ci-check-bindings.sh` check the Kotlin output. Change the `pessimal_ffi`
description so it does not say Swift only. Add Android to `Platform`, `UsagePlatformRecord` and the
`pessimal.platform` row.

- **Pros:** no change to the crate's Rust code. The bindings are generated.
- **Cons:** an Android client also needs the PessimalKit layer written in Kotlin. Android has never been
  built, so the TLS stack is untested there. The bindings check grows.

### Option B: A native Rust client for Linux and Windows

Build the Linux and Windows client in Rust (egui, or a TUI) on top of `pessimal_client_core`, with no
bridge.

- **Pros:** no FFI bridge and no Swift or Kotlin code. `FleetState::apply` and `poll_once` are already
  usable as they are.
- **Cons:** it is a different client from the Apple ones, with its own UI. It still needs what PessimalKit
  and `FleetSession` provide, written in Rust. Windows is built and tested nowhere today.

## Recommended Action

1. Ask the owner to confirm the client platform list, Windows especially. Update the roadmap goal to
   match.
2. Before any work on a Linux or Windows client starts, decide what kind of client it is (Option B or
   something else) and write the plan first.
3. Do Option A before the FFI surface grows further, because each Record added must be right in both
   Swift and Kotlin.
4. Code in `clients/common` that uses a platform API must work on every client platform on the list.

## Technical Details

- `tools/uniffi/regen.sh`: hardcodes `--language swift` and the Swift output directory
- `tools/uniffi/Cargo.toml`: the `pessimal-uniffi-bindgen` binary
- `clients/ffi/pessimal_ffi/uniffi.toml`: only `[bindings.swift]`
- `clients/ffi/pessimal_ffi/Cargo.toml`: the Swift-only description, and the rustls setup
- `clients/ffi/pessimal_ffi/src/usage.rs`: `UsagePlatformRecord`
- `clients/ffi/pessimal_ffi/src/session.rs`: `FleetSession`, which a native Rust client would rebuild
- `scripts/ci-check-bindings.sh`: checks Swift bindings only
- `clients/apple/PessimalKit/`: the shared Swift layer an Android client would need in Kotlin
- `clients/common/pessimal_client_core/src/fold.rs`: `FleetState::apply`
- `clients/common/pessimal_client_core/src/gather.rs`: `poll_once`
- `clients/common/query/pessimal_query_signoz/Cargo.toml`: the TLS choice and its reasons
- `common/pessimal_usage/src/attribute.rs`: `Platform`
- `common/pessimal_usage_otlp/src/lib.rs`: `install_crypto_provider()`
- `docs/plans/2026-09-06-pessimal-roadmap.md`: the goal names only iOS and macOS clients
- `docs/plans/2026-09-11-m8-usage-reporting.md`: the `pessimal.platform` row
- `.buildkite/pipeline.yml`: which platforms are built

## Acceptance Criteria

- [ ] The owner has confirmed the client platform list, and the roadmap goal names every client platform
  on it.
- [ ] The kind of Linux and Windows client is decided and written in a plan.
- [ ] `tools/uniffi/regen.sh` generates Kotlin bindings as well as Swift.
- [ ] `clients/ffi/pessimal_ffi/uniffi.toml` has a `[bindings.kotlin]` section.
- [ ] `scripts/ci-check-bindings.sh` fails when the Kotlin bindings are stale.
- [ ] `Platform`, `UsagePlatformRecord` and the `pessimal.platform` row list every client platform on the
  confirmed list.
- [x] The client crates use a TLS stack that is not Apple-only: rustls with `ring` and
  `rustls-platform-verifier`, with no system OpenSSL (ab5193f).
- [ ] The client TLS stack is built for every client platform on the confirmed list.

## Work Log

### 2026-09-06

- 2747c41 added the SigNoz query adapter with `native-tls`, on the assumption that the clients were
  Apple-only.

### 2026-09-07

- 2bc8c68 scaffolded `pessimal_ffi` and `tools/uniffi/regen.sh`, generating Swift only.

### 2026-09-09

- 25fd273 implemented the `pessimal_ffi` UniFFI bridge.
- 3f5eddc made CI fail on stale bindings (Swift only).
- ab5193f moved the client crates from `native-tls` to rustls, `ring` and `rustls-platform-verifier`,
  added `install_crypto_provider()`, and checked the handshake against live SigNoz Cloud.
- 213b98a created `todos/client-core-must-reach-beyond-apple.md` with the owner's platform list, the
  Swift-only bridge, and the TLS reasons.

### 2026-09-10

- 06ec246 added the Bazel workspace and `clients/apple/PessimalKit` (shared Swift for both apps).
- 0521efb changed `pessimal_client_core` and FFI records.

### 2026-09-11

- 4ddd371 added `common/pessimal_usage` and `common/pessimal_usage_otlp`. Its `Platform` enum lists
  ios, macos, linux and windows, with no Android.
- 3aa5ad1 added `pessimal_ffi/src/usage.rs` with `UsagePlatformRecord`, with the same four values.

### 2026-09-12

- 68b2271 added used-of-total records; the FFI surface grew again.
- 8a2d4cb checked the derived totals against a real backend.

### 2026-09-14

- 32a24dc moved CI to Buildkite and deleted GitHub Actions. `cargo test --workspace` on Windows was
  dropped, so Windows is built nowhere. `regen.sh` changed but still generates Swift only.
- No commit adds Kotlin bindings, an Android build, or a decision on Linux or Windows clients. Still
  pending.
- Rewritten from `todos/client-core-must-reach-beyond-apple.md` into this format.

## Resources

- `docs/plans/2026-09-06-pessimal-roadmap.md`, Goal
- `docs/plans/2026-09-11-m8-usage-reporting.md`, the attribute table and step 4
- `docs/plans/2026-09-12-distribution.md`, Windows deferred
- `docs/HANDOFF.md`, CI and releases
- `CLAUDE.md`, Cautions: both rustls providers in the agent graph
- Commits ab5193f (TLS change) and 213b98a (original todo)
- <https://mozilla.github.io/uniffi-rs/>
