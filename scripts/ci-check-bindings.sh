#!/usr/bin/env bash
# Fails when the committed Swift bindings are not exactly what tools/uniffi/regen.sh generates from the
# Rust source as it stands.
#
# Usage: scripts/ci-check-bindings.sh
#
# Runs in the `:apple: macOS menu bar app` Buildkite step, inside the guest's rsync'd checkout. Run it
# BEFORE scripts/build-macos-app.sh: against stale bindings the build fails first, with Swift compile
# errors that do not say the bindings are the cause.
#
# The bindings are build outputs that are committed, so they go stale silently. A stale .swift means a
# Rust signature change has no effect, because the app builds compile the committed file rather than
# the code; a stale header gives link errors that look unrelated. Regenerating and comparing makes a
# forgotten regen fail here rather than at someone's next build.
#
# The comparison is against a copy taken before regenerating, not against git: the guest's checkout is
# an rsync that excludes .git, so there is no index to diff with. And the committed files are removed
# from the bindings directory before regen runs, so regen has to write every one of them back. Were
# they left in place, a regen that silently wrote nothing -- or wrote somewhere else -- would compare
# equal to the copy and pass; this way that shows up as a missing file.
#
# If regen fails, the committed files are put back. If it succeeds and they differ, the regenerated
# files are left in place: on a laptop that is the fix, and a guest is thrown away anyway.
set -euo pipefail

guest_image="ci-macos-rust-bazel-ios-20260910-v2"

die() {
  printf 'error: %s\n' "$1" >&2
  exit "${2:-1}"
}

usage() {
  awk 'NR > 1 && /^#/ { sub(/^# ?/, ""); print; next } NR > 1 { exit }' "$0"
}

while [ $# -gt 0 ]; do
  case "$1" in
    -h|--help) usage; exit 0 ;;
    *) die "unexpected argument $1 (see --help)" 2 ;;
  esac
done

# regen.sh builds pessimal_ffi and the bindgen with cargo before generating anything.
for tool in cargo diff cp rm mktemp awk; do
  command -v "$tool" >/dev/null 2>&1 \
    || die "$tool is not on PATH; the $guest_image guest image is expected to provide it" 2
done

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
bindings_rel="clients/apple/PessimalFFI/Sources"
bindings="$root/$bindings_rel"
regen="$root/tools/uniffi/regen.sh"

[ -x "$regen" ] || die "$regen is missing or not executable"
[ -d "$bindings" ] || die "no committed bindings directory at $bindings"

work="$(mktemp -d "${TMPDIR:-/tmp}/ci-check-bindings.XXXXXX")"
committed="$work/committed"
removed="no"
regenerated="no"

cleanup() {
  if [ "$removed" = "yes" ] && [ "$regenerated" = "no" ]; then
    # Regen never finished, so nothing was compared. Put the committed files back rather than leave a
    # checkout whose bindings directory is half empty.
    cp -Rp "$committed/." "$bindings/" 2>/dev/null \
      || printf 'error: could not restore the committed bindings from %s\n' "$committed" >&2
  fi
  rm -rf "$work"
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

cp -Rp "$bindings" "$committed"

# Hidden files are not bindings (a laptop's .DS_Store, say). They stay in place, so they compare equal.
files=""
for entry in "$committed"/*; do
  [ -e "$entry" ] || continue
  files="$files ${entry##*/}"
done
[ -n "$files" ] || die "$bindings is empty; there are no committed bindings to check"
echo "committed bindings in $bindings_rel:$files"

removed="yes"
for name in $files; do
  rm -rf "${bindings:?}/$name"
done

echo "regenerating with tools/uniffi/regen.sh"
regen_status=0
"$regen" || regen_status=$?
if [ "$regen_status" -ne 0 ]; then
  die "tools/uniffi/regen.sh failed (exit $regen_status); nothing was compared, and the committed bindings are put back"
fi
regenerated="yes"

# A regen that exits 0 without writing a file is a fault in regen, not stale bindings, and must not be
# reported as the latter. Put the committed file back so the checkout is no worse than it was.
missing=""
for name in $files; do
  if [ ! -e "$bindings/$name" ]; then
    missing="$missing $name"
    cp -Rp "$committed/$name" "$bindings/$name"
  fi
done
if [ -n "$missing" ]; then
  die "tools/uniffi/regen.sh exited 0 but did not write:$missing into $bindings_rel. That is a fault in regen or in where it writes, not stale bindings; the committed files are put back"
fi

status=0
diff -ru "$committed" "$bindings" > "$work/bindings.diff" 2>&1 || status=$?

case "$status" in
  0)
    echo "ok: regen wrote back every committed file, byte for byte:$files"
    ;;
  1)
    cat "$work/bindings.diff"
    echo >&2
    printf 'error: the committed Swift bindings do not match the Rust source.\n' >&2
    printf 'Run ./tools/uniffi/regen.sh and commit every file it writes, together:%s\n' "$files" >&2
    printf 'Copying only the .swift gives link errors that look unrelated.\n' >&2
    printf 'The regenerated files are now in %s.\n' "$bindings_rel" >&2
    exit 1
    ;;
  *)
    cat "$work/bindings.diff" >&2
    die "diff could not compare $committed with $bindings (exit $status)"
    ;;
esac
