#!/usr/bin/env bash
# The whole of the Buildkite `release-macos` step: three unsigned macOS artefacts in, three finished
# release assets out.
#
# Usage:
#   scripts/release-macos-artifacts.sh --version 0.1.0 --in unsigned/ --out dist/
#
# Expects in --in, exactly as scripts/release-build-macos.sh writes them (each tar holds exactly one
# payload, at any depth — two is a failure, not a choice):
#   unsigned-aarch64-apple-darwin.tar    one file named pessimal-agent, the native slice
#   unsigned-x86_64-apple-darwin.tar     one file named pessimal-agent, the cross-built slice
#   unsigned-Pessimal.app.tar            one directory named Pessimal.app
# Produces in --out, each with a `<asset>.sha256` beside it for release-publish's transport check:
#   pessimal-agent-<version>-aarch64-apple-darwin.tar.gz
#   pessimal-agent-<version>-x86_64-apple-darwin.tar.gz
#   Pessimal-<version>-macos.zip
#
# IT BUILDS AND RUNS NOTHING THAT WAS BUILT. scripts/release-build-macos.sh compiles both agent slices
# and Pessimal.app in the release-macos-build step, in a different guest, with no credential. This
# script runs in the release-macos step, which holds the signing token. Any program run here could read
# that token from its parent's environment, so this step runs only Apple's tools and the packager. It
# signs, notarizes, staples and packages. release-verify-macos runs the signed binaries later, with no
# token.
#
# Credentials: the step's tart-ci plugin exports DOPPLER_TOKEN (from the Buildkite secret
# DOPPLER_PESSIMAL_PRD_MACOS_NOTARISATION) into the guest. scripts/lib/macos-signing.sh stops
# exporting it the moment it is sourced, reads the five secrets with it, and unsets it. Nothing in
# this script prints a secret, and nothing in the guest could redact one if it did.
#
# The token is REQUIRED, and its absence fails at the top, before anything is unpacked. There is no
# unsigned or ad-hoc fallback: an asset under a release name carries a Developer ID signature or does
# not exist. The five Apple secrets are also dropped from the environment before the fetch, so in
# this script Doppler is their only source — a value left in a shell cannot stand in for a failed
# read. (The environment fallback in the library is for scripts/release-macos-app.sh, run by hand.)
# If a release goes wrong, the tag and its draft are deleted and it is cut again, so there is no mode
# in here that runs part of a release without the credential.
#
# Both slices are signed here, in one Apple silicon guest. Measured: the cross-built x86_64 Mach-O
# arrives `code object is not signed at all`, because Apple's linker ad-hoc signs only its native
# arm64 output. An Intel Mac will still run an unsigned x86_64 binary, so this is not about launching;
# it is that there is no signature there to be replaced, notarytool rejects anything without a
# Developer ID signature, hardened runtime and secure timestamp, and a browser-downloaded copy is then
# refused by Gatekeeper. So `codesign --force` on the Intel slice is mandatory rather than cosmetic,
# and the signature is asserted on each slice separately — including the arm64 one, whose linker-made
# ad-hoc signature passes `codesign --verify --strict` all by itself.
#
# Unpacking and the bundle-version check happen before the credential window opens, so a version
# disagreement costs no secret fetch at all.
#
# The app is stapled and the binaries are not. A bundle can hold a ticket; a bare Mach-O has nowhere
# to put one. See packaging/macos/GATEKEEPER.md — that asymmetry is the single most surprising thing
# about this release and it is written down, not inferred. The stapled ticket is checked again on the
# zip this script writes, not only on the bundle it was made from.
#
# There is no early-exit branch anywhere in here. The tag is the intent, so there is nothing to opt
# out of, and "exit 0 because there was nothing to do" is exactly what produced the green run with no
# artefact (build #33).
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/lib/macos-signing.sh
source "$ROOT_DIR/scripts/lib/macos-signing.sh"

# The packager's escape hatch exists for trying it on an unsigned binary locally. Inherited from a
# developer's shell it would let a slice whose signing silently failed be packaged, which is the one
# thing its guard is for.
unset PESSIMAL_PACKAGE_ALLOW_UNSIGNED

VERSION=""
IN_DIR=""
OUT_DIR=""
AGENT_IDENTIFIER="com.lightless-labs.pessimal.agent"

usage() { awk 'NR > 1 && /^#/ { sub(/^# ?/, ""); print; next } NR > 1 { exit }' "${BASH_SOURCE[0]}"; }

while [[ $# -gt 0 ]]; do
  case "$1" in
    --version) VERSION="${2:?--version needs a value}"; shift 2 ;;
    --in) IN_DIR="${2:?--in needs a directory}"; shift 2 ;;
    --out) OUT_DIR="${2:?--out needs a directory}"; shift 2 ;;
    --help|-h) usage; exit 0 ;;
    *) macos_signing_fail "unknown argument: $1" ;;
  esac
