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
  `pessimal_usage` holds Pessimal's own usage reporting — span types, the attribute allowlist,
  consent, and the OTLP/JSON encoder — also pure; `pessimal_usage_otlp` is the adapter that ships
  them.
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
| Sample the host | `cargo run -p pessimal_agent_host -- --config pessimal.example.toml --sample` |
| Verify OTLP export | `scripts/ci-export-smoke.sh grpc http://localhost:4317` |

Bazel, `tools/uniffi/regen.sh`, and the Xcode project arrive with M4/M5. There is no `MODULE.bazel`
yet — do not add build instructions for them to the docs before they work.

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
  namespaced `pessimal.agent.*` for the agents and `pessimal.client.*` for the clients.
- Usage reporting may only emit attributes on `pessimal_usage::AttributeKey`. That is enforced by the
  types, not by review: nothing in that crate accepts arbitrary text. Do not add an escape hatch.

## Naming Conventions

- Rust crates: `pessimal_{area}_{name}` (snake_case), e.g. `pessimal_agent_host`.
- Swift modules: `Pessimal{Name}` (PascalCase), e.g. `PessimalFFI`.
- Bazel targets: match the crate name from `Cargo.toml`.

## Cautions

- **Do not use self-hosted runners for fork PRs.** This is a public repo. PR CI runs on hosted
  `macos-15` (free and unmetered for public repos); the self-hosted `getmac-tahoe` runner is only
  used for pushes to `main` and for release workflows, which forks cannot trigger.
- The bindgen CLI in `tools/uniffi/` and the `uniffi` runtime crate must stay on the same version.
  Both take it from `[workspace.dependencies]`; a mismatch surfaces as an opaque API-checksum panic
  at app runtime, not at build time.
- **MSRV is 1.95**, set by `sysinfo`. When the Bazel toolchain lands it must pin at least that, which
  may mean a newer `rules_rust` than the 0.68.1 the sibling projects use.
- The agent pulls `aws-lc-rs` transitively (reqwest's default rustls provider). That is fine for a
  server-side binary but is the usual source of iOS cross-compilation pain, so the *client* crates
  must choose their own reqwest TLS features rather than inheriting the agent's.
- **The agent's graph enables *both* rustls providers.** `opentelemetry-otlp`'s features carry
  `reqwest-rustls` (→ `aws-lc-rs`) and `tls-ring` (→ `ring`), and rustls refuses to pick between two
  from crate features — `ClientConfig::builder()` panics with "Could not automatically determine the
  process-level CryptoProvider". It does not bite today only because both of the agent's TLS paths
  name a provider explicitly. So any crate using `reqwest/rustls-no-provider` that is linked into the
  agent **must** call its own `install_crypto_provider()` first. Measured 2026-09-11; see
  `docs/plans/2026-09-11-m8-usage-reporting.md` step 4.
- Regenerating bindings means copying **both** the `.swift` and the `FFI.h` file. Copying only one
  produces link errors that look unrelated.
