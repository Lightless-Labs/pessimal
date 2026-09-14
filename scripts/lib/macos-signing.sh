# shellcheck shell=bash
# The Apple credential path, in one place: fetch, forget the token, open an ephemeral keychain, sign,
# notarize, tear everything down. Sourced — never run.
#
#   source "$ROOT_DIR/scripts/lib/macos-signing.sh"
#   macos_signing_require_release_tools  # at the TOP of a caller, before any work
#   macos_signing_fetch_secrets          # also unsets DOPPLER_TOKEN
#   macos_signing_open_keychain          # installs the cleanup trap
#   macos_signing_codesign_binary path com.example.id
#   macos_signing_notary_submit some.zip
#
# It is a library because there are several callers (release-macos-app.sh by hand,
# release-macos-artifacts.sh in the Buildkite `release-macos` step, notarize-macos-{app,binary}.sh as
# children) and the credential mistakes are the ones you only get to make once. Keeping them in one
# file means a fix lands for everybody rather than for whichever copy somebody remembered.
#
# Where it runs in CI: the `release-macos` step, inside a Tart guest started by the tart-ci plugin on
# the ci-macos-apple-silicon queue. The plugin resolves the Buildkite secret
# DOPPLER_SERVICE_ACCOUNT_TOKEN (the Doppler service account token) and exports it into the
# guest as DOPPLER_TOKEN. The guest has no `doppler` CLI and no Buildkite agent token, so nothing in the
# guest can redact a value from the log after the fact. The only defence is that no secret is ever
# printed, echoed or traced here, and none may be.
#
# Six decisions worth knowing before editing:
#
# 1. Sourcing this file installs nothing. No fetch, no keychain, no trap. That matters because the
#    child scripts source it too: if the trap were installed at source time, a child exiting normally
#    would delete its parent's keychain and its parent's decoded .p8 halfway through a release. The
#    trap is installed by macos_signing_open_keychain, so only the process that created the keychain
#    destroys it. Children inherit the *state* through exported CODESIGN_IDENTITY / CODESIGN_KEYCHAIN /
#    APPLE_NOTARY_* and the *behaviour* through these functions.
#
#    The one thing sourcing does do is stop EXPORTING DOPPLER_TOKEN (see 2), so children do not inherit
#    it. It does NOT remove the token from the starting environment of the process that received it:
#    any process of the same user can still read that with `ps -E`. So `export -n` protects nothing
#    against code that runs during a build. The release therefore builds in one Buildkite step
#    (release-macos-build, no token) and signs in another (release-macos), where no built code runs.
#    A hand run of release-macos-app.sh builds and signs in one process, on your own machine.
#
# 2. DOPPLER_TOKEN is unset as the last act of the fetch. The token opens the whole config; once the
#    five values are in hand nothing downstream needs it, and what is downstream is codesign, ditto,
#    notarytool and the packager. scripts/release-ios-testflight-buildkite.sh:146 already does this;
#    the macOS path did not, which is the defect this library exists to fix. For the same reason the
#    three raw key blobs are unset once the keychain holds the identity and the .p8 is on disk.
#
# 3. Everything that talks to a keychain names the keychain. A default search list is somebody else's
#    state and it can be wrong in ways that are invisible — the machine this was written on has a user
#    search list whose entries read, verbatim, like:
#        "/Library/Keychains/    /Users/thomas/Library/Keychains/login.keychain-db"
#    That is `/Library/Keychains/` + four spaces + a real path: the list was rebuilt from
#    `security list-keychains`'s INDENTED output with the indentation kept, so each entry became a
#    relative path. The old release-macos-app.sh cleanup did exactly that (`tr -d '"'`, then
#    `IFS= read -r`), and its setup had the opposite bug — an unquoted `-s $ORIGINAL_USER_KEYCHAINS`
#    that word-splits any path containing a space. The reader below strips the indentation and the
#    quotes, keeps internal spaces, and is expanded as a quoted array; it fixes both. It restores what
#    it found, corrupt or not: repairing a host's keychain configuration is not a release's decision.
#
# 4. "It verifies" is not "we signed it". The arm64 agent slice arrives ad-hoc signed by Apple's linker,
#    and `codesign --verify --strict` passes an ad-hoc signature. So a slice whose Developer ID signing
#    silently did nothing would still verify. macos_signing_assert_developer_id reads the signature back
#    and insists on the authority, the hardened runtime and the secure timestamp.
#
# 5. A fresh Tart guest is not a developer's Mac. It carries Apple's roots but not the Developer ID
#    intermediate that issued our leaf, and codesign does not fetch a missing issuer. Descartes measured
#    the failure on this cluster (its build #63, "unable to build chain", with the intermediate sitting
#    in the ephemeral keychain) and the fix it ships is copied here: download the matching intermediate
#    from Apple's PKI, check its subject, import it into the ephemeral keychain, and also add it to the
#    System keychain when passwordless sudo exists, because trustd does not reliably consult a
#    session-modified search list. That adds one public certificate and changes no trust setting.
#    scripts/release-ios-testflight-buildkite.sh imports the WWDR intermediates for the same reason.
#    For the same reason the identity is looked up WITHOUT `-v`: validity is evaluated against a chain
#    the new keychain may not yet be able to build, so "valid" is reported as a warning, never a gate.
#    The guest clock is nudged first, best effort, because the timestamp server, certificate validity
#    and the notary service all compare against it and a resumed VM's clock drifts.
#
# 6. Notarisation is judged by what Apple says, twice over, and never by an exit code. See
#    macos_signing_notary_submit.
#
# Required secrets, named identically to the sibling project Descartes so one naming scheme serves
# both rather than drifting into two:
#   MACOS_DEVELOPER_ID_CERT_P12_BASE64   Developer ID Application cert + key, as base64 of a .p12
#   MACOS_DEVELOPER_ID_CERT_PASSWORD     the .p12's password
#   APPLE_NOTARY_KEY_ID                  App Store Connect API key id
#   APPLE_NOTARY_ISSUER_ID               App Store Connect issuer id
#   APPLE_NOTARY_KEY_P8_BASE64           the .p8 private key, base64

