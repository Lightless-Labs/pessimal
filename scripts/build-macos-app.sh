#!/usr/bin/env bash
# Assembles `Pessimal.app`, the macOS menu bar client, with plain `swiftc`.
#
# There is no Xcode project and no Bazel target behind this: the app is one Swift module compiled
# in a single invocation, linked against the `pessimal_ffi` Rust staticlib, and dropped into a
# hand-built bundle. That keeps the whole build readable in one file and runnable from CI without
# an Xcode workspace to keep in sync.
#
# Usage: scripts/build-macos-app.sh [--release]
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BINDINGS_DIR="$ROOT_DIR/clients/apple/PessimalFFI/Sources"
APP_SOURCE_DIR="$ROOT_DIR/clients/apple/macos/Sources"
SHARED_SOURCE_DIR="$ROOT_DIR/clients/apple/PessimalKit/Sources"
RESOURCES_DIR="$ROOT_DIR/clients/apple/macos/Resources"
INFO_PLIST_TEMPLATE="$RESOURCES_DIR/Info.plist"
BUILD_ROOT="${PESSIMAL_MACOS_BUILD_DIR:-$ROOT_DIR/.build/macos}"
APP_NAME="Pessimal.app"
APP_DIR="$BUILD_ROOT/$APP_NAME"
CONTENTS_DIR="$APP_DIR/Contents"
MACOS_DIR="$CONTENTS_DIR/MacOS"
RESOURCES_OUT_DIR="$CONTENTS_DIR/Resources"
BINARY_PATH="$MACOS_DIR/Pessimal"
INFO_PLIST_PATH="$CONTENTS_DIR/Info.plist"
CODESIGN_IDENTITY="${CODESIGN_IDENTITY:-}"
CODESIGN_KEYCHAIN="${CODESIGN_KEYCHAIN:-}"

CARGO_PROFILE="debug"

usage() {
  cat <<'EOF'
Usage:
  scripts/build-macos-app.sh [--release]

Builds the pessimal_ffi staticlib with cargo, compiles the generated UniFFI bindings together with
the macOS app sources into one Swift module, and assembles .build/macos/Pessimal.app.

Options:
  --release   Build the Rust staticlib with the release profile and link that one instead of debug.
  --help      Show this message.

Environment:
  PESSIMAL_MACOS_BUILD_DIR           Where the bundle is assembled (default .build/macos).
  PESSIMAL_MACOS_VERSION             Override CFBundleShortVersionString (default: workspace version).
  PESSIMAL_MACOS_BUILD               Override CFBundleVersion (default: the version's digits).
  PESSIMAL_MACOS_DEPLOYMENT_TARGET   Override the compiled deployment target (default: the
                                     LSMinimumSystemVersion in the Info.plist template).
  CODESIGN_IDENTITY                  When set, the finished bundle is signed with it.
  CODESIGN_KEYCHAIN                  Keychain to find that identity in.

Signing here is enough to run the app locally. Distribution also needs scripts/notarize-macos-app.sh.
EOF
}

fail() {
  echo "error: $*" >&2
  exit 2
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --release)
      CARGO_PROFILE="release"
      shift
      ;;
    --help | -h)
      usage
      exit 0
      ;;
    *)
      echo "error: unknown argument: $1" >&2
      echo >&2
      usage >&2
      exit 2
      ;;
  esac
done

if [[ "$(uname -s)" != "Darwin" ]]; then
  fail "macOS app bundles must be built on macOS"
fi

command -v swiftc >/dev/null || fail "swiftc is required (install the Xcode command line tools)"
command -v cargo >/dev/null || fail "cargo is required"
command -v otool >/dev/null || fail "otool is required to verify what the binary linked"
command -v plutil >/dev/null || fail "plutil is required to validate the generated Info.plist"

# Three generated files, and all three matter. `PessimalFFI.swift` guards its import with
# `#if canImport(PessimalFFIFFI)`, so without the modulemap and its header the bindings compile to
# nothing and the failure reads `cannot find type 'RustBuffer' in scope` — a message that never
# mentions modules. Better to say so here than to hand a developer that error.
for generated in PessimalFFI.swift PessimalFFIFFI.h PessimalFFIFFI.modulemap; do
  [[ -f "$BINDINGS_DIR/$generated" ]] ||
    fail "missing generated binding $BINDINGS_DIR/$generated — run ./tools/uniffi/regen.sh"
done

[[ -f "$INFO_PLIST_TEMPLATE" ]] || fail "missing Info.plist template: $INFO_PLIST_TEMPLATE"
[[ -d "$APP_SOURCE_DIR" ]] || fail "missing app sources: $APP_SOURCE_DIR"

# The app is one Swift module, so there is no `import PessimalKit` to write: anything shared with
# the iOS app compiles into the same module as the app's own sources. Collected separately only so
# that a missing app source is reported as such.
app_sources=()
while IFS= read -r source; do
  app_sources+=("$source")
done < <(find "$APP_SOURCE_DIR" -type f -name '*.swift' | LC_ALL=C sort)

