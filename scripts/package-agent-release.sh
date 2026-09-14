#!/usr/bin/env bash
# Builds one finished release archive from one already-built binary, and then proves it.
#
# Usage:
#   scripts/package-agent-release.sh --binary PATH --triple TRIPLE --version VERSION --out DIR
#
# Prints the absolute path of the archive on stdout and nothing else; every diagnostic and every
# assertion goes to stderr, so a caller can do `archive=$(package-agent-release.sh ...)`. Also writes
# `<archive>.sha256` beside it, for `release-publish`'s transport check.
#
# This is the ONLY place in the release that creates an agent archive. scripts/release-build-linux.sh
# calls it in the `release-linux` Buildkite step, scripts/release-macos-artifacts.sh calls it in
# `release-macos` once the slices are signed, and `release-publish` never repackages anything -- it
# uploads what this wrote. That matters because two packagers is two layouts, and the layout is the
# part users cannot work around.
#
# The archive name is not computed here. It comes from scripts/release-manifest.sh, which is the one
# place the asset names are spelled, because mise and ubi resolve an asset by matching its *filename*
# against the host triple: a name this script invented independently could differ from the name
# `release-publish` expects by one character and the symptom would be "no release found".
#
# Why the layout is exactly one top-level directory and no top-level files: mise applies
# `strip_components = 1` automatically when an archive has a single root directory, and ubi locates
# the binary by basename at any depth. Add a top-level README next to the directory and mise stops
# stripping, so the binary installs one path level too deep.
#
# Why the modes are set explicitly rather than inherited: ubi's partial match for an executable is
# "basename starts with the exe stem AND has the executable bit set", and both
# `pessimal.example.toml` and `pessimal-agent.service` start with a stem that qualifies. Ship either
# with an executable bit and ubi may install a TOML file as the agent. 0644 here is a functional
# requirement, not tidiness.
#
# Every shipped triple gets a `.tar.gz`. There is no `.zip` branch: Windows is deferred from the
# first release (see scripts/release-manifest.sh), and an archive branch nothing can exercise is a
# branch nobody notices breaking.
set -euo pipefail

# COPYFILE_DISABLE is Apple tar's switch for packing file metadata as AppleDouble `._name` members.
# Those are junk in a tarball, they are extra top-level-adjacent entries that confuse
# `strip_components`, and on Linux they extract as visible garbage. Exported rather than set per
# invocation so no tar call in this script can forget it. It is not the whole story on its own --
# the same tar still carries xattrs as pax headers, and its listing hides `._` members -- which is
# why the staged files are stripped and the archive is re-read below with something that isn't tar.
export COPYFILE_DISABLE=1

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

say() { printf '  %s\n' "$1" >&2; }
die() { printf 'package-agent-release: %s\n' "$1" >&2; exit 1; }

# Every tool, checked before anything is staged, so a guest image that lacks one fails in the first
# second naming it rather than halfway through an archive. The image names are the ones the release
# steps run in; their contents were never inventoried, which is exactly why this is checked.
case "$(uname -s)" in
  Darwin) host_image="ci-macos-rust-bazel-ios-20260910-v2 (or this Mac)" ;;
  *)      host_image="ci-linux-arm64-rust-bazel (or this host)" ;;
esac
need() {
  command -v "$1" >/dev/null 2>&1 \
    || die "$1 is required and is not on PATH in $host_image${2:+ -- $2}"
}
need tar
need gzip "the extended-attribute check reads the compressed stream"
need install
need mktemp
need awk
need grep
if ! command -v sha256sum >/dev/null 2>&1 && ! command -v shasum >/dev/null 2>&1; then
  die "neither sha256sum nor shasum is on PATH in $host_image; cannot emit the transport hash"
