#!/usr/bin/env bash
# Checks that every commit a pull request adds is a conventional commit, with a pinned cocogitto.
#
# Usage: scripts/ci-check-commits.sh [--base REV]
#
#   --base REV   check REV..HEAD against a revision already in this repository, instead of fetching
#                main from GitHub. For running it by hand: `--base origin/main` needs no network and
#                writes nothing into .git.
#
# WHERE IT MUST RUN: in a checkout that still has its .git, because it reads history. On Buildkite that
# is the shared checkout the tart-ci plugin mounts into the guest, so in `:linux: Rust workspace` it runs
# BEFORE the step rsyncs into its work directory -- that rsync excludes .git, and a copy of this script
# run from there fails, saying so. Pull-request builds only (`build.pull_request.id != null`): a push to
# main has no pull request range to check.
#
# The range is main..HEAD: what this pull request adds, not what main has accumulated. Not
# `cog check --from-latest-tag`, which measured on 2026-09-14 fails outright here with "unable to get
# any tag" -- the one tag, pessimal-ios-v0.1.0, does not carry cog.toml's tag_prefix "v" -- and would
# in any case hold a pull request to account for commits it did not write.
#
# main is read from GitHub rather than trusted from the checkout: the agent's origin/main is whatever
# it last fetched, and a stale base widens the range to main's commits. Anonymously, over HTTPS, because
# the guest holds no SSH key and the repository is public. When the checkout already has that commit
# nothing is fetched at all; otherwise the fetch adds main's objects and FETCH_HEAD to the checkout.
#
# git and cog run with a throwaway HOME whose only configuration trusts every repository. The shared
# checkout belongs to the host's agent user, the guest runs as another, and both git and the libgit2
# that cog links refuse a repository owned by someone else unless `safe.directory` says otherwise. Older
# git reads that setting only from global configuration, and libgit2 never reads it from `-c`, so a
# private HOME is the one place that works for both without editing anybody's real global config.
# Whether the guest mount actually trips the ownership check was not measured.
#
# cocogitto 7.0.0, the version cog.toml's hooks were measured with. cocogitto publishes no checksum
# file, so each digest is the `digest` GitHub records for the release asset, read with
# `gh api repos/cocogitto/cocogitto/releases/tags/7.0.0` on 2026-09-14 and confirmed by hashing the
# downloaded bytes. The darwin entry exists so the check runs on a Mac too.
set -euo pipefail

guest_image="ci-linux-arm64-rust-bazel"
cog_version="7.0.0"
repo_url="https://github.com/Lightless-Labs/pessimal.git"

die() {
  printf 'error: %s\n' "$1" >&2
  exit "${2:-1}"
}

usage() {
  awk 'NR > 1 && /^#/ { sub(/^# ?/, ""); print; next } NR > 1 { exit }' "$0"
}

base_rev=""
while [ $# -gt 0 ]; do
  case "$1" in
    --base)
      [ $# -ge 2 ] && [ -n "$2" ] || die "--base needs a revision" 2
      base_rev="$2"
      shift 2
      ;;
    -h|--help) usage; exit 0 ;;
    *) die "unexpected argument $1 (see --help)" 2 ;;
  esac
done

platform="$(uname -s)-$(uname -m)"
case "$platform" in
  Linux-aarch64|Linux-arm64)
    cog_triple="aarch64-unknown-linux-gnu"
    cog_sha256="379949f50c4f8245ef9532913e83f351c048b55e40ebd52ed8a850642c07a753"
    ;;
  Darwin-arm64)
    cog_triple="aarch64-apple-darwin"
    cog_sha256="429303b6d68c6461aa221ab4de82b26d0bbcdee7127947ea9c7c411ab9bc574c"
    ;;
  *)
    die "no pinned cocogitto for $platform; this runs on the ci-linux-arm64 queue ($guest_image)" 2
    ;;
esac

for tool in git curl tar awk mktemp; do
  command -v "$tool" >/dev/null 2>&1 \
    || die "$tool is not on PATH; the $guest_image guest image is expected to provide it" 2
done
if command -v sha256sum >/dev/null 2>&1; then
  sha256_of() { sha256sum "$1" | awk '{ print $1 }'; }
