#!/usr/bin/env bash
# Proves a published release is real, while holding no credential.
#
# Usage:
#   scripts/verify-release.sh v0.1.0                                # download, check sums, execute
#   scripts/verify-release.sh v0.1.0 --triple aarch64-apple-darwin  # override host detection
#   scripts/verify-release.sh v0.1.0 --channels                     # by hand, AFTER release-promote
#   scripts/verify-release.sh v0.1.0 --keep                         # leave the downloads on disk
#
# This is the two `release-verify` Buildkite steps, one in the ci-linux-arm64 guest and one in the
# macOS guest, run after `release-publish` has made the release a prerelease and before
# `release-promote` makes it latest.
#
# Everything upstream of this script hashed bytes it had produced itself, which says nothing about
# what GitHub will serve. So this fetches over the public download URL with plain anonymous curl --
# never `gh`, never an authenticated API call -- and unsets any token the caller leaked into the
# environment. That is the whole supply-chain value: a verifier that can read a draft, or that holds
# write scope, cannot tell "published" from merely "uploaded".
#
# It EXECUTES a binary rather than reading a mode bit. "Has the executable bit" and "runs against this
# libc and gets past Gatekeeper" are different claims, and only the second is the claim a user meets.
# No single machine can execute every asset, and this cluster cannot execute some of them at all: it
# is Apple silicon throughout, so there is no x86_64 Linux host and no Windows host. Every archive is
# still downloaded, hash-checked, listed and extracted; what runs is the binary native to this host,
# plus the x86_64 macOS slice under Rosetta when the macOS guest has Rosetta.
#
# A check that cannot be made on the host it happens to be running on prints SKIP and names the
# reason, and is counted as a skip rather than a pass. Reporting a check that never ran as green is
# the one failure mode this release design cannot survive, because nothing downstream looks again.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# Hard-coded on purpose: this script must not learn which repository to verify from the environment
# it is running in, or a misconfigured step could verify somebody else's release and pass.
REPO="Lightless-Labs/pessimal"
MANIFEST="$ROOT_DIR/scripts/release-manifest.sh"

tag=""
triple=""
channels="no"
keep="no"

usage() { sed -n '2,8p' "$0" | sed 's/^# \{0,1\}//'; }
usage_die() { printf 'error: %s\n\n' "$1" >&2; usage >&2; exit 2; }
die() { printf 'FAIL  %s\n' "$1" >&2; exit 1; }

checks=0
skip_count=0
skip_log=""
ok() { printf 'ok    %s\n' "$1"; checks=$((checks + 1)); }
skip() { printf 'SKIP  %s\n' "$1"; skip_count=$((skip_count + 1)); skip_log="$skip_log  - $1"$'\n'; }
note() { printf '      %s\n' "$1"; }

while [ $# -gt 0 ]; do
  case "$1" in
    --triple) triple="${2:?--triple needs a value}"; shift 2 ;;
    --channels) channels="yes"; shift ;;
    --keep) keep="yes"; shift ;;
    -h|--help) usage; exit 0 ;;
    -*) usage_die "unknown option $1" ;;
    *) [ -z "$tag" ] || usage_die "unexpected argument $1"; tag="$1"; shift ;;
  esac
done

# Every tool, before the first download, so a guest image that lacks one fails in the first second
# naming it rather than after fetching the release. The images were never inventoried.
case "$(uname -s)" in
  Darwin) image="ci-macos-rust-bazel-ios-20260910-v2 (the macOS release-verify guest) or this Mac" ;;
  *)      image="ci-linux-arm64-rust-bazel (the Linux release-verify guest) or this host" ;;
esac
need() {
  command -v "$1" >/dev/null 2>&1 || die "$1 is required and is not on PATH in $image${2:+ -- $2}"
}
need curl "every asset is fetched anonymously over the public URL"
# --retry-all-errors arrived in curl 7.71. An older curl rejects the option outright, and every
# download would then fail looking like a missing asset rather than an old tool. Captured rather than
# piped into `grep -q`: under pipefail, grep exiting at the first match can SIGPIPE curl mid-write and
# fail the check on exactly the curl that passes it.
curl_help="$(curl --help all 2>/dev/null || true)"
case "$curl_help" in
  *--retry-all-errors*) ;;
  *) die "the curl on PATH in $image does not support --retry-all-errors (curl 7.71 or newer is required)" ;;
