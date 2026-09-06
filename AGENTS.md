# Pessimal

Host telemetry agents (Rust, cross-platform) plus iOS and macOS menu bar clients with a shared Rust
core behind UniFFI.

## Session Start

Read [`docs/HANDOFF.md`](docs/HANDOFF.md) for current state and pending work. Update it before
compaction or at session end. Read [`docs/plans/`](docs/plans/) for the roadmap and milestone plans.

## Process

When picking up a milestone that has no dedicated plan, write the plan first.
When a plan is deepened, reviewed, or completed, record it in the plan header
(e.g. `**Completed:** 2026-09-06`).

## Git

This repo is public and open source. Trunk-based: commit directly to `main`, no feature branches.
Conventional commits, validated by `cog` (see `cog.toml`).

Set `SSH_AUTH_SOCK` before any authenticated git operation:

```bash
export SSH_AUTH_SOCK=~/.ssh/agent.sock
ssh-add --apple-load-keychain
```

The second line is required after a reboot: the socket survives but the agent comes back empty.
Never ask the user to run it.

## Layout

Three top-level Rust trees, plus the Apple client code:

- `common/` — shared by both sides. `pessimal_core` is the hexagon: domain types, liveness policy,
  alert evaluation, and the ports adapters implement. It has no I/O and no async runtime.
- `agents/` — `agents/common/` holds what every agent shares (collection traits, OTLP export,
  config, backend presets); `agents/host/` is the host telemetry binary. Future agents get their
  own directory alongside it.
- `clients/` — `clients/common/` holds what every client shares (backend query adapters, polling
  orchestration); `clients/ffi/` is the UniFFI bridge; `clients/apple/` holds generated Swift
  bindings, shared SwiftUI, and the two apps.

Anything shared by *both* agents and clients belongs in `common/`, not in one side's `common/`.

## Build Commands

| Task | Command |
|------|---------|
| Test Rust | `cargo test --workspace` |
| Lint Rust | `cargo clippy --workspace --all-targets -- -D warnings` |
| Format | `cargo fmt --all` |
| Build Apple | `bazel build //...` |
| Test Apple | `bazel test //...` |
| Regenerate FFI bindings | `./tools/uniffi/regen.sh` |
| Generate Xcode project | `bazel run //:xcodeproj` |

## Conventions

- Favour `ast-grep` over `grep` when researching and operating over code.
- Commit early and eagerly. Favour atomic commits.
- Use a TDD approach. Run tests regularly to tighten the feedback loop.
- Follow hexagonal architecture and DDD. Core has no external dependencies; the FFI layer is a
  bridging adapter with no business logic.
- Use UUIDv7 for all IDs. Strictly.
- All entities have a URN: `pessimal::$ENVIRONMENT::$SERVICE::$MODEL::$UUID`. The prefix is
  configurable per deployment for backward compatibility.
- Metric names follow OpenTelemetry system semantic conventions. Pessimal's own instrumentation is
  namespaced `pessimal.agent.*`.

## Naming Conventions

- Rust crates: `pessimal_{area}_{name}` (snake_case), e.g. `pessimal_agent_host`.
- Swift modules: `Pessimal{Name}` (PascalCase), e.g. `PessimalFFI`.
- Bazel targets: match the crate name from `Cargo.toml`.

## Cautions

- **Do not use self-hosted runners for fork PRs.** This is a public repo. PR CI runs on hosted
  `macos-15` (free and unmetered for public repos); the self-hosted `getmac-tahoe` runner is only
  used for pushes to `main` and for release workflows, which forks cannot trigger.
- The bindgen CLI in `tools/uniffi/` and the `uniffi` runtime crate must stay on the same version.
  A mismatch surfaces as an opaque API-checksum panic at app runtime, not at build time.
- Regenerating bindings means copying **both** the `.swift` and the `FFI.h` file. Copying only one
  produces link errors that look unrelated.
