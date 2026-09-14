#!/usr/bin/env bash
# Signs and notarizes ONE bare Mach-O — the host agent binary — and deliberately does not staple it.
#
# Usage:
#   scripts/notarize-macos-binary.sh <binary> [--identifier com.lightless-labs.pessimal.agent]
#   scripts/notarize-macos-binary.sh --self-test <scratch copy of a binary> [--identifier ID]
#
# The real path expects its caller to have sourced scripts/lib/macos-signing.sh and opened a keychain
# (scripts/release-macos-artifacts.sh does both, in the Buildkite release-macos step), so
# CODESIGN_IDENTITY, CODESIGN_KEYCHAIN and the APPLE_NOTARY_* credentials are already exported.
# --self-test needs none of that.
#
# Written fresh rather than bent out of notarize-macos-app.sh, because that script is bundle-shaped
# end to end: it uses --deep on verify, `ditto --keepParent`, Info.plist and `stapler`, and all four
# are wrong here. None of them appears below. The shared half — credential selection and the Accepted
# assertion — is in the library, so there is one notary implementation, not two.
#
# THERE IS NO STAPLE STEP, AND ADDING ONE WILL NOT WORK. A notarisation ticket cannot be attached to
# a standalone binary; Apple DTS is explicit that "it's not currently possible to staple tickets to
# them". Gatekeeper resolves the ticket online at first launch instead. That has a consequence for
# anyone provisioning this agent onto an egress-restricted host, which is why it is written down at
# length in packaging/macos/GATEKEEPER.md rather than only here.
#
# The sequence is forced by what notarytool accepts — UDIF disk images, signed flat packages and zip
# archives, and nothing else. So: codesign the binary, zip it purely to have something submittable,
# submit the zip, delete the zip. That zip is an intermediate; it is never a release asset, and the
# release asset is the .tar.gz built later from the signed binary itself.
#
# The Gatekeeper check is `spctl --type open --context context:primary-signature`, NOT `--type
# execute`. Measured on Developer-ID-signed, notarized command-line tools (most recently Anthropic's
# claude.exe, 2026-09-13): `--type execute` answers "rejected (the code is valid but does not seem to
# be an app)" for any bare Mach-O, so it would fail every release after Apple had already said
# Accepted. The primary-signature form answers "accepted / source=Notarized Developer ID" for those
# tools and "rejected" for an ad-hoc or unsigned one, and the `source=` line is what this asserts. On
# a host whose Gatekeeper assessments are switched off, spctl answers "accepted" to everything with an
# override note; that host cannot give a verdict, so the check says it was not performed rather than
# reporting a pass it never earned, and the notary log's Accepted stands as the evidence.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LIB="$ROOT_DIR/scripts/lib/macos-signing.sh"
# shellcheck source=scripts/lib/macos-signing.sh
source "$LIB"

IDENTIFIER="com.lightless-labs.pessimal.agent"
BINARY=""
SELF_TEST="no"

usage() { sed -n '2,37p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; }

while [[ $# -gt 0 ]]; do
  case "$1" in
    --identifier) IDENTIFIER="${2:?--identifier needs a value}"; shift 2 ;;
    --self-test) SELF_TEST="yes"; shift ;;
    --help|-h) usage; exit 0 ;;
    -*) macos_signing_fail "unknown flag: $1" ;;
    *) [[ -z "$BINARY" ]] || macos_signing_fail "only one binary at a time: already have $BINARY"
       BINARY="$1"; shift ;;
  esac
done

# Every tool either path needs, before either path starts.
[[ "$(uname -s)" == "Darwin" ]] || macos_signing_fail "macOS signing must run on macOS"
macos_signing_require_tools codesign
macos_signing_require_python3
if [[ "$SELF_TEST" != "yes" ]]; then
  macos_signing_require_tools ditto xcrun spctl
  macos_signing_require_xcrun_tools notarytool