esac
need tar "the agent archives are listed and extracted"
need grep
need awk
need sort
need comm
need find "archive members are checked for stray executable bits"
need mktemp
if command -v shasum >/dev/null 2>&1; then
  sum_cmd=(shasum -a 256 -c)
elif command -v sha256sum >/dev/null 2>&1; then
  sum_cmd=(sha256sum -c)
else
  die "neither shasum nor sha256sum is on PATH in $image; this is the check a user performs, so there is no substitute for it"
fi
if [ "$(uname -s)" = "Darwin" ]; then
  need codesign "the agent slices and the app are signature-checked"
  need spctl "Gatekeeper's notarisation verdict is asserted"
  need ditto "Pessimal.app is expanded the way Finder would"
  need xcrun "stapler validate checks the app's ticket"
  [ -x /usr/libexec/PlistBuddy ] || die "/usr/libexec/PlistBuddy is missing in $image; the app's executable name is read with it"
fi
if [ "$channels" = "yes" ]; then
  need cmp "--channels compares /releases/latest's SHA256SUMS with the tag's"
  need mise "--channels verifies the mise github: backend"
  need ubi "--channels verifies standalone ubi"
fi

[ -n "$tag" ] || usage_die "a tag is required, e.g. v0.1.0"
# The same shape as the release steps' Buildkite condition and release-guard's check, so the tag, the
# manifest version and the string the binary prints cannot drift apart between them.
if ! printf '%s\n' "$tag" | grep -Eq '^v[0-9]+\.[0-9]+\.[0-9]+$'; then
  usage_die "tag must look like v1.2.3 (got '$tag')"
fi
version="${tag#v}"

# Holding nothing is the point, so do not merely avoid using a token -- remove it. A token in the
# environment would let curl follow an authenticated redirect and let mise see a draft, which is
# exactly the distinction this script exists to make. The cost is real and is accepted: anonymous
# mise and ubi share the host IP's 60 API requests/hour, so a rate-limit failure in --channels is
# user-experience data about this install path rather than a bug in the release. MISE_GITHUB_TOKEN
# and GITHUB_API_TOKEN are mise's own spellings of the same credential, so they go too, and
# DOPPLER_TOKEN because nothing here may be able to fetch one.
unset GH_TOKEN GITHUB_TOKEN GH_ENTERPRISE_TOKEN GITHUB_ENTERPRISE_TOKEN \
  MISE_GITHUB_TOKEN GITHUB_API_TOKEN DOPPLER_TOKEN

base_url="https://github.com/$REPO/releases/download/$tag"

detect_triple() {
  local os arch
  os="$(uname -s)"
  arch="$(uname -m)"
  case "$os-$arch" in
    Darwin-arm64|Darwin-aarch64) printf 'aarch64-apple-darwin\n' ;;
    Darwin-x86_64)               printf 'x86_64-apple-darwin\n' ;;
    Linux-x86_64)                printf 'x86_64-unknown-linux-gnu\n' ;;
    Linux-aarch64|Linux-arm64)   printf 'aarch64-unknown-linux-gnu\n' ;;
    *) die "no release asset is native to $os/$arch" ;;
  esac
}
host_triple="$(detect_triple)"
triple="${triple:-$host_triple}"

fetch() {
  # A release un-drafted seconds ago can 404 briefly while the asset CDN catches up, and this design
  # is forward-only: a false red here leaves a prerelease for a human to untangle rather than
  # retrying itself. So retry the transport, and only the transport.
  curl -fsSL --retry 5 --retry-delay 5 --retry-all-errors -o "$2" "$1"
}

