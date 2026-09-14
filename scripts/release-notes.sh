#!/usr/bin/env bash
# Writes a release body to stdout: the caveats first, then how to install, then GitHub's changelog.
#
# Usage:
#   GITHUB_TOKEN=... scripts/release-notes.sh v0.1.0   # caveats + install + generated changelog
#   scripts/release-notes.sh v0.1.0 --no-api           # everything except the changelog; offline
#
# The caveats are hard-coded, and they come first, because the release body is the only documentation
# most people will ever read. An unstapled macOS binary, an arm64-only app, a missing Windows build,
# a glibc floor and a binary the pipeline could not execute are all things a user must meet here
# rather than discover by hitting them. They are deliberately not derived from anything: a caveat
# that can be computed can also silently become an empty string, and nothing downstream would notice.
#
# The changelog half is one POST to GitHub's generate-notes endpoint, appended. It goes through
# Python's urllib rather than `gh` because the Buildkite guest images have no `gh` CLI -- the same
# reason scripts/release-github.py talks REST, and the same shape Descartes' release script uses. The
# token is read from GITHUB_TOKEN in the environment, handed to the child through the environment,
# and never printed.
#
# The `release-publish` Buildkite step does not use that half. scripts/release-github.py runs this with
# --no-api, after dropping the token from its own environment, and makes the generate-notes call
# itself -- so the token lives in exactly one process there. The API mode is for reading the finished
# notes by hand before a tag is cut.
set -euo pipefail

# Hard-coded for the same reason as in verify-release.sh: the notes must not describe whatever
# repository the environment happens to point at.
REPO="Lightless-Labs/pessimal"

tag=""
no_api="no"

usage() { sed -n '2,7p' "$0" | sed 's/^# \{0,1\}//'; }
usage_die() { printf 'error: %s\n\n' "$1" >&2; usage >&2; exit 2; }
die() { printf 'error: %s\n' "$1" >&2; exit 1; }

while [ $# -gt 0 ]; do
  case "$1" in
    --no-api) no_api="yes"; shift ;;
    -h|--help) usage; exit 0 ;;
    -*) usage_die "unknown option $1" ;;
    *) [ -z "$tag" ] || usage_die "unexpected argument $1"; tag="$1"; shift ;;
  esac
done

[ -n "$tag" ] || usage_die "a tag is required, e.g. v0.1.0"
# The same shape the release steps' Buildkite condition and `release-guard` enforce, so the notes
# cannot describe a tag the release machinery would refuse.
if ! printf '%s\n' "$tag" | grep -Eq '^v[0-9]+\.[0-9]+\.[0-9]+$'; then
  usage_die "tag must look like v1.2.3 (got '$tag')"
fi
version="${tag#v}"

# Everything the API half needs, checked before a single line is written, so a caller never gets
# half a notes file followed by an error it might not read. The image is named because its contents
# were never inventoried.
if [ "$no_api" = "no" ]; then
  command -v python3 >/dev/null 2>&1 \
    || die "python3 is required for the generated changelog and is not on PATH in ci-linux-arm64-rust-bazel (or this host). Re-run with --no-api for the caveats and install block alone."
  [ -n "${GITHUB_TOKEN:-}" ] \
    || die "GITHUB_TOKEN is not set; generate-notes needs an authenticated call. Re-run with --no-api for the caveats and install block alone."
fi

# A quoted heredoc piped through sed, rather than an expanding one: the body is Markdown and is full
# of backticks, which an expanding heredoc would run as command substitutions.
cat <<'NOTES' | sed -e "s|@TAG@|$tag|g" -e "s|@VERSION@|$version|g" -e "s|@REPO@|$REPO|g"
## Before you install

