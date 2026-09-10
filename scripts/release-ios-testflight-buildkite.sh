#!/usr/bin/env bash
# Sign and upload the iOS app to TestFlight from a Buildkite guest.
#
#   scripts/release-ios-testflight-buildkite.sh <version> <build-number>
#
# This exists for the same reason Descartes has
# scripts/release-macos-notifier-buildkite.sh: the tart-ci plugin injects a Doppler *service token*
# into the guest and nothing else. There is no `doppler` CLI there, so the secrets have to be read
# over the REST API and exported before fastlane runs.
#
# Secrets, from Doppler project lightless-labs-pessimal, config prd_ios_deployment. Any of them
# already present in the environment is used as-is, which is what makes this runnable by hand:
#   APPLE_DISTRIBUTION_CERTIFICATE_P12_BASE64   Apple Distribution cert + key, base64 of a .p12
#   APPLE_DISTRIBUTION_CERTIFICATE_PASSWORD     that .p12's password
#   APP_STORE_CONNECT_API_KEY_ID                App Store Connect API key id
#   APP_STORE_CONNECT_API_KEY_ISSUER_ID         its issuer id
#   APP_STORE_CONNECT_API_KEY_BASE64            the .p8, base64
#   APPLE_TEAM_ID                               the team that owns them
#   GH_TOKEN                                    optional; release notes and tag push
set -euo pipefail

cd "$(dirname "$0")/.."

fail() { echo "error: $*" >&2; exit 2; }

VERSION="${1:-}"
BUILD_NUMBER="${2:-}"
[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail "first argument must be a semver version, got '${VERSION}'"
[[ "$BUILD_NUMBER" =~ ^[0-9]+$ ]] || fail "second argument must be a build number, got '${BUILD_NUMBER}'"

DOPPLER_PROJECT="${DOPPLER_PROJECT:-lightless-labs-pessimal}"
DOPPLER_CONFIG="${DOPPLER_CONFIG:-prd_ios_deployment}"

# Secret *names* only. Printed on failure so a naming mismatch between this repo and the vault is one
# round trip to diagnose rather than several. Values are never printed.
list_doppler_secret_names() {
  [[ -n "${DOPPLER_TOKEN:-}" ]] || return 0
  DOPPLER_BOOTSTRAP_TOKEN="$DOPPLER_TOKEN" \
  DOPPLER_PROJECT_NAME="$DOPPLER_PROJECT" \
  DOPPLER_CONFIG_NAME="$DOPPLER_CONFIG" \
  python3 <<'PY' || true
import base64, json, os, urllib.parse, urllib.request

token = os.environ["DOPPLER_BOOTSTRAP_TOKEN"]
qs = urllib.parse.urlencode({
    "project": os.environ["DOPPLER_PROJECT_NAME"],
    "config": os.environ["DOPPLER_CONFIG_NAME"],
})
req = urllib.request.Request(
    f"https://api.doppler.com/v3/configs/config/secrets?{qs}",
    headers={"Authorization": "Basic " + base64.b64encode(f"{token}:".encode()).decode(),
             "Accept": "application/json",
             "User-Agent": "pessimal-ios-release/1"},
)
try:
    with urllib.request.urlopen(req, timeout=20) as res:
        names = sorted((json.load(res).get("secrets") or {}).keys())
    print("  config holds: " + (", ".join(names) if names else "(nothing)"))
except Exception as exc:
    print(f"  (could not list the config's secret names: {exc})")
PY
}

fetch_secret() {
  local name="$1" optional="${2:-}" value
  # Already in the environment, or no token to fetch with: nothing to do either way.
  if [[ -z "${DOPPLER_TOKEN:-}" || -n "${!name:-}" ]]; then
    return 0
  fi
  command -v python3 >/dev/null || fail "DOPPLER_TOKEN is set but python3 is unavailable for the REST fetch"

  if ! value="$(
    DOPPLER_BOOTSTRAP_TOKEN="$DOPPLER_TOKEN" \
    DOPPLER_PROJECT_NAME="$DOPPLER_PROJECT" \
    DOPPLER_CONFIG_NAME="$DOPPLER_CONFIG" \
    DOPPLER_SECRET_NAME="$name" \
    python3 <<'PY'
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
             "User-Agent": "pessimal-ios-release/1"},
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
  )" || [[ -z "$value" ]]; then
    if [[ "$optional" == "optional" ]]; then
      echo "note: optional secret unavailable: $name" >&2
      return 0
    fi
    echo "error: could not read required secret $name from $DOPPLER_PROJECT/$DOPPLER_CONFIG" >&2
    list_doppler_secret_names >&2
    exit 2
  fi
  printf -v "$name" '%s' "$value"
  export "$name"
}

fetch_secret APPLE_DISTRIBUTION_CERTIFICATE_P12_BASE64
fetch_secret APPLE_DISTRIBUTION_CERTIFICATE_PASSWORD
fetch_secret APP_STORE_CONNECT_API_KEY_ID
fetch_secret APP_STORE_CONNECT_API_KEY_ISSUER_ID
fetch_secret APP_STORE_CONNECT_API_KEY_BASE64
fetch_secret APPLE_TEAM_ID
fetch_secret GH_TOKEN optional

# The service token's job is done. It must not reach fastlane, Bazel, or anything either of them
# spawns: it opens the whole config, while everything below needs only what was just read.
unset DOPPLER_TOKEN

echo "--- signing and uploading Pessimal $VERSION ($BUILD_NUMBER)"
exec bundle exec fastlane pessimal_beta_testflight \
  version:"$VERSION" \
  build_number:"$BUILD_NUMBER"
