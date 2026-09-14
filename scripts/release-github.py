#!/usr/bin/env python3
"""Publishes a Pessimal release to GitHub, and later promotes it. The only code that holds GITHUB_TOKEN.

Usage:

    scripts/release-github.py publish --tag v0.2.0 --dist DIR [--notes-file FILE] [--expect-commit SHA]
    scripts/release-github.py promote --tag v0.2.0 [--skip-formula]
    scripts/release-github.py bump-formula --tag v0.2.0

`publish` is the `release-publish` Buildkite step, `promote` is `release-promote`, and `bump-formula`
is the tail of `promote` on its own, for re-running by hand after a tap failure.

Why it is shaped this way:

* **REST from urllib, not `gh`.** The Buildkite guest images have no `gh` CLI. Descartes' release
  script (lightless-labs/public/descartes, scripts/release-macos-notifier-buildkite.sh) talks to the
  same API the same way, and its upload and Homebrew blocks are the templates for this file.

* **The credential.** `GITHUB_TOKEN` from the environment, or -- in the Buildkite steps, where the
  tart-ci plugin injects only `DOPPLER_TOKEN` -- read from Doppler (`lightless-labs-pessimal` /
  `prd_github_release`, which holds nothing else). Both are removed from this process's environment
  before anything else happens, so no child process (release-manifest.sh, release-notes.sh) ever
  inherits either. The token is only ever sent to api.github.com and uploads.github.com; it is never
  forwarded across a redirect, because an asset download answers with a 302 to a signed storage URL
  that must not see it; and every line this prints passes through `redact()`.

* **Forward-only.** publish: draft (invisible to everyone) -> upload every asset -> download every
  asset back and check it against the hash its producer wrote -> compute SHA256SUMS from those
  downloaded bytes, so it attests to what GitHub serves rather than to what a guest happened to hold
  -> upload it -> flip to a *prerelease* with `make_latest: "false"`, so /releases/latest still means
  the previous release while release-verify runs. promote: prerelease -> latest. Nothing here ever
  un-publishes, deletes a published release, or demotes one; a failed verify leaves a prerelease for a
  human to decide about.

* **Idempotent on retry.** A draft left by an earlier attempt is reused, and its assets replaced. A
  prerelease that already exists is checked against this build's artifacts and left alone: if it is
  exactly what publish would have produced (a retry after the flip), publish succeeds without touching
  it; if not, it refuses. Promote on a promoted release is a no-op.

* **Asset names come from scripts/release-manifest.sh,** never from this file, so the publisher, the
  producers and the verifier cannot disagree about what a release contains.

* **The Homebrew formula bump is best-effort,** exactly as Descartes does it: by the time it runs the
  release is already latest, so a tap failure prints the manual bump and exits 0 rather than reddening
  a release that shipped. The formula is rendered from packaging/homebrew/pessimal-agent.rb.template,
  whose placeholders are:

      @VERSION@   0.2.0              @TAG@   v0.2.0              @REPO@   Lightless-Labs/pessimal
      @SHA256_<TRIPLE>@  the SHA256SUMS hash of pessimal-agent-<version>-<triple>.tar.gz, with the
                         triple upper-cased and `-` turned into `_`, e.g. @SHA256_AARCH64_APPLE_DARWIN@
      @SHA256_MACOS_APP@ the hash of Pessimal-<version>-macos.zip

  A rendered formula that still contains an @NAME@ placeholder is never pushed.

The Buildkite step runs this as `python3 scripts/release-github.py ...`, so the step command, not this
file, is what must first check that python3 exists in the guest image.

Stdlib only, Python 3.9 compatible, like scripts/otlp-trace-probe.py: the guest has no pip install.
"""

import argparse
import base64
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import time
import traceback
import urllib.error
import urllib.parse
import urllib.request

REPO = "Lightless-Labs/pessimal"
TAP_REPO = "Lightless-Labs/homebrew-tap"
FORMULA_PATH = "Formula/pessimal-agent.rb"

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
MANIFEST = os.path.join(ROOT, "scripts", "release-manifest.sh")
NOTES = os.path.join(ROOT, "scripts", "release-notes.sh")
FORMULA_TEMPLATE = os.path.join(ROOT, "packaging", "homebrew", "pessimal-agent.rb.template")

GITHUB_API = "https://api.github.com"
GITHUB_HOSTS = ("api.github.com", "uploads.github.com")
DOPPLER_API = "https://api.doppler.com"
DOPPLER_PROJECT = "lightless-labs-pessimal"
DOPPLER_CONFIG = "prd_github_release"

GUEST_IMAGE = "ci-linux-arm64-rust-bazel"
USER_AGENT = "pessimal-release/1"
TAG_RE = re.compile(r"^v([0-9]+)\.([0-9]+)\.([0-9]+)$")
SHA_LINE_RE = re.compile(r"^([0-9a-f]{64}) [ *](\S+)$")
PLACEHOLDER_RE = re.compile(r"@[A-Z0-9_]+@")
SUMS = "SHA256SUMS"

RETRYABLE_STATUS = (429, 500, 502, 503, 504)
MAX_ATTEMPTS = 4

# ---------------------------------------------------------------------------------------------------
# Output. Every line goes through redact(): the values in _SECRETS are replaced wherever they appear,
# including inside an error body GitHub or Doppler chose to echo back.

_SECRETS = []


def remember_secret(value):
    if value and value not in _SECRETS:
        _SECRETS.append(value)


def redact(text):
    text = str(text)
    for secret in _SECRETS:
        text = text.replace(secret, "[REDACTED]")
    return text


def say(message):
    sys.stdout.write(redact(message) + "\n")
    sys.stdout.flush()