fi
[[ -n "$BINARY" ]] || macos_signing_fail "a binary path is required (see --help)"
[[ -f "$BINARY" ]] || macos_signing_fail "not a file: $BINARY"

# --- self-test ---------------------------------------------------------------
# Everything here needs no signing identity, no notary account and no network, so it is runnable on a
# developer machine — including one whose keychain search list is corrupted, because ad-hoc signing
# consults no keychain and no identity at all, and every `security` call below goes to a stand-in.
#
# It proves the things in this path that could otherwise fail success-shaped: the --identifier fix,
# that the Developer ID assertion rejects the linker's ad-hoc signature, that a notary verdict other
# than Accepted fails even when notarytool exits 0 (and that Accepted must be confirmed by the log),
# that the keychain-list reader keeps a space-bearing path whole, and that a notarisation failure or
# an interrupt after the keychain opens still removes the keychain and both decoded key files.
#
# spctl is deliberately not asserted here: it rejects an ad-hoc signature by design, so asserting it
# would only teach us to ignore a failure. spctl belongs to the real path below and to
# scripts/verify-release.sh, which runs on a machine that never saw the keychain.
if [[ "$SELF_TEST" == "yes" ]]; then
  scratch="$(mktemp -d)"
  # shellcheck disable=SC2064  # expand $scratch now: it must be removed even if the variable changes
  trap "rm -rf '$scratch'" EXIT

  # Runs one library function in a fresh bash with the same `set -euo pipefail` a real caller has, so
  # macos_signing_fail's `exit` ends that child and not the self-test. Inherits this process's env.
  in_child() { bash -c 'set -euo pipefail; source "$1"; shift; "$@"' self-test "$LIB" "$@"; }

  echo "self-test 1: ad-hoc signing a scratch copy of $BINARY as $IDENTIFIER"
  cp "$BINARY" "$scratch/literal"
  codesign --force -s - --options runtime --identifier "$IDENTIFIER" "$scratch/literal"
  codesign --verify --strict --verbose=2 "$scratch/literal"
  # codesign -d writes to stderr, so the merge is required, not defensive.
  got="$(codesign -dvvv "$scratch/literal" 2>&1 | sed -n 's/^Identifier=//p')"
  echo "self-test 1: checked reported identifier, found '$got'"
  [[ "$got" == "$IDENTIFIER" ]] || macos_signing_fail "codesign reports Identifier='$got', expected '$IDENTIFIER'"

  # The same assertion through the function the release path actually calls, with the release argv
  # (--timestamp included) and only the identity swapped for `-`. A future edit that drops --identifier
  # from the function fails this rung rather than shipping.
  echo "self-test 2: repeating through macos_signing_codesign_binary with an ad-hoc identity"
  cp "$BINARY" "$scratch/via-library"
  CODESIGN_IDENTITY="-" CODESIGN_KEYCHAIN="" macos_signing_codesign_binary "$scratch/via-library" "$IDENTIFIER"
  got="$(codesign -dvvv "$scratch/via-library" 2>&1 | sed -n 's/^Identifier=//p')"
  echo "self-test 2: checked reported identifier via library, found '$got'"
  [[ "$got" == "$IDENTIFIER" ]] || macos_signing_fail "library path reports Identifier='$got', expected '$IDENTIFIER'"

  # `codesign --verify --strict` just passed on an ad-hoc signature. The Developer ID assertion must not.
  echo "self-test 3: macos_signing_assert_developer_id must reject that ad-hoc signature"
  if in_child macos_signing_assert_developer_id "$scratch/via-library" 2> "$scratch/assert.err"; then
    macos_signing_fail "macos_signing_assert_developer_id accepted an ad-hoc signature"
  fi
  echo "self-test 3: rejected, as required: $(tail -n 1 "$scratch/assert.err")"
  # And rejected by the authority check itself. The ad-hoc copy also lacks a timestamp, so without this
  # a broken authority check would hide behind the timestamp check and still read as a pass.
  grep -q 'not signed by a Developer ID Application identity' "$scratch/assert.err" \
    || macos_signing_fail "the ad-hoc signature was rejected, but not by the Developer ID authority check"

  # A stand-in xcrun that answers `notarytool submit` with a chosen JSON verdict and exit code, and
  # `notarytool log <id>` with a developer log for a chosen job and status, so the verdict logic runs
  # for real with no Apple account.
  mkdir -p "$scratch/bin"
  cat > "$scratch/bin/xcrun" <<'STUB'
