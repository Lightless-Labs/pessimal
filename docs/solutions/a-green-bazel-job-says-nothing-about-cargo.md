# A passing Bazel build does not mean cargo can build

**Measured:** 2026-09-10, Buildkite builds #2 and #3, on the self-hosted Apple silicon Mac.

## What happened

In build #2, `bazelisk build --config=ci //clients/apple/ios:Pessimal` passed on a new Tart VM. It built
the ipa, with the uniffi symbols linked. The next job used the same VM image and ran
`scripts/build-macos-app.sh`, which uses plain `cargo`. It failed:

```
error: rustc 1.88.0 is not supported by the following packages:
  pessimal_core@0.1.0 requires rustc 1.95
  pessimal_client_core@0.1.0 requires rustc 1.95
  ...
```

Same image, same commit. The iOS build compiles more Rust than the macOS build, and it passed.

## Why

`MODULE.bazel` sets its own Rust version:

```python
rust.toolchain(edition = "2024", versions = ["1.95.0"], ...)
```

Bazel downloads and uses that version. It ignores the VM's rustup. `cargo` uses the image's default
toolchain, which is 1.88 on `ci-macos-rust-bazel-ios-20260910-v2`.

So the two build systems can use different Rust versions, and only Bazel's version is fixed. A
rules_rust update or a new VM image can change either one.

## What this means

- A passing Bazel job tells you nothing about the Rust version that cargo uses on the same machine.
- Only cargo checks the minimum Rust version. `cargo clippy` and `cargo test` catch a regression. The
  Bazel jobs do not.
- A `rust-toolchain.toml` fixes cargo's version, but Bazel ignores it. If the two versions must match,
  something must check that.

## The fix

The pipeline checks the Rust version and installs `stable` only when it is older than 1.95. It installs
`stable`, not a fixed version, because the workspace follows stable on purpose.

The first attempt installed `stable` every time. That broke the Linux job, which had passed before:

```
error: could not create temp file /opt/rustup/tmp/...: Permission denied (os error 13)
```

On the Linux image, rustup is owned by root, and the job user cannot write to it. That image already had
a new enough Rust, so it did not need the install. Before you change a job to fix one failure, check
what the passing jobs depend on.