def warn(message):
    sys.stderr.write(redact("warning: " + message) + "\n")
    sys.stderr.flush()


class Refusal(Exception):
    """A reason to stop that is already worded for the log. Never carries a credential."""


def die(message):
    raise Refusal(message)


class TransportError(Exception):
    """The request never produced an HTTP response, after every retry."""


# ---------------------------------------------------------------------------------------------------
# Inputs


def version_of(tag):
    match = TAG_RE.match(tag)
    if not match:
        die("tag must be vMAJOR.MINOR.PATCH (got %r); only such tags are releases" % tag)
    buildkite_tag = os.environ.get("BUILDKITE_TAG", "")
    if buildkite_tag and buildkite_tag != tag:
        die("--tag %s disagrees with BUILDKITE_TAG=%s; a step publishes the tag its build is for"
            % (tag, buildkite_tag))
    return tag[1:], tuple(int(part) for part in match.groups())


def run_script(argv, what):
    """Runs one of this repository's scripts through bash and returns its stdout."""
    result = subprocess.run(["bash"] + argv, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                            universal_newlines=True)
    if result.returncode != 0:
        die("%s failed (exit %d): %s" % (what, result.returncode, result.stderr.strip()))
    return result.stdout


def manifest(version, group):
    names = [line.strip() for line in run_script([MANIFEST, version, group],
                                                 "scripts/release-manifest.sh %s %s" % (version, group)
                                                 ).splitlines() if line.strip()]
    if not names:
        die("scripts/release-manifest.sh %s %s printed no names" % (version, group))
    if len(set(names)) != len(names):
        die("scripts/release-manifest.sh %s %s printed a name twice: %s" % (version, group, names))
    return names


def payload_names(version):
    names = manifest(version, "all")
    if SUMS not in names:
        die("scripts/release-manifest.sh %s all does not list %s, which publish computes and uploads"
            % (version, SUMS))
    return names, [name for name in names if name != SUMS]


def sha256_bytes(data):
    return hashlib.sha256(data).hexdigest()


def sha256_file(path):
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def read_producer_hash(path, name):
    """The `<asset>.sha256` a producer wrote beside its asset: one `sha256sum`/`shasum -a 256` line."""
    with open(path, "r", encoding="utf-8") as handle:
        lines = [line.rstrip("\r\n") for line in handle if line.strip()]
    if len(lines) != 1:
        die("%s holds %d lines; a producer .sha256 is exactly one `<hash>  <name>` line" % (path, len(lines)))
    match = SHA_LINE_RE.match(lines[0])
    if not match:
        die("%s is not `<64 hex>  <name>`: %r" % (path, lines[0][:120]))
    if match.group(2) != name:
        die("%s names %r, but it sits beside %s; the hash may belong to a different file"
            % (path, match.group(2), name))
    return match.group(1)


def collect_dist(dist, payload):
    """Finds every payload asset and its producer .sha256 anywhere under `dist`, and proves them.

    Recursive, because the Buildkite artifacts plugin keeps each producer's relative path, so the Linux
    and macOS steps' outputs can land in different subdirectories. Missing AND extra both fail: an extra
    asset-shaped file means a producer and the manifest disagree about what a release contains, and that
    must stop here rather than at a user's install.
    """
    if not os.path.isdir(dist):
        die("--dist %s is not a directory; the release-linux and release-macos artifacts should have been "
            "downloaded there on the host before the guest started" % dist)
    found = {}
    for dirpath, _dirnames, filenames in os.walk(dist):
        for filename in filenames:
            found.setdefault(filename, []).append(os.path.join(dirpath, filename))

    wanted = set(payload) | set(name + ".sha256" for name in payload)
    problems = []
    for name in sorted(wanted):
        paths = found.get(name, [])
        if not paths:
            problems.append("missing: %s" % name)
        elif len(paths) > 1:
            problems.append("found more than once, so which one to publish is a guess: %s (%s)"
                            % (name, ", ".join(sorted(paths))))
    asset_shaped = re.compile(r"^(pessimal-agent-|Pessimal-|SHA256SUMS)")
    for name in sorted(found):
        if name in wanted or not asset_shaped.match(name):
            continue
        if name.startswith(SUMS):
            problems.append("unexpected: %s -- SHA256SUMS is computed by publish from the bytes it "
                            "downloads back, and no producer may supply one" % name)
        else:
            problems.append("unexpected: %s is shaped like a release asset but release-manifest.sh does "
                            "not list it" % name)
    if problems:
        die("the artifacts under %s are not exactly the release (%d problem(s)):\n  %s"
            % (dist, len(problems), "\n  ".join(problems)))

    ignored = sorted(name for name in found if name not in wanted)
    if ignored:
        say("ignoring %d file(s) under %s that are not release assets: %s"
            % (len(ignored), dist, ", ".join(ignored)))

    assets = []
    for name in payload:
        path = found[name][0]
        size = os.path.getsize(path)
        if size == 0:
            die("%s is empty" % path)
        producer = read_producer_hash(found[name + ".sha256"][0], name)
        local = sha256_file(path)
        if local != producer:
            die("%s hashes to %s, but its producer recorded %s: the file changed between the step that "
                "made it and this one (artifact transport). Nothing has been sent to GitHub."
                % (name, local, producer))
        assets.append({"name": name, "path": path, "size": size, "sha256": producer})
        say("ok    %-58s %10d bytes  sha256 %s  (matches its producer .sha256)" % (name, size, producer))
    return assets