#!/usr/bin/env bash
case "${1:-} ${2:-}" in
  "notarytool submit") printf '%s\n' "$STUB_NOTARY_STDOUT"; exit "$STUB_NOTARY_EXIT" ;;
  "notarytool log")
    echo "STUB-NOTARY-LOG-FETCHED" >&2
    [[ "${STUB_NOTARY_LOG_EXIT:-0}" == "0" ]] || exit "$STUB_NOTARY_LOG_EXIT"
    printf '{\n  "jobId": "%s",\n  "status": "%s",\n  "statusSummary": "stub",\n  "issues": null\n}\n' \
      "${STUB_NOTARY_LOG_JOB:-$3}" "${STUB_NOTARY_LOG_STATUS:-Accepted}"
    exit 0 ;;
esac
echo "stub xcrun: unexpected arguments: $*" >&2
exit 64
STUB
  chmod 0755 "$scratch/bin/xcrun"
  echo "not a real submission" > "$scratch/submission.zip"

  # expect: pass | fail. Any real notary credentials in this environment are dropped, and a keychain
  # profile name selects a branch without authenticating anything — the stub never reads it.
  notary_case() {
    local label="$1" expect="$2" stdout="$3" exit_code="$4" log_status="${5:-Accepted}" log_exit="${6:-0}" log_job="${7:-}"
    local result="pass"
    if ! env -u APPLE_NOTARY_KEY_PATH -u APPLE_NOTARY_KEY_ID -u APPLE_NOTARY_ISSUER_ID \
        -u APPLE_ID -u APPLE_TEAM_ID -u APPLE_APP_SPECIFIC_PASSWORD \
        APPLE_NOTARY_KEYCHAIN_PROFILE="self-test" PATH="$scratch/bin:$PATH" \
        MACOS_SIGNING_NOTARY_LOG_RETRY_SECONDS=0 \
        STUB_NOTARY_STDOUT="$stdout" STUB_NOTARY_EXIT="$exit_code" \
        STUB_NOTARY_LOG_STATUS="$log_status" STUB_NOTARY_LOG_EXIT="$log_exit" STUB_NOTARY_LOG_JOB="$log_job" \
        bash -c 'set -euo pipefail; source "$1"; macos_signing_notary_submit "$2"' \
          self-test "$LIB" "$scratch/submission.zip" > "$scratch/notary.log" 2>&1; then
      result="fail"
    fi
    echo "self-test 4: $label -> expected $expect, found $result ($(grep -E '^error:' "$scratch/notary.log" | tail -n 1 || true))"
    [[ "$result" == "$expect" ]] || { sed 's/^/    /' "$scratch/notary.log" >&2; macos_signing_fail "notary verdict case '$label' was $result, expected $expect"; }
  }
  job1="00000000-0000-0000-0000-000000000001"
  notary_case "Invalid, exit 0" fail "{\"id\":\"$job1\",\"status\":\"Invalid\",\"message\":\"Processing complete\"}" 0 Invalid
  grep -q STUB-NOTARY-LOG-FETCHED "$scratch/notary.log" \
    || macos_signing_fail "an Invalid verdict did not fetch notarytool log"
  echo "self-test 4: the Invalid verdict fetched notarytool log"
  notary_case "Accepted, exit 0, log Accepted" pass '{"id":"00000000-0000-0000-0000-000000000002","status":"Accepted","message":"Processing complete"}' 0
  grep -q 'confirmed by the submission and by its log' "$scratch/notary.log" \
    || macos_signing_fail "an Accepted verdict passed without confirming it against the log"
  notary_case "Accepted, exit 1" fail '{"id":"00000000-0000-0000-0000-000000000003","status":"Accepted","message":"Processing complete"}' 1
  notary_case "no JSON at all, exit 0" fail 'Successfully uploaded file' 0
  notary_case "In Progress (a --wait timeout), exit 0" fail '{"id":"00000000-0000-0000-0000-000000000004","status":"In Progress","message":"still going"}' 0
  notary_case "Accepted, but the log says Invalid" fail '{"id":"00000000-0000-0000-0000-000000000005","status":"Accepted","message":"Processing complete"}' 0 Invalid
  notary_case "Accepted, but the log is another job's" fail '{"id":"00000000-0000-0000-0000-000000000006","status":"Accepted","message":"Processing complete"}' 0 Accepted 0 "00000000-0000-0000-0000-00000000ffff"
  notary_case "Accepted, but the log never arrives" fail '{"id":"00000000-0000-0000-0000-000000000007","status":"Accepted","message":"Processing complete"}' 0 Accepted 1

  # The indented, quoted shape `security list-keychains` really prints, with a space inside one path.
  echo "self-test 5: the keychain-list reader keeps a space-bearing path as one entry"
  parsed="$(printf '    "%s"\n' "/Users/someone/Library/Keychains/login.keychain-db" \
    "/Volumes/Signing Disk/release signing.keychain-db" | macos_signing_parse_keychain_list)"
  expected="$(printf '%s\n' "/Users/someone/Library/Keychains/login.keychain-db" \
    "/Volumes/Signing Disk/release signing.keychain-db")"
  entries=()
  while IFS= read -r entry; do entries+=("$entry"); done <<< "$parsed"
  echo "self-test 5: found ${#entries[@]} entries: $(printf '[%s] ' "${entries[@]}")"
  [[ "$parsed" == "$expected" && ${#entries[@]} -eq 2 ]] \
    || macos_signing_fail "keychain-list reader produced '$parsed', expected '$expected'"

  # A stand-in `security` that keeps a keychain as a plain file and records every call, one argument
  # per line, so the keychain lifecycle runs through the library's real code and the real trap while
  # the host's keychains and search list are never touched. The intermediate import and the clock
  # nudge are replaced in the driver: one needs a real Developer ID leaf, the other sudo.
  cat > "$scratch/bin/security" <<'STUB'
#!/usr/bin/env bash
{ echo "--- security"; printf '%s\n' "$@"; } >> "$STUB_SECURITY_LOG"
case "${1:-}" in
  list-keychains)
    if [[ " $* " != *" -s "* ]]; then
      printf '    "%s"\n' "/Users/someone/Library/Keychains/login.keychain-db" "/Volumes/Signing Disk/release signing.keychain-db"
    fi ;;
  create-keychain) : > "${!#}" ;;
  delete-keychain) rm -f "${!#}" ;;
  find-identity) printf '  1) 0123456789ABCDEF0123456789ABCDEF01234567 "Developer ID Application: Self Test (SELFTEST00)"\n     1 identities found\n' ;;
