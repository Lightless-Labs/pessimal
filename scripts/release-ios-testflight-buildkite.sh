#!/usr/bin/env bash
# Build, sign and upload the iOS app to TestFlight from a Buildkite guest.
#
#   scripts/release-ios-testflight-buildkite.sh <version> <build-number>
#
# The tart-ci plugin injects a Doppler service token into the guest, and the guest has no `doppler`
# CLI, so the secrets are read over the REST API.
#
# Secrets, from Doppler project lightless-labs-pessimal, config prd_ios_deployment. A secret that is
# already in the environment is used as it is, so you can also run this by hand:
#   APPLE_DISTRIBUTION_CERTIFICATE_P12_BASE64   Apple Distribution certificate and key, base64 of a .p12
#   APPLE_DISTRIBUTION_CERTIFICATE_PASSWORD     the .p12 password
#   APP_STORE_CONNECT_API_KEY_ID                App Store Connect API key id
#   APP_STORE_CONNECT_API_KEY_ISSUER_ID         its issuer id
#   APP_STORE_CONNECT_API_KEY_BASE64            the .p8, base64
#   SIGNOZ_OTLP_ENDPOINT                        optional; where the app sends usage reports
#   SIGNOZ_OTLP_INGESTION_KEY                   optional; the key for that endpoint
#
# APPLE_TEAM_ID is not a secret. It is in clients/apple/ios/BUILD.bazel, so it is not fetched.
set -euo pipefail

cd "$(dirname "$0")/.."

fail() { echo "error: $*" >&2; exit 2; }

VERSION="${1:-}"
BUILD_NUMBER="${2:-}"
[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail "first argument must be a semver version, got '${VERSION}'"
[[ "$BUILD_NUMBER" =~ ^[0-9]+$ ]] || fail "second argument must be a build number, got '${BUILD_NUMBER}'"

# Same team for every app in the band. Overridable, but not a secret, and not in the vault.
export APPLE_TEAM_ID="${APPLE_TEAM_ID:-PKPPLFK854}"

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
  # shellcheck disable=SC2163  # exports the variable whose name is in $name, on purpose
  export "$name"
}

fetch_secret APPLE_DISTRIBUTION_CERTIFICATE_P12_BASE64
fetch_secret APPLE_DISTRIBUTION_CERTIFICATE_PASSWORD
fetch_secret APP_STORE_CONNECT_API_KEY_ID
fetch_secret APP_STORE_CONNECT_API_KEY_ISSUER_ID
fetch_secret APP_STORE_CONNECT_API_KEY_BASE64

# Pessimal's own usage-reporting destination. Optional: a build without it reports nothing, which is
# better than a failed release.
fetch_secret SIGNOZ_OTLP_ENDPOINT optional
fetch_secret SIGNOZ_OTLP_INGESTION_KEY optional

# The service token opens the whole config. Nothing below needs it, so Bazel must not inherit it.
unset DOPPLER_TOKEN

for tool in bazelisk python3 curl openssl security xcrun; do
  command -v "$tool" >/dev/null || fail "$tool is not on PATH in this guest"
done

APP_TARGET="//clients/apple/ios:Pessimal"
PROFILE_NAME="com.lightless-labs.pessimal.ios"
IPA=".bazel/bin/clients/apple/ios/Pessimal.ipa"

# ---------------------------------------------------------------------------------------------------
# Cleanup. A signing keychain, a decoded .p12 or a .p8 left on disk is a credential left behind, so
# this runs on every exit path.
# ---------------------------------------------------------------------------------------------------
KEYCHAIN="$HOME/Library/Keychains/pessimal-signing.keychain"
KEYCHAIN_OPEN=no
ORIGINAL_KEYCHAINS=()
WORK_DIR="$(mktemp -d)"
chmod 700 "$WORK_DIR"