def sums_text(digests):
    """SHA256SUMS in `shasum -a 256` format: two spaces, bare names, LC_ALL=C order, LF endings."""
    return "".join("%s  %s\n" % (digests[name], name) for name in sorted(digests))


def parse_sums(text, source):
    digests = {}
    for number, line in enumerate(text.splitlines(), 1):
        if not line.strip():
            continue
        match = re.match(r"^([0-9a-f]{64})  ([^ /]+)$", line)
        if not match:
            die("%s line %d is not `<64 hex>  <bare name>`: %r" % (source, number, line[:120]))
        digests[match.group(2)] = match.group(1)
    if not digests:
        die("%s is empty" % source)
    return digests


# ---------------------------------------------------------------------------------------------------
# Credentials and transport


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    """Turns every redirect into an HTTPError the caller handles, so no header ever follows one."""

    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def _excerpt(body):
    # Redact the whole body BEFORE cutting it short: a credential straddling the cut would otherwise
    # survive as a prefix that no longer matches the value redact() looks for.
    if isinstance(body, bytes):
        body = body.decode("utf-8", "replace")
    return redact(" ".join(str(body).split()))[:400]


def _retry_after(headers, attempt):
    after = headers.get("Retry-After") if headers is not None else None
    if after and after.isdigit():
        return min(int(after), 30)
    return min(2 ** attempt, 30)


def _retryable(status, headers):
    if status in RETRYABLE_STATUS:
        return True
    # GitHub's primary and secondary rate limits arrive as 403 with a rate-limit signal.
    return status == 403 and headers is not None and (
        headers.get("Retry-After") is not None or headers.get("X-RateLimit-Remaining") == "0")


def fetch_github_token_from_doppler(doppler_token):
    base = DOPPLER_API
    project = os.environ.get("DOPPLER_PROJECT") or DOPPLER_PROJECT
    config = os.environ.get("DOPPLER_CONFIG") or DOPPLER_CONFIG
    query = urllib.parse.urlencode({"project": project, "config": config, "name": "GITHUB_TOKEN"})
    basic = base64.b64encode((doppler_token + ":").encode()).decode()
    remember_secret(basic)
    request = urllib.request.Request("%s/v3/configs/config/secret?%s" % (base, query),
                                     headers={"Accept": "application/json", "User-Agent": USER_AGENT})
    request.add_unredirected_header("Authorization", "Basic " + basic)
    opener = urllib.request.build_opener(_NoRedirect)
    try:
        with opener.open(request, timeout=20) as response:
            payload = json.load(response)
    except urllib.error.HTTPError as exc:
        die("reading GITHUB_TOKEN from Doppler %s/%s failed: HTTP %d: %s"
            % (project, config, exc.code, _excerpt(exc.read())))
    except (OSError, ValueError) as exc:
        die("reading GITHUB_TOKEN from Doppler %s/%s failed: %s" % (project, config, redact(exc)))
    value = payload.get("value") if isinstance(payload, dict) else None
    if isinstance(value, dict):
        value = value.get("computed") or value.get("raw")
    if not isinstance(value, str) or not value:
        die("Doppler %s/%s returned no value for GITHUB_TOKEN" % (project, config))
    say("GITHUB_TOKEN read from Doppler %s/%s (the value is not shown)" % (project, config))
    return value


def take_credentials():
    """Removes every credential from the environment, whatever the subcommand, and returns them."""
    github_token = os.environ.pop("GITHUB_TOKEN", "")
    doppler_token = os.environ.pop("DOPPLER_TOKEN", "")
    # Not read, only kept away from children: nothing this runs needs them.
    for other in ("GH_TOKEN", "HOMEBREW_GITHUB_API_TOKEN"):
        remember_secret(os.environ.pop(other, ""))
    remember_secret(github_token)
    remember_secret(doppler_token)
    return github_token, doppler_token


def require_credentials(credentials, action):
    """Up front, before any local work: a step that cannot publish should say so in its first line."""
    if not credentials[0] and not credentials[1]:
        die("no GITHUB_TOKEN in the environment, and no DOPPLER_TOKEN to read it from Doppler with. "
            "%s writes to GitHub, so it refuses to run without one. In Buildkite the tart-ci plugin's "
            "`doppler_token_secret: DOPPLER_PESSIMAL_PRD_GITHUB_RELEASE` supplies DOPPLER_TOKEN; by "
            "hand, export GITHUB_TOKEN." % action)


def github_client(credentials, action):
    require_credentials(credentials, action)
    github_token, doppler_token = credentials
    if not github_token:
        github_token = fetch_github_token_from_doppler(doppler_token)
        remember_secret(github_token)
    return GitHub(github_token, GITHUB_API)


