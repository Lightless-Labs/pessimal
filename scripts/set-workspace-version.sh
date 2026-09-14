#!/usr/bin/env bash
# Moves the [workspace.package] version in Cargo.toml to the version being released.
#
# Usage: scripts/set-workspace-version.sh <semver>      # e.g. 0.2.0, never v0.2.0
#
# This is cog's pre_bump_hook. Nothing kept the git tag and the manifest in agreement before it:
# `cog bump` writes a tag and a changelog, and `.buildkite/pipeline.yml` then reads the marketing
# version out of [workspace.package] -- two numbers with no mechanism between them. The
# `release-guard` Buildkite step (scripts/release-guard.sh) refuses a tag whose digits do not equal
# the manifest's, so a bump that forgets this step now fails at the release, before anything is
# built, rather than shipping a mislabelled binary.
#
# Everything here is about blast radius. Cargo.toml carries a dozen other `version = "..."` lines
# -- `rust-version`, and every dependency in [workspace.dependencies] -- and a `sed -i` that
# rewrites the wrong one changes what the whole workspace compiles against while still printing a
# plausible version. So the edit is scoped to the [workspace.package] table by tracking table
# headers (the same discipline as scripts/build-macos-app.sh:120-131), and the rewrite is asserted
# to change nothing but the version pair before it is allowed to replace the manifest.
#
# The read-back deliberately uses the awk from .buildkite/pipeline.yml's ios-testflight step character
# for character, and scripts/release-guard.sh carries the same transcription: the parser that proves
# the write landed is the parser that consumes the value downstream, so they cannot drift. Two
# different-but-equivalent parsers would be two chances to disagree silently.
set -euo pipefail

die() { printf 'error: %s\n' "$1" >&2; exit 1; }

# Every tool, before anything is touched. This runs where `cog bump` runs -- a maintainer's machine,
# not a guest image -- so the message names the tool and the reason rather than an image.
need() { command -v "$1" >/dev/null 2>&1 || die "$1 is required on this machine and is not on PATH -- $2"; }
need git "the hook checks the work tree and stages its own edit"
need awk "the [workspace.package] table is rewritten and read back with awk"
need diff "the rewrite is asserted to change only the version line"
need mktemp "the rewrite is staged in a temporary file first"
need cargo "Cargo.lock must move with the version, because every release build passes --locked"

version="${1:-}"
[[ -n "$version" ]] || die "usage: scripts/set-workspace-version.sh <semver>"

# cog expands {{version}} without the tag_prefix, so the hook never sees a leading `v`. A hand run
# easily does, and `version = "v0.2.0"` is a manifest cargo rejects -- name the mistake rather than
# writing it.
[[ "$version" != v* ]] || die "version must not carry the tag prefix: got '$version', want '${version#v}'"
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "not a release semver: '$version' (want MAJOR.MINOR.PATCH)"

# Two steps, not `cd "$(git rev-parse --show-toplevel)" || die`: a failed substitution hands `cd` an
# empty string, which bash treats as a successful no-op (measured with bash 3.2), so that `die` could
# never fire and the script would carry on in whatever directory it was started from.
top="$(git rev-parse --show-toplevel 2>/dev/null)" || die "not inside a git work tree"
cd "$top"

# The hook runs inside cog's commit sequence, and cog commits whatever the work tree holds when the
# hook finishes. Refusing unrelated dirt is what stops a half-finished edit from being tagged as a
# release. cocogitto 7.0.0 already refuses a dirty tree before it starts (measured), so under cog
# this is belt and braces; it is load-bearing for a hand run, and for any cog that stops checking.
#
# CHANGELOG.md is on the allowlist because cog writes it *before* pre_bump_hooks run -- measured
# 2026-09-13 with cocogitto 7.0.0, where the hook's first `git status --porcelain` read
# `?? CHANGELOG.md`. Refusing it would refuse every real bump while `cog bump --dry-run` -- which
# returns before the changelog and the hooks -- stayed green.
#
# The status is captured first rather than read from `< <(git status --porcelain)`: a failure inside
# a process substitution is invisible to `set -e`, and an unreadable status would look like a clean
# tree.
status="$(git status --porcelain)" || die "git status failed; cannot tell whether the work tree is clean"
while IFS= read -r entry; do
  [[ -n "$entry" ]] || continue
  path="${entry:3}"
  case "$path" in
    Cargo.toml|Cargo.lock|CHANGELOG.md) ;;
    *) die "work tree has unrelated changes ($path); commit or stash before bumping" ;;
  esac
done <<< "$status"

[[ -f Cargo.toml ]] || die "no Cargo.toml at the repository root"

