---
status: pending
priority: p2
issue_id: "010"
tags: [build, bazel, rust, agents-md, disk]
dependencies: []
---

# Finish the Bazel setup, and stop AGENTS.md telling agents it doesn't exist

## Why

On the agents VM, `target/` held 13 GB on 2026-09-23, and that was after the disk-maintenance sweep
had already removed 5.6 GB from it the same morning. It's one of the handful of cargo `target/`
directories that keep refilling the VM's disk.

Agents aren't misbehaving. They're following `AGENTS.md`, and `AGENTS.md` is out of date:

- Line 57 says "Bazel, `tools/uniffi/regen.sh`, and the Xcode project arrive with M4/M5. There is no
  `MODULE.bazel`". But `MODULE.bazel` exists, `rules_rust` + `crate_universe` are wired
  (`crate.from_cargo` over `//:Cargo.toml`), and six crates already have Bazel targets.
- The command table (lines 51-54) documents only cargo: `cargo test --workspace`,
  `cargo clippy --workspace`, `cargo run -p pessimal_agent_host`.

So every agent builds the whole workspace with cargo into `target/`, next to a Bazel output base that
already builds most of the same crates.

## What's covered and what isn't

Already have Bazel targets: `common/pessimal_core`, `common/pessimal_usage`,
`common/pessimal_usage_otlp`, `clients/common/pessimal_client_core`,
`clients/common/query/pessimal_query_signoz`, `clients/ffi/pessimal_ffi`.

No BUILD file yet: `agents/common/pessimal_agent_core`, `agents/host/pessimal_agent_host`,
`tools/uniffi`.

Since `crate_universe` already reads the root workspace, their third-party deps already resolve.
What's missing is the three BUILD files. Mind the MSRV note at line 91: the Bazel Rust toolchain
must pin at least 1.95 (set by `sysinfo`).

## Leave the release path alone

The shipping builds use cargo on purpose: `scripts/release-build-linux.sh`,
`scripts/check-glibc-floor.sh`, `scripts/build-macos-app.sh`, `scripts/install-agent-launchd.sh`.
Don't change them as part of this. The goal is a Bazel path for everyday dev/test, so `target/` stops
being regenerated on the VM. Moving releases to Bazel, if ever, is a separate decision.

## Done when

- `bazel build //...` and `bazel test //...` cover all nine workspace crates.
- AGENTS.md line 57 is corrected and the command table leads with Bazel for dev/test, noting that
  cargo stays for the release scripts.
- `target/` stops reappearing on the VM between weekly sweeps.