class GitHub(object):
    def __init__(self, token, api):
        self._token = token
        self.api = api
        self._authed = urllib.request.build_opener(_NoRedirect)

    def _check_destination(self, url):
        parts = urllib.parse.urlsplit(url)
        allowed = parts.scheme == "https" and parts.hostname in GITHUB_HOSTS
        if not allowed:
            die("refusing to send the GitHub credential to %s://%s: only %s may receive it"
                % (parts.scheme, parts.hostname, " and ".join(GITHUB_HOSTS)))

    def request(self, method, url, data=None, content_type="application/json",
                accept="application/vnd.github+json", timeout=60, retry=True):
        """One call. Returns (status, headers, body) for any HTTP answer, redirects included."""
        if not url.startswith("http"):
            url = self.api + url
        self._check_destination(url)
        attempts = MAX_ATTEMPTS if retry else 1
        for attempt in range(attempts):
            request = urllib.request.Request(url, data=data, method=method, headers={
                "Accept": accept, "User-Agent": USER_AGENT, "X-GitHub-Api-Version": "2022-11-28"})
            if data is not None:
                request.add_header("Content-Type", content_type)
            # Unredirected: urllib copies ordinary headers onto a redirected request, and this one must
            # never leave the host it was meant for. _NoRedirect stops redirects anyway; this is the
            # second lock on the same door.
            request.add_unredirected_header("Authorization", "Bearer " + self._token)
            try:
                with self._authed.open(request, timeout=timeout) as response:
                    return response.status, response.headers, response.read()
            except urllib.error.HTTPError as exc:
                body = exc.read()
                if attempt < attempts - 1 and _retryable(exc.code, exc.headers):
                    warn("%s %s: HTTP %d, retrying (attempt %d of %d)"
                         % (method, _path(url), exc.code, attempt + 1, attempts))
                    time.sleep(_retry_after(exc.headers, attempt))
                    continue
                return exc.code, exc.headers, body
            except OSError as exc:  # URLError, timeouts, resets
                if attempt < attempts - 1:
                    warn("%s %s: %s, retrying (attempt %d of %d)"
                         % (method, _path(url), redact(exc), attempt + 1, attempts))
                    time.sleep(_retry_after(None, attempt))
                    continue
                raise TransportError("%s %s: %s" % (method, _path(url), redact(exc))) from None
        raise TransportError("%s %s: no attempt was made" % (method, _path(url)))

    def call(self, method, path, payload=None, expect=(200,), allow=(), retry=True):
        data = json.dumps(payload).encode() if payload is not None else None
        status, _headers, body = self.request(method, path, data=data, retry=retry)
        if status in allow:
            return status, None
        if status not in expect:
            die("%s %s answered HTTP %d, expected %s: %s"
                % (method, _path(path), status, "/".join(str(code) for code in expect), _excerpt(body)))
        return status, (json.loads(body.decode("utf-8")) if body else None)

    def paged(self, path):
        items = []
        for page in range(1, 51):
            separator = "&" if "?" in path else "?"
            _status, batch = self.call("GET", "%s%sper_page=100&page=%d" % (path, separator, page))
            items.extend(batch)
            if len(batch) < 100:
                return items
        die("%s has more than 5000 entries; refusing to guess" % path)

    def download_asset(self, asset_id):
        """An asset's bytes, drafts included. The API answers with a 302 to signed storage."""
        url = "%s/repos/%s/releases/assets/%d" % (self.api, REPO, asset_id)
        status, headers, body = self.request("GET", url, accept="application/octet-stream", timeout=600)
        if status == 200:
            return body
        if status not in (301, 302, 303, 307, 308):
            die("downloading asset %d back failed: HTTP %d: %s" % (asset_id, status, _excerpt(body)))
        location = headers.get("Location") if headers is not None else None
        if not location:
            die("downloading asset %d back: HTTP %d with no Location header" % (asset_id, status))
        parts = urllib.parse.urlsplit(location)
        if parts.scheme != "https":
            die("asset %d redirected to a non-https URL (%s://%s); refusing to trust those bytes"
                % (asset_id, parts.scheme, parts.hostname))
        # The signed URL is itself a short-lived credential for the draft's bytes, so it is not printed,
        # and it gets no Authorization header: storage rejects a second auth mechanism, and the token
        # has no business leaving GitHub's API hosts.
        for attempt in range(MAX_ATTEMPTS):
            request = urllib.request.Request(location, headers={"User-Agent": USER_AGENT})
            try:
                with urllib.request.urlopen(request, timeout=600) as response:
                    return response.read()
            except urllib.error.HTTPError as exc:
                if attempt < MAX_ATTEMPTS - 1 and _retryable(exc.code, exc.headers):
                    time.sleep(_retry_after(exc.headers, attempt))
                    continue
                die("downloading asset %d from signed storage (host %s) failed: HTTP %d"
                    % (asset_id, parts.hostname, exc.code))
            except OSError as exc:
                if attempt < MAX_ATTEMPTS - 1:
                    time.sleep(_retry_after(None, attempt))
                    continue
                die("downloading asset %d from signed storage (host %s) failed: %s"
                    % (asset_id, parts.hostname, redact(exc)))
        die("downloading asset %d: no attempt was made" % asset_id)

    def upload_asset(self, release, name, data):
        """One POST to the release's upload_url. Not retried here: see upload_with_cleanup."""
        base = release["upload_url"].split("{", 1)[0]
        url = "%s?%s" % (base, urllib.parse.urlencode({"name": name}))
        return self.request("POST", url, data=data, content_type="application/octet-stream",
                            timeout=600, retry=False)


def _path(url):
    """The path of a URL, for logs: never the query string, which on a redirect is a signature."""
    return urllib.parse.urlsplit(url).path if url.startswith("http") else url.split("?", 1)[0]


# ---------------------------------------------------------------------------------------------------
# GitHub release operations


def resolve_tag_commit(gh, tag):
    status, ref = gh.call("GET", "/repos/%s/git/ref/tags/%s" % (REPO, urllib.parse.quote(tag)),
                          allow=(404,))
    if status == 404:
        die("tag %s does not exist on GitHub. A draft naming a missing tag CREATES that tag from the default "
            "branch when it is published, so publish refuses rather than invent one." % tag)
    target = ref["object"]
    for _ in range(5):
        if target["type"] == "commit":
            return target["sha"]
        if target["type"] != "tag":
            die("refs/tags/%s points at a %s, not a commit" % (tag, target["type"]))
        _status, annotated = gh.call("GET", "/repos/%s/git/tags/%s" % (REPO, target["sha"]))
        target = annotated["object"]
    die("refs/tags/%s is nested more than five annotated tags deep" % tag)


