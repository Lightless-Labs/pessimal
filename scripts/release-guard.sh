#!/usr/bin/env bash
# Decides whether a tag is allowed to become a release, before a single thing is built for it.
#
# Usage:
#   scripts/release-guard.sh                          # the release-guard step: reads BUILDKITE_TAG
#   scripts/release-guard.sh --tag v0.2.0             # by hand
#
# Prints the release version, alone, on stdout; every check and every refusal goes to stderr, so a
# caller can do `version="$(scripts/release-guard.sh)"`.
#
# It refuses unless all three hold, and says which one did not:
#   1. the tag's digits equal [workspace.package] version in Cargo.toml -- otherwise the binary would
#      print one version under a release named for another;
#   2. the tagged commit is an ancestor of main on GitHub -- a v-tag pushed on a side branch must not
#      summon the signing credentials;
#   3. no *published* release exists for the tag -- releases are forward-only, and re-running an old
#      tag must not overwrite what users already downloaded. A draft does not count: release-publish
#      reuses one.
#
# Everything is asked of GitHub anonymously, and that is deliberate rather than a limitation. The step
# holds no secret, and the repository is public, so an anonymous `git fetch` and an anonymous REST GET
# see exactly what a stranger sees -- which is also what the release will be judged against. It works
# in a scratch bare repository rather than the checkout because the release steps run in a Tart guest
# on an rsync'd copy with no `.git`, and because the Buildkite checkout's `origin` may be an SSH URL
# the guest has no key for. The user's and system git config are ignored for the same reason: a
# `url.<ssh>.insteadOf` rewrite on the host would turn an anonymous fetch into a key-requiring one.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# Hard-coded on purpose, like verify-release.sh: the guard must not learn which repository to trust
# from the environment it happens to run in.
REPO="Lightless-Labs/pessimal"
REPO_URL="https://github.com/$REPO.git"
API="https://api.github.com/repos/$REPO"

say() { printf 'release-guard: %s\n' "$1" >&2; }
die() { printf 'release-guard: REFUSED: %s\n' "$1" >&2; exit 1; }

case "$(uname -s)" in
  Darwin) image="this Mac (the release-guard step itself runs in ci-linux-arm64-rust-bazel)" ;;
  *)      image="ci-linux-arm64-rust-bazel (the release-guard step's guest image) or this host" ;;
esac
need() {
  command -v "$1" >/dev/null 2>&1 || die "$1 is required and is not on PATH in $image${2:+ -- $2}"
}
need awk "it reads [workspace.package] version out of Cargo.toml"
need git "the tag and main are fetched anonymously to check ancestry"
need curl "the published-release check is an anonymous REST GET"
need grep "the tag and the version are shape-checked with grep -E"
need mktemp

tag=""
while [ $# -gt 0 ]; do
  case "$1" in
    --tag) tag="${2:?--tag needs a value, e.g. v0.2.0}"; shift 2 ;;
    -h|--help) sed -n '2,9p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) die "unknown argument $1 (see --help)" ;;
  esac
done

buildkite_tag="${BUILDKITE_TAG:-}"
if [ -n "$tag" ] && [ -n "$buildkite_tag" ] && [ "$tag" != "$buildkite_tag" ]; then
  die "--tag $tag disagrees with BUILDKITE_TAG=$buildkite_tag; the guard checks the tag this build is for, and there is only one"
fi
tag="${tag:-$buildkite_tag}"

[ -f "$ROOT_DIR/Cargo.toml" ] || die "no Cargo.toml at $ROOT_DIR; run this from a checkout of the repository"

# The parser is .buildkite/pipeline.yml's (the ios-testflight step's `version=` line), with Buildkite's
# `$$` escapes resolved to `$`, and the same transcription scripts/set-workspace-version.sh verifies
# its own write with. The value this compares against is therefore the value every other consumer
# reads. Do not "improve" it here without changing it there.
version="$(awk '/^\[workspace\.package\]/{f=1; next} /^\[/{f=0} f && /^version[[:space:]]*=/{gsub(/["[:space:]]/,"",$0); sub(/^version=/,"",$0); print; exit}' "$ROOT_DIR/Cargo.toml")"
if ! printf '%s\n' "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$'; then
  die "could not read a MAJOR.MINOR.PATCH version from [workspace.package] in Cargo.toml (got '$version')"
fi

if [ -z "$tag" ]; then
  die "BUILDKITE_TAG is empty and no --tag was given. Either this is not a tag build (the release steps are gated on build.tag), or BUILDKITE_TAG was not forwarded to the guest -- add it to the tart-ci plugin's env allowlist for this step"
fi

# 1. The tag's shape, and its digits against the manifest. The same regex as the release steps'
#    Buildkite condition and scripts/release-manifest.sh, so nothing downstream sees a tag this let by.
if ! printf '%s\n' "$tag" | grep -Eq '^v[0-9]+\.[0-9]+\.[0-9]+$'; then
  die "tag '$tag' is not vMAJOR.MINOR.PATCH; only such tags are releases (pessimal-ios-v* tags are not)"