# Refuse to be executed. A library run directly would fetch nothing, sign nothing and exit 0, which
# is the most expensive kind of success.
if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
  echo "error: scripts/lib/macos-signing.sh is a library; source it, do not run it" >&2
  exit 2
fi

# Decision 1: children do not inherit the token. It stays in this process's starting environment.
export -n DOPPLER_TOKEN

MACOS_SIGNING_ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# Named in every missing-tool message. Nobody has listed what this image contains — the probe that
# would have done so could not be read back — so a missing tool must say which image lacked it.
MACOS_SIGNING_CI_IMAGE="${PESSIMAL_CI_IMAGE:-ci-macos-rust-bazel-ios-20260910-v2}"
# The five secret names, in the order they are fetched.
MACOS_SIGNING_SECRET_NAMES=(
  MACOS_DEVELOPER_ID_CERT_P12_BASE64
  MACOS_DEVELOPER_ID_CERT_PASSWORD
  APPLE_NOTARY_KEY_ID
  APPLE_NOTARY_ISSUER_ID
  APPLE_NOTARY_KEY_P8_BASE64
)
# Declared empty at source time so the cleanup trap is safe to run under `set -u` even if it
# fires between the trap being installed and the snapshot being taken. Every expansion of it is
# guarded by a count: this runs under macOS's bash 3.2, where "${empty[@]}" is an unbound-variable
# error under `set -u`.
MACOS_SIGNING_ORIGINAL_KEYCHAINS=()

macos_signing_fail() { echo "error: $*" >&2; exit 2; }

macos_signing_missing_tool() {
  macos_signing_fail "$1 is required and was not found${2:+ ($2)}. In CI the release-macos step runs" \
    "in the $MACOS_SIGNING_CI_IMAGE Tart guest image, whose contents were never measured: add $1 to" \
    "that image (or install it on this Mac for a hand run)."
}

macos_signing_require_tools() {
  local tool
  for tool in "$@"; do
    command -v "$tool" >/dev/null 2>&1 || macos_signing_missing_tool "$tool"
  done
}

# `command -v xcrun` proves nothing about notarytool or stapler: xcrun is a shim that is present
# whenever the command line tools are, and the subcommands come from whichever Xcode is selected.
macos_signing_require_xcrun_tools() {
  local tool
  for tool in "$@"; do
    xcrun --find "$tool" >/dev/null 2>&1 || macos_signing_missing_tool "xcrun $tool"
  done
}

# /usr/bin/python3 on macOS is a stub that exists with or without an interpreter behind it, so ask it
# to run rather than asking whether it is on PATH.
macos_signing_require_python3() {
  macos_signing_require_tools python3
  python3 -c 'import base64, json, urllib.request' >/dev/null 2>&1 \
    || macos_signing_missing_tool python3 "a python3 is on PATH but does not run"
}

# Everything a signed, notarized release needs, checked before any of it starts: a tool found missing
# after a secret fetch or a notary round trip is a tool found missing too late.
macos_signing_require_release_tools() {
  [[ "$(uname -s)" == "Darwin" ]] || macos_signing_fail "macOS signing must run on macOS"
  macos_signing_require_tools security openssl base64 curl codesign ditto spctl shasum tar xattr \
    xcrun /usr/libexec/PlistBuddy
  macos_signing_require_python3
  macos_signing_require_xcrun_tools notarytool stapler
}

# --- secrets -----------------------------------------------------------------