done

# Every tool and the one credential, before any work: a missing notarytool or a missing token found
# after the unpack is found too late to say so plainly.
macos_signing_require_release_tools
[[ -n "${DOPPLER_TOKEN:-}" ]] || macos_signing_fail "DOPPLER_TOKEN is not set, so the Apple signing" \
  "secrets cannot be read and nothing can be signed. In the Buildkite release-macos step the tart-ci" \
  "plugin's doppler_token_secret delivers it from the cluster secret DOPPLER_PESSIMAL_PRD_MACOS_NOTARISATION;" \
  "if it is missing there, that secret is absent, its access policy does not cover this build, or the" \
  "calling script (scripts/release-build-macos.sh) did not hand it on. There is no unsigned fallback."
unset MACOS_DEVELOPER_ID_CERT_P12_BASE64 MACOS_DEVELOPER_ID_CERT_PASSWORD \
  APPLE_NOTARY_KEY_ID APPLE_NOTARY_ISSUER_ID APPLE_NOTARY_KEY_P8_BASE64
[[ -n "$VERSION" ]] || macos_signing_fail "--version is required"
[[ -n "$IN_DIR" && -d "$IN_DIR" ]] || macos_signing_fail "--in must name an existing directory"
[[ -n "$OUT_DIR" ]] || macos_signing_fail "--out is required"
mkdir -p "$OUT_DIR"
IN_DIR="$(cd "$IN_DIR" && pwd)"
OUT_DIR="$(cd "$OUT_DIR" && pwd)"

for tarball in unsigned-aarch64-apple-darwin.tar unsigned-x86_64-apple-darwin.tar unsigned-Pessimal.app.tar; do
  [[ -s "$IN_DIR/$tarball" ]] || macos_signing_fail "missing or empty input: $IN_DIR/$tarball"
done

WORK_DIR="$(mktemp -d)"
# One cleanup for the scratch directory, reachable two ways: this trap until the keychain opens, and
# the signing library's own trap after that, which calls the hook. Setting a second trap here once
# the keychain is open would replace the library's and leave the keychain and key files behind.
macos_signing_cleanup_hook() { rm -rf "$WORK_DIR"; }
trap macos_signing_cleanup_hook EXIT

# The inner layout of the unsigned tars belongs to scripts/release-build-macos.sh, so find the payload
# rather than assuming a path, and insist on exactly one hit. "Two binaries in the tar" must be a
# failure and not a coin toss about which one gets signed.
extract_one() {
  local tarball="$1" dest="$2" kind="$3" name="$4" hits=0 found=""
  mkdir -p "$dest"
  tar -xf "$IN_DIR/$tarball" -C "$dest"
  while IFS= read -r candidate; do
    hits=$((hits + 1))
    found="$candidate"
  done < <(find "$dest" -type "$kind" -name "$name" 2>/dev/null || true)
  [[ $hits -eq 1 ]] || macos_signing_fail "expected exactly one '$name' in $tarball, found $hits"
  printf '%s\n' "$found"
}

echo "--- unpacking the unsigned artefacts"
ARM_BINARY="$(extract_one unsigned-aarch64-apple-darwin.tar "$WORK_DIR/aarch64" f pessimal-agent)"
INTEL_BINARY="$(extract_one unsigned-x86_64-apple-darwin.tar "$WORK_DIR/x86_64" f pessimal-agent)"
APP_BUNDLE="$(extract_one unsigned-Pessimal.app.tar "$WORK_DIR/app" d Pessimal.app)"
echo "arm64 agent:  $ARM_BINARY"
echo "x86_64 agent: $INTEL_BINARY"
echo "app bundle:   $APP_BUNDLE"

# Asserted before a single byte is signed, so a version disagreement costs no notary round trip.
# scripts/build-macos-app.sh stamps CFBundleShortVersionString from [workspace.package] version, so
# this is the check that the git tag, the asset manifest and the bundle all say the same thing.
BUNDLE_VERSION="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$APP_BUNDLE/Contents/Info.plist")"
echo "checked CFBundleShortVersionString, found '$BUNDLE_VERSION', expected '$VERSION'"
[[ "$BUNDLE_VERSION" == "$VERSION" ]] || macos_signing_fail \
  "the bundle says $BUNDLE_VERSION but the release is $VERSION — Cargo.toml and the tag disagree"

APP_ARTIFACT_DIR="$WORK_DIR/app-dist"
mkdir -p "$APP_ARTIFACT_DIR"
APP_ZIP_SRC="$APP_ARTIFACT_DIR/Pessimal.app.zip"