# Gatekeeper's verdict on a path, asserted on exit status AND the source line: exit 0 alone also
# covers verdicts such as "accepted, source=Developer ID" that say nothing about notarisation, and
# notarisation is the claim the release notes make. Four tries fifteen seconds apart, because the
# notary CDN can lag the notarytool verdict by a minute, and this release is forward-only: a false
# red strands a prerelease for a human to untangle.
#   assess_notarized <label> <spctl --type/--context args...> -- <path>
assess_notarized() {
  local label="$1" path out status attempt
  shift
  local args=()
  while [ "$1" != "--" ]; do args+=("$1"); shift; done
  path="$2"
  for attempt in 1 2 3 4; do
    status=0
    out="$(spctl --assess "${args[@]}" --verbose=4 "$path" 2>&1)" || status=$?
    note "spctl --assess ${args[*]} (attempt $attempt of 4) exited $status: $(printf '%s' "$out" | tr '\n' ' ')"
    if [ "$status" -eq 0 ] && printf '%s\n' "$out" | grep -q '^source=Notarized Developer ID$'; then
      ok "spctl --assess ${args[*]} accepted $label with source=Notarized Developer ID (attempt $attempt of 4)"
      return 0
    fi
    [ "$attempt" -lt 4 ] || break
    sleep 15
  done
  die "spctl --assess ${args[*]} did not report source=Notarized Developer ID for $label after 4 attempts over 45s; last verdict: $(printf '%s' "$out" | tr '\n' ' ')"
}

# With assessments disabled, spctl has no verdict to give about notarisation, so asserting one would
# test the guest image rather than the release. Said as a SKIP, never folded into a pass.
gatekeeper_enabled() {
  spctl --status 2>/dev/null | grep -q 'assessments enabled'
}

# /usr/bin/true is universal, so this fails only when Rosetta is absent or too slow to start. Both are
# properties of the guest image, not of the release. Uses the same 900 s limit as release-macos-build.
rosetta_available() {
  run_watched 900 arch -x86_64 /usr/bin/true
  [ "$run_status" -eq 0 ]
}

# Runs a command with stdin closed and a watchdog, and sets run_out (stdout and stderr) and run_status
# (124 when the watchdog killed it). A --version that never returns would otherwise hold the step
# until Buildkite's own timeout, and then say nothing about which binary hung.
#   run_watched <seconds> <command...>
run_watched() {
  local limit="$1" pid waited=0 out_file
  shift
  out_file="$work/run-watched.out"
  "$@" >"$out_file" 2>&1 </dev/null &
  pid=$!
  while kill -0 "$pid" 2>/dev/null; do
    if [ "$waited" -ge "$limit" ]; then
      kill -9 "$pid" 2>/dev/null || true
      wait "$pid" 2>/dev/null || true
      run_status=124
      run_out="$(cat "$out_file")"
      return 0
    fi
    sleep 1
    waited=$((waited + 1))
  done
  run_status=0
  wait "$pid" || run_status=$?
  run_out="$(cat "$out_file")"
}

work="$(mktemp -d)"
dist="$work/dist"
extract="$work/extract"
mkdir -p "$dist" "$extract"
cleanup() {
  if [ "$keep" = "yes" ]; then
    printf 'kept the downloads: %s\n' "$work"
  else
    rm -rf "$work"
  fi
}
# EXIT alone owns cleanup; INT and TERM only turn into an exit. A handler that merely returned would
# let the script carry on past the interrupted download and print verdicts about files it never got.
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# Run through bash rather than by its exec bit, so a checkout that lost its modes in an rsync is not a
# false red for a reason unrelated to the release.
[ -f "$MANIFEST" ] || die "$MANIFEST is missing; it is the single source of the asset names and this script deliberately holds none of its own"
manifest() { bash "$MANIFEST" "$@"; }
count() { printf '%s\n' "$1" | grep -c . || true; }

all_names="$(manifest "$version" all)" || die "release-manifest.sh $version all failed"
agent_names="$(manifest "$version" agents)" || die "release-manifest.sh $version agents failed"
app_asset="$(manifest "$version" app)" || die "release-manifest.sh $version app failed"
sums_name="$(manifest "$version" sums)" || die "release-manifest.sh $version sums failed"
total="$(count "$all_names")"
[ "$(count "$app_asset")" -eq 1 ] || die "release-manifest.sh $version app did not print exactly one name"
[ "$sums_name" = "SHA256SUMS" ] || die "release-manifest.sh $version sums printed '$sums_name', expected SHA256SUMS"
# The groups must partition `all`, or this script would check a different release than publish made.
regrouped="$(printf '%s\n%s\n%s\n' "$agent_names" "$app_asset" "$sums_name" | LC_ALL=C sort)"
[ "$regrouped" = "$(printf '%s\n' "$all_names" | LC_ALL=C sort)" ] \
  || die "release-manifest.sh's agents + app + sums groups are not exactly its 'all' group: $(printf '%s' "$all_names" | tr '\n' ' ')"