# Table-scoped rewrite. `in_package` flips on every table header, so a `version` key under
# [workspace.dependencies] -- or under any future table -- is structurally unreachable. `rewrote`
# stops at the first match so a malformed manifest with two [workspace.package] tables cannot have
# both rewritten. The replacement is interpolated into an awk sub(), where `&` would mean "the
# matched text"; the semver check above is what makes that safe.
tmp="$(mktemp "${TMPDIR:-/tmp}/set-workspace-version.XXXXXX")"
# EXIT owns the cleanup; INT and TERM only turn into an exit. A handler on INT that merely removed the
# file would return into the script and carry on rewriting the manifest after a Ctrl-C.
trap 'rm -f "$tmp"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

awk -v new="$version" '
  /^[[:space:]]*\[/ { in_package = ($0 ~ /^[[:space:]]*\[workspace\.package\][[:space:]]*$/) }
  !rewrote && in_package && /^[[:space:]]*version[[:space:]]*=/ {
    sub(/=.*/, "= \"" new "\"")
    rewrote = 1
  }
  { print }
  END { if (!rewrote) exit 3 }
' Cargo.toml > "$tmp" || die "no version key inside [workspace.package] in Cargo.toml"

[[ -s "$tmp" ]] || die "rewrite produced an empty Cargo.toml; refusing to install it"

# Blast-radius assertion, made against the rewrite itself and before it is installed, so a failure
# leaves Cargo.toml exactly as it was. Diffing the candidate against the current file rather than
# against the index measures what *this run* changed, independent of whatever a previous, aborted
# bump left staged. Zero changed lines -- a re-release of the version already in the manifest is a
# legitimate no-op, and is what the first `cog bump --auto` produces -- or exactly one removed
# `version = ...` and one added `version = "<new>"`, and nothing else.
#
# The count is the point. Filtering out "version lines" and demanding the remainder be empty would
# pass the likeliest scoping failure of all: some other table's `version = "..."` rewritten too,
# which is a version line on both sides of the diff.
changes="$(diff -U0 Cargo.toml "$tmp" | grep -E '^[+-][^+-]' || true)"
if [[ -n "$changes" ]]; then
  removed="$(printf '%s\n' "$changes" | grep -c '^-' || true)"
  added="$(printf '%s\n' "$changes" | grep -c '^+' || true)"
  flat="$(printf '%s' "$changes" | tr '\n' ' ')"
  [[ "$removed" == 1 && "$added" == 1 ]] ||
    die "the rewrite changed $removed line(s) out and $added in, want 1 and 1: $flat"
  printf '%s\n' "$changes" | grep -qE '^-[[:space:]]*version[[:space:]]*=' ||
    die "the rewrite removed something other than a version line: $flat"
  # The value is compared as a plain string, not a regex: escaping its dots would mean relying on
  # how backslashes survive `${var//pattern/replacement}`, which bash has changed across releases
  # (5.2's patsub_replacement), and `/usr/bin/env bash` on a stock Mac is still 3.2.
  added_line="$(printf '%s\n' "$changes" | grep '^+')"
  [[ "$added_line" =~ ^\+[[:space:]]*version[[:space:]]*= && "${added_line#*=}" == " \"$version\"" ]] ||
    die "the rewrite added something other than version = \"$version\": $flat"
fi

# Copy the bytes rather than `mv` the temp file over Cargo.toml: mktemp creates 0600, and moving it
# into place would silently take the manifest's mode with it.
cat "$tmp" > Cargo.toml

# Every build in CI passes --locked, so a Cargo.lock whose member entries still carry the old
# version fails the build rather than the bump. --workspace restricts the update to this
# workspace's own packages (no dependency churn sneaking into a release commit) and --offline keeps
# a version bump from depending on the network.
cargo update --workspace --offline

# The verifying parser is the ios-testflight step's in .buildkite/pipeline.yml, transcribed with
# Buildkite's `$$` escapes resolved to `$`. Do not "improve" it here without changing it there, and in
# scripts/release-guard.sh.
read_back="$(awk '/^\[workspace\.package\]/{f=1; next} /^\[/{f=0} f && /^version[[:space:]]*=/{gsub(/["[:space:]]/,"",$0); sub(/^version=/,"",$0); print; exit}' Cargo.toml)"
[[ "$read_back" == "$version" ]] || die "read-back disagrees: Cargo.toml says '$read_back', expected '$version'"

# Staging is the hook's own job. cocogitto 7.0.0 does stage what a pre-bump hook touched -- measured
# 2026-09-13 in a scratch clone with this line removed, the bump commit still carried both files --
# but that is observed behaviour, not documented contract, and nothing in this repository pins the
# cog a maintainer bumps with. If it ever stopped being true the failure would be silent: the tag
# would carry the new version and the commit the old one. So this script makes itself sufficient
# instead of depending on it.
git add Cargo.toml Cargo.lock

echo "[workspace.package] version = $read_back (Cargo.toml and Cargo.lock staged)"