# One secret, by name, into a variable of that name. Fetched one at a time rather than via
# `doppler run` or a bulk download, so a missing secret names itself instead of surfacing later as an
# empty variable and an unsigned app.
#
# Three sources, in order. The REST call is first and is not a convenience: in the Buildkite
# `release-macos` step the tart-ci plugin injects DOPPLER_TOKEN and nothing else, and the guest has no
# `doppler` CLI — the same situation scripts/release-ios-testflight-buildkite.sh:6-9 describes — so
# there the REST path is the only path. The CLI stays second for the hand-run case of a developer
# already logged in.
macos_signing_fetch_secret() {
  local name="$1" optional="${2:-}" value=""
  local project="${DOPPLER_PROJECT:-lightless-labs-pessimal}"
  local config="${DOPPLER_CONFIG:-prd_macos_notarisation}"

  if [[ -n "${DOPPLER_TOKEN:-}" ]]; then
    macos_signing_require_python3
    # Same REST reader as scripts/release-ios-testflight-buildkite.sh:69-128, deliberately: one shape
    # for both release paths, and it is the shape that has already been run against this API.
    #
    # The token goes to the child through the environment, never on argv -- an `Authorization:` header
    # spelled as a curl argument is visible to `ps` for the whole life of the request. `computed` is
    # the value after any secret reference is resolved, which is what the CLI's --plain prints; the
    # body is parsed in the child and only the value is ever written to a pipe. An error prints the
    # HTTP status and Doppler's error body, which names the problem and never carries a value.
    value="$(
      DOPPLER_BOOTSTRAP_TOKEN="$DOPPLER_TOKEN" \
      DOPPLER_PROJECT_NAME="$project" \
      DOPPLER_CONFIG_NAME="$config" \
      DOPPLER_SECRET_NAME="$name" \
      python3 <<'PY' || true
import base64, json, os, sys, urllib.error, urllib.parse, urllib.request

name = os.environ["DOPPLER_SECRET_NAME"]
token = os.environ["DOPPLER_BOOTSTRAP_TOKEN"]
qs = urllib.parse.urlencode({
    "project": os.environ["DOPPLER_PROJECT_NAME"],
    "config": os.environ["DOPPLER_CONFIG_NAME"],
    "name": name,
})
req = urllib.request.Request(
    f"https://api.doppler.com/v3/configs/config/secret?{qs}",
    headers={"Authorization": "Basic " + base64.b64encode(f"{token}:".encode()).decode(),
             "Accept": "application/json",
             "User-Agent": "pessimal-macos-release/1"},
)
try:
    with urllib.request.urlopen(req, timeout=20) as res:
        payload = json.load(res)
except urllib.error.HTTPError as exc:
    print(f"Doppler read failed for {name}: HTTP {exc.code}: {exc.read(300).decode(errors='replace')}",
          file=sys.stderr)
    sys.exit(2)
except Exception as exc:
    print(f"Doppler read failed for {name}: {exc}", file=sys.stderr)
    sys.exit(2)

value = payload.get("value", {})
secret = value.get("computed") or value.get("raw") or value.get("value") if isinstance(value, dict) else value
if not isinstance(secret, str) or not secret:
    print(f"Doppler returned no value for {name}", file=sys.stderr)
    sys.exit(2)
print(secret, end="")
PY
    )"
  elif command -v doppler >/dev/null 2>&1; then
    value="$(doppler secrets get "$name" --plain \
      --project "$project" --config "$config" 2>/dev/null || true)"
  fi
  # Fall back to the environment whenever Doppler did not supply it -- not only when doppler is
  # absent. Having the CLI installed but pointed at the wrong project is the common case, and
  # treating that as "unset" would tell a developer who had exported the value themselves that
  # their own environment was empty.
  if [[ -z "$value" ]]; then
    value="${!name:-}"
  fi
  if [[ -z "$value" ]]; then
    [[ -n "$optional" ]] && return 0
    if [[ -z "${DOPPLER_TOKEN:-}" ]]; then
      # Named first because it is the cause, not the symptom: without the token nothing was asked.
      macos_signing_fail "$name could not be read: DOPPLER_TOKEN is not set, no logged-in doppler CLI" \
        "supplied it, and it is not in the environment. In the Buildkite release-macos step the tart-ci" \
        "plugin's doppler_token_secret delivers DOPPLER_TOKEN from the cluster secret" \
        "DOPPLER_SERVICE_ACCOUNT_TOKEN."
    fi
    macos_signing_fail "$name is not set (looked in Doppler $project/$config, then the environment)"
  fi
  printf -v "$name" '%s' "$value"
  export "${name?}"
}

macos_signing_fetch_secrets() {
  local name
  for name in "${MACOS_SIGNING_SECRET_NAMES[@]}"; do
    macos_signing_fetch_secret "$name"
  done

  # The service token's job is done. It must not reach codesign, notarytool, the packager, or anything
  # any of them spawns: it opens the whole config, while everything below needs only what was just read.
  unset DOPPLER_TOKEN
}

macos_signing_base64_decode() {
  # macOS base64 predates --decode in some releases; -D is the portable spelling here.
  if base64 --help 2>&1 | grep -q -- '--decode'; then base64 --decode; else base64 -D; fi
}

# --- the user keychain search list -------------------------------------------

# `security list-keychains` output on stdin, one bare path per line out: the leading indent and the
# surrounding quotes are stripped and nothing else is touched. Internal spaces are preserved, which is
# the whole point — a path with a space in it must stay one list entry. Split from the reader below so
# notarize-macos-binary.sh --self-test can prove it on a fixture without touching a real keychain list.
macos_signing_parse_keychain_list() {
  sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//' -e 's/^"//' -e 's/"$//' | grep -v '^$' || true
}

macos_signing_user_keychain_list() {
  security list-keychains -d user 2>/dev/null | macos_signing_parse_keychain_list
}