close_keychain() {
  [[ "$KEYCHAIN_OPEN" == yes ]] || return 0
  security delete-keychain "$KEYCHAIN" >/dev/null 2>&1 || true
  if (( ${#ORIGINAL_KEYCHAINS[@]} > 0 )); then
    security list-keychains -d user -s "${ORIGINAL_KEYCHAINS[@]}" >/dev/null 2>&1 || true
  fi
  rm -f "$KEYCHAIN" "$KEYCHAIN-db"
  KEYCHAIN_OPEN=no
}

cleanup() {
  close_keychain
  rm -rf "$WORK_DIR"
}
trap cleanup EXIT
trap 'cleanup; exit 130' INT
trap 'cleanup; exit 143' TERM

# ---------------------------------------------------------------------------------------------------
# The provisioning profile. asc.py installs it through the App Store Connect API. If the name is wrong,
# it lists every profile the key can see.
# ---------------------------------------------------------------------------------------------------
echo "--- fetching the distribution profile from App Store Connect"
scripts/asc.py install-profile --name "$PROFILE_NAME"

# Bazel matches a profile by the Name inside it, not by its filename, so look it up the same way.
profile_path=""
for candidate in "$HOME/Library/MobileDevice/Provisioning Profiles"/*.mobileprovision; do
  [[ -f "$candidate" ]] || continue
  name="$(security cms -D -i "$candidate" 2>/dev/null | plutil -extract Name raw -o - - 2>/dev/null || true)"
  if [[ "$name" == "$PROFILE_NAME" ]]; then
    profile_path="$candidate"
    break
  fi
done
[[ -n "$profile_path" ]] || fail "no installed provisioning profile is named $PROFILE_NAME"
echo "using the profile at $profile_path"

# ---------------------------------------------------------------------------------------------------
# A temporary keychain, never the login keychain. codesign searches the user's keychain list, so the
# keychain goes on that list, and the original list is restored on exit.
# ---------------------------------------------------------------------------------------------------
echo "--- opening a temporary signing keychain"
security delete-keychain "$KEYCHAIN" >/dev/null 2>&1 || true   # left by a killed earlier run

while IFS= read -r line; do
  path="$(printf '%s' "$line" | sed -e 's/^[[:space:]]*"//' -e 's/"[[:space:]]*$//')"
  [[ -n "$path" ]] || continue
  ORIGINAL_KEYCHAINS+=("$path")
  [[ -e "$path" ]] || echo "warning: the keychain search list names a file that does not exist: $path"
done < <(security list-keychains -d user)

KEYCHAIN_PASSWORD="$(openssl rand -hex 24)"
security create-keychain -p "$KEYCHAIN_PASSWORD" "$KEYCHAIN"
KEYCHAIN_OPEN=yes
# Unlocked for an hour, so a slow Bazel build cannot lock it before codesign runs.
security set-keychain-settings -lut 3600 "$KEYCHAIN"
security unlock-keychain -p "$KEYCHAIN_PASSWORD" "$KEYCHAIN"
# The ${x[@]+...} form keeps an empty list from failing under set -u on bash before 4.4.
security list-keychains -d user -s "$KEYCHAIN" ${ORIGINAL_KEYCHAINS[@]+"${ORIGINAL_KEYCHAINS[@]}"}

# A .p12 has the certificate and its key, but not Apple's intermediate certificate. Without the
# intermediate, codesign cannot use the identity. Apple uses several generations at once, so try each
# and fail only if none can be fetched.
imported=0
for generation in G3 G4 G5 G6; do
  cer="$WORK_DIR/AppleWWDRCA$generation.cer"
  if curl -fsS --max-time 20 -o "$cer" "https://www.apple.com/certificateauthority/AppleWWDRCA$generation.cer"; then
    # An already-imported certificate exits non-zero, which is fine here.
    security import "$cer" -k "$KEYCHAIN" -T /usr/bin/codesign >/dev/null 2>&1 || true
    imported=$((imported + 1))
  else
    echo "warning: could not fetch the WWDR $generation intermediate"
  fi
done
(( imported > 0 )) || fail "could not fetch any Apple WWDR intermediate certificate"
echo "WWDR intermediates imported: $imported of 4"

p12="$WORK_DIR/distribution.p12"
printf '%s' "$APPLE_DISTRIBUTION_CERTIFICATE_P12_BASE64" | base64 --decode > "$p12"
[[ -s "$p12" ]] || fail "the decoded .p12 is empty; check APPLE_DISTRIBUTION_CERTIFICATE_P12_BASE64"
security import "$p12" -k "$KEYCHAIN" -P "$APPLE_DISTRIBUTION_CERTIFICATE_PASSWORD" \
  -T /usr/bin/codesign -T /usr/bin/security >/dev/null
rm -f "$p12"
# Without this, codesign asks for the keychain password in a dialog that nobody can answer.
security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$KEYCHAIN_PASSWORD" "$KEYCHAIN" >/dev/null
unset APPLE_DISTRIBUTION_CERTIFICATE_P12_BASE64 APPLE_DISTRIBUTION_CERTIFICATE_PASSWORD

# rules_apple reports a certificate mismatch only after the whole build, with one message for three
# different causes. This check names the cause in seconds.
scripts/signing-diagnostics.py "$profile_path" "$KEYCHAIN" \
  || fail "no signing identity in this keychain matches $(basename "$profile_path"); see above"

# ---------------------------------------------------------------------------------------------------
# Build.
# ---------------------------------------------------------------------------------------------------
echo "--- building Pessimal $VERSION ($BUILD_NUMBER)"
bazel_args=(
  build "$APP_TARGET"
  --config=beta
  --config=ios_device
  "--embed_label=$VERSION.$BUILD_NUMBER"
  --ios_multi_cpus=arm64
)

# The usage-reporting destination goes into the app through --action_env. Add each flag only when it
# has a value: an empty flag still changes Bazel's cache key, and a build with neither reports nothing.
# Never print the key.
if [[ -n "${SIGNOZ_OTLP_ENDPOINT:-}" && -n "${SIGNOZ_OTLP_INGESTION_KEY:-}" ]]; then
  bazel_args+=(
    "--action_env=SIGNOZ_OTLP_ENDPOINT=$SIGNOZ_OTLP_ENDPOINT"
    "--action_env=SIGNOZ_OTLP_INGESTION_KEY=$SIGNOZ_OTLP_INGESTION_KEY"
  )
  echo "Usage reporting will be built in (endpoint $SIGNOZ_OTLP_ENDPOINT, key not shown)"
else
  echo "No usage-reporting destination in the environment; this build will report nothing"
fi

bazelisk "${bazel_args[@]}"
[[ -f "$IPA" ]] || fail "the build passed but there is no ipa at $IPA"

# Signing is done. Remove the keychain before the upload, not after it.
close_keychain

# ---------------------------------------------------------------------------------------------------
# Upload. altool reads the App Store Connect key from API_PRIVATE_KEYS_DIR/AuthKey_<id>.p8.
# ---------------------------------------------------------------------------------------------------
echo "--- uploading $IPA to TestFlight"
keys_dir="$WORK_DIR/private_keys"
mkdir -m 700 "$keys_dir"
printf '%s' "$APP_STORE_CONNECT_API_KEY_BASE64" | base64 --decode \
  > "$keys_dir/AuthKey_$APP_STORE_CONNECT_API_KEY_ID.p8"

upload_log="$WORK_DIR/altool.log"
set +e
API_PRIVATE_KEYS_DIR="$keys_dir" xcrun altool --upload-app -f "$IPA" -t ios \
  --api-key "$APP_STORE_CONNECT_API_KEY_ID" \
  --api-issuer "$APP_STORE_CONNECT_API_KEY_ISSUER_ID" 2>&1 | tee "$upload_log"
status=${PIPESTATUS[0]}
set -e

# altool exits 1 on a failure (measured with Xcode 26.2). Its output is checked as well, because older
# versions exited 0 after some failed uploads. Its error lines contain "ERROR:".
if (( status != 0 )) || grep -qE "ERROR:|ERROR ITMS-|UPLOAD FAILED" "$upload_log"; then
  fail "the upload to App Store Connect failed (altool exit $status); see above"
fi
echo "uploaded Pessimal $VERSION ($BUILD_NUMBER) to App Store Connect"