def published_release(gh, tag):
    """The published (prerelease or not) release for a tag, or None. Drafts are invisible here."""
    status, release = gh.call("GET", "/repos/%s/releases/tags/%s" % (REPO, urllib.parse.quote(tag)),
                              allow=(404,))
    return None if status == 404 else release


def latest_release(gh):
    status, release = gh.call("GET", "/repos/%s/releases/latest" % REPO, allow=(404,))
    return None if status == 404 else release


def release_assets(gh, release_id):
    return gh.paged("/repos/%s/releases/%d/assets" % (REPO, release_id))


def delete_asset(gh, asset):
    gh.call("DELETE", "/repos/%s/releases/assets/%d" % (REPO, asset["id"]), expect=(204,), allow=(404,))


def assert_asset_set(assets, names, expected_sizes, label):
    """The release holds exactly the manifest, every upload finished, every size as expected."""
    on_release = sorted(asset["name"] for asset in assets)
    missing = sorted(set(names) - set(on_release))
    extra = sorted(set(on_release) - set(names))
    problems = []
    if missing:
        problems.append("missing: %s" % ", ".join(missing))
    if extra:
        problems.append("not in release-manifest.sh: %s" % ", ".join(extra))
    if len(on_release) != len(set(on_release)):
        problems.append("an asset name appears twice: %s" % on_release)
    for asset in assets:
        # The REST enum is `uploaded` | `open`; `open` is an upload that never finished.
        if asset.get("state") != "uploaded":
            problems.append("%s is in state %r, not 'uploaded'" % (asset["name"], asset.get("state")))
        want = expected_sizes.get(asset["name"])
        if want is not None and asset.get("size") != want:
            problems.append("%s is %s bytes on GitHub, expected %d" % (asset["name"], asset.get("size"), want))
    if problems:
        die("%s does not hold exactly the release's %d assets:\n  %s"
            % (label, len(names), "\n  ".join(problems)))


def generate_changelog(gh, tag):
    # The endpoint only computes text, so a retry is harmless. `gh` is not on the guest, which is why
    # this is here rather than in scripts/release-notes.sh's API mode.
    _status, notes = gh.call("POST", "/repos/%s/releases/generate-notes" % REPO,
                             {"tag_name": tag, "target_commitish": "main"})
    body = notes.get("body") if isinstance(notes, dict) else None
    if not isinstance(body, str) or not body.strip():
        die("generate-notes returned no changelog body for %s; notes that quietly lost their changelog look "
            "exactly like notes that never had one" % tag)
    return body


def notes_caveats(tag, notes_file):
    if notes_file:
        with open(notes_file, "r", encoding="utf-8") as handle:
            text = handle.read()
        source = notes_file
    else:
        # --no-api: the caveats and the install block. This process has already dropped the token from
        # its environment, so the child could not call the API even if asked to.
        text = run_script([NOTES, tag, "--no-api"], "scripts/release-notes.sh %s --no-api" % tag)
        source = "scripts/release-notes.sh %s --no-api" % tag
    if not text.strip():
        die("the release notes from %s are empty" % source)
    return text, source


def upload_with_cleanup(gh, release, name, data):
    """Uploads one asset, retrying after deleting whatever a failed attempt left under the same name."""
    for attempt in range(1, MAX_ATTEMPTS):
        try:
            status, _headers, body = gh.upload_asset(release, name, data)
        except TransportError as exc:
            status, body = 0, str(exc).encode()
        if status == 201:
            asset = json.loads(body.decode("utf-8"))
            if asset.get("state") == "uploaded" and asset.get("size") == len(data):
                return asset
            problem = "GitHub recorded state=%r size=%r for %d bytes" % (asset.get("state"), asset.get("size"), len(data))
        else:
            problem = "HTTP %d: %s" % (status, _excerpt(body)) if status else _excerpt(body)
        warn("upload of %s failed (attempt %d of %d): %s" % (name, attempt, MAX_ATTEMPTS - 1, problem))
        # A failed upload can leave an `open` asset under this name, and the retry would then fail with
        # 422 already_exists. The release is still a draft, so removing it touches nothing public.
        for leftover in release_assets(gh, release["id"]):
            if leftover["name"] == name:
                delete_asset(gh, leftover)
        if attempt < MAX_ATTEMPTS - 1:
            time.sleep(min(2 ** attempt, 30))
    die("could not upload %s after %d attempts. The release is still a DRAFT, invisible to users; re-running "
        "release-publish reuses it." % (name, MAX_ATTEMPTS - 1))


def read_back(gh, assets_on_release, local_assets):
    """Downloads every payload asset back out of GitHub and checks it against its producer's hash."""
    by_name = dict((asset["name"], asset) for asset in assets_on_release)
    digests, problems = {}, []
    for local in local_assets:
        remote = by_name.get(local["name"])
        if remote is None:
            problems.append("%s is not on the release" % local["name"])
            continue
        data = gh.download_asset(remote["id"])
        digest = sha256_bytes(data)
        if digest != local["sha256"] or len(data) != local["size"]:
            problems.append("%s: GitHub served %d bytes hashing to %s; its producer recorded %d bytes hashing "
                            "to %s" % (local["name"], len(data), digest, local["size"], local["sha256"]))
            continue
        digests[local["name"]] = digest
        say("ok    downloaded back %-45s sha256 %s  (matches its producer)" % (local["name"], digest))
    return digests, problems


