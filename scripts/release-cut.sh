#!/usr/bin/env bash
# Cuts an agent release from CI: the `release-cut` Buildkite step, after every CI step on main passes.
#
#   scripts/release-cut.sh
#
# It clones main, and if the commits since the last v* tag need a new version, runs `cog bump --auto`:
# cog sets [workspace.package] version, writes CHANGELOG.md, commits `chore(version): vX.Y.Z`, tags,
# and pushes main and then the tag. The tag starts the release build. If nothing needs a new version,
# cog does nothing and exits 0.
#
# The version commit starts another main build. That build finds nothing to release, so it stops there.
#
# It works in its own clone, not the Buildkite checkout, because cog refuses a detached HEAD and the
# guest cannot write to the host's checkout safely. It pushes with LL_CLI_RELEASE_GH_TOKEN, which it
# reads from Doppler and passes to git only through the environment, never through a file.
set -euo pipefail

die() { printf 'release-cut: error: %s\n' "$*" >&2; exit 1; }
say() { printf 'release-cut: %s\n' "$*"; }

REPO_URL="https://github.com/Lightless-Labs/pessimal.git"
COG_VERSION="7.0.0"
COG_SHA256_LINUX_ARM64="379949f50c4f8245ef9532913e83f351c048b55e40ebd52ed8a850642c07a753"
COG_SHA256_DARWIN_ARM64="429303b6d68c6461aa221ab4de82b26d0bbcdee7127947ea9c7c411ab9bc574c"

for tool in git cargo curl python3 tar; do
  command -v "$tool" >/dev/null || die "$tool is not on PATH in this guest"
done
commit="${BUILDKITE_COMMIT:?BUILDKITE_COMMIT is not set; add it to the tart-ci env allowlist}"
[[ -n "${DOPPLER_TOKEN:-}" ]] || die "DOPPLER_TOKEN is not set; the step needs doppler_token_secret: DOPPLER_SERVICE_ACCOUNT_TOKEN"

# git reads GIT_CONFIG_COUNT from 2.31 on. Older git would ignore the credential and fail to push.
git_version="$(git --version | awk '{print $3}')"
[[ "$(printf '%s\n%s\n' 2.31 "$git_version" | sort -V | head -1)" == 2.31 ]] \
  || die "git $git_version is older than 2.31, which this script needs to pass the credential safely"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# ---- the token -------------------------------------------------------------------------------------
gh_token="$(DOPPLER_BOOTSTRAP_TOKEN="$DOPPLER_TOKEN" python3 <<'PY'
import base64, json, os, sys, urllib.error, urllib.parse, urllib.request
token = os.environ["DOPPLER_BOOTSTRAP_TOKEN"]
query = urllib.parse.urlencode({"project": "lightless-labs-pessimal", "config": "prd_macos_notarisation",
                                "name": "LL_CLI_RELEASE_GH_TOKEN"})
request = urllib.request.Request("https://api.doppler.com/v3/configs/config/secret?" + query, headers={
    "Authorization": "Basic " + base64.b64encode((token + ":").encode()).decode(),
    "Accept": "application/json", "User-Agent": "pessimal-release-cut/1"})
try:
    with urllib.request.urlopen(request, timeout=20) as response:
        value = json.load(response).get("value") or {}
except urllib.error.HTTPError as exc:
    sys.exit("Doppler read failed for LL_CLI_RELEASE_GH_TOKEN: HTTP %d" % exc.code)
secret = value.get("computed") or value.get("raw") if isinstance(value, dict) else None
if not secret:
    sys.exit("Doppler returned no value for LL_CLI_RELEASE_GH_TOKEN")
print(secret, end="")
PY
)" || die "could not read LL_CLI_RELEASE_GH_TOKEN from Doppler"
unset DOPPLER_TOKEN

# Every git command that talks to GitHub gets the credential this way: environment only.
with_credential() {
  local header
  header="AUTHORIZATION: basic $(printf 'x-access-token:%s' "$gh_token" | base64 | tr -d '\n')"
  GIT_CONFIG_COUNT=1 \
  GIT_CONFIG_KEY_0="http.https://github.com/.extraheader" \
  GIT_CONFIG_VALUE_0="$header" \
    "$@"
}