elif command -v shasum >/dev/null 2>&1; then
  sha256_of() { shasum -a 256 "$1" | awk '{ print $1 }'; }
else
  die "neither sha256sum nor shasum is on PATH; the $guest_image guest image is expected to provide sha256sum" 2
fi

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [ ! -e "$root/.git" ]; then
  die "no .git in $root, and this check reads history. On Buildkite, run it from the shared checkout before the step rsyncs into its work directory: that rsync excludes .git."
fi
cd "$root"

work="$(mktemp -d "${TMPDIR:-/tmp}/ci-check-commits.XXXXXX")"
trap 'rm -rf "$work"' EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

# `*` for git 2.36+ and libgit2; the checkout's own paths as well, for a git that predates `*`.
mkdir -p "$work/home"
printf '[safe]\n\tdirectory = *\n\tdirectory = "%s"\n\tdirectory = "%s"\n' \
  "$root" "$(pwd -P)" > "$work/home/.gitconfig"

run_git() {
  HOME="$work/home" GIT_TERMINAL_PROMPT=0 git "$@"
}

if ! head_error="$(run_git rev-parse --verify 'HEAD^{commit}' 2>&1 >/dev/null)"; then
  printf '%s\n' "$head_error" >&2
  die "git cannot read HEAD in $root"
fi
head_sha="$(run_git rev-parse --verify 'HEAD^{commit}')"
if [ "$(run_git rev-parse --is-shallow-repository)" = "true" ]; then
  die "$root is a shallow clone, so main..HEAD may stop short of the real history. Check out without --depth."
fi

if [ -n "$base_rev" ]; then
  base_sha="$(run_git rev-parse --verify --quiet "${base_rev}^{commit}")" \
    || die "--base $base_rev does not name a commit in $root"
  base_label="$base_rev, as this repository already has it"
else
  echo "reading main from $repo_url"
  base_sha="$(run_git ls-remote "$repo_url" refs/heads/main | awk '{ print $1 }')" \
    || die "could not reach $repo_url to read main"
  [ -n "$base_sha" ] || die "$repo_url has no refs/heads/main"
  if ! run_git cat-file -e "${base_sha}^{commit}" 2>/dev/null; then
    echo "fetching main ($base_sha) anonymously"
    run_git fetch --no-tags --quiet "$repo_url" refs/heads/main \
      || die "could not fetch main from $repo_url into $root"
    run_git cat-file -e "${base_sha}^{commit}" 2>/dev/null \
      || die "main moved while it was being fetched and $base_sha is still missing; re-run"
  fi
  base_label="main on GitHub"
fi

archive="cocogitto-${cog_version}-${cog_triple}.tar.gz"
url="https://github.com/cocogitto/cocogitto/releases/download/${cog_version}/${archive}"
curl --proto '=https' --tlsv1.2 -fsSL --retry 3 -o "$work/$archive" "$url"
actual_sha256="$(sha256_of "$work/$archive")"
if [ "$actual_sha256" != "$cog_sha256" ]; then
  die "sha256 mismatch for $archive: expected $cog_sha256, got $actual_sha256. Not extracting it."
fi
tar -xzf "$work/$archive" -C "$work" "$cog_triple/cog"
cog="$work/$cog_triple/cog"
[ -x "$cog" ] || die "the $archive tarball held no executable $cog_triple/cog"
echo "cocogitto: $("$cog" --version), $archive sha256 $actual_sha256"

range="$base_sha..$head_sha"
total="$(run_git rev-list --count "$range")"
merges="$(run_git rev-list --count --merges "$range")"
echo "base: $base_sha ($base_label)"
echo "head: $head_sha"
if [ "$total" -eq 0 ]; then
  echo "0 commits in base..head: HEAD is already contained in the base, so there is nothing to check."
else
  echo "$total commit(s) in base..head, $merges of them merges (cog.toml decides whether merges are checked):"
  run_git log --format='  %h %s' "$range"
fi

HOME="$work/home" "$cog" check "$range"
echo "ok: cog check passed for the $total commit(s) in ${base_sha:0:12}..${head_sha:0:12}"
