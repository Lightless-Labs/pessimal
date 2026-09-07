# The Bazel Rust toolchain has to provide 1.95, and 0.68.1 probably cannot

Pessimal's MSRV is 1.95, set by `sysinfo` 0.39 rather than by choice. Cargo does not enforce
`rust-version`, but `rules_rust` does: the Bazel toolchain has to actually provide 1.95 or newer.

The sibling projects all pin `rules_rust` 0.68.1, which predates Rust 1.95. Copying that pin is the
obvious mistake to make at M4.

**What was checked (2026-09-06):** `rules_rust` 0.74.0 is the current release (2026-08-28), against
0.68.1 in the monorepo. It was *not* possible to confirm which toolchain versions either release
knows about — `rust/known_shas.bzl` no longer exists at that path in either, so the mechanism has
changed and needs reading rather than assuming.

**What to do at M4:**

1. Read how current `rules_rust` resolves a toolchain version before writing `MODULE.bazel` — the
   static SHA list is gone, so `rust.toolchain(versions = ["1.95.0"])` may or may not need an
   accompanying integrity override.
2. Pin `rules_rust` 0.74.0, not 0.68.1.
3. Then check that 0.74.0 still works with Bazel 8.2.1 and `rules_apple` 4.3.3. Bazel 8 is pinned
   because `rules_apple` is not Bazel 9 ready; if the newer `rules_rust` has moved on from Bazel 8,
   that tension has to be resolved before anything else in M4.

Related: [MSRV note in AGENTS.md](../AGENTS.md).