def cmd_publish(args, credentials):
    version, _numbers = version_of(args.tag)
    names, payload = payload_names(version)
    say("publish %s: %d assets from scripts/release-manifest.sh (%d uploaded as built, plus %s)"
        % (args.tag, len(names), len(payload), SUMS))
    local_assets = collect_dist(args.dist, payload)
    caveats, notes_source = notes_caveats(args.tag, args.notes_file)
    say("ok    release notes from %s (%d lines)" % (notes_source, len(caveats.splitlines())))

    gh = github_client(credentials, "publish")
    commit = resolve_tag_commit(gh, args.tag)
    expected = args.expect_commit
    if not expected and re.match(r"^[0-9a-f]{40}$", os.environ.get("BUILDKITE_COMMIT", "")):
        expected = os.environ["BUILDKITE_COMMIT"]
    if expected and commit != expected:
        die("tag %s points at %s on GitHub, but this build is for %s: the tag moved after the build started. "
            "Nothing has been published." % (args.tag, commit, expected))
    if expected:
        say("ok    tag %s exists on GitHub at %s, the commit this build is for" % (args.tag, commit))
    else:
        say("ok    tag %s exists on GitHub at %s (SKIP: no --expect-commit or BUILDKITE_COMMIT to compare with)"
            % (args.tag, commit))

    existing = published_release(gh, args.tag)
    if existing is not None:
        return finish_already_published(gh, existing, args.tag, names, local_assets)

    body = caveats.rstrip("\n") + "\n\n" + generate_changelog(gh, args.tag).strip("\n") + "\n"
    fields = {"tag_name": args.tag, "name": args.tag, "body": body,
              "draft": True, "prerelease": True, "make_latest": "false"}
    drafts = [r for r in gh.paged("/repos/%s/releases" % REPO)
              if r.get("draft") and r.get("tag_name") == args.tag]
    if len(drafts) > 1:
        die("%d draft releases exist for %s (ids %s). GitHub allows several drafts per tag, and publish will not "
            "guess which is real: delete the extras in the GitHub UI (drafts are invisible to users, so that "
            "loses nothing) and re-run this step."
            % (len(drafts), args.tag, ", ".join(str(r["id"]) for r in drafts)))
    if drafts:
        _status, release = gh.call("PATCH", "/repos/%s/releases/%d" % (REPO, drafts[0]["id"]), fields)
        say("ok    reusing the draft left by an earlier attempt (release id %d)" % release["id"])
    else:
        # Not retried: a POST that timed out may still have created a draft, and a blind retry would make
        # two. Re-running the step finds and reuses whichever one exists.
        _status, release = gh.call("POST", "/repos/%s/releases" % REPO, fields, expect=(201,), retry=False)
        say("ok    created draft release id %d for %s" % (release["id"], args.tag))
    if not release.get("draft"):
        die("release %d for %s is not a draft after create/update; stopping before any upload" % (release["id"], args.tag))

    for leftover in release_assets(gh, release["id"]):
        say("      removing %s (state %s) from the draft: every asset is uploaded fresh, and only the manifest's "
            "names may remain" % (leftover["name"], leftover.get("state")))
        delete_asset(gh, leftover)

    sizes = {}
    for local in local_assets:
        with open(local["path"], "rb") as handle:
            data = handle.read()
        if sha256_bytes(data) != local["sha256"]:
            die("%s changed on disk while publish was running" % local["path"])
        upload_with_cleanup(gh, release, local["name"], data)
        sizes[local["name"]] = len(data)
        say("ok    uploaded %s (%d bytes)" % (local["name"], len(data)))

    digests, problems = read_back(gh, release_assets(gh, release["id"]), local_assets)
    if problems:
        die("the draft does not hold the bytes that were built:\n  %s\nThe release is still a DRAFT, invisible "
            "to users; re-running release-publish replaces every asset." % "\n  ".join(problems))

    sums = sums_text(digests).encode()
    upload_with_cleanup(gh, release, SUMS, sums)
    sizes[SUMS] = len(sums)
    assets = release_assets(gh, release["id"])
    sums_asset = [a for a in assets if a["name"] == SUMS]
    if len(sums_asset) != 1 or gh.download_asset(sums_asset[0]["id"]) != sums:
        die("%s downloaded back from the draft is not the file that was uploaded" % SUMS)
    say("ok    %s computed from the downloaded bytes, uploaded, and read back identical:" % SUMS)
    for line in sums.decode().splitlines():
        say("      %s" % line)

    assert_asset_set(assets, names, sizes, "draft %d" % release["id"])
    say("ok    the draft holds exactly the %d manifest assets, every one uploaded at the expected size" % len(names))

    _status, flipped = gh.call("PATCH", "/repos/%s/releases/%d" % (REPO, release["id"]),
                               {"draft": False, "prerelease": True, "make_latest": "false"})
    if flipped.get("draft") or not flipped.get("prerelease"):
        die("release %d did not become a published prerelease (draft=%r, prerelease=%r)"
            % (release["id"], flipped.get("draft"), flipped.get("prerelease")))
    latest = latest_release(gh)
    if latest is not None and latest.get("id") == release["id"]:
        die("%s became /releases/latest while still a prerelease; only release-promote may do that. It is "
            "published: a human must decide whether to leave it or fix forward." % args.tag)
    say("")
    say("published %s as a PRERELEASE: %s" % (args.tag, flipped.get("html_url")))
    say("/releases/latest still points at %s until release-promote runs"
        % (latest.get("tag_name") if latest else "nothing (no promoted release exists yet)"))
    return 0


