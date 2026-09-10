#!/usr/bin/env bash
# Build, sign and notarize Pessimal.app from secrets held in Doppler.
#
#   DOPPLER_TOKEN=... DOPPLER_PROJECT=pessimal DOPPLER_CONFIG=prd \
#     ./scripts/release-macos-app.sh
#
# This is the wrapper that makes the notarize script usable in CI. It exists because the two halves
# want different shapes: Doppler holds the signing certificate and the notary key as base64 blobs,
# while `codesign` wants an identity in a keychain and `notarytool` wants a .p8 on disk. Everything
# it creates is ephemeral and removed on exit, including on failure.
#
# The secret names are deliberately identical to the sibling project Descartes, so one Doppler
# config can serve both rather than drifting into two.
#
# Required secrets:
#   MACOS_DEVELOPER_ID_CERT_P12_BASE64   Developer ID Application cert + key, as base64 of a .p12
#   MACOS_DEVELOPER_ID_CERT_PASSWORD     the .p12's password
#   APPLE_NOTARY_KEY_ID                  App Store Connect API key id
#   APPLE_NOTARY_ISSUER_ID               App Store Connect issuer id
#   APPLE_NOTARY_KEY_P8_BASE64           the .p8 private key, base64
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"
BUILD_ROOT="${PESSIMAL_RELEASE_BUILD_DIR:-$ROOT_DIR/.build/macos/release-signing}"
DOPPLER_PROJECT="${DOPPLER_PROJECT:-pessimal}"
DOPPLER_CONFIG="${DOPPLER_CONFIG:-prd}"