fi
if [ "${tag#v}" != "$version" ]; then
  die "tag $tag says ${tag#v}, but [workspace.package] version in Cargo.toml at this commit is $version. cog bump's pre_bump_hook (scripts/set-workspace-version.sh) moves that version; a tag made without it, or on the wrong commit, is not a release"
fi
say "ok    tag $tag matches [workspace.package] version $version"

# 2. Ancestry, against GitHub's own view of the tag and of main.
scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# No user or system config (no insteadOf rewrite to SSH, no credential helper), and never a prompt:
# an anonymous fetch that falls back to asking for a password would hang the step instead of failing.
anon_git() {
  GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null GIT_TERMINAL_PROMPT=0 \
    git -c credential.helper= -C "$scratch/repo.git" "$@"
}
GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null git init --quiet --bare "$scratch/repo.git"

# Full history of main, not --depth: a shallow main cannot prove an ancestor it does not contain.
# Measured 2026-09-13: this fetch is 1.4 MB and about 12 s, so a blob filter would buy nothing and
# cost compatibility with an older git in the guest.
if ! fetch_log="$(anon_git fetch --no-tags "$REPO_URL" \
    "+refs/heads/main:refs/remotes/origin/main" "+refs/tags/$tag:refs/tags/$tag" 2>&1)"; then
  if ! anon_git ls-remote --exit-code "$REPO_URL" "refs/tags/$tag" >/dev/null 2>&1; then
    die "tag $tag does not exist on GitHub ($REPO_URL). The guard judges what GitHub serves, not what this checkout holds"
  fi
  die "anonymous git fetch of main and $tag from $REPO_URL failed: $(printf '%s' "$fetch_log" | tr '\n' ' ')"
fi

tag_commit="$(anon_git rev-parse --verify --quiet "refs/tags/$tag^{commit}")" \
  || die "refs/tags/$tag on GitHub does not resolve to a commit"
main_commit="$(anon_git rev-parse --verify --quiet refs/remotes/origin/main)" \
  || die "main on GitHub did not resolve to a commit after the fetch"

# The commit this build is actually building. Buildkite checks out BUILDKITE_COMMIT in every step, so if
# the tag moved after the build started, every later step would build something the guard never saw.
build_commit="${BUILDKITE_COMMIT:-}"
if printf '%s\n' "$build_commit" | grep -Eq '^[0-9a-f]{40}$'; then
  case "$build_commit" in
    "$tag_commit") say "ok    this build's commit is the commit $tag points at on GitHub ($tag_commit)" ;;
    *) die "this build is for commit $build_commit, but $tag on GitHub points at $tag_commit. The tag moved after the build started; start a new build for the tag" ;;
  esac
else
  # Not a check that passed: a check that could not be made, said as such.
  say "SKIP  build commit == tag commit: BUILDKITE_COMMIT is '${build_commit:-unset}', not a full SHA; forward it through the tart-ci env allowlist to get this check"
fi

set +e
anon_git merge-base --is-ancestor "$tag_commit" "$main_commit"
ancestry=$?
set -e
case "$ancestry" in
  0) say "ok    $tag ($tag_commit) is an ancestor of main on GitHub ($main_commit)" ;;
  1) die "$tag points at $tag_commit, which is not an ancestor of main ($main_commit). Releases are cut from main only; cog bump pushes main before the tag for exactly this reason. If the tag reached GitHub before its commit did, push main and start a new build" ;;
  *) die "git merge-base --is-ancestor failed (exit $ancestry) comparing $tag_commit with main $main_commit" ;;
esac

# 3. No published release. Anonymous on purpose: a draft is invisible to an anonymous caller, so 404
#    means "nothing a user can see", which is exactly the condition. `--retry` covers transport and
#    5xx only; a 404 is an answer, not a failure. Any token in the environment is kept out of it --
#    this step holds none, and a token would make drafts visible and change the question.
unset GITHUB_TOKEN GH_TOKEN
body="$scratch/release.json"
if ! status="$(curl -sS --retry 3 --retry-delay 5 -o "$body" -w '%{http_code}' \
    -H 'Accept: application/vnd.github+json' -H 'X-GitHub-Api-Version: 2022-11-28' \
    -H 'User-Agent: pessimal-release-guard/1' \
    "$API/releases/tags/$tag")"; then
  die "the anonymous GET $API/releases/tags/$tag did not complete (curl error above); cannot prove no published release exists"
fi
case "$status" in
  404) say "ok    no published release exists for $tag (anonymous GET releases/tags/$tag: 404)" ;;
  200) die "a published release already exists for $tag: https://github.com/$REPO/releases/tag/$tag. Nothing in the pipeline overwrites a published release. If it is the broken result of an earlier attempt, a human deletes that release and the tag (docs/runbooks/cutting-a-release.md) and cuts again; otherwise cut the next version with cog bump" ;;
  403|429) die "GitHub refused the anonymous GET with HTTP $status, almost certainly the 60-requests-an-hour anonymous rate limit shared by this cluster's IP. Retry the step later: nothing has been built yet" ;;
  *) die "unexpected HTTP $status from the anonymous GET $API/releases/tags/$tag: $(head -c 300 "$body" | tr '\n' ' ')" ;;
esac

say "$tag may be released as version $version"
printf '%s\n' "$version"