# --- cleanup, unconditionally ------------------------------------------------
# A leaked signing keychain or a .p8 left on disk is a credential left behind, so this runs on
# success, failure and interrupt alike — a notarisation failure included, because a child script's
# non-zero exit ends the caller through `set -e`, and the caller's EXIT trap is this.
#
# A caller that needs its own cleanup defines macos_signing_cleanup_hook rather than setting its own
# trap. A second `trap … EXIT` would REPLACE this one, and the keychain and both key files would then
# outlive the run with nothing reporting it.
macos_signing_cleanup() {
  if [[ ${#MACOS_SIGNING_ORIGINAL_KEYCHAINS[@]} -gt 0 ]]; then
    security list-keychains -d user -s "${MACOS_SIGNING_ORIGINAL_KEYCHAINS[@]}" >/dev/null 2>&1 || true
  fi
  if [[ -n "${MACOS_SIGNING_KEYCHAIN_PATH:-}" ]]; then
    security delete-keychain "$MACOS_SIGNING_KEYCHAIN_PATH" >/dev/null 2>&1 || true
    rm -f "$MACOS_SIGNING_KEYCHAIN_PATH"
  fi
  rm -f "${MACOS_SIGNING_CERT_PATH:-}" "${MACOS_SIGNING_NOTARY_KEY_PATH:-}" \
    "${MACOS_SIGNING_INTERMEDIATE_PATH:-}" "${MACOS_SIGNING_CERT_BUNDLE_PATH:-}"
  if declare -F macos_signing_cleanup_hook >/dev/null; then
    macos_signing_cleanup_hook || true
  fi
}

# EXIT covers success and `set -e` failures. INT and TERM exit explicitly after cleaning up: a signal
# trap that only cleans up hands control back to the script, which would carry on signing against a
# keychain that no longer exists.
macos_signing_install_cleanup_trap() {
  trap macos_signing_cleanup EXIT
  trap 'macos_signing_cleanup; trap - EXIT; exit 130' INT
  trap 'macos_signing_cleanup; trap - EXIT; exit 143' TERM
}

# --- the guest clock ---------------------------------------------------------
# Decision 5. Best effort and silent on failure, exactly as Descartes does it: without passwordless
# sudo (a developer's Mac) nothing happens, and the two timestamps still land in the log.
macos_signing_sync_clock() {
  echo "Clock (UTC) before time sync: $(date -u '+%Y-%m-%dT%H:%M:%SZ')"
  if command -v sntp >/dev/null 2>&1 && command -v sudo >/dev/null 2>&1; then
    sudo -n sntp -sS time.apple.com >/dev/null 2>&1 || true
  fi
  echo "Clock (UTC) after time sync:  $(date -u '+%Y-%m-%dT%H:%M:%SZ')"
}

# --- certificates in the ephemeral keychain ----------------------------------

# Queries a PEM bundle. openssl reads only the first certificate of a PEM stream, so the bundle is
# split in Python and each certificate asked separately. Public certificates only; nothing here
# touches a private key.
#   leaf-issuer <bundle>             the issuer of the first Developer ID Application certificate
#   has-subject <bundle> <cn> <ou>   exit 0 when some certificate's subject holds both fragments
macos_signing_pem_query() {
  python3 - "$@" <<'PY'
import re, subprocess, sys

mode, path = sys.argv[1], sys.argv[2]
with open(path, "r") as f:
    certs = re.findall(r"-----BEGIN CERTIFICATE-----\n[\s\S]*?\n-----END CERTIFICATE-----", f.read())

def field(cert, which):
    out = subprocess.check_output(
        ["openssl", "x509", "-noout", which, "-nameopt", "sep_comma_plus_space"],
        input=cert.encode(), stderr=subprocess.DEVNULL,
    ).decode().strip()
    return re.sub(r"^(subject|issuer)\s*=\s*", "", out)

for cert in certs:
    try:
        subject = field(cert, "-subject")
    except Exception:
        continue
    if mode == "leaf-issuer" and "Developer ID Application" in subject:
        sys.stdout.write(field(cert, "-issuer"))
        sys.exit(0)
    if mode == "has-subject" and sys.argv[3] in subject and sys.argv[4] in subject:
        sys.exit(0)
sys.exit(1)
PY
}

# `security find-certificate -a -p` exits 0 when nothing matches, so presence is judged by the output.
macos_signing_keychain_has_subject() {
  local keychain="$1" cn="$2" ou="$3" found=1
  if security find-certificate -a -p "$keychain" > "$MACOS_SIGNING_CERT_BUNDLE_PATH" 2>/dev/null \
      && [[ -s "$MACOS_SIGNING_CERT_BUNDLE_PATH" ]] \
      && macos_signing_pem_query has-subject "$MACOS_SIGNING_CERT_BUNDLE_PATH" "$cn" "$ou"; then
    found=0
  fi
  rm -f "$MACOS_SIGNING_CERT_BUNDLE_PATH"
  return "$found"
}

# Decision 5, ported from Descartes' import_developer_id_intermediate. Apple publishes both
# generations of the Developer ID CA: leaves issued since about 2021 chain through G2 (OU=G2), older
# ones through the original (OU=Apple Certification Authority). The leaf's own issuer picks which.
macos_signing_import_intermediate() {
  local keychain="$1" issuer="" ou url subject
  local cn="CN=Developer ID Certification Authority"

  if security find-certificate -a -p "$keychain" > "$MACOS_SIGNING_CERT_BUNDLE_PATH" 2>/dev/null \
      && [[ -s "$MACOS_SIGNING_CERT_BUNDLE_PATH" ]]; then
    issuer="$(macos_signing_pem_query leaf-issuer "$MACOS_SIGNING_CERT_BUNDLE_PATH" || true)"
  fi
  rm -f "$MACOS_SIGNING_CERT_BUNDLE_PATH"
  [[ -n "$issuer" ]] || macos_signing_fail \
    "no Developer ID Application certificate in the imported .p12, so there is no issuer to chain to"
  echo "Developer ID leaf issuer: $issuer"
  [[ "$issuer" == *"$cn"* ]] || macos_signing_fail \
    "unexpected Developer ID leaf issuer; cannot select an Apple intermediate: $issuer"

  if [[ "$issuer" == *"OU=G2"* ]]; then
    ou="OU=G2"
    url="https://www.apple.com/certificateauthority/DeveloperIDG2CA.cer"
  else
    ou="OU=Apple Certification Authority"
    url="https://www.apple.com/certificateauthority/DeveloperIDCA.cer"
  fi

  if macos_signing_keychain_has_subject "$keychain" "$cn" "$ou"; then
    echo "Developer ID intermediate already in the ephemeral keychain ($ou)"
  else
    echo "Downloading the Developer ID intermediate from Apple PKI: $url"
    curl -fsSL --max-time 30 "$url" -o "$MACOS_SIGNING_INTERMEDIATE_PATH" \
      || macos_signing_fail "could not download the Developer ID intermediate from $url"
    subject="$(openssl x509 -inform DER -in "$MACOS_SIGNING_INTERMEDIATE_PATH" -noout -subject \
      -nameopt sep_comma_plus_space 2>/dev/null || true)"
    [[ "$subject" == *"$cn"* && "$subject" == *"$ou"* ]] || macos_signing_fail \
      "the downloaded intermediate does not match the leaf's issuer: ${subject:-<unparseable>}"
    security import "$MACOS_SIGNING_INTERMEDIATE_PATH" -k "$keychain" -T /usr/bin/codesign >/dev/null
    macos_signing_keychain_has_subject "$keychain" "$cn" "$ou" \
      || macos_signing_fail "the Developer ID intermediate import did not land in the ephemeral keychain"
    echo "Imported the Developer ID intermediate into the ephemeral keychain ($ou)"
  fi

  # Descartes' build #63: in the ephemeral keychain is not always enough for trustd. Checked against
  # the System keychain whatever the route above, and only ever an addition of a public certificate.
  if macos_signing_keychain_has_subject /Library/Keychains/System.keychain "$cn" "$ou"; then
    echo "Developer ID intermediate already in the System keychain ($ou)"
  elif [[ -s "$MACOS_SIGNING_INTERMEDIATE_PATH" ]] && command -v sudo >/dev/null 2>&1 && sudo -n true 2>/dev/null; then
    if sudo -n security add-certificates -k /Library/Keychains/System.keychain "$MACOS_SIGNING_INTERMEDIATE_PATH" 2>/dev/null \
        && macos_signing_keychain_has_subject /Library/Keychains/System.keychain "$cn" "$ou"; then
      echo "Installed the Developer ID intermediate into the System keychain ($ou)"
    else
      echo "warning: could not add the Developer ID intermediate to the System keychain; codesign may fail to build the chain" >&2
    fi
  else
    echo "warning: the Developer ID intermediate is only in the ephemeral keychain (no passwordless sudo, or it came from the .p12)" >&2
  fi
  rm -f "$MACOS_SIGNING_INTERMEDIATE_PATH"
}

# --- an ephemeral keychain ---------------------------------------------------
# Decodes the credentials, builds a throwaway keychain, imports the identity and exports the three
# variables the notarize scripts read. Installs the cleanup trap; call it exactly once per process.
macos_signing_open_keychain() {
  macos_signing_require_tools security openssl base64 codesign curl
  macos_signing_require_python3

  local build_root="${PESSIMAL_RELEASE_BUILD_DIR:-$MACOS_SIGNING_ROOT_DIR/.build/macos/release-signing}"
  mkdir -p "$build_root"

  MACOS_SIGNING_CERT_PATH="$build_root/developer-id.p12"
  MACOS_SIGNING_NOTARY_KEY_PATH="$build_root/AuthKey_${APPLE_NOTARY_KEY_ID}.p8"
  MACOS_SIGNING_KEYCHAIN_PATH="$build_root/pessimal-signing.keychain-db"
  MACOS_SIGNING_INTERMEDIATE_PATH="$build_root/developer-id-intermediate.cer"
  MACOS_SIGNING_CERT_BUNDLE_PATH="$build_root/keychain-certs.pem"
  local keychain_password
  keychain_password="$(openssl rand -base64 48)"

  # A keychain left at this path by a killed earlier run (SIGKILL runs no trap) would make
  # create-keychain fail, or worse be inherited. Removed first, and never snapshotted below.
  security delete-keychain "$MACOS_SIGNING_KEYCHAIN_PATH" >/dev/null 2>&1 || true
  rm -f "$MACOS_SIGNING_KEYCHAIN_PATH"

  # Snapshot the search list into an array BEFORE the trap exists, so an interrupt one line later
  # still restores something. See decision 3 for why this is a parsed array and not a string.
  MACOS_SIGNING_ORIGINAL_KEYCHAINS=()
  local entry
  while IFS= read -r entry; do
    if [[ -n "$entry" && "$entry" != "$MACOS_SIGNING_KEYCHAIN_PATH" ]]; then
      MACOS_SIGNING_ORIGINAL_KEYCHAINS+=("$entry")
    fi
  done < <(macos_signing_user_keychain_list)

  macos_signing_install_cleanup_trap

  macos_signing_sync_clock

  printf '%s' "$MACOS_DEVELOPER_ID_CERT_P12_BASE64" | macos_signing_base64_decode > "$MACOS_SIGNING_CERT_PATH"
  printf '%s' "$APPLE_NOTARY_KEY_P8_BASE64" | macos_signing_base64_decode > "$MACOS_SIGNING_NOTARY_KEY_PATH"
  chmod 0600 "$MACOS_SIGNING_CERT_PATH" "$MACOS_SIGNING_NOTARY_KEY_PATH"
  [[ -s "$MACOS_SIGNING_CERT_PATH" ]] || macos_signing_fail "the decoded .p12 is empty; check MACOS_DEVELOPER_ID_CERT_P12_BASE64"
  [[ -s "$MACOS_SIGNING_NOTARY_KEY_PATH" ]] || macos_signing_fail "the decoded .p8 is empty; check APPLE_NOTARY_KEY_P8_BASE64"

  security create-keychain -p "$keychain_password" "$MACOS_SIGNING_KEYCHAIN_PATH"
  # Long enough for a notarization round trip; the default 5 minutes relocks mid-submission.
  security set-keychain-settings -lut 21600 "$MACOS_SIGNING_KEYCHAIN_PATH"
  security unlock-keychain -p "$keychain_password" "$MACOS_SIGNING_KEYCHAIN_PATH"

  # The private key must be in the .p12: a leaf certificate without its key forms no identity.
  security import "$MACOS_SIGNING_CERT_PATH" \
    -k "$MACOS_SIGNING_KEYCHAIN_PATH" \
    -P "$MACOS_DEVELOPER_ID_CERT_PASSWORD" \
    -T /usr/bin/codesign \
    -T /usr/bin/security >/dev/null

  macos_signing_import_intermediate "$MACOS_SIGNING_KEYCHAIN_PATH"

  # Without this, codesign blocks on a GUI prompt for keychain access and the build hangs rather than
  # failing — the single most confusing way for unattended signing to go wrong.
  security set-key-partition-list -S apple-tool:,apple:,codesign: \
    -s -k "$keychain_password" "$MACOS_SIGNING_KEYCHAIN_PATH" >/dev/null 2>&1

  # Put the ephemeral keychain on the user search list, FIRST, as Descartes does: codesign and trustd
  # build the chain from the list even when --keychain names where the identity is. Quoted array
  # expansion, not `-s $LIST`; see decision 3. It is not made the default keychain.
  if [[ ${#MACOS_SIGNING_ORIGINAL_KEYCHAINS[@]} -gt 0 ]]; then
    security list-keychains -d user -s "$MACOS_SIGNING_KEYCHAIN_PATH" "${MACOS_SIGNING_ORIGINAL_KEYCHAINS[@]}" >/dev/null
  else
    security list-keychains -d user -s "$MACOS_SIGNING_KEYCHAIN_PATH" >/dev/null
  fi

  # The keychain is named explicitly. `find-identity` with no keychain argument searches the ambient
  # list, which on a corrupted host answers about certificates nobody asked for — or about none.
  # No `-v` (decision 5). No `exit` in the awk: an early exit can SIGPIPE `security`, and under
  # pipefail that aborts the run.
  local identity="${CODESIGN_IDENTITY:-}"
  if [[ -z "$identity" ]]; then
    identity="$(security find-identity -p codesigning "$MACOS_SIGNING_KEYCHAIN_PATH" \
      | awk -F'"' '!found && /Developer ID Application: / {print $2; found = 1}')"
  fi
  if [[ -z "$identity" ]]; then
    echo "Codesigning identities in the ephemeral keychain, including invalid ones:" >&2
    security find-identity -p codesigning "$MACOS_SIGNING_KEYCHAIN_PATH" >&2 || true
    macos_signing_fail "no Developer ID Application identity in the imported .p12 — it must hold the certificate AND its private key"
  fi
  # Captured first and grepped second: `security … | grep -q` can SIGPIPE security, and under pipefail
  # that would report a valid identity as invalid.
  local valid_identities
  valid_identities="$(security find-identity -v -p codesigning "$MACOS_SIGNING_KEYCHAIN_PATH" 2>/dev/null || true)"
  if grep -qF "\"$identity\"" <<< "$valid_identities"; then
    echo "The identity is reported valid in the ephemeral keychain"
  else
    echo "warning: '$identity' is not reported valid yet; codesign will say whether it can build the chain" >&2
    security find-identity -p codesigning "$MACOS_SIGNING_KEYCHAIN_PATH" >&2 || true
  fi

  # The keychain holds the identity and the .p8 is on disk, so the raw blobs have no reader left.
  # Unexported here, nothing this process spawns inherits a copy of the private keys.
  unset MACOS_DEVELOPER_ID_CERT_P12_BASE64 MACOS_DEVELOPER_ID_CERT_PASSWORD APPLE_NOTARY_KEY_P8_BASE64

  export CODESIGN_IDENTITY="$identity"
  export CODESIGN_KEYCHAIN="$MACOS_SIGNING_KEYCHAIN_PATH"
  export APPLE_NOTARY_KEY_PATH="$MACOS_SIGNING_NOTARY_KEY_PATH"
  echo "Signing as: $CODESIGN_IDENTITY"
}

# --- signing a bare Mach-O ---------------------------------------------------

# --identifier is mandatory, not decoration. Measured: without it codesign derives the identifier
# from the file, e.g. `noident-binary-5555494426274bf73b4c341e81776b2166df4c3c` — so it changes with
# the filename and with every build, and the designated requirement churns every release. With it the
# identifier is `com.lightless-labs.pessimal.agent` and stays put.
#
# No --deep: a single Mach-O has no nested code, and --deep on something that does is the wrong tool
# anyway. The argv is identical for an ad-hoc identity (`-`), which is how notarize-macos-binary.sh
# --self-test drives this exact function with no certificate: codesign accepts --timestamp on an
# ad-hoc signature and simply records none. No release path passes `-`, and one that did would be
# refused by macos_signing_assert_developer_id before anything was submitted. Only a real identity
# gets --keychain, and for a real identity it is required, so signing never falls back to the
# ambient search list.
macos_signing_codesign_binary() {
  local path="${1:?a binary path is required}" identifier="${2:?a code-signing identifier is required}"
  local identity="${CODESIGN_IDENTITY:?CODESIGN_IDENTITY is required; call macos_signing_open_keychain first}"
  local args=(--force --timestamp --options runtime --identifier "$identifier")

  if [[ "$identity" != "-" ]]; then
    args+=(--keychain "${CODESIGN_KEYCHAIN:?CODESIGN_KEYCHAIN is required for a real identity; call macos_signing_open_keychain first}")
  fi

  codesign "${args[@]}" --sign "$identity" "$path"
  codesign --verify --strict --verbose=2 "$path"
  echo "codesign: $path signed as $identifier"
}

# Decision 4. Reads the signature back and fails unless it is the one the notary service needs. Every
# failure prints what codesign actually reported, so the log names the situation — "Signature=adhoc"
# says the Developer ID step never happened, "Signed Time=" says the timestamp server was not reached.
macos_signing_assert_developer_id() {
  local path="${1:?a binary path is required}" details summary
  details="$(codesign -dvv "$path" 2>&1)" \
    || macos_signing_fail "codesign cannot read a signature on $path: $details"
  summary="$(grep -E '^(Identifier|Signature|Authority|Timestamp|Signed Time)=|flags=' <<< "$details" | tr '\n' ';')"

  grep -q '^Authority=Developer ID Application: ' <<< "$details" \
    || macos_signing_fail "$path is not signed by a Developer ID Application identity; codesign reports: $summary"
  grep -Eq '^CodeDirectory .*flags=0x[0-9a-f]+\([^)]*runtime' <<< "$details" \
    || macos_signing_fail "$path lacks the hardened runtime flag the notary service requires; codesign reports: $summary"
  grep -q '^Timestamp=' <<< "$details" \
    || macos_signing_fail "$path has no secure timestamp; codesign reports: $summary"
  echo "checked $path: Developer ID Application authority, hardened runtime, secure timestamp"
}

# --- notarisation ------------------------------------------------------------

# Selects the credential branch once, for both `submit` and `log`. The three branches are the ones
# notarize-macos-app.sh has always supported; they live here now so the app path and the bare-binary
# path cannot disagree about which credentials they honour.
macos_signing_notary_auth_args() {
  MACOS_SIGNING_NOTARY_AUTH=()
  if [[ -n "${APPLE_NOTARY_KEY_PATH:-}" && -n "${APPLE_NOTARY_KEY_ID:-}" && -n "${APPLE_NOTARY_ISSUER_ID:-}" ]]; then
    MACOS_SIGNING_NOTARY_AUTH=(--key "$APPLE_NOTARY_KEY_PATH" --key-id "$APPLE_NOTARY_KEY_ID" --issuer "$APPLE_NOTARY_ISSUER_ID")
  elif [[ -n "${APPLE_NOTARY_KEYCHAIN_PROFILE:-}" ]]; then
    MACOS_SIGNING_NOTARY_AUTH=(--keychain-profile "$APPLE_NOTARY_KEYCHAIN_PROFILE")
  elif [[ -n "${APPLE_ID:-}" && -n "${APPLE_TEAM_ID:-}" && -n "${APPLE_APP_SPECIFIC_PASSWORD:-}" ]]; then
    MACOS_SIGNING_NOTARY_AUTH=(--apple-id "$APPLE_ID" --team-id "$APPLE_TEAM_ID" --password "$APPLE_APP_SPECIFIC_PASSWORD")
  else
    macos_signing_fail "set APPLE_NOTARY_KEY_PATH/APPLE_NOTARY_KEY_ID/APPLE_NOTARY_ISSUER_ID, APPLE_NOTARY_KEYCHAIN_PROFILE, or APPLE_ID/APPLE_TEAM_ID/APPLE_APP_SPECIFIC_PASSWORD"
  fi
}

# Reads notarytool JSON from a file and prints `<id> <status>` for the last object carrying a status.
# Scans for objects rather than parsing the whole file, so one stray non-JSON line cannot turn an
# Accepted verdict into "unparseable". Both notarytool shapes are handled: submit reports `id`, the
# developer log reports `jobId`. Exit 1 when no status is found at all. python3 rather than jq because
# python3 is already unavoidable for the Doppler fetch; jq would be one more unmeasured tool.
macos_signing_notary_verdict() {
  python3 - "$1" <<'PY'
import json, sys

text = open(sys.argv[1], errors="replace").read()
decoder = json.JSONDecoder()
found, i = None, 0
while True:
    i = text.find("{", i)
    if i < 0:
        break
    try:
        obj, end = decoder.raw_decode(text, i)
    except ValueError:
        i += 1
        continue
    if isinstance(obj, dict) and isinstance(obj.get("status"), str):
        found = obj
    i = end
if found is None:
    sys.exit(1)
print(found.get("id") or found.get("jobId") or "-", found["status"])
PY
}

# Fetches the developer log for a submission into a file, retrying while it propagates.
macos_signing_notary_log() {
  local id="$1" dest="$2" attempt delay="${MACOS_SIGNING_NOTARY_LOG_RETRY_SECONDS:-15}"
  for attempt in 1 2 3 4; do
    if xcrun notarytool log "$id" "${MACOS_SIGNING_NOTARY_AUTH[@]}" > "$dest"; then
      return 0
    fi
    if (( attempt < 4 )); then
      echo "notarytool log $id is not available yet (attempt $attempt of 4); retrying in ${delay}s" >&2
      sleep "$delay"
    fi
  done
  return 1
}

# Submits one zip and asserts the verdict. `--wait` returning 0 means the submission reached a
# terminal state, NOT that the state is Accepted — notarytool's man page documents no EXIT STATUS
# section at all. So the verdict is read out of --output-format json, and then read AGAIN out of the
# developer log (`notarytool log <id>`), which must also say Accepted for the same job id. Anything
# else prints the log and fails. The log is printed on success too: it is where Apple's warnings live,
# and it carries file names and hashes, never a credential.
#
# The app path got away without this by accident: `stapler staple` fails when there is no ticket. A
# bare Mach-O cannot be stapled at all (see packaging/macos/GATEKEEPER.md), so the binary path has no
# such accident to rely on and this assertion is the only thing standing between a rejected
# submission and a shipped unnotarized binary.
#
# Only stdout is captured, because only stdout is JSON. notarytool's stderr goes straight to the log:
# merged in, a warning line could interleave with the verdict.
macos_signing_notary_submit() {
  local zip_path="${1:?a zip to submit is required}"
  macos_signing_require_tools xcrun
  macos_signing_require_python3
  [[ -s "$zip_path" ]] || macos_signing_fail "nothing to notarize at $zip_path"

  macos_signing_notary_auth_args

  local scratch out log verdict="" id="" status="" rc=0
  scratch="$(mktemp -d)"
  out="$scratch/submit.json"
  log="$scratch/log.json"

  echo "notarytool: submitting $(basename "$zip_path") and waiting for a verdict"
  # set -e must not swallow this: a non-zero exit still carries JSON worth reading.
  xcrun notarytool submit "$zip_path" "${MACOS_SIGNING_NOTARY_AUTH[@]}" --wait --output-format json > "$out" || rc=$?

  verdict="$(macos_signing_notary_verdict "$out" || true)"
  id="${verdict%% *}"
  status="${verdict#* }"
  if [[ -z "$verdict" ]]; then
    cat "$out" >&2
    rm -rf "$scratch"
    macos_signing_fail "could not read a status out of notarytool's JSON (exit $rc) for $zip_path"
  fi
  echo "notarytool: submission $id status=$status (exit $rc)"

  if [[ "$status" != "Accepted" ]]; then
    if [[ "$id" != "-" ]]; then
      echo "--- notarytool log $id ---" >&2
      if macos_signing_notary_log "$id" "$log"; then cat "$log" >&2; fi
    fi
    rm -rf "$scratch"
    macos_signing_fail "notarisation of $zip_path is $status, not Accepted"
  fi
  if [[ $rc -ne 0 ]]; then
    rm -rf "$scratch"
    macos_signing_fail "notarytool reported Accepted but exited $rc for $zip_path"
  fi
  if [[ "$id" == "-" ]]; then
    rm -rf "$scratch"
    macos_signing_fail "notarytool reported Accepted with no submission id for $zip_path, so there is no log to confirm it"
  fi

  if ! macos_signing_notary_log "$id" "$log"; then
    rm -rf "$scratch"
    macos_signing_fail "notarytool said Accepted for $id but its developer log could not be fetched, so the verdict is unconfirmed"
  fi
  echo "--- notarytool log $id ---"
  cat "$log"
  echo
  local log_verdict log_id log_status
  log_verdict="$(macos_signing_notary_verdict "$log" || true)"
  log_id="${log_verdict%% *}"
  log_status="${log_verdict#* }"
  rm -rf "$scratch"
  echo "notarytool log: checked job id and status, found id=${log_id:-none} status=${log_status:-none}"
  [[ -n "$log_verdict" ]] || macos_signing_fail "the developer log for $id carries no status"
  [[ "$log_id" == "$id" ]] || macos_signing_fail "the developer log is for job $log_id, not submission $id"
  [[ "$log_status" == "Accepted" ]] || macos_signing_fail "the developer log for $id says $log_status, not Accepted"
  echo "notarytool: $(basename "$zip_path") is Accepted, confirmed by the submission and by its log"
}
