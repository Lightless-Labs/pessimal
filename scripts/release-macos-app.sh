#!/usr/bin/env bash
# Build, sign and notarize Pessimal.app from secrets held in Doppler.
#
#   DOPPLER_TOKEN=... DOPPLER_PROJECT=lightless-labs-pessimal DOPPLER_CONFIG=prd_macos_notarisation \
#     ./scripts/release-macos-app.sh
#
# This is the wrapper that makes the notarize script usable by hand. It exists because the two halves
# want different shapes: Doppler holds the signing certificate and the notary key as base64 blobs,
# while `codesign` wants an identity in a keychain and `notarytool` wants a .p8 on disk. Everything
# it creates is ephemeral and removed on exit, including on failure.
#
# All of that now lives in scripts/lib/macos-signing.sh, because the Buildkite release-macos step
# needs exactly the same thing and a second copy would be a second place to fix the next credential
# bug. What is left here is the order, and the order is the point:
#
#   BUILD FIRST, FETCH SECOND. The build used to run at line 129, i.e. after the certificate was on
#   disk and a keychain had been unlocked for six hours (-lut 21600) with codesign: in its partition
#   list. A Swift compile plus every build.rs in a 322-package dependency graph is a great deal of
#   third-party code to run inside that window for no benefit — the build needs no credential. So it
#   happens first, and the credential window is now just sign + notarize.
#
# Note this script is not on the CI path: the Buildkite release-macos step builds with
# scripts/release-build-macos.sh and signs through scripts/release-macos-artifacts.sh, which does not
# build. This one stays because it is the artefact somebody reuses next, and a hand-run path with the
# old credential hygiene would be a trap.
#
# Required secrets (see the library for what each is):
#   MACOS_DEVELOPER_ID_CERT_P12_BASE64, MACOS_DEVELOPER_ID_CERT_PASSWORD,
#   APPLE_NOTARY_KEY_ID, APPLE_NOTARY_ISSUER_ID, APPLE_NOTARY_KEY_P8_BASE64
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"
# Exported so the library's fetch sees them; the defaults are identical on both sides.
export DOPPLER_PROJECT="${DOPPLER_PROJECT:-lightless-labs-pessimal}"
export DOPPLER_CONFIG="${DOPPLER_CONFIG:-prd_macos_notarisation}"

usage() { sed -n '2,29p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; }
[[ "${1:-}" == "--help" || "${1:-}" == "-h" ]] && { usage; exit 0; }

# Sourced BEFORE the build on purpose: sourcing stops exporting DOPPLER_TOKEN, and the caller put the
# token in this process's environment, so without this the reorder below would still hand it to
# swiftc, cargo and every build.rs. The fetch later reads it as a plain shell variable.
# shellcheck source=scripts/lib/macos-signing.sh
source "$ROOT_DIR/scripts/lib/macos-signing.sh"

# Every tool the sign / notarize / staple half needs, checked before a twenty-minute build rather
# than after it.
macos_signing_require_release_tools

# --- build, with no credential anywhere near it ------------------------------
"$ROOT_DIR/scripts/build-macos-app.sh" --release

# --- then the credential window, as narrow as it goes ------------------------
# fetch_secrets unsets DOPPLER_TOKEN on its way out; open_keychain installs the cleanup trap that
# removes the keychain and both decoded key files on success, failure and interrupt alike.
macos_signing_fetch_secrets
macos_signing_open_keychain

"$ROOT_DIR/scripts/notarize-macos-app.sh"

echo "Released a notarized Pessimal.app."
