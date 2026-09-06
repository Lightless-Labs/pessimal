//! UniFFI bindgen CLI entry point.
//!
//! Exists so the bindgen binary and the `uniffi` runtime crate are the same version. A mismatch
//! produces bindings whose API checksums disagree with the compiled library, which surfaces much
//! later as an opaque runtime panic in the app.

fn main() {
    uniffi::uniffi_bindgen_main();
}
