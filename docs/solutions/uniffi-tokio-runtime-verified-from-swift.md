# The tokio reactor drives reqwest from the Swift executor

**Verified:** 2026-09-07, UniFFI 0.32, Swift 6.2.3, macOS.

## The question

`pessimal_ffi` exports async functions that call `reqwest`. reqwest needs a tokio reactor. But the
*Swift* executor polls the future, and that executor is not a tokio reactor. Also, Swift can resume the
future on a different thread after each await.

The three sibling projects each wrote their own `runtime.rs` for this problem. The design for
`pessimal_client_core` identifies this as the first thing to check in M4. The reason is the failure
mode: the program stops and waits forever, and no error tells you that something is wrong.

## The answer

`#[uniffi::export(async_runtime = "tokio")]` with `uniffi = { features = ["tokio"] }` is enough. You do
not need your own runtime.

An end-to-end test proved this. Do not use the macro source as proof:

1. An exported `async fn` does a real `reqwest::get`.
2. `cargo build -p pessimal_ffi` → `libpessimal_ffi.a` and `.dylib`.
3. `pessimal-uniffi-bindgen generate --library …/libpessimal_ffi.dylib --language swift`.
4. `swiftc` compiles a Swift binary that links the staticlib. The binary awaits the call from a `Task`.
5. The call returned the real HTTP body.

A test in Rust cannot prove this. It runs the future on a tokio worker, and that case was never in
doubt.

## How to keep this true

[`scripts/swift-smoke.sh`](../../scripts/swift-smoke.sh) is that spike, and the repo keeps it. It
compiles a Swift binary against the committed bindings and runs it. It operates on macOS only, so the
Linux CI step cannot run it. But it is the only check that tests the boundary in the same way as the
apps.

## Two problems that took time

- **`swiftc` needs the modulemap explicitly.** The generated `PessimalFFI.swift` contains
  `#if canImport(PessimalFFIFFI)`. Without `-Xcc -fmodule-map-file=…/PessimalFFIFFI.modulemap`, this
  block compiles to nothing, and the compiler gives no warning. The symptom is
  `cannot find type 'RustBuffer' in scope`. That message does not mention modules.
- **You cannot use `-parse-as-library` with top-level code** in `main.swift`. Remove the flag, or use
  `@main`.