ok "release-manifest.sh lists $total assets for version $version: $(count "$agent_names") agent archive(s), the app, SHA256SUMS"

# Asked of the manifest by triple rather than pattern-matched out of the list, so this script and the
# packager name this host's archive through the same case arm.
native_asset="$(manifest "$version" triple "$triple")" || die "release-manifest.sh defines no agent asset for triple $triple"
printf '%s\n' "$agent_names" | grep -Fxq -- "$native_asset" || die "$native_asset (for $triple) is not among the agent assets release-manifest.sh lists"

printf '\nverifying %s as an anonymous user would, from %s\n' "$tag" "$base_url"
note "host: $(uname -s) $(uname -m) (host triple $host_triple); the archive native to it: $native_asset"
if [ "$triple" != "$host_triple" ]; then
  note "--triple overrides host detection; executing a $triple binary on a $host_triple host may fail for that reason alone"
fi
printf '\n'

if [ "$channels" = "no" ]; then
  # Download every asset, not just this host's: `shasum -c` is only the check a user runs if it runs
  # over the whole file, and a missing asset has to fail here rather than in an install six weeks on.
  while IFS= read -r name <&3; do
    [ -n "$name" ] || continue
    fetch "$base_url/$name" "$dist/$name" || die "could not download $name from $base_url -- is the release published, and is the asset name exactly right?"
    [ -s "$dist/$name" ] || die "$name downloaded as an empty file"
    note "fetched $name ($(wc -c < "$dist/$name" | tr -d ' ') bytes)"
  done 3<<EOF