def finish_already_published(gh, release, tag, names, local_assets):
    """A retry after the flip succeeds only if the published prerelease is exactly this build's output."""
    if not release.get("prerelease"):
        die("%s is already published and promoted (not a prerelease): %s. Releases are forward-only and "
            "publish never touches a published release; cut the next version instead."
            % (tag, release.get("html_url")))
    say("a published prerelease already exists for %s (%s); checking, without changing it, whether it is exactly "
        "what this step would have produced" % (tag, release.get("html_url")))
    problems = []
    assets = release_assets(gh, release["id"])
    try:
        assert_asset_set(assets, names, dict((a["name"], a["size"]) for a in local_assets), "prerelease %s" % tag)
    except Refusal as refusal:
        problems.append(str(refusal))
    if not problems:
        digests, problems = read_back(gh, assets, local_assets)
        if not problems:
            sums_asset = [a for a in assets if a["name"] == SUMS][0]
            if gh.download_asset(sums_asset["id"]).decode("utf-8", "replace") != sums_text(digests):
                problems.append("%s on the prerelease is not the file these bytes produce" % SUMS)
    if problems:
        die("the published prerelease for %s does not match this build's artifacts:\n  %s\npublish never modifies "
            "a published release. A human decides: fix forward with a new tag, or delete this prerelease by "
            "hand in the GitHub UI and re-run release-publish." % (tag, "\n  ".join(problems)))
    say("ok    the prerelease for %s already holds exactly this build's assets and %s; nothing to do" % (tag, SUMS))
    return 0


def cmd_promote(args, credentials):
    version, numbers = version_of(args.tag)
    names = manifest(version, "all")
    gh = github_client(credentials, "promote")
    release = published_release(gh, args.tag)
    if release is None:
        die("no published release exists for %s: release-publish has not run, or failed before its flip. "
            "Nothing to promote." % args.tag)
    assert_asset_set(release_assets(gh, release["id"]), names, {}, "release %s" % args.tag)
    say("ok    %s holds exactly the %d manifest assets" % (args.tag, len(names)))

    if release.get("prerelease"):
        latest = latest_release(gh)
        if latest is not None and latest.get("id") != release["id"]:
            match = TAG_RE.match(latest.get("tag_name", ""))
            if match and tuple(int(part) for part in match.groups()) > numbers:
                die("refusing to promote %s: %s is already latest and is newer. Promotion is forward-only."
                    % (args.tag, latest.get("tag_name")))
        _status, promoted = gh.call("PATCH", "/repos/%s/releases/%d" % (REPO, release["id"]),
                                    {"prerelease": False, "make_latest": "true"})
        if promoted.get("prerelease") or promoted.get("draft"):
            die("release %d is still prerelease=%r draft=%r after the promote call"
                % (release["id"], promoted.get("prerelease"), promoted.get("draft")))
        say("ok    %s is no longer a prerelease" % args.tag)
    else:
        say("ok    %s was already promoted; not changing it" % args.tag)

    latest = None
    for attempt in range(1, 5):
        latest = latest_release(gh)
        if latest is not None and latest.get("id") == release["id"]:
            break
        if attempt < 4:
            time.sleep(5)
    if latest is None or latest.get("id") != release["id"]:
        current = latest.get("tag_name") if latest else "nothing"
        match = TAG_RE.match(current) if latest else None
        if match and tuple(int(part) for part in match.groups()) > numbers:
            say("%s is promoted but is not /releases/latest (that is %s, a later release); the Homebrew "
                "formula follows latest, so it is not bumped from here" % (args.tag, current))
            return 0
        die("%s is promoted, but /releases/latest resolves to %s after 20 s. Retry release-promote."
            % (args.tag, current))
    say("ok    /releases/latest resolves to %s: %s" % (args.tag, release.get("html_url")))

    if args.skip_formula:
        say("SKIP  Homebrew formula bump: --skip-formula")
        return 0
    bump_formula(gh, args.tag, version, release, FORMULA_TEMPLATE)
    return 0


# ---------------------------------------------------------------------------------------------------
# Homebrew


def render_formula(template_path, tag, version, digests):
    with open(template_path, "r", encoding="utf-8") as handle:
        text = handle.read()
    values = {"@VERSION@": version, "@TAG@": tag, "@REPO@": REPO}
    for name in manifest(version, "agents"):
        prefix, suffix = "pessimal-agent-%s-" % version, ".tar.gz"
        if not (name.startswith(prefix) and name.endswith(suffix)):
            die("cannot derive a triple from agent asset %s" % name)
        if name not in digests:
            die("%s has no line in %s" % (name, SUMS))
        triple = name[len(prefix):-len(suffix)]
        values["@SHA256_%s@" % triple.upper().replace("-", "_")] = digests[name]
    app = manifest(version, "app")[0]
    if app in digests:
        values["@SHA256_MACOS_APP@"] = digests[app]
    for placeholder, value in values.items():
        text = text.replace(placeholder, value)
    leftover = sorted(set(PLACEHOLDER_RE.findall(text)))
    if leftover:
        die("%s still has placeholder(s) after rendering: %s (known: %s)"
            % (template_path, ", ".join(leftover), ", ".join(sorted(values))))
    return text


