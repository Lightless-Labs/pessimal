# Pessimal Handoff

**Updated:** 2026-09-06

## Current state

- **M0 scaffold** — done. Cargo workspace across `common/`, `agents/`, `clients/`, `tools/`.
  MIT, conventional commits via `cog`.
- **M1 domain core** — done. `pessimal_core` covers metric identity, time ranges and series,
  liveness, alert evaluation, URNs, and ports. Green under `cargo test` and `clippy -D warnings`.
- **M2 host agent** — in progress.
- Everything from M3 on is unstarted; `clients/` holds placeholder crates so the workspace parses.

## Known issues

- None yet.

## Next up

Finish M2: `pessimal_agent_core` then `pessimal_agent_host`, verified against a local OTLP
collector rather than only unit tests.