$all_names
EOF
  ok "all $total assets downloaded anonymously over $base_url, none empty"

  sums="$dist/SHA256SUMS"

  # mise's github backend auto-discovers a file named exactly SHA256SUMS and enforces it, so these
  # are not cosmetics: a malformed SHA256SUMS is a failed install for every mise user, and it fails
  # at their machine rather than here.
  if grep -q "$(printf '\r')" "$sums"; then
    die "SHA256SUMS contains a carriage return; it must be LF-only for shasum -c and for mise"
  fi
  ok "SHA256SUMS has no CR line endings"

  malformed="$(grep -nEv '^[0-9a-f]{64}  [^ /]+$' "$sums" || true)"
  if [ -n "$malformed" ]; then
    die "SHA256SUMS is not plain two-space 'shasum -a 256' text with bare filenames (no ./, no directory component, no binary-mode '*'); offending line(s): $(printf '%s' "$malformed" | tr '\n' '|')"
  fi
  ok "every SHA256SUMS line is 64 hex digits, two spaces, then a bare filename"

  expected_sums="$(printf '%s\n' "$all_names" | grep -vx 'SHA256SUMS' | LC_ALL=C sort)"
  actual_sums="$(awk '{print $2}' "$sums" | LC_ALL=C sort)"
  mismatch="$(LC_ALL=C comm -3 <(printf '%s\n' "$expected_sums") <(printf '%s\n' "$actual_sums") || true)"
  if [ -n "$mismatch" ]; then
    die "SHA256SUMS does not name exactly the $((total - 1)) other assets the manifest lists; difference (left=expected only, right=found only): $(printf '%s' "$mismatch" | tr '\n' '|' | tr '\t' ' ')"
  fi
  ok "SHA256SUMS names exactly the $((total - 1)) other assets the manifest lists, character for character"

  if ! (cd "$dist" && "${sum_cmd[@]}" SHA256SUMS); then
    die "a downloaded asset does not match its line in SHA256SUMS (see the FAILED line above); the bytes GitHub served are not the bytes release-publish hashed"
  fi
  ok "the downloaded bytes match SHA256SUMS (${sum_cmd[*]} SHA256SUMS)"

  # Every agent archive, whether or not this host can run it: the layout and the modes are what mise
  # and ubi act on, and they can be proven anywhere. The list is read on fd 3 so that nothing run
  # inside the loop -- the extracted binary above all -- can swallow the rest of it from stdin.
  while IFS= read -r asset <&3; do
    [ -n "$asset" ] || continue
    prefix="pessimal-agent-$version-"
    case "$asset" in
      "$prefix"*.tar.gz) asset_triple="${asset#"$prefix"}"; asset_triple="${asset_triple%.tar.gz}" ;;
      *) die "do not know how to verify agent asset $asset; every shipped agent archive is pessimal-agent-<version>-<triple>.tar.gz" ;;
    esac
    [ "$(manifest "$version" triple "$asset_triple")" = "$asset" ] \
      || die "$asset does not round-trip through release-manifest.sh triple $asset_triple"

    archive="$dist/$asset"
    top="pessimal-agent-$version-$asset_triple"
    listing="$(tar -tzf "$archive")" || die "$asset is not a readable .tar.gz"
    # One top-level directory and zero top-level files is what makes mise apply strip_components=1
    # automatically; an extra top-level file silently turns that off for every mise user.
    top_files="$(printf '%s\n' "$listing" | grep -v '/' || true)"
    [ -z "$top_files" ] || die "$asset has top-level file(s), which stops mise stripping the leading directory: $(printf '%s' "$top_files" | tr '\n' '|')"
    top_level="$(printf '%s\n' "$listing" | awk -F/ 'NF>0 {print $1}' | sort -u)"
    [ "$(count "$top_level")" -eq 1 ] || die "$asset has more than one top-level directory: $(printf '%s' "$top_level" | tr '\n' ' ')"
    [ "$top_level" = "$top" ] || die "$asset's top-level directory is '$top_level', expected '$top' -- the directory name carries the version, so this also catches an archive built from the wrong one"
    ok "$asset holds exactly one top-level directory, $top/, and no top-level files"

    mkdir -p "$extract/$asset_triple"
    tar -xzf "$archive" -C "$extract/$asset_triple" || die "$asset did not extract; tar reported the error above"
    binary="$extract/$asset_triple/$top/pessimal-agent"
    [ -f "$binary" ] || die "$asset does not contain pessimal-agent"
    # ubi's partial match is "basename starts with the exe stem AND has the executable bit", so the
    # mode is load-bearing for installation, not only for running.
    [ -x "$binary" ] || die "pessimal-agent in $asset is not executable ($(ls -l "$binary"))"
    # The other half of the same ubi rule: pessimal.example.toml and pessimal-agent.service both start
    # with the exe stem, so an exec bit on either makes it an install candidate.
    stray_exec="$(find "$extract/$asset_triple/$top" -type f -perm -u+x ! -name pessimal-agent)"
    [ -z "$stray_exec" ] || die "$asset ships executable member(s) besides the binary, which ubi's partial match would consider installing: $(printf '%s' "$stray_exec" | tr '\n' ' ')"
    ok "$asset: pessimal-agent is executable and nothing else in the archive is"

    if [ "$(uname -s)" = "Darwin" ]; then
      case "$asset_triple" in
        *-apple-darwin)
          # Structure, not host ISA: codesign reads an x86_64 slice on an arm64 host as readily as a
          # native one, so both slices are checked here on a machine that never saw the keychain.
          codesign --verify --strict --verbose=2 "$binary" || die "codesign --verify --strict rejected pessimal-agent from $asset; it is not validly signed as shipped"
          ok "codesign --verify --strict passed on pessimal-agent from $asset"
          # Printed, not asserted: the identifier and authority are what a launchd, firewall or TCC
          # rule is keyed on, so the log should say which ones shipped.
          note "signature: $(codesign -dvv "$binary" 2>&1 | grep -E '^(Identifier=|Authority=|TeamIdentifier=|CodeDirectory )' | tr '\n' ' ')"
          if ! gatekeeper_enabled; then
            skip "spctl notarisation assessment of $asset's binary: Gatekeeper assessments are disabled in this image (spctl --status), so there is no verdict to assert; release-macos's notarytool status is the remaining evidence"
          else
            # NOT `--type execute`, which is what a bundle takes. Measured on macOS 26.2 against two
            # notarized Developer ID bare Mach-O CLIs (2026-09-14): `spctl --assess --type execute`
            # says "rejected (the code is valid but does not seem to be an app)" and exits 3, whatever
            # the notarisation status, so it would fail every release. `--type open --context
            # context:primary-signature` is the form for non-bundle code: it said "accepted
            # source=Notarized Developer ID" for both, and "rejected" for the ad-hoc arm64 agent. It
            # reads structure, not host ISA, so the x86_64 slice is assessed here too -- measured on
            # the same arm64 host: a thin x86_64 slice lipo'd out of a notarized universal CLI was
            # "accepted source=Notarized Developer ID", while the ad-hoc and the unsigned x86_64 agent
            # were both "rejected" (exit 3). A bare Mach-O cannot carry a stapled ticket
            # (packaging/macos/GATEKEEPER.md), so this verdict is fetched from Apple online.
            assess_notarized "pessimal-agent from $asset" --type open --context context:primary-signature -- "$binary"
          fi
          ;;
      esac
    fi

    # What executes here: the binary native to this host, and on an Apple silicon Mac with Rosetta
    # also the x86_64 slice, which is the one Intel Mac users get and the one signed across
    # architectures. Rosetta was measured on this project's Apple silicon Tart guest (2026-09-14): the
    # first run of a new x86_64 binary took 14 s while Rosetta translated it, then printed its version.
    # An earlier probe that killed it after 15 s had mistaken that translation for a hang, which is why
    # the watchdog below allows far longer than any warm run needs.
    runner=""
    if [ "$asset_triple" = "$triple" ]; then
      runner="native"
    elif [ "$asset_triple" = "x86_64-apple-darwin" ] && [ "$host_triple" = "aarch64-apple-darwin" ]; then
      if rosetta_available; then
        runner="rosetta"
      else
        skip "executing $asset: this cluster has no Intel Mac, and Rosetta did not start in this guest (arch -x86_64 /usr/bin/true, exit $run_status); its signature and notarisation are checked above"
      fi
    else
      case "$asset_triple" in
        x86_64-unknown-linux-gnu)
          skip "executing $asset: this cluster has no x86_64 Linux machine; its checksum, layout and modes are verified above and its glibc floor by release-linux" ;;
        *)
          skip "executing $asset: the $asset_triple binary is not native to this $host_triple host; the release-verify step on the other queue runs it" ;;
      esac
    fi
    [ -n "$runner" ] || continue

    if [ "$runner" = "rosetta" ]; then
      run_watched 900 arch -x86_64 "$binary" --version
      how="under Rosetta (arch -x86_64) on this $host_triple host"
      # A slow Rosetta is a property of the guest, as in release-macos-build: report it, do not fail.
      if [ "$run_status" -eq 124 ]; then
        skip "executing $asset: Rosetta did not finish --version within 900 s in this guest; its signature and notarisation are checked above"
        continue
      fi
    else
      run_watched 300 "$binary" --version
      how="natively on this host"
    fi
    [ "$run_status" -ne 124 ] || die "pessimal-agent from $asset was still running $how after 300 s of --version, and was killed: $run_out"
    [ "$run_status" -eq 0 ] || die "the extracted binary from $asset did not run $how (exit $run_status): $run_out"
    printed_version="$(printf '%s' "$run_out" | tr -d '\r')"
    case "$printed_version" in
      *"pessimal-agent"*) ;;
      *) die "--version printed '$printed_version', which does not name pessimal-agent" ;;
    esac
    case "$printed_version" in
      *"$version"*) ;;
      *) die "--version printed '$printed_version', which does not contain the tag's version $version" ;;
    esac
    ok "executed pessimal-agent from $asset $how; --version printed '$printed_version'"
  done 3<<EOF
