# A green Bazel job says nothing about whether cargo can build on the same machine

**Measured:** 2026-09-10, Buildkite builds #2 and #3 on the self-hosted Apple silicon mini.

## What happened

Build #2 on a fresh Tart guest: `bazelisk build --config=ci //clients/apple/ios:Pessimal` passed, the
ipa was produced, and the uniffi symbols were linked in. The next job on an identical guest ran
`scripts/build-macos-app.sh`, which drives plain `cargo`, and failed:

```
error: rustc 1.88.0 is not supported by the following packages:
  pessimal_core@0.1.0 requires rustc 1.95
  pessimal_client_core@0.1.0 requires rustc 1.95
  ...
```

Same image, same commit, same minute. The iOS app is *more* Rust than the macOS bundle is — it builds
the whole workspace through crate_universe — and it compiled without complaint.

## Why

`MODULE.bazel` pins its own toolchain:

```python
rust.toolchain(edition = "2024", versions = ["1.95.0"], ...)
```

Bazel downloads that toolchain and uses it. The guest's rustup is invisible to it. `cargo`, meanwhile,
uses whatever the image's default toolchain is — 1.88 on `ci-macos-rust-bazel-ios-20260910-v2`.

So the two build systems in this repo disagree about what "the Rust toolchain" means, and only one of
them is pinned. A rules_rust bump or an image rebake can move either independently.

## What this invalidates

**"The Bazel job passes, so the toolchain on that guest is fine"** is not a valid inference, in either
direction. It is the CI equivalent of the lesson in
[`backend-ingestion-lag-breaks-liveness.md`](backend-ingestion-lag-breaks-liveness.md): a green check
that does not exercise the thing you are inferring about tells you nothing about it.

Two corollaries worth keeping:

- MSRV is enforced by cargo, not by Bazel. `cargo clippy`/`cargo test` are the only jobs that will
  ever notice an MSRV regression; the Bazel jobs will keep passing past it.
- Conversely, a `rust-toolchain.toml` would fix cargo and still not describe what Bazel uses. If the
  two are meant to agree, that has to be asserted somewhere, because nothing makes it true.

## The fix, and the fix that broke things

The pipeline now measures the toolchain and installs `stable` only when it falls short of MSRV —
`stable` rather than a pin, because `.github/workflows/ci.yml` installs `stable` and the workspace
tracks it deliberately.

The first attempt installed unconditionally and turned a green Linux job red:

```
error: could not create temp file /opt/rustup/tmp/...: Permission denied (os error 13)
```

That guest's rustup is a root-owned system install the job user cannot write to — and it already
shipped a new enough toolchain, so it never needed the install. Fixing the job that failed without
asking what the job that passed depended on simply moved the red.