usage() { sed -n '2,25p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; }
[[ "${1:-}" == "--help" || "${1:-}" == "-h" ]] && { usage; exit 0; }

fail() { echo "error: $*" >&2; exit 2; }

[[ "$(uname -s)" == "Darwin" ]] || fail "macOS release builds must run on macOS"
for tool in security openssl base64 xcrun; do
  command -v "$tool" >/dev/null || fail "$tool is required"
done

# --- secrets -----------------------------------------------------------------
# Fetched one at a time rather than via `doppler run`, so a missing secret names itself instead of
# surfacing later as an empty variable and an unsigned app.
fetch_secret() {
  local name="$1" optional="${2:-}" value=""
  if command -v doppler >/dev/null 2>&1; then
    value="$(doppler secrets get "$name" --plain \
      --project "$DOPPLER_PROJECT" --config "$DOPPLER_CONFIG" 2>/dev/null || true)"
  fi
  # Fall back to the environment whenever Doppler did not supply it -- not only when doppler is
  # absent. Having the CLI installed but pointed at the wrong project is the common case, and
  # treating that as "unset" would tell a developer who had exported the value themselves that
  # their own environment was empty.
  if [[ -z "$value" ]]; then
    value="${!name:-}"
  fi
  if [[ -z "$value" ]]; then
    [[ -n "$optional" ]] && return 0
    fail "$name is not set (looked in Doppler $DOPPLER_PROJECT/$DOPPLER_CONFIG, then the environment)"
  fi
  printf -v "$name" '%s' "$value"
  export "$name"
}

fetch_secret MACOS_DEVELOPER_ID_CERT_P12_BASE64
fetch_secret MACOS_DEVELOPER_ID_CERT_PASSWORD
fetch_secret APPLE_NOTARY_KEY_ID
fetch_secret APPLE_NOTARY_ISSUER_ID
fetch_secret APPLE_NOTARY_KEY_P8_BASE64

base64_decode() {
  # macOS base64 predates --decode in some releases; -D is the portable spelling here.
  if base64 --help 2>&1 | grep -q -- '--decode'; then base64 --decode; else base64 -D; fi
}

mkdir -p "$BUILD_ROOT"
CERT_PATH="$BUILD_ROOT/developer-id.p12"
NOTARY_KEY_PATH="$BUILD_ROOT/AuthKey_${APPLE_NOTARY_KEY_ID}.p8"
KEYCHAIN_PATH="$BUILD_ROOT/pessimal-signing.keychain-db"
KEYCHAIN_PASSWORD="$(openssl rand -base64 48)"
ORIGINAL_USER_KEYCHAINS="$(security list-keychains -d user 2>/dev/null | tr -d '"' || true)"

# --- cleanup, unconditionally ------------------------------------------------
# A leaked signing keychain or a .p8 left on disk is a credential left behind, so this runs on
# success, failure and interrupt alike.
cleanup() {
  if [[ -n "$ORIGINAL_USER_KEYCHAINS" ]]; then
    local keychains=()
    while IFS= read -r k; do [[ -n "$k" ]] && keychains+=("$k"); done <<< "$ORIGINAL_USER_KEYCHAINS"
    [[ ${#keychains[@]} -gt 0 ]] && security list-keychains -d user -s "${keychains[@]}" >/dev/null 2>&1 || true
  fi
  security delete-keychain "$KEYCHAIN_PATH" >/dev/null 2>&1 || true
  rm -f "$CERT_PATH" "$NOTARY_KEY_PATH"
}
trap cleanup EXIT INT TERM

printf '%s' "$MACOS_DEVELOPER_ID_CERT_P12_BASE64" | base64_decode > "$CERT_PATH"
printf '%s' "$APPLE_NOTARY_KEY_P8_BASE64" | base64_decode > "$NOTARY_KEY_PATH"
chmod 0600 "$CERT_PATH" "$NOTARY_KEY_PATH"
[[ -s "$CERT_PATH" ]] || fail "the decoded .p12 is empty; check MACOS_DEVELOPER_ID_CERT_P12_BASE64"
[[ -s "$NOTARY_KEY_PATH" ]] || fail "the decoded .p8 is empty; check APPLE_NOTARY_KEY_P8_BASE64"

# --- an ephemeral keychain ---------------------------------------------------
security create-keychain -p "$KEYCHAIN_PASSWORD" "$KEYCHAIN_PATH"
# Long enough for a notarization round trip; the default 5 minutes relocks mid-submission.
security set-keychain-settings -lut 21600 "$KEYCHAIN_PATH"
security unlock-keychain -p "$KEYCHAIN_PASSWORD" "$KEYCHAIN_PATH"
security list-keychains -d user -s $ORIGINAL_USER_KEYCHAINS "$KEYCHAIN_PATH" >/dev/null

security import "$CERT_PATH" \
  -k "$KEYCHAIN_PATH" \
  -P "$MACOS_DEVELOPER_ID_CERT_PASSWORD" \
  -T /usr/bin/codesign \
  -T /usr/bin/security >/dev/null

# Without this, codesign blocks on a GUI prompt for keychain access and the build hangs rather than
# failing — the single most confusing way for unattended signing to go wrong.
security set-key-partition-list -S apple-tool:,apple:,codesign: \
  -s -k "$KEYCHAIN_PASSWORD" "$KEYCHAIN_PATH" >/dev/null 2>&1

IDENTITY="${CODESIGN_IDENTITY:-$(security find-identity -v -p codesigning "$KEYCHAIN_PATH" \
  | awk -F'"' '/Developer ID Application/ {print $2; exit}')}"
[[ -n "$IDENTITY" ]] || fail "no Developer ID Application identity found in the imported certificate"
echo "Signing as: $IDENTITY"

# --- build, then notarize ----------------------------------------------------
export CODESIGN_IDENTITY="$IDENTITY"
export CODESIGN_KEYCHAIN="$KEYCHAIN_PATH"
export APPLE_NOTARY_KEY_PATH="$NOTARY_KEY_PATH"

"$ROOT_DIR/scripts/build-macos-app.sh" --release
"$ROOT_DIR/scripts/notarize-macos-app.sh"

echo "Released a notarized Pessimal.app."