$agent_names
EOF

  # Nothing to execute and nothing to download: named so the omission reads as a decision.
  skip "Windows: no Windows asset is published (deferred from the first release; see scripts/release-manifest.sh), and this cluster has no Windows machine to execute one on"

  if [ "$(uname -s)" = "Darwin" ]; then
    app_dir="$extract/app"
    mkdir -p "$app_dir"
    # ditto, not unzip: the bundle carries symlinks and resource metadata, and ditto is what wrote
    # the archive in release-macos.
    ditto -x -k "$dist/$app_asset" "$app_dir" || die "ditto could not expand $app_asset"
    app="$app_dir/Pessimal.app"
    [ -d "$app" ] || die "$app_asset did not expand to Pessimal.app (found: $(ls "$app_dir" | tr '\n' ' '))"
    ok "$app_asset expanded to Pessimal.app"
    codesign --verify --deep --strict --verbose=2 "$app" || die "codesign --verify --deep --strict rejected Pessimal.app as shipped"
    ok "codesign --verify --deep --strict passed on Pessimal.app"
    # Evidence for the arm64-only caveat in the release notes, printed rather than asserted: the app
    # is built with -target "$(uname -m)-apple-macos..." and there is no lipo step.
    app_exe="$app/Contents/MacOS/$(/usr/libexec/PlistBuddy -c 'Print :CFBundleExecutable' "$app/Contents/Info.plist" 2>/dev/null || echo Pessimal)"
    if [ -f "$app_exe" ]; then
      note "Pessimal.app architectures: $(lipo -archs "$app_exe" 2>/dev/null || echo unknown)"
    fi
    xcrun stapler validate "$app" || die "xcrun stapler validate found no valid stapled ticket in Pessimal.app; a bundle can hold one, so this is a release-macos failure rather than a Gatekeeper quirk"
    ok "xcrun stapler validate found a stapled notarisation ticket in Pessimal.app"
    if gatekeeper_enabled; then
      # `--type execute` is the right form here, unlike for the bare agent binary: this is a bundle.
      assess_notarized "Pessimal.app" --type execute -- "$app"
    else
      skip "spctl --assess on Pessimal.app: Gatekeeper assessments are disabled in this image (spctl --status); the stapled ticket above is the evidence"
    fi
    # Nothing launches the app anywhere in the pipeline: a headless guest cannot exercise a menu bar
    # app. That is the runbook's last human step -- browser download, Finder expand, launch on a
    # physical SIP-enabled Mac.
    skip "launching Pessimal.app: no guest can exercise a menu bar app, and only a browser download expanded in Finder sets com.apple.quarantine"
  else
    skip "$app_asset: downloaded and hash-checked, but stapler, spctl and ditto exist only on macOS (host is $(uname -s)); the macOS release-verify step checks it"
  fi
