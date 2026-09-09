#!/usr/bin/env bash
# Signs, notarizes, and staples `Pessimal.app` so Gatekeeper lets it run on a machine that has never
# seen this developer.
#
# Run scripts/build-macos-app.sh first; this takes the finished bundle, re-signs it with the
# hardened runtime, submits it to Apple's notary service, and staples the returned ticket so the app
# launches without a network round trip.
#
# Usage: scripts/notarize-macos-app.sh [Pessimal.app]
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APP_DIR="${1:-${PESSIMAL_MACOS_APP:-$ROOT_DIR/.build/macos/Pessimal.app}}"
# `dist`, not `release`: the sibling build script's --release names a cargo profile, and a directory
# called release next to it would read as that rather than as "what gets shipped".
ARTIFACT_DIR="${PESSIMAL_MACOS_ARTIFACT_DIR:-$ROOT_DIR/.build/macos/dist}"
ZIP_PATH="$ARTIFACT_DIR/Pessimal.app.zip"
CODESIGN_IDENTITY="${CODESIGN_IDENTITY:-}"
CODESIGN_KEYCHAIN="${CODESIGN_KEYCHAIN:-}"
KEYCHAIN_PROFILE="${APPLE_NOTARY_KEYCHAIN_PROFILE:-}"
APPLE_NOTARY_KEY_PATH="${APPLE_NOTARY_KEY_PATH:-}"
APPLE_NOTARY_KEY_ID="${APPLE_NOTARY_KEY_ID:-}"
APPLE_NOTARY_ISSUER_ID="${APPLE_NOTARY_ISSUER_ID:-}"
APPLE_ID="${APPLE_ID:-}"
APPLE_TEAM_ID="${APPLE_TEAM_ID:-}"
APPLE_APP_SPECIFIC_PASSWORD="${APPLE_APP_SPECIFIC_PASSWORD:-}"

usage() {
  cat <<'EOF'
Usage:
  CODESIGN_IDENTITY="Developer ID Application: ..." \
  APPLE_NOTARY_KEYCHAIN_PROFILE="pessimal-notary" \
  scripts/notarize-macos-app.sh [Pessimal.app]

Preferred CI notary credentials:
  APPLE_NOTARY_KEY_PATH, APPLE_NOTARY_KEY_ID, APPLE_NOTARY_ISSUER_ID

Alternative notary credentials:
  APPLE_NOTARY_KEYCHAIN_PROFILE
  APPLE_ID, APPLE_TEAM_ID, APPLE_APP_SPECIFIC_PASSWORD

Other environment:
  CODESIGN_KEYCHAIN                Keychain to find the signing identity in.
  PESSIMAL_MACOS_APP               The bundle to notarize (default .build/macos/Pessimal.app).
  PESSIMAL_MACOS_ARTIFACT_DIR      Where the stapled zip is written (default .build/macos/dist).

The script signs with hardened runtime, zips the app, submits it with xcrun notarytool,
staples the ticket, and verifies the stapled app with Gatekeeper.
EOF
}

if [[ "${1:-}" == "--help" || "${1:-}" == "-h" ]]; then
  usage
  exit 0
fi

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "error: macOS notarization must run on macOS" >&2
  exit 2
fi

[[ -d "$APP_DIR" ]] || { echo "error: app bundle not found: $APP_DIR — run scripts/build-macos-app.sh first" >&2; exit 2; }
[[ -x "$APP_DIR/Contents/MacOS/Pessimal" ]] || { echo "error: no executable at $APP_DIR/Contents/MacOS/Pessimal" >&2; exit 2; }
[[ -n "$CODESIGN_IDENTITY" ]] || { echo "error: CODESIGN_IDENTITY is required" >&2; exit 2; }
command -v codesign >/dev/null || { echo "error: codesign is required" >&2; exit 2; }
command -v xcrun >/dev/null || { echo "error: xcrun is required" >&2; exit 2; }
command -v spctl >/dev/null || { echo "error: spctl is required" >&2; exit 2; }
command -v ditto >/dev/null || { echo "error: ditto is required" >&2; exit 2; }

mkdir -p "$ARTIFACT_DIR"

# Hardened runtime is what the notary service requires. The Rust core is linked statically, so the
# bundle holds exactly one piece of code and this one signature covers all of it; no entitlements
# are needed for a client that only makes outbound HTTPS requests.
codesign_args=(--force --timestamp --options runtime --sign "$CODESIGN_IDENTITY")
if [[ -n "$CODESIGN_KEYCHAIN" ]]; then
  codesign_args+=(--keychain "$CODESIGN_KEYCHAIN")
fi
codesign "${codesign_args[@]}" "$APP_DIR"
codesign --verify --deep --strict --verbose=2 "$APP_DIR"

rm -f "$ZIP_PATH"
ditto -c -k --keepParent "$APP_DIR" "$ZIP_PATH"

if [[ -n "$APPLE_NOTARY_KEY_PATH" && -n "$APPLE_NOTARY_KEY_ID" && -n "$APPLE_NOTARY_ISSUER_ID" ]]; then
  xcrun notarytool submit "$ZIP_PATH" --key "$APPLE_NOTARY_KEY_PATH" --key-id "$APPLE_NOTARY_KEY_ID" --issuer "$APPLE_NOTARY_ISSUER_ID" --wait
elif [[ -n "$KEYCHAIN_PROFILE" ]]; then
  xcrun notarytool submit "$ZIP_PATH" --keychain-profile "$KEYCHAIN_PROFILE" --wait
elif [[ -n "$APPLE_ID" && -n "$APPLE_TEAM_ID" && -n "$APPLE_APP_SPECIFIC_PASSWORD" ]]; then
  xcrun notarytool submit "$ZIP_PATH" --apple-id "$APPLE_ID" --team-id "$APPLE_TEAM_ID" --password "$APPLE_APP_SPECIFIC_PASSWORD" --wait
else
  echo "error: set APPLE_NOTARY_KEY_PATH/APPLE_NOTARY_KEY_ID/APPLE_NOTARY_ISSUER_ID, APPLE_NOTARY_KEYCHAIN_PROFILE, or APPLE_ID/APPLE_TEAM_ID/APPLE_APP_SPECIFIC_PASSWORD" >&2
  exit 2
fi

xcrun stapler staple "$APP_DIR" || {
  echo "Initial stapling failed; retrying for notary ticket propagation..." >&2
  for i in 1 2 3 4; do
    sleep 15
    if xcrun stapler staple "$APP_DIR"; then
      break
    fi
    if (( i == 4 )); then
      echo "error: stapling failed after retries" >&2
      exit 1
    fi
  done
}
xcrun stapler validate "$APP_DIR"
spctl --assess --type execute --verbose=4 "$APP_DIR"

# Publish the stapled app, not the pre-staple submission zip.
rm -f "$ZIP_PATH"
ditto -c -k --keepParent "$APP_DIR" "$ZIP_PATH"

BUNDLE_ID="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$APP_DIR/Contents/Info.plist" 2>/dev/null || echo unknown)"

cat <<EOF
Notarized macOS app:
  $APP_DIR
Bundle identifier:
  $BUNDLE_ID
Stapled release artifact:
  $ZIP_PATH
EOF
