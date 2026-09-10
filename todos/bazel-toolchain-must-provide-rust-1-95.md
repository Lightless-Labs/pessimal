# Bazel works, and `rules_rust` 0.74.0 provides Rust 1.95 — verified

**Resolved 2026-09-09.** An earlier version of this note claimed Bazel could not be run here. That
was wrong, and the owner was right to push back.

## What was actually wrong

The build machine is an 11 GB VM that was running with ~2.8 GB free and 5.3 of 6 GB of swap in use.
Bazel's server starts fine under that pressure but cannot finish initialising inside the client's
default connect window, so the client gives up after 120s and reports that it could not connect. The
earlier investigation read that as "Bazel does not work here" and stopped, which was a diagnosis
from a symptom rather than a cause — and it never retried after clearing the stale servers.

## What works

```bash
bazelisk --host_jvm_args=-Xmx1500m --connect_timeout_secs=900 \
  build --jobs=1 "--local_resources=memory=HOST_RAM*.3" //target
```

A minimal probe — `rules_rust` 0.74.0, `rust.toolchain(edition = "2024", versions = ["1.95.0"])`,
one `rust_binary`, Bazel 8.2.1 — built successfully: 168 actions in 727s, and the produced binary
runs. The toolchain it fetched reports `rustc 1.95.0 (59807616e 2026-04-14)`.

So the MSRV concern is settled: **`rules_rust` 0.74.0 can provide Rust 1.95.0 under Bazel 8.2.1**,
and it does not need a `sha256s` override. Pin 0.74.0, not the siblings' 0.68.1, which predates 1.95.

The caveat is time, not capability: ~12 minutes for a hello-world under this memory pressure. Expect
real builds to be slow here, and keep the JVM capped and `--jobs` low so Bazel does not compete with
cargo for the little memory there is.

## Resolved in full (2026-09-10)

Bazel now builds the whole iOS chain: `rules_rust` 0.74.0 + `rules_apple` 4.3.3 + `rules_swift`
3.4.1 + `rules_xcodeproj` 3.0.0 on Bazel 8.2.1, Rust 1.95, crate_universe against the same
`Cargo.lock` cargo uses, producing `Pessimal.ipa`. Nothing below is blocking any more; it is kept
because the memory advice still applies and because the wrong turn is worth remembering.

One caution for anyone reading an old revision of this file: it previously said Bazel could not run
here. That was wrong, and an agent later read it and skipped Bazel verification on its word. A stale
"this does not work" note is more dangerous than no note.

## What is still open

The macOS app currently builds with plain `swiftc` (following the sibling project Descartes) rather
than with `rules_apple`, because that path was taken while Bazel was believed unusable. It works, it
is notarizable, and CI builds it. Whether to move it onto Bazel is now a choice rather than a
constraint.

**iOS is the case that most wants Bazel**, since App Store submission wants an Xcode project and the
siblings generate one with `rules_xcodeproj`. Two things to check before committing to that, neither
of which was reached: whether `rules_apple` 4.3.3 is happy alongside `rules_rust` 0.74.0, and
whether Bazel 8.2.1 remains the right pin (it was chosen because `rules_apple` is not Bazel 9 ready).