fi
if [ "$(uname -s)" = "Darwin" ]; then
  need codesign "an *-apple-darwin input's signature is checked before it is packaged"
  need xattr "staged files must be stripped of quarantine and other extended attributes"
  # /usr/bin/python3 on macOS is a stub that exists with or without an interpreter behind it, so it is
  # asked to run rather than asked whether it is on PATH.
  python3 -c 'import tarfile' >/dev/null 2>&1 \
    || die "a working python3 is required in $host_image -- bsdtar's own listing hides ._ members, so the archive is re-read with Python"
fi

binary="" triple="" version="" out=""
while [ $# -gt 0 ]; do
  case "$1" in
    --binary)  binary="${2:?--binary needs a path}"; shift 2 ;;
    --triple)  triple="${2:?--triple needs a target triple}"; shift 2 ;;
    --version) version="${2:?--version needs a version}"; shift 2 ;;
    --out)     out="${2:?--out needs a directory}"; shift 2 ;;
    -h|--help) sed -n '2,9p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) die "unknown argument $1" ;;
  esac
done

[ -n "$binary" ]  || die "--binary is required"
[ -n "$triple" ]  || die "--triple is required"
[ -n "$version" ] || die "--version is required"
[ -n "$out" ]     || die "--out is required"
[ -f "$binary" ]  || die "no such binary: $binary"

# release-manifest.sh validates the version and rejects an unknown (or deferred) triple, so both
# checks happen once, there, rather than drifting into a second copy here.
asset="$("$repo_root/scripts/release-manifest.sh" "$version" triple "$triple")"
top="pessimal-agent-$version-$triple"
exe="pessimal-agent"

# The cross-built x86_64 Mach-O arrives from the linker with no signature at all (measured --
# `codesign -dvvv` says "code object is not signed at all"), unlike the native arm64 one, which the
# linker ad-hoc signs. Packaging it before it has been signed is the mistake this catches, and it is
# a quiet one: Intel macOS runs unsigned code, so the slice still works from `curl | tar` and under
# mise/ubi, and only a user who downloads through a browser finds Gatekeeper refusing an
# un-notarized binary. A green build and a download that fails for some users is the worst shape of
# failure, so it stops here. (This checks for *a* valid signature, which the native arm64 slice has
# from the linker; the Developer ID itself is asserted by scripts/release-macos-artifacts.sh.)
case "$triple" in
  *-apple-darwin)
    [ "$(uname -s)" = "Darwin" ] \
      || die "an *-apple-darwin archive can only be built on macOS (its signature is checked with codesign)"
    if codesign --verify --strict "$binary" 2>/dev/null; then
      say "signature: $binary passes codesign --verify --strict"
    elif [ "${PESSIMAL_PACKAGE_ALLOW_UNSIGNED:-}" = "1" ]; then
      # Loud, never silent. The escape hatch exists so this script can be tried locally against an
      # unsigned build (target/x86_64-apple-darwin is one); it must never look like a clean run.
      # scripts/release-macos-artifacts.sh unsets it, so the release path cannot inherit it.
      printf 'package-agent-release: WARNING: %s is NOT validly signed, packaging anyway because\n' "$binary" >&2
      printf '  PESSIMAL_PACKAGE_ALLOW_UNSIGNED=1 is set. A release asset built this way is not\n' >&2
      printf '  notarized, and Gatekeeper will refuse it after a browser download. codesign said:\n' >&2
      codesign --verify --strict "$binary" 2>&1 | sed 's/^/    /' >&2 || true
    else
      codesign --verify --strict "$binary" 2>&1 | sed 's/^/  /' >&2 || true
      die "$binary is not validly signed; sign it first, or set PESSIMAL_PACKAGE_ALLOW_UNSIGNED=1 to try this script locally on an unsigned build"
    fi
    ;;
esac

staging="$(mktemp -d)"
# INT/TERM exit explicitly so the EXIT trap runs once and the script does not carry on afterwards.
trap 'rm -rf "$staging"' EXIT
trap 'exit 1' INT TERM
mkdir -p "$out"
out="$(cd "$out" && pwd)"

root="$staging/$top"
mkdir -p "$root"
# The top-level directory's own mode, which tar records: 0755, because a 0644 directory cannot be
# entered. "0644 everything else" is about the regular files.
chmod 0755 "$root"