esac
exit 0
STUB
  chmod 0755 "$scratch/bin/security"
  # And a `doppler` that knows nothing, so a developer's logged-in CLI is never asked during a self-test.
  printf '#!/usr/bin/env bash\nexit 1\n' > "$scratch/bin/doppler"
  chmod 0755 "$scratch/bin/doppler"

  # $2 is how the driver ends once the keychain is open: a notarisation failure, SIGINT or SIGTERM.
  lifecycle_case() {
    local label="$1" ending="$2" expect_rc="$3" rc=0 build_dir="$scratch/signing-$2"
    : > "$scratch/security-$ending.log"
    env -u APPLE_NOTARY_KEYCHAIN_PROFILE -u APPLE_ID -u APPLE_TEAM_ID -u APPLE_APP_SPECIFIC_PASSWORD \
      -u CODESIGN_IDENTITY -u CODESIGN_KEYCHAIN \
      PATH="$scratch/bin:$PATH" STUB_SECURITY_LOG="$scratch/security-$ending.log" \
      PESSIMAL_RELEASE_BUILD_DIR="$build_dir" DOPPLER_TOKEN="" \
      MACOS_DEVELOPER_ID_CERT_P12_BASE64="$(printf 'not a p12' | base64)" \
      MACOS_DEVELOPER_ID_CERT_PASSWORD="self-test" APPLE_NOTARY_KEY_ID="SELFTEST00" \
      APPLE_NOTARY_ISSUER_ID="00000000-0000-0000-0000-000000000000" \
      APPLE_NOTARY_KEY_P8_BASE64="$(printf 'not a key' | base64)" \
      STUB_NOTARY_STDOUT="{\"id\":\"$job1\",\"status\":\"Invalid\"}" STUB_NOTARY_EXIT=0 \
      STUB_NOTARY_LOG_STATUS=Invalid MACOS_SIGNING_NOTARY_LOG_RETRY_SECONDS=0 \
      bash -c '
        set -euo pipefail
        source "$1"
        macos_signing_import_intermediate() { echo "(self-test: intermediate import replaced)"; }
        macos_signing_sync_clock() { :; }
        macos_signing_fetch_secrets
        [[ -z "${DOPPLER_TOKEN+set}" ]] || { echo "DOPPLER_TOKEN is still set after the fetch" >&2; exit 97; }
        macos_signing_open_keychain
        for f in "$MACOS_SIGNING_KEYCHAIN_PATH" "$MACOS_SIGNING_CERT_PATH" "$MACOS_SIGNING_NOTARY_KEY_PATH"; do
          [[ -e "$f" ]] || { echo "expected $f to exist while the keychain is open" >&2; exit 98; }
        done
        case "$3" in
          notary) macos_signing_notary_submit "$2" ;;
          interrupt) kill -INT $$; sleep 5 ;;
          terminate) kill -TERM $$; sleep 5 ;;
        esac
        echo "the driver carried on past its ending" >&2
        exit 99
      ' self-test "$LIB" "$scratch/submission.zip" "$ending" > "$scratch/lifecycle-$ending.out" 2>&1 || rc=$?

    local leftovers
    # `|| true`: a driver that died before creating the directory makes find fail, and under pipefail
    # that would end the self-test here with no message instead of at the named check below.
    leftovers="$(find "$build_dir" -type f 2>/dev/null | tr '\n' ' ' || true)"
    echo "self-test 6: $label -> exit $rc (expected $expect_rc); files left in the signing dir: '${leftovers}'"
    if [[ $rc -ne $expect_rc || -n "$leftovers" ]]; then
      sed 's/^/    /' "$scratch/lifecycle-$ending.out" >&2
      macos_signing_fail "$label: the keychain or a decoded key file outlived the run, or the run ended the wrong way"
    fi
    grep -qx 'delete-keychain' "$scratch/security-$ending.log" \
      || macos_signing_fail "$label: security delete-keychain was never called"

    # Every `list-keychains … -s …` write, in order, arguments joined with `|`. The first is the open
    # (ephemeral keychain first, then both originals); the last is the restore (both originals, as two
    # separate arguments, the space-bearing one whole). Nothing else is acceptable for either.
    local writes open_write restore_write
    writes="$(python3 - "$scratch/security-$ending.log" <<'PY'
