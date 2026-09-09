# The client core is shared across more platforms than it currently bridges

The intended platform split, confirmed by the owner:

- **Agents** — Linux, macOS, Windows, later more. Shared code in `agents/common/pessimal_agent_core`.
- **Clients** — macOS and iOS now; Android, Linux and Windows later. Shared code in
  `clients/common/pessimal_client_core`.

The Rust layout already matches that, and `common/pessimal_core` holds what both sides share. Two
things do **not** yet match it.

## `pessimal_ffi` bridges to Swift only

UniFFI generates Kotlin as well as Swift, so Android is a bindings target rather than a rewrite —
`tools/uniffi/regen.sh` takes `--language kotlin` and the crate needs no change. Worth doing before
the FFI surface grows much further, because every Record added is another thing to get right twice.

What genuinely needs deciding is **Linux and Windows clients**, where there is no UniFFI story at
all. A native Rust GUI over `pessimal_client_core` directly (egui, or a TUI) would skip the bridge
entirely, and the crate is already shaped for it: `FleetState::apply` is a pure fold and
`poll_once` is one async call. That is a different kind of client from the Apple ones, not the same
client ported, and it should be decided as such rather than discovered.

## TLS was chosen for Apple and has been corrected

The client crates used `native-tls` on the explicit assumption that clients were Apple-only. They now
use rustls with the `ring` provider and `rustls-platform-verifier`, which cross-compiles to all five
targets, needs no system OpenSSL, and still reads the platform trust store so a self-hosted SigNoz
behind an internal CA works. Fixed 2026-09-09; recorded here because the *reasoning* generalises:
anything in `clients/common` that reaches for a platform API needs to justify itself against the
whole list, not against the two platforms that exist today.