install -m 0755 "$binary" "$root/$exe"
install -m 0644 "$repo_root/LICENSE" "$root/LICENSE"
install -m 0644 "$repo_root/pessimal.example.toml" "$root/pessimal.example.toml"
install -m 0644 "$repo_root/packaging/tarball/README.md" "$root/README.md"

# The service unit only belongs where there is a systemd to read it. Shipping it on darwin would
# also hand ubi a second candidate whose basename starts with the exe stem.
expected="$exe LICENSE README.md pessimal.example.toml"
case "$triple" in
  *-linux-gnu)
    install -m 0644 "$repo_root/packaging/systemd/pessimal-agent.service" "$root/pessimal-agent.service"
    expected="$expected pessimal-agent.service"
    ;;
esac

# COPYFILE_DISABLE stops the `._` sidecars but not the other way macOS bsdtar carries metadata:
# measured, a staged file carrying com.apple.quarantine still reaches the .tar.gz as
# LIBARCHIVE.xattr/SCHILY.xattr pax headers with COPYFILE_DISABLE=1 set. `install` preserves xattrs,
# so a quarantined input binary would ship its quarantine inside the archive, and any extractor that
# restores xattrs would re-quarantine it on a channel packaging/macos/GATEKEEPER.md promises never
# sets the flag. GNU tar stores no xattrs without --xattrs, so this is macOS-only.
if [ "$(uname -s)" = "Darwin" ]; then
  xattr -cr "$root" || die "could not clear extended attributes from the staged files in $root"
fi

archive="$out/$asset"
rm -f "$archive" "$archive.sha256"

# Only -czf/-C/-tzvf, which GNU tar in the Linux guest and bsdtar on macOS agree on.
case "$asset" in
  *.tar.gz) tar -czf "$archive" -C "$staging" "$top" ;;
  *) die "don't know how to build an archive named $asset" ;;
esac

# ---- self-assertions: everything below must hold, or nothing is published ----------------------

[ -s "$archive" ] || die "the archive is empty or absent: $archive"
say "non-empty: $asset is $(wc -c < "$archive" | tr -d ' ') bytes"

# The listing is one "mode name" line per member. On macOS it does NOT come from tar: Apple's bsdtar
# hides `._` members from `tar -tv` even with COPYFILE_DISABLE=1 (measured -- an archive holding
# `._LICENSE` listed without it, while Python's tarfile saw it), so every assertion below would pass
# blind on the one platform that produces sidecars. Python's tarfile reads the stream with no
# AppleDouble logic. On Linux, GNU tar's own listing has no such filter and is used as-is: mode is the
# first field and the name the last, hence $1 and $NF.
listing="$staging/listing"
if [ "$(uname -s)" = "Darwin" ]; then
  python3 - "$archive" > "$listing" <<'PY'
import stat
import sys
import tarfile

with tarfile.open(sys.argv[1], "r:gz") as archive:
    for member in archive.getmembers():
        kind = stat.S_IFDIR if member.isdir() else stat.S_IFREG if member.isfile() else 0
        suffix = "/" if member.isdir() else ""
        print(stat.filemode(kind | member.mode), member.name + suffix)
PY
else
  tar -tzvf "$archive" | awk '{ print $1, $NF }' > "$listing"
fi