fi

if [ "$channels" = "yes" ]; then
  # /releases/latest returns "the most recent non-prerelease, non-draft release", so this is the only
  # assertion that release-promote actually landed. A pinned @version install passes either way, and
  # would have passed throughout the verification window while latest still meant the release before.
  fetch "https://github.com/$REPO/releases/latest/download/SHA256SUMS" "$work/latest-SHA256SUMS" || die "releases/latest/download/SHA256SUMS could not be fetched at all, so /releases/latest resolves to nothing: no non-prerelease, non-draft release exists"
  fetch "$base_url/SHA256SUMS" "$work/tag-SHA256SUMS" || die "could not download $tag's SHA256SUMS from $base_url"
  if ! cmp -s "$work/latest-SHA256SUMS" "$work/tag-SHA256SUMS"; then
    die "releases/latest/download/SHA256SUMS differs from $tag's, so /releases/latest does not resolve to $tag (still a prerelease, or a newer release exists)"
  fi
  ok "releases/latest/download/SHA256SUMS is byte-identical to $tag's, so /releases/latest resolves to $tag"

  # Both backends, because the repo's own distribution plan once documented the deprecated `ubi:`
  # spelling while mise now recommends `github:`. Their presence was checked at the top: an absent
  # tool is not a passing check.
  mise_out="$(mise exec "github:$REPO@$version" -- pessimal-agent --version 2>&1)" || die "mise exec github:$REPO@$version failed: $mise_out"
  case "$(printf '%s' "$mise_out" | tr -d '\r')" in
    *"$version"*) ok "mise exec github:$REPO@$version printed a version containing $version" ;;
    *) die "mise exec github:$REPO@$version printed '$mise_out', which does not contain $version" ;;
  esac

  # --in, because ubi installs into ./bin by default and would litter the checkout.
  mkdir -p "$work/ubi"
  ubi --project "$REPO" --tag "$tag" --exe pessimal-agent --in "$work/ubi" || die "ubi --project $REPO --tag $tag --exe pessimal-agent failed; the asset set does not satisfy ubi's matcher"
  ubi_out="$("$work/ubi/pessimal-agent" --version 2>&1)" || die "the binary ubi installed did not run: $ubi_out"
  case "$(printf '%s' "$ubi_out" | tr -d '\r')" in
    *"$version"*) ok "ubi --project $REPO --tag $tag installed a binary that printed '$ubi_out'" ;;
    *) die "the binary ubi installed printed '$ubi_out', which does not contain $version" ;;
  esac
fi

printf '\n%s: %d check(s) passed, %d skipped\n' "$tag" "$checks" "$skip_count"
if [ "$skip_count" -gt 0 ]; then
  printf 'not proven on this host:\n%s' "$skip_log"
fi