shared_sources=()
if [[ -d "$SHARED_SOURCE_DIR" ]]; then
  while IFS= read -r source; do
    shared_sources+=("$source")
  done < <(find "$SHARED_SOURCE_DIR" -type f -name '*.swift' | LC_ALL=C sort)
fi

if [[ ${#app_sources[@]} -eq 0 ]]; then
  fail "no Swift sources under $APP_SOURCE_DIR — nothing to build"
fi

# The workspace version is the app's version. Scoped to the `[workspace.package]` table so that a
# dependency's `version = "..."` cannot be picked up by accident.
VERSION="${PESSIMAL_MACOS_VERSION:-}"
if [[ -z "$VERSION" ]]; then
  VERSION="$(awk '
    /^\[/ { in_package = ($0 ~ /^\[workspace\.package\]/) }
    in_package && /^[[:space:]]*version[[:space:]]*=/ {
      if (match($0, /"[^"]*"/)) { print substr($0, RSTART + 1, RLENGTH - 2); exit }
    }
  ' "$ROOT_DIR/Cargo.toml")"
fi
[[ -n "$VERSION" ]] || fail "could not read [workspace.package] version from $ROOT_DIR/Cargo.toml"
# These are about to become the right-hand side of a sed expression and then XML. A `&` or a `/`
# there does not fail, it substitutes something other than what was asked for.
[[ "$VERSION" =~ ^[A-Za-z0-9._+-]+$ ]] || fail "version is not plist-safe: $VERSION"

# CFBundleVersion has to be a monotonic build counter, not a marketing string. Absent a real one,
# the version's digits are stable, ordered, and honest about where they came from.
# CFBundleVersion defaults to the marketing version, which is already dotted digits and reads
# sensibly in Finder. Stripping the dots would turn 0.1.0 into "010", which sorts wrongly the
# moment a version reaches double digits (0.10.0 -> "0100" < "011" for 0.1.1). A release
# pipeline should set PESSIMAL_MACOS_BUILD to something monotonic.
BUILD_NUMBER="${PESSIMAL_MACOS_BUILD:-$VERSION}"
BUILD_NUMBER="${BUILD_NUMBER:-1}"
[[ "$BUILD_NUMBER" =~ ^[0-9.]+$ ]] || fail "CFBundleVersion must be digits and dots, got: $BUILD_NUMBER"

# The plist promises macOS 14. swiftc would otherwise compile for the SDK's own target — macOS 26
# on a current toolchain — and the app would refuse to launch on every system the plist claims to
# support, with no warning at build time. One source of truth: read the promise and compile to it.
DEPLOYMENT_TARGET="${PESSIMAL_MACOS_DEPLOYMENT_TARGET:-}"
if [[ -z "$DEPLOYMENT_TARGET" ]]; then
  DEPLOYMENT_TARGET="$(/usr/libexec/PlistBuddy -c 'Print :LSMinimumSystemVersion' "$INFO_PLIST_TEMPLATE" 2>/dev/null || true)"
fi
[[ -n "$DEPLOYMENT_TARGET" ]] || fail "no LSMinimumSystemVersion in $INFO_PLIST_TEMPLATE and no PESSIMAL_MACOS_DEPLOYMENT_TARGET set"

# The same deployment target the Swift side compiles to, for the same reason. Without it the C
# dependencies (aws-lc-rs and friends, built through cc-rs) inherit the *build machine's* macOS
# version, and the link answers with a warning per object file — several hundred of them — that the
# archive was built for a newer macOS than it is being linked into. The warnings are the readable
# symptom; the unreadable one is a C object calling an API that macOS 14 does not have.
echo "Building pessimal_ffi ($CARGO_PROFILE) for macOS $DEPLOYMENT_TARGET..."
if [[ "$CARGO_PROFILE" == "release" ]]; then
  (cd "$ROOT_DIR" && MACOSX_DEPLOYMENT_TARGET="$DEPLOYMENT_TARGET" cargo build --release -p pessimal_ffi)
else
  (cd "$ROOT_DIR" && MACOSX_DEPLOYMENT_TARGET="$DEPLOYMENT_TARGET" cargo build -p pessimal_ffi)
fi

# Linked as a positional input rather than with `-L target/<profile> -l pessimal_ffi`. Cargo emits
# the staticlib and the cdylib into the same directory, the linker prefers the dylib when both are
# on a search path, and that dylib's install name is an absolute path into this checkout's target/
# — an app that launches on the build machine and nowhere else. Naming the archive removes the
# choice; the otool assertion below keeps it removed.
CARGO_TARGET_ROOT="${CARGO_TARGET_DIR:-$ROOT_DIR/target}"
STATICLIB="$CARGO_TARGET_ROOT/$CARGO_PROFILE/libpessimal_ffi.a"
[[ -f "$STATICLIB" ]] || fail "cargo did not produce $STATICLIB"

rm -rf "$APP_DIR"
mkdir -p "$MACOS_DIR" "$RESOURCES_OUT_DIR"

# Every check from here on runs after something has already been written into the bundle, and a
# half-assembled Pessimal.app is indistinguishable from a finished one to anything downstream:
# notarize-macos-app.sh asks only whether the directory and its executable exist, and would sign
# and ship precisely the artifact these checks exist to reject. So a failure takes the bundle with
# it. Installed after the mkdir so that --help and the pre-flight guards never delete anything.
remove_partial_bundle() {
  local status=$?
  if [[ $status -ne 0 ]]; then
    rm -rf "$APP_DIR"
    echo "removed the incomplete bundle at $APP_DIR" >&2
  fi
}
trap remove_partial_bundle EXIT

LINK_LOG="$BUILD_ROOT/swiftc.log"
echo "Compiling ${#app_sources[@]} app source(s) and ${#shared_sources[@]} shared source(s) for macOS $DEPLOYMENT_TARGET..."
swiftc \
  -O \
  -parse-as-library \
  -target "$(uname -m)-apple-macos$DEPLOYMENT_TARGET" \
  -Xcc -fmodule-map-file="$BINDINGS_DIR/PessimalFFIFFI.modulemap" \
  -I "$BINDINGS_DIR" \
  "$STATICLIB" \
  -framework SystemConfiguration \
  -framework CoreFoundation \
  -framework Security \
  "$BINDINGS_DIR/PessimalFFI.swift" \
  ${shared_sources[@]+"${shared_sources[@]}"} \
  "${app_sources[@]}" \
  -o "$BINARY_PATH" 2>&1 | tee "$LINK_LOG"
chmod 0755 "$BINARY_PATH"

# Cargo does not fingerprint MACOSX_DEPLOYMENT_TARGET: nothing in the dependency graph declares
# `rerun-if-env-changed` for it, so a target/ already warm from `cargo test` hands this link the
# objects it happens to hold — C dependencies compiled against the *build machine's* macOS — and
# rebuilds nothing. The result links cleanly and is stamped for macOS 14 while carrying code built
# for something newer, which fails on a user's Mac and never on this one. The linker is the only
# party that sees both sides, so its warning is the check.
if grep -q 'built for newer' "$LINK_LOG"; then
  echo "error: $STATICLIB holds objects built for a newer macOS than $DEPLOYMENT_TARGET." >&2
  echo "       Cargo could not tell that MACOSX_DEPLOYMENT_TARGET changed and reused a stale build." >&2
  echo "       Rebuild them with 'cargo clean' first, or give this script its own CARGO_TARGET_DIR." >&2
  echo "       The linker named the offending objects in $LINK_LOG." >&2
  exit 2
fi

# The link is only static if nothing here says otherwise. A dependency on libpessimal_ffi means the
# dylib won it, and the bundle would be broken for everyone but this machine.
# Captured rather than piped straight into grep: in a pipeline an otool that fails outright is
# masked by grep's own "no match", and the check would pass by not having run.
LINKED_LIBRARIES="$(otool -L "$BINARY_PATH")"
if printf '%s\n' "$LINKED_LIBRARIES" | grep -q 'libpessimal_ffi'; then
  fail "the binary links libpessimal_ffi dynamically; it must be linked against $STATICLIB"
fi

sed \
  -e "s/__PESSIMAL_VERSION__/$VERSION/g" \
  -e "s/__PESSIMAL_BUILD__/$BUILD_NUMBER/g" \
  "$INFO_PLIST_TEMPLATE" > "$INFO_PLIST_PATH"
plutil -lint "$INFO_PLIST_PATH" >/dev/null || fail "the generated Info.plist is not valid: $INFO_PLIST_PATH"

# A placeholder this script does not know about is a placeholder nothing substitutes, and it would
# ship as literal text in a plist that stays perfectly valid XML while saying the wrong thing.
if grep -q '__PESSIMAL_[A-Z_]*__' "$INFO_PLIST_PATH"; then
  fail "unsubstituted placeholder in $INFO_PLIST_PATH: $(grep -o '__PESSIMAL_[A-Z_]*__' "$INFO_PLIST_PATH" | sort -u | tr '\n' ' ')"
fi

# Anything else in Resources/ — an .icns, a localisation — is a real bundle resource. The Info.plist
# there is the template that was just substituted, and belongs in Contents/, not Contents/Resources/.
while IFS= read -r resource; do
  if [[ "$(basename "$resource")" == "Info.plist" ]]; then
    continue
  fi
  cp -R "$resource" "$RESOURCES_OUT_DIR/"
done < <(find "$RESOURCES_DIR" -mindepth 1 -maxdepth 1)

if [[ -n "$CODESIGN_IDENTITY" ]]; then
  codesign_args=(--force --timestamp --options runtime --sign "$CODESIGN_IDENTITY")
  if [[ -n "$CODESIGN_KEYCHAIN" ]]; then
    codesign_args+=(--keychain "$CODESIGN_KEYCHAIN")
  fi
  codesign "${codesign_args[@]}" "$APP_DIR"
  codesign --verify --deep --strict --verbose=2 "$APP_DIR"
fi

BUNDLE_ID="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$INFO_PLIST_PATH" 2>/dev/null || echo unknown)"

cat <<EOF
Built macOS app:
  $APP_DIR
Version:
  $VERSION ($BUILD_NUMBER), macOS $DEPLOYMENT_TARGET or later
Bundle identifier:
  $BUNDLE_ID
EOF