# ---- cog ---------------------------------------------------------------------------------------------
case "$(uname -s)-$(uname -m)" in
  Linux-aarch64|Linux-arm64) cog_triple="aarch64-unknown-linux-gnu"; cog_sha256="$COG_SHA256_LINUX_ARM64" ;;
  Darwin-arm64)              cog_triple="aarch64-apple-darwin";      cog_sha256="$COG_SHA256_DARWIN_ARM64" ;;
  *) die "no pinned cocogitto for $(uname -s)-$(uname -m)" ;;
esac
archive="cocogitto-$COG_VERSION-$cog_triple.tar.gz"
curl -fsSL --max-time 120 -o "$work/$archive" \
  "https://github.com/cocogitto/cocogitto/releases/download/$COG_VERSION/$archive"
if command -v sha256sum >/dev/null; then actual="$(sha256sum "$work/$archive" | awk '{print $1}')"
else actual="$(shasum -a 256 "$work/$archive" | awk '{print $1}')"; fi
[[ "$actual" == "$cog_sha256" ]] || die "$archive has sha256 $actual, expected $cog_sha256"
tar -xzf "$work/$archive" -C "$work"
cog="$(find "$work" -type f -name cog | head -1)"
[[ -x "$cog" ]] || die "no cog binary in $archive"

# ---- the clone ---------------------------------------------------------------------------------------
git clone --quiet --branch main "$REPO_URL" "$work/repo"
cd "$work/repo"
head="$(git rev-parse HEAD)"
if [[ "$head" != "$commit" ]]; then
  say "main is at $head, not this build's $commit. The newer build decides whether to release."
  exit 0
fi

export GIT_AUTHOR_NAME="Pessimal release" GIT_AUTHOR_EMAIL="pessimal-release@users.noreply.github.com"
export GIT_COMMITTER_NAME="$GIT_AUTHOR_NAME" GIT_COMMITTER_EMAIL="$GIT_AUTHOR_EMAIL"

# A cut that pushed main but failed to push its tag leaves an untagged version commit at the top of
# main. Finish that one instead of cutting a new version.
subject="$(git log -1 --format=%s)"
if [[ "$subject" =~ ^chore\(version\):\ (v[0-9]+\.[0-9]+\.[0-9]+)$ ]]; then
  tag="${BASH_REMATCH[1]}"
  if [[ -z "$(git ls-remote --tags origin "refs/tags/$tag")" ]]; then
    say "$tag was committed but never tagged; tagging it now"
    git tag "$tag" HEAD
    with_credential git push --quiet origin "refs/tags/$tag"
    say "pushed $tag"
  else
    say "$tag is already tagged; nothing to release"
  fi
  exit 0
fi

# The version hook runs `cargo update --workspace --offline`, which needs the dependencies downloaded.
cargo fetch --locked --quiet

say "cocogitto $("$cog" --version | awk '{print $2}'): next version is $("$cog" bump --auto --dry-run | tail -1)"
set +e
with_credential "$cog" bump --auto
status=$?
set -e

if (( status != 0 )); then
  # Someone pushed to main while this ran, so the push was rejected. The build for that push cuts.
  if [[ "$(git ls-remote origin refs/heads/main | awk '{print $1}')" != "$commit" ]]; then
    say "main moved while cutting; the build for the newer commit will release"
    exit 0
  fi
  die "cog bump failed (exit $status); see above"
fi

if [[ "$(git rev-parse HEAD)" == "$commit" ]]; then
  say "no commit since the last release needs a new version; nothing to release"
  exit 0
fi

tag="$(git tag --points-at HEAD | grep -E '^v[0-9]+\.[0-9]+\.[0-9]+$' | head -1)"
[[ -n "$tag" ]] || die "cog committed a version but made no v* tag"
[[ -n "$(git ls-remote --tags origin "refs/tags/$tag")" ]] || die "$tag is not on GitHub after the push"
say "released $tag: the tag build will build, sign and publish it"
