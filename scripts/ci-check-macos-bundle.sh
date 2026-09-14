#!/usr/bin/env bash
# Asserts that a built Pessimal.app is a real, self-contained menu bar app, not merely a directory
# that scripts/build-macos-app.sh happened to finish writing.
#
# Usage: scripts/ci-check-macos-bundle.sh [path/to/Pessimal.app]
#
# The default is ${PESSIMAL_MACOS_BUILD_DIR:-.build/macos}/Pessimal.app, the same place the build
# script assembles into. Runs in the `:apple: macOS menu bar app` Buildkite step, after the bundle is
# built, inside the guest's rsync'd checkout; it needs no .git.
#
# The app is assembled by a script rather than an Xcode project, so nothing else notices when the
# bundle is subtly wrong -- it still launches on the machine that built it. Four assertions, each for
# a failure that looks like success there:
#
#   * the executable links the pessimal_ffi archive, not the dylib. A dynamically linked app runs
#     only on the machine that built it, and only until the next `cargo clean`;
#   * Info.plist carries no unsubstituted __PESSIMAL_ placeholder. Searched in the XML form, so a
#     binary plist cannot hide one;
#   * Info.plist lints;
#   * LSUIElement is true. Without it the menu bar app also shows a Dock icon.
#
# Every assertion runs and reports, so one red build names every problem rather than the first.
set -euo pipefail

guest_image="ci-macos-rust-bazel-ios-20260910-v2"
plist_buddy="/usr/libexec/PlistBuddy"

die() {
  printf 'error: %s\n' "$1" >&2
  exit "${2:-1}"
}

usage() {
  awk 'NR > 1 && /^#/ { sub(/^# ?/, ""); print; next } NR > 1 { exit }' "$0"
}

bundle=""
while [ $# -gt 0 ]; do
  case "$1" in
    -h|--help) usage; exit 0 ;;
    -*) die "unknown option $1 (see --help)" 2 ;;
    *)
      [ -z "$bundle" ] || die "expected at most one bundle path, got '$bundle' and '$1'" 2
      bundle="$1"
      shift
      ;;
  esac
done

[ "$(uname -s)" = "Darwin" ] || die "this inspects a macOS bundle with otool and plutil; run it on macOS" 2
for tool in otool plutil grep awk; do
  command -v "$tool" >/dev/null 2>&1 \
    || die "$tool is not on PATH; the $guest_image guest image is expected to provide it" 2
done
[ -x "$plist_buddy" ] || die "$plist_buddy is missing; the $guest_image guest image is expected to provide it" 2

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [ -z "$bundle" ]; then
  build_dir="${PESSIMAL_MACOS_BUILD_DIR:-$root/.build/macos}"
  bundle="$build_dir/Pessimal.app"
fi

[ -d "$bundle" ] || die "no bundle at $bundle; run scripts/build-macos-app.sh first"
binary="$bundle/Contents/MacOS/Pessimal"
plist="$bundle/Contents/Info.plist"
[ -f "$binary" ] || die "no executable in the bundle: $binary"
[ -x "$binary" ] || die "the bundle's executable is not executable: $binary"
[ -f "$plist" ] || die "no Info.plist in the bundle: $plist"

echo "checking $bundle"
failures=0

fail() {
  printf 'error: %s\n' "$1" >&2
  failures=$((failures + 1))
}

# 1. Statically linked. The load commands are captured before searching them: piping otool straight
#    into `grep -q` under pipefail can report a SIGPIPE'd otool as "no match", which here would read
#    as a pass. And "no pessimal_ffi" only means something if otool actually listed load commands:
#    measured, `otool -L` on a file that is not Mach-O prints "is not an object file" and exits 0, so
#    at least one tab-indented dependency line is required before the absence counts.
tab="$(printf '\t')"
if ! load_commands="$(otool -L "$binary" 2>&1)"; then
  fail "otool could not read the load commands of $binary"
  printf '%s\n' "$load_commands" >&2
elif ! printf '%s\n' "$load_commands" | grep -q "^$tab"; then
  fail "otool listed no load commands for $binary; it is not a Mach-O executable"
  printf '%s\n' "$load_commands" >&2
elif printf '%s\n' "$load_commands" | grep -q pessimal_ffi; then
  fail "the app links libpessimal_ffi dynamically; it must link the archive"
  printf '%s\n' "$load_commands" >&2
else
  echo "ok: the executable does not load libpessimal_ffi dynamically"
fi

# 2. No unsubstituted placeholders. The XML conversion fails on a broken plist, which assertion 3
#    reports; the raw bytes are searched instead in that case, since a placeholder is ASCII either way.
if plist_text="$(plutil -convert xml1 -o - "$plist" 2>/dev/null)"; then
  placeholders="$(printf '%s\n' "$plist_text" | grep -n '__PESSIMAL_' || true)"
else
  placeholders="$(grep -a -n '__PESSIMAL_' "$plist" || true)"
fi
if [ -n "$placeholders" ]; then
  fail "Info.plist still contains unsubstituted placeholders"
  printf '%s\n' "$placeholders" >&2
else
  echo "ok: Info.plist has no __PESSIMAL_ placeholders"
fi

# 3. The plist lints.
if lint="$(plutil -lint "$plist" 2>&1)"; then
  echo "ok: $lint"
else
  fail "Info.plist does not lint"
  printf '%s\n' "$lint" >&2
fi

# 4. LSUIElement is exactly true. PlistBuddy exits non-zero when the key is absent; that is the same
#    finding as a false value, so its output is compared rather than its status trusted.
ui_element="$("$plist_buddy" -c 'Print :LSUIElement' "$plist" 2>&1 || true)"
if [ "$ui_element" = "true" ]; then
  echo "ok: LSUIElement is true"
else
  fail "LSUIElement is not set; this would show a Dock icon (PlistBuddy says: $ui_element)"
fi

if [ "$failures" -ne 0 ]; then
  die "$failures of 4 bundle assertions failed for $bundle"
fi
echo "all 4 bundle assertions passed for $bundle"