def bump_formula(gh, tag, version, release, template_path):
    """Best-effort. Returns True when the tap holds the formula for this release, False otherwise."""
    manual = ("bump by hand: render %s for %s and commit it as %s in https://github.com/%s"
              % (os.path.relpath(template_path, ROOT), tag, FORMULA_PATH, TAP_REPO))
    try:
        if not os.path.isfile(template_path):
            warn("no formula template at %s; skipping the Homebrew bump. %s" % (template_path, manual))
            return False
        sums_assets = [a for a in release_assets(gh, release["id"]) if a["name"] == SUMS]
        if len(sums_assets) != 1:
            die("the release has %d %s assets" % (len(sums_assets), SUMS))
        digests = parse_sums(gh.download_asset(sums_assets[0]["id"]).decode("utf-8"), "%s on %s" % (SUMS, tag))
        rendered = render_formula(template_path, tag, version, digests)
        path = "/repos/%s/contents/%s" % (TAP_REPO, urllib.parse.quote(FORMULA_PATH))
        for attempt in range(2):
            status, current = gh.call("GET", path, allow=(404,))
            payload = {"content": base64.b64encode(rendered.encode()).decode()}
            if status == 404:
                payload["message"] = "pessimal-agent %s (new formula)" % version
            else:
                if base64.b64decode(current["content"]).decode("utf-8") == rendered:
                    say("ok    %s/%s is already current for %s; no bump needed" % (TAP_REPO, FORMULA_PATH, tag))
                    return True
                payload["message"] = "pessimal-agent: update to %s" % version
                payload["sha"] = current["sha"]
            put_status, _headers, body = gh.request("PUT", path, data=json.dumps(payload).encode())
            if put_status in (200, 201):
                commit = json.loads(body.decode("utf-8")).get("commit", {}).get("sha", "")[:9]
                say("ok    %s %s/%s for %s: commit %s"
                    % ("created" if status == 404 else "updated", TAP_REPO, FORMULA_PATH, tag, commit))
                return True
            # 409: the file changed under us. 422: it was created between the GET and the PUT. Both mean
            # "read it again", once.
            if put_status in (409, 422) and attempt == 0:
                warn("the tap formula changed concurrently (HTTP %d); re-reading once" % put_status)
                continue
            die("PUT %s answered HTTP %d: %s" % (FORMULA_PATH, put_status, _excerpt(body)))
        die("the tap formula update conflicted twice")
    except Exception as exc:  # best-effort means every failure, including the unforeseen ones
        warn("Homebrew formula bump FAILED: %s: %s" % (type(exc).__name__, redact(exc)))
        warn("the release itself is published and promoted; only the tap is stale for %s. %s" % (tag, manual))
        return False


def cmd_bump_formula(args, credentials):
    version, _numbers = version_of(args.tag)
    template = args.template or FORMULA_TEMPLATE
    gh = github_client(credentials, "bump-formula")
    release = published_release(gh, args.tag)
    if release is None or release.get("prerelease"):
        die("%s is not a promoted release; the formula only ever points at a release that passed release-verify"
            % args.tag)
    latest = latest_release(gh)
    if latest is None or latest.get("id") != release["id"]:
        die("%s is not /releases/latest (that is %s); bumping the formula to it would move the tap backwards"
            % (args.tag, latest.get("tag_name") if latest else "nothing"))
    return 0 if bump_formula(gh, args.tag, version, release, template) else 1


# ---------------------------------------------------------------------------------------------------


def parse_args(argv):
    parser = argparse.ArgumentParser(
        prog="release-github.py",
        description="Publish, promote, and bump the Homebrew formula for a Pessimal GitHub release.")
    commands = parser.add_subparsers(dest="command", metavar="{publish,promote,bump-formula}")
    commands.required = True

    publish = commands.add_parser("publish", help="draft, upload, verify, SHA256SUMS, prerelease")
    publish.add_argument("--tag", required=True, help="the release tag, e.g. v0.2.0")
    publish.add_argument("--dist", required=True,
                         help="directory holding every manifest asset and its producer .sha256 (searched recursively)")
    publish.add_argument("--notes-file",
                         help="release notes to use instead of `scripts/release-notes.sh TAG --no-api`; "
                              "GitHub's generated changelog is appended either way")
    publish.add_argument("--expect-commit",
                         help="refuse unless the tag points at this commit (default: BUILDKITE_COMMIT when set)")

    promote = commands.add_parser("promote", help="prerelease -> latest, then the best-effort formula bump")
    promote.add_argument("--tag", required=True, help="the release tag, e.g. v0.2.0")
    promote.add_argument("--skip-formula", action="store_true", help="do not touch the Homebrew tap")

    bump = commands.add_parser("bump-formula", help="render and push Formula/pessimal-agent.rb for a promoted release")
    bump.add_argument("--tag", required=True, help="the release tag, e.g. v0.2.0")
    bump.add_argument("--template", help="formula template (default packaging/homebrew/pessimal-agent.rb.template)")
    return parser.parse_args(argv)


def main(argv):
    # Before argument parsing, before anything can spawn a child: from here on no process this starts can
    # inherit a credential, and every message is redacted against them.
    credentials = take_credentials()
    if sys.version_info < (3, 9):
        sys.stderr.write("release-github: Python 3.9 or newer is required (this is %s); the release steps run "
                         "python3 from %s\n" % (sys.version.split()[0], GUEST_IMAGE))
        return 2
    args = parse_args(argv)
    try:
        if shutil.which("bash") is None:
            die("bash is required (it runs scripts/release-manifest.sh) and is not on PATH in %s" % GUEST_IMAGE)
        require_credentials(credentials, args.command)
        handler = {"publish": cmd_publish, "promote": cmd_promote, "bump-formula": cmd_bump_formula}[args.command]
        return handler(args, credentials)
    except Refusal as refusal:
        sys.stderr.write(redact("release-github %s: REFUSED: %s" % (args.command, refusal)) + "\n")
        return 1
    except TransportError as exc:
        sys.stderr.write(redact("release-github %s: GitHub could not be reached: %s" % (args.command, exc)) + "\n")
        return 1
    except Exception:  # an unexpected failure is still printed, and still redacted
        sys.stderr.write(redact("release-github %s: unexpected failure:\n%s" % (args.command, traceback.format_exc())))
        return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
