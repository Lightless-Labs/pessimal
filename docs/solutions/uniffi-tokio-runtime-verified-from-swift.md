# The tokio reactor really does drive reqwest from Swift's executor

**Verified:** 2026-09-07, UniFFI 0.32, Swift 6.2.3, macOS.

## The question

`pessimal_ffi` exports async functions that end up in `reqwest`. reqwest needs a tokio reactor. But
the future is polled by *Swift's* executor, which is not one, and Swift may resume it on a different
thread after every await.

The three sibling projects each grew a hand-rolled `runtime.rs` for this. The design for
`pessimal_client_core` flags it as the thing to check on day one of M4, because the failure mode is
a hang rather than an error — nothing tells you it is wrong.

## The answer

`#[uniffi::export(async_runtime = "tokio")]` with `uniffi = { features = ["tokio"] }` is sufficient.
No hand-rolled runtime is needed.

Proved end to end, not by reading the macro:

1. An exported `async fn` doing a real `reqwest::get`.
2. `cargo build -p pessimal_ffi` → `libpessimal_ffi.a` and `.dylib`.
3. `pessimal-uniffi-bindgen generate --library …/libpessimal_ffi.dylib --language swift`.
4. A Swift binary compiled with `swiftc`, linking the staticlib, awaiting the call from a `Task`.
5. It returned the real HTTP body.

A Rust-side test would have proved nothing: it drives the future on a tokio worker, which is exactly
the case that was never in doubt.

## Keeping it true

[`scripts/swift-smoke.sh`](../../scripts/swift-smoke.sh) is that spike, kept. It compiles a Swift
binary against the committed bindings and runs it. macOS only, so it cannot live in the Linux CI
leg, but it is the only check that exercises the boundary as the apps will.

## Two things that cost time

- **`swiftc` needs the modulemap explicitly.** The generated `PessimalFFI.swift` does
  `#if canImport(PessimalFFIFFI)`, which silently compiles to nothing without
  `-Xcc -fmodule-map-file=…/PessimalFFIFFI.modulemap`. The symptom is
  `cannot find type 'RustBuffer' in scope`, which does not mention modules at all.
- **`-parse-as-library` cannot be combined with top-level code** in `main.swift`. Drop the flag, or
  use `@main`.
