#!/usr/bin/env bash
# Regenerate the Swift bindings from the built Rust library.
#
#   ./tools/uniffi/regen.sh
#
# Both output files must be regenerated and committed together with any change to pessimal_ffi's
# Rust signatures. Copying only the .swift gives undefined-symbol link errors; a stale .swift
# against a fresh library gives a runtime "UniFFI API checksum mismatch"; and a stale committed
# .swift means your Rust change silently has no effect, because the build compiles the committed
# file rather than your code.
#
# CI runs this and then `git diff --exit-code`, so a forgotten regeneration fails Rust CI rather
# than the iOS build.
set -euo pipefail

cd "$(dirname "$0")/../.."
OUT="clients/apple/PessimalFFI/Sources"

# The bindings must come from a library built from the CURRENT source, or the generated API
# checksums disagree with the compiled scaffolding and the app panics at its first call.
cargo build -p pessimal_ffi
cargo build -p pessimal_uniffi_bindgen

mkdir -p "$OUT"
# The cdylib's extension is platform-specific, and the bindgen reads the library rather than the
# source, so this has to resolve it rather than hardcode one.
case "$(uname -s)" in
  Darwin) LIB=target/debug/libpessimal_ffi.dylib ;;
  *)      LIB=target/debug/libpessimal_ffi.so ;;
esac
[ -f "$LIB" ] || { echo "no cdylib at $LIB" >&2; exit 1; }

./target/debug/pessimal-uniffi-bindgen generate \
    --library "$LIB" \
    --language swift \
    --out-dir "$OUT"

echo "regenerated into $OUT:"
ls -1 "$OUT"