- **The macOS agent binaries are signed and notarized, but NOT stapled.** A bare Mach-O executable
  has nowhere to hold a notarisation ticket, so Gatekeeper resolves it over the network at first
  launch. On a host with restricted egress that first launch can fail, and under `launchd` it fails
  quietly. The mechanism, the channel rule and the fix are in
  [packaging/macos/GATEKEEPER.md](https://github.com/@REPO@/blob/@TAG@/packaging/macos/GATEKEEPER.md).
- **`Pessimal-@VERSION@-macos.zip` (the menu bar app) is Apple silicon only.** It is signed,
  notarized *and* stapled, but `scripts/build-macos-app.sh` compiles with
  `-target "$(uname -m)-apple-macos…"` and there is no `lipo` step, so no x86_64 slice exists. It
  will not launch on an Intel Mac. The agent itself ships for both macOS architectures.
- **There is no Windows build in this release.** Not an oversight: a cross-build of the agent for
  Windows ran for over twenty minutes without finishing, and the machines that build and check this
  release include none that could run a Windows binary, so it would have been the one asset nobody
  had ever executed. It is deferred, not abandoned.
- **The Linux tarballs need glibc 2.28 or newer** (Debian 10+, Ubuntu 18.10+, RHEL 8+). They are
  cross-built with `cargo-zigbuild` against a glibc 2.28 target, and the release fails if either
  binary asks for a newer glibc symbol than that. There is no musl build: a static musl binary
  resolves names through its own stub resolver with no nsswitch, which is a behaviour change rather
  than a build problem, so it is deferred deliberately.
- **The x86_64 Linux binary was never executed by the release pipeline.** Its glibc floor and its
  checksum are verified, but the build machines are Apple silicon and run arm64 Linux, so only the
  aarch64 Linux and macOS binaries are launched before a release is promoted. The build log says
  whether the x86_64 macOS binary was also run under Rosetta.

## Install

The agent, on any supported platform:

```sh
mise use -g github:@REPO@
```

`github:` is the current spelling. mise's `ubi:` backend is deprecated, and its `exe=` option does
not exist on the `github:` backend.

Or take a tarball from the assets below and check it first:

```sh
shasum -a 256 -c --ignore-missing SHA256SUMS
```

`SHA256SUMS` must come from this same release: it is computed over the bytes GitHub serves here, so a
copy from anywhere else proves nothing. Each agent tarball also carries `LICENSE`,
`pessimal.example.toml`, a `README.md` with the install and service steps, and on Linux the
`pessimal-agent.service` systemd unit.

NOTES

if [ "$no_api" = "no" ]; then
  # One call, and a hard failure if it does not answer: notes that quietly lost their changelog look
  # exactly like notes that never had one. Only the body reaches stdout; every error goes to stderr
  # and carries the HTTP status and GitHub's message, never a request header.
  if ! changelog="$(PESSIMAL_NOTES_REPO="$REPO" PESSIMAL_NOTES_TAG="$tag" python3 - <<'PY'
import json
import os
import sys
import urllib.error
import urllib.request

repo = os.environ["PESSIMAL_NOTES_REPO"]
tag = os.environ["PESSIMAL_NOTES_TAG"]
request = urllib.request.Request(
    "https://api.github.com/repos/%s/releases/generate-notes" % repo,
    method="POST",
    data=json.dumps({"tag_name": tag, "target_commitish": "main"}).encode(),
    headers={
        "Authorization": "Bearer " + os.environ["GITHUB_TOKEN"],
        "Accept": "application/vnd.github+json",
        "Content-Type": "application/json",
        "User-Agent": "pessimal-release-notes/1",
        "X-GitHub-Api-Version": "2022-11-28",
    },
)
try:
    with urllib.request.urlopen(request, timeout=60) as response:
        payload = json.load(response)
except urllib.error.HTTPError as exc:
    sys.stderr.write("generate-notes for %s@%s: HTTP %d: %s\n"
                     % (repo, tag, exc.code, exc.read(300).decode("utf-8", "replace")))
    sys.exit(2)
except Exception as exc:  # network, TLS, JSON: all mean "no changelog", all must be loud
    sys.stderr.write("generate-notes for %s@%s failed: %s\n" % (repo, tag, exc))
    sys.exit(2)

body = payload.get("body")
if not isinstance(body, str):
    sys.stderr.write("generate-notes for %s@%s returned no body\n" % (repo, tag))
    sys.exit(2)
sys.stdout.write(body)
PY
)"; then
    die "the generate-notes call failed for $tag (reason above). Fix it, or re-run with --no-api to publish the caveats and install block alone."
  fi
  [ -n "$changelog" ] || die "the generate-notes API returned an empty body for $tag"
  printf '%s\n' "$changelog"
fi