echo "--- opening the credential window"
macos_signing_fetch_secrets      # unsets DOPPLER_TOKEN on its way out
macos_signing_open_keychain      # replaces the trap above with one that also calls the hook

echo "--- signing and notarizing the two agent slices (not stapled: a bare Mach-O cannot hold a ticket)"
for slice_binary in "$ARM_BINARY" "$INTEL_BINARY"; do
  "$ROOT_DIR/scripts/notarize-macos-binary.sh" "$slice_binary" --identifier "$AGENT_IDENTIFIER"
done
# Asserted per slice, deliberately: one check over both would read as a pass if either passed. And
# both halves per slice, because --verify alone passes the arm64 slice's ad-hoc linker signature.
for slice_binary in "$ARM_BINARY" "$INTEL_BINARY"; do
  codesign --verify --strict --verbose=2 "$slice_binary"
  macos_signing_assert_developer_id "$slice_binary"
done

echo "--- signing, notarizing and STAPLING Pessimal.app"
# Pointed at our extracted bundle and our own artifact directory rather than .build/macos/dist: the
# bundle came out of a tar, and the build tree it was made in is not this script's to write into.
PESSIMAL_MACOS_APP="$APP_BUNDLE" PESSIMAL_MACOS_ARTIFACT_DIR="$APP_ARTIFACT_DIR" \
  "$ROOT_DIR/scripts/notarize-macos-app.sh" "$APP_BUNDLE"

APP_ASSET="Pessimal-$VERSION-macos.zip"
APP_ZIP_DST="$OUT_DIR/$APP_ASSET"
[[ -s "$APP_ZIP_SRC" ]] || macos_signing_fail "no app zip was produced at $APP_ZIP_SRC"
# notarize-macos-app.sh names its output after the bundle; the release asset is named after the
# version. The rename is a real step and its result is asserted, not assumed.
cp "$APP_ZIP_SRC" "$APP_ZIP_DST"
[[ -s "$APP_ZIP_DST" ]] || macos_signing_fail "the renamed app zip is missing or empty: $APP_ZIP_DST"
echo "checked the renamed app zip, found $(wc -c < "$APP_ZIP_DST" | tr -d ' ') bytes at $APP_ZIP_DST"

# notarize-macos-app.sh validated the staple on the bundle and then zipped it again. This reads the
# ticket back out of the zip that will be published, so a zip of the pre-staple bundle — or of the
# wrong one — fails here rather than on a Mac with no network.
APP_CHECK_DIR="$WORK_DIR/app-check"
mkdir -p "$APP_CHECK_DIR"
ditto -x -k "$APP_ZIP_DST" "$APP_CHECK_DIR"
[[ -d "$APP_CHECK_DIR/Pessimal.app" ]] || macos_signing_fail "$APP_ASSET does not unpack to Pessimal.app"
xcrun stapler validate "$APP_CHECK_DIR/Pessimal.app" \
  || macos_signing_fail "$APP_ASSET holds a Pessimal.app with no valid stapled ticket"
echo "checked $APP_ASSET: the Pessimal.app inside carries a valid stapled notarisation ticket"

# Same two-space format and bare filename as scripts/package-agent-release.sh writes for the tarballs,
# so release-publish verifies all three the same way.
( cd "$OUT_DIR" && shasum -a 256 "$APP_ASSET" > "$APP_ASSET.sha256" )

echo "--- packaging the two agent tarballs"
# No PESSIMAL_PACKAGE_ALLOW_UNSIGNED (it was unset above). The packager refuses an unsigned
# *-apple-darwin input, and the Developer ID assertion above has already refused an ad-hoc one.
ARM_ARCHIVE="$("$ROOT_DIR/scripts/package-agent-release.sh" \
  --binary "$ARM_BINARY" --triple aarch64-apple-darwin --version "$VERSION" --out "$OUT_DIR")"
INTEL_ARCHIVE="$("$ROOT_DIR/scripts/package-agent-release.sh" \
  --binary "$INTEL_BINARY" --triple x86_64-apple-darwin --version "$VERSION" --out "$OUT_DIR")"

for artefact in "$ARM_ARCHIVE" "$INTEL_ARCHIVE" "$APP_ZIP_DST"; do
  [[ -s "$artefact" ]] || macos_signing_fail "expected a non-empty artefact at $artefact"
  [[ -s "$artefact.sha256" ]] || macos_signing_fail "expected a non-empty transport hash at $artefact.sha256"
done

echo "--- three signed macOS release assets (agents notarized; app notarized and stapled)"
cat <<EOF
$ARM_ARCHIVE
$INTEL_ARCHIVE
$APP_ZIP_DST
EOF