import sys
calls = [b.rstrip("\n").split("\n") for b in open(sys.argv[1]).read().split("--- security\n") if b.strip()]
for call in calls:
    if call[0] == "list-keychains" and "-s" in call:
        print("|".join(call))
PY
)"
    open_write="$(head -n 1 <<< "$writes")"
    restore_write="$(tail -n 1 <<< "$writes")"
    local originals="/Users/someone/Library/Keychains/login.keychain-db|/Volumes/Signing Disk/release signing.keychain-db"
    echo "self-test 6: $label -> search list on open:    $open_write"
    echo "self-test 6: $label -> search list on cleanup: $restore_write"
    [[ "$open_write" == "list-keychains|-d|user|-s|$build_dir/pessimal-signing.keychain-db|$originals" ]] \
      || macos_signing_fail "$label: the ephemeral keychain was not put first on the search list ahead of both originals"
    [[ "$restore_write" == "list-keychains|-d|user|-s|$originals" ]] \
      || macos_signing_fail "$label: cleanup did not restore the original search list argument for argument"
  }
  lifecycle_case "notarisation fails after the keychain opened" notary 2
  lifecycle_case "SIGINT after the keychain opened" interrupt 130
  lifecycle_case "SIGTERM after the keychain opened" terminate 143

  echo "self-test: passed — no identity, no notary account, no real keychain and no staple step were needed"
  exit 0