top_dirs=0 top_files=0 members=0 problems=""
while read -r mode name; do
  [ -n "$name" ] || continue
  bare="${name%/}"
  case "$(basename "$bare")" in
    ._*) problems="$problems\n  AppleDouble sidecar in the archive: $name" ;;
  esac
  case "$name" in
    *\\*) problems="$problems\n  member name contains a backslash: $name" ;;
  esac

  depth="$(printf '%s' "$bare" | tr -cd '/' | wc -c | tr -d ' ')"
  if [ "$depth" = "0" ]; then
    case "$mode" in
      d*) top_dirs=$((top_dirs + 1))
          [ "$bare" = "$top" ] || problems="$problems\n  the top-level directory is '$bare', expected '$top'" ;;
      *)  top_files=$((top_files + 1))
          problems="$problems\n  top-level file '$name' would stop mise applying strip_components=1" ;;
    esac
    continue
  fi
  [ "$depth" = "1" ] || problems="$problems\n  member nested deeper than one level: $name"

  # A subdirectory inside the top level is neither a 0755 binary nor a 0644 data file, and saying
  # so plainly beats reporting it as a file with the wrong mode.
  case "$mode" in
    d*) problems="$problems\n  unexpected directory inside the archive: $name (the layout is flat under $top/)"
        continue ;;
  esac
  members=$((members + 1))

  # Exact mode strings rather than a numeric conversion: these are what a user sees in `tar tvf`,
  # and the failure message is readable because of it.
  file="$(basename "$bare")"
  case " $expected " in
    *" $file "*) ;;
    *) problems="$problems\n  unexpected file in the archive: $name" ;;
  esac
  if [ "$file" = "$exe" ]; then
    [ "$mode" = "-rwxr-xr-x" ] || problems="$problems\n  the binary is $mode, expected -rwxr-xr-x (0755)"
  else
    [ "$mode" = "-rw-r--r--" ] || problems="$problems\n  $file is $mode, expected -rw-r--r-- (0644); an executable bit here makes it an ubi candidate"
  fi
done < "$listing"

[ "$top_dirs" = "1" ] || problems="$problems\n  found $top_dirs top-level directories, expected exactly 1"
[ "$top_files" = "0" ] || problems="$problems\n  found $top_files top-level files, expected 0"

# shellcheck disable=SC2086  # $expected is this script's own space-separated list of bare filenames
want_count="$(printf '%s\n' $expected | wc -l | tr -d ' ')"
[ "$members" = "$want_count" ] \
  || problems="$problems\n  the archive holds $members files, expected $want_count ($expected)"

# Name-for-name, not just a count: a missing LICENSE and an extra stray file cancel out in a count.
for want in $expected; do
  grep -q " $top/$want\$" "$listing" || problems="$problems\n  missing from the archive: $want"
done

# Extended attributes never show in a -tv listing, so read the pax headers themselves. grep -c
# rather than -q: -q exits at the first match, gzip takes SIGPIPE, and under pipefail the pipeline
# would report failure for exactly the case it found.
xattr_headers="$(gzip -dc "$archive" | LC_ALL=C grep -ac -e 'LIBARCHIVE\.xattr\.' -e 'SCHILY\.xattr\.' || true)"
[ "${xattr_headers:-0}" = "0" ] \
  || problems="$problems\n  the archive carries extended-attribute pax headers ($xattr_headers); a quarantine flag may be riding inside it"

if [ -n "$problems" ]; then
  printf 'package-agent-release: %s is not a valid release archive:' "$asset" >&2
  # shellcheck disable=SC2059  # $problems is this script's own assembled message, not a format string
  printf "$problems\n" >&2
  exit 1
fi

say "layout: 1 top-level directory ($top/), 0 top-level files, $members member(s)"
say "modes: $exe at 0755, $((members - 1)) other file(s) at 0644"
say "contents: $expected"
say "clean: no AppleDouble sidecar, no backslash in any member name, no xattr pax headers"

# The transport check `release-publish` makes before it trusts a downloaded Buildkite artifact.
# `sha256sum` and `shasum -a 256` emit the same two-space format; prefer sha256sum because a minimal
# Linux image may have coreutils but not perl's Digest::SHA. The `cd` is what keeps the filename bare
# -- a path component here would make the file unusable with `shasum -c` from anywhere else.
if command -v sha256sum >/dev/null 2>&1; then
  ( cd "$out" && sha256sum "$asset" > "$asset.sha256" )
else
  ( cd "$out" && shasum -a 256 "$asset" > "$asset.sha256" )
fi
say "hash: $(cut -c1-16 < "$archive.sha256")... in $asset.sha256"

printf '%s\n' "$archive"