fi

# --- the real path -----------------------------------------------------------
[[ -n "${CODESIGN_IDENTITY:-}" ]] || macos_signing_fail \
  "CODESIGN_IDENTITY is not set — open a keychain first (scripts/release-macos-artifacts.sh does)"

macos_signing_codesign_binary "$BINARY" "$IDENTIFIER"
# Before the notary round trip, not after: a signature Apple will reject should cost seconds, not the
# queue time of a submission.
macos_signing_assert_developer_id "$BINARY"

submission_dir="$(mktemp -d)"
submission_zip="$submission_dir/$(basename "$BINARY").zip"
# shellcheck disable=SC2064  # expand $submission_dir now, for the same reason as above
trap "rm -rf '$submission_dir'" EXIT

ditto -c -k "$BINARY" "$submission_zip"
macos_signing_notary_submit "$submission_zip"
rm -rf "$submission_dir"
trap - EXIT

# Gatekeeper has to fetch the ticket from Apple because there is nowhere on this file to keep one, so
# the assessment can fail for a minute or two after the verdict while the notary CDN catches up. Same
# shape as notarize-macos-app.sh's staple retry: one try, then four more fifteen seconds apart.
assess() {
  local verdict=""
  verdict="$(spctl --assess --type open --context context:primary-signature --verbose=4 "$BINARY" 2>&1)" || {
    echo "$verdict" >&2
    return 1
  }
  echo "$verdict"
  if ! grep -q 'source=Notarized Developer ID' <<< "$verdict"; then
    echo "spctl accepted $BINARY but not as 'source=Notarized Developer ID'" >&2
    return 1
  fi
}
gatekeeper="$(spctl --status 2>&1 || true)"
if [[ "$gatekeeper" != *"assessments enabled"* ]]; then
  cat >&2 <<EOF
NOTE: Gatekeeper assessment of $BINARY was NOT performed. This host reports '$gatekeeper', and with
assessments off spctl accepts everything, so its answer would mean nothing. The evidence of
notarisation is the Accepted verdict above, confirmed by the notary log; scripts/verify-release.sh
repeats the spctl check on the published download.
EOF
elif ! assess; then
  echo "Initial Gatekeeper assessment failed; retrying for notary ticket propagation..." >&2
  for i in 1 2 3 4; do
    sleep 15
    if assess; then
      break
    fi
    if (( i == 4 )); then
      macos_signing_fail "spctl --assess --type open --context context:primary-signature still rejects $BINARY after 4 retries"
    fi
  done
fi

codesign --verify --strict --verbose=2 "$BINARY"
cat <<EOF
Signed and notarized (NOT stapled — a bare Mach-O cannot hold a ticket):
  $BINARY
Code-signing identifier:
  $IDENTIFIER
First launch on a quarantined copy needs network access to Apple; see packaging/macos/GATEKEEPER.md.
EOF
