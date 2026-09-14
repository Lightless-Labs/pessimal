#!/usr/bin/env bash
# Builds the unsigned macOS release inputs: the whole of the `release-macos-build` Buildkite step.
#
# Usage:
#   scripts/release-build-macos.sh [--out DIR]
#     --out   the upload directory; must start empty (default .build/release/unsigned)
#
# Runs in the ci-macos-rust-bazel-ios-20260910-v2 Tart guest, on a tag build only: BUILDKITE_TAG=vX.Y.Z
# must be set and must agree with [workspace.package] version. It:
#   1. builds both agent slices (aarch64 natively, x86_64 with --target) and Pessimal.app;
#   2. runs them (x86_64 only under Rosetta, and only if the guest has it -- it says which);
#   3. writes unsigned-aarch64-apple-darwin.tar, unsigned-x86_64-apple-darwin.tar and
#      unsigned-Pessimal.app.tar into --out, for the step's `artifact_paths`.
#
# It signs nothing and holds no credential. The `release-macos` step downloads the three tars into a
# new guest and signs them there with scripts/release-macos-artifacts.sh. The build and the signing are in
# different steps because a build runs code from every build.rs in the dependency graph, and a process
# can read its parent's starting environment with `ps -E`. Unexporting a token does not clear that, so
# the only safe place for the signing token is a guest where nothing is built.
set -euo pipefail

# This step must never hold the signing token. If it does, the pipeline gave it one by mistake.
if [[ -n "${DOPPLER_TOKEN:-}" ]]; then
  printf 'release-build-macos: error: DOPPLER_TOKEN is set. The build step must not hold a credential; remove doppler_token_secret from release-macos-build.\n' >&2
  exit 1
fi

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
IMAGE="ci-macos-rust-bazel-ios-20260910-v2"
MSRV="1.95"

say() { printf '%s\n' "$*"; }
die() { printf 'release-build-macos: error: %s\n' "$*" >&2; exit 1; }
phase() { printf -- '--- %s (t+%ss)\n' "$*" "$SECONDS"; }

out_dir="$ROOT_DIR/.build/release/unsigned"
work_dir="$ROOT_DIR/.build/release/work"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --out) out_dir="${2:?--out needs a directory}"; shift 2 ;;
    -h|--help) sed -n '2,19p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) die "unknown argument: $1" ;;
  esac
done

# ---- preconditions: everything that can be known before a byte is compiled ---------------------

[[ "$(uname -s)" == "Darwin" ]] || die "this builds the macOS release on the $IMAGE guest; this host is $(uname -s)"
[[ "$(uname -m)" == "arm64" ]] \
  || die "this must run on Apple silicon ($IMAGE, queue ci-macos-apple-silicon): the aarch64 slice and Pessimal.app are built for the host architecture, and this host is $(uname -m)"

# Every tool, before anything runs: the image was never inventoried. The release-macos step signs on
# the same image, so its tools (notarytool, stapler, codesign) are checked here too. Finding one
# missing now costs a minute. Finding it after the build costs the build.
missing=""
for tool in cargo rustc rustup swiftc otool plutil lipo arch tar gzip install mktemp find ditto codesign \
            xattr shasum awk sort xcrun security openssl base64 curl spctl; do
  command -v "$tool" >/dev/null 2>&1 || missing="$missing $tool"
done
[[ -x /usr/libexec/PlistBuddy ]] || missing="$missing /usr/libexec/PlistBuddy"
if command -v xcrun >/dev/null 2>&1; then
  xcrun --find notarytool >/dev/null 2>&1 || missing="$missing notarytool(xcrun)"
  xcrun --find stapler >/dev/null 2>&1 || missing="$missing stapler(xcrun)"
fi
# /usr/bin/python3 on macOS is a stub that exists whether or not an interpreter stands behind it, so it
# is asked to run rather than asked whether it is on PATH.
python3 -c 'import base64, json, tarfile, urllib.request' >/dev/null 2>&1 || missing="$missing python3(a-working-one)"
[[ -z "$missing" ]] || die "missing from the $IMAGE guest:$missing"

if [[ -z "${HOME:-}" ]]; then
  export HOME=/Users/admin
fi

# The exact parser the pipeline's TestFlight step and scripts/set-workspace-version.sh use.
cargo_version="$(awk '/^\[workspace\.package\]/{f=1; next} /^\[/{f=0} f && /^version[[:space:]]*=/{gsub(/["[:space:]]/,"",$0); sub(/^version=/,"",$0); print; exit}' "$ROOT_DIR/Cargo.toml")"
[[ "$cargo_version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] \
  || die "could not read a MAJOR.MINOR.PATCH version from [workspace.package] in Cargo.toml (got '$cargo_version')"

# The tag is the whole of the intent. There is no untagged mode: a release that goes wrong is fixed by
# deleting the tag and cutting it again, so nothing here needs to run without one. release-guard has
# already compared the tag with Cargo.toml; it is compared again because this is the step whose
# binaries and bundle carry the version.
tag="${BUILDKITE_TAG:-}"
[[ -n "$tag" ]] \
  || die "BUILDKITE_TAG is empty. This builds releases from tags only. On a tag build, BUILDKITE_TAG is probably missing from the tart-ci plugin's env: allowlist; the guest only sees listed variables."
[[ "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "BUILDKITE_TAG '$tag' is not a vX.Y.Z release tag"
version="${tag#v}"
[[ "$version" == "$cargo_version" ]] \
  || die "tag $tag says $version but [workspace.package] version in Cargo.toml says $cargo_version"

say "release build for $tag (Cargo.toml agrees: $cargo_version); unsigned, for the release-macos step to sign"

# The upload directory must start empty: anything already in it would be uploaded with this run's
# tars. The work directory is this script's own, so it is cleared.
mkdir -p "$out_dir"
out_dir="$(cd "$out_dir" && pwd)"
if [[ -n "$(ls -A "$out_dir")" ]]; then
  die "$out_dir is not empty; refusing to mix this run's files with what is already there: $(ls -A "$out_dir" | tr '\n' ' ')"
fi
rm -rf "$work_dir" && mkdir -p "$work_dir"
work_dir="$(cd "$work_dir" && pwd)"
[[ "$out_dir" != "$work_dir" ]] || die "--out must not be $work_dir"

# ---- toolchain ---------------------------------------------------------------------------------

phase "rust toolchain"
# Mirrors the pipeline's MSRV gate -- this image ships rustc 1.88 and sysinfo needs 1.95 -- plus the
# x86_64-apple-darwin standard library the Intel slice needs.
rust_target_installed() { rustup target list --installed 2>/dev/null | grep -x "$1" >/dev/null; }
cargo_meets_msrv() {
  local have
  have="$(cargo -V | awk '{print $2}')"
  [[ "$(printf '%s\n%s\n' "$MSRV" "$have" | sort -V | head -1)" == "$MSRV" ]]
}
say "cargo reports $(cargo -V | awk '{print $2}'); MSRV is $MSRV"
if ! cargo_meets_msrv || ! rust_target_installed x86_64-apple-darwin; then
  if cargo_meets_msrv && [[ -w "${RUSTUP_HOME:-$HOME/.rustup}" ]]; then
    say "adding the x86_64-apple-darwin standard library to the current toolchain"
    rustup target add x86_64-apple-darwin
  else
    if [[ ! -w "${RUSTUP_HOME:-$HOME/.rustup}" ]]; then
      export RUSTUP_HOME="$HOME/.rustup-ci"
    fi
    say "installing stable with the x86_64-apple-darwin target into ${RUSTUP_HOME:-$HOME/.rustup}"
    rustup toolchain install stable --profile minimal --target x86_64-apple-darwin
    rustup default stable
  fi
fi
cargo_meets_msrv || die "cargo is still below MSRV $MSRV after the toolchain step: $(cargo -V)"
rust_target_installed x86_64-apple-darwin \
  || die "the x86_64-apple-darwin standard library is still not installed after the toolchain step"
say "$(cargo -V); targets: $(rustup target list --installed | tr '\n' ' ')"

# ---- build: everything, before any credential is touched ----------------------------------------

target_dir="$(cd "$ROOT_DIR" && cargo metadata --format-version 1 --no-deps --locked \
  | python3 -c 'import json, sys; print(json.load(sys.stdin)["target_directory"])')"
[[ -n "$target_dir" ]] || die "cargo metadata reported no target_directory"
arm_binary="$target_dir/release/pessimal-agent"
intel_binary="$target_dir/x86_64-apple-darwin/release/pessimal-agent"
rm -f "$arm_binary" "$intel_binary"

phase "cargo build --release --locked -p pessimal_agent_host (aarch64-apple-darwin, native)"
(cd "$ROOT_DIR" && cargo build --release --locked -p pessimal_agent_host)
[[ -s "$arm_binary" ]] || die "cargo reported success but wrote no binary at $arm_binary"

phase "cargo build --release --locked --target x86_64-apple-darwin -p pessimal_agent_host"
# Cross-built on this arm64 guest rather than on an Intel machine: measured working for this exact
# dependency graph, aws-lc-sys and ring included. Unlike the native slice it arrives with no signature
# at all, which is why release-macos-artifacts.sh signs both.
(cd "$ROOT_DIR" && cargo build --release --locked --target x86_64-apple-darwin -p pessimal_agent_host)
[[ -s "$intel_binary" ]] || die "cargo reported success but wrote no binary at $intel_binary"

phase "scripts/build-macos-app.sh --release"
# Its own CARGO_TARGET_DIR, because Cargo does not fingerprint MACOSX_DEPLOYMENT_TARGET: the app's
# staticlib build would otherwise reuse C objects the agent builds above compiled for this guest's
# macOS, and build-macos-app.sh's own "built for newer" check would then (correctly) fail the run.
# CODESIGN_IDENTITY/CODESIGN_KEYCHAIN are stripped so the bundle is built unsigned whatever a hand run
# has in its environment; the release-macos step signs it.
# build-macos-app.sh runs cargo without --locked. The agent builds above already proved Cargo.lock
# resolves the whole workspace unchanged, and the lockfile's hash is compared afterwards so a build
# that rewrote it cannot pass.
lock_before="$(shasum -a 256 "$ROOT_DIR/Cargo.lock" | awk '{print $1}')"
app_build_dir="$work_dir/app-build"
env -u CODESIGN_IDENTITY -u CODESIGN_KEYCHAIN \
  CARGO_TARGET_DIR="$target_dir/macos-app" \
  PESSIMAL_MACOS_BUILD_DIR="$app_build_dir" \
  "$ROOT_DIR/scripts/build-macos-app.sh" --release
lock_after="$(shasum -a 256 "$ROOT_DIR/Cargo.lock" | awk '{print $1}')"
[[ "$lock_before" == "$lock_after" ]] || die "building Pessimal.app changed Cargo.lock; the release would not be the locked graph"
app_bundle="$app_build_dir/Pessimal.app"
[[ -x "$app_bundle/Contents/MacOS/Pessimal" ]] || die "build-macos-app.sh reported success but $app_bundle has no executable"

# ---- prove the unsigned build before paying for a signature -------------------------------------

phase "architecture, version and a first execution"
check_arch() {
  local path="$1" want="$2" have
  have="$(lipo -archs "$path" 2>&1)" || die "lipo cannot read $path: $have"
  say "checked the architectures of $path, found '$have', expected '$want'"
  [[ "$have" == "$want" ]] || die "$path is '$have', not '$want'"
}
check_arch "$arm_binary" arm64
check_arch "$intel_binary" x86_64
check_arch "$app_bundle/Contents/MacOS/Pessimal" arm64

bundle_version="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$app_bundle/Contents/Info.plist")"
say "checked CFBundleShortVersionString, found '$bundle_version', expected '$version'"
[[ "$bundle_version" == "$version" ]] || die "Pessimal.app says $bundle_version but this release is $version"

# Runs "$@", but gives up after $1 seconds: returns its status, or 124 if it had to be killed. macOS
# ships no timeout(1) and bash 3.2 has no `wait -n`, hence the poll.
with_deadline() {
  local seconds="$1" pid waited=0 status=0
  shift
  "$@" &
  pid=$!
  while kill -0 "$pid" 2>/dev/null; do
    if (( waited >= seconds )); then
      kill -9 "$pid" 2>/dev/null || true
      wait "$pid" 2>/dev/null || true
      return 124
    fi
    sleep 1
    waited=$((waited + 1))
  done
  wait "$pid" || status=$?
  return "$status"
}

# Rosetta is a property of the guest image, which nobody inventoried. Asked once, answered plainly --
# and under a deadline, because asking is not always quick. MEASURED on a Tart macOS guest (the
# development Mac) on 2026-09-14: one `arch -x86_64 /usr/bin/true` had not returned after 120s, though it
# did return eventually; the next took 2.4s. The x86_64 agent's `--version` then took 12s on its first
# run, and in a stubbed run of this script on the same guest under load, the probe plus both version
# checks took 163s. Why the slow one was slow was not established, and a freshly cloned CI guest was
# not measured at all -- so the deadlines are generous, and running out of them is said out loud.
#
# So the x86_64 slice is held to a different standard from the native one, on purpose. The arm64
# binary must run, within 5 minutes, or the release stops. Under Rosetta, an exit status or a version
# that is wrong stops the release just the same -- that is the binary talking -- but running out of
# time is reported as NOT EXECUTED and the build goes on: that is Rosetta talking, and this cluster
# has no Intel Mac to settle it either way.
rosetta_seconds=900
rosetta_probe_status=0
rosetta_reason=""
with_deadline "$rosetta_seconds" arch -x86_64 /usr/bin/true >/dev/null 2>&1 || rosetta_probe_status=$?
case "$rosetta_probe_status" in
  0)   rosetta="yes" ;;
  124) rosetta="no"; rosetta_reason="\`arch -x86_64 /usr/bin/true\` did not return within ${rosetta_seconds}s" ;;
  *)   rosetta="no"; rosetta_reason="\`arch -x86_64 /usr/bin/true\` failed (exit $rosetta_probe_status): Rosetta is not installed in this guest" ;;
esac

# run_version LABEL SECONDS ON_TIMEOUT(die|report) COMMAND...
run_version() {
  local label="$1" seconds="$2" on_timeout="$3" reported status=0
  shift 3
  reported="$(with_deadline "$seconds" "$@" --version 2>&1)" || status=$?
  if [[ "$status" == "124" ]]; then
    [[ "$on_timeout" == "report" ]] || die "$label: '$* --version' did not finish within ${seconds}s"
    say "NOT EXECUTED: $label. '$* --version' did not finish within ${seconds}s under Rosetta, so it was never seen to run."
    return 0
  fi
  [[ "$status" == "0" ]] || die "$label: '$* --version' exited $status: $reported"
  say "checked '$label --version', found '$reported', expected 'pessimal-agent $version'"
  [[ "$reported" == "pessimal-agent $version" ]] || die "$label does not report version $version"
}
run_version "unsigned aarch64 slice" 300 die "$arm_binary"
if [[ "$rosetta" == "yes" ]]; then
  run_version "unsigned x86_64 slice" "$rosetta_seconds" report arch -x86_64 "$intel_binary"
else
  say "NOT EXECUTED: the x86_64 slice. $rosetta_reason, so only its architecture was checked."
fi

# ---- the three unsigned tars: the release-macos step's input, exactly -------------------------

phase "writing the three unsigned tars into $out_dir"
# Plain .tar, one payload each at the top, COPYFILE_DISABLE=1 so no AppleDouble `._` member rides
# along. release-macos-artifacts.sh insists on exactly one `pessimal-agent` / one `Pessimal.app` per
# tar, so each tar holds that and nothing else, and is re-listed here to prove it.
export COPYFILE_DISABLE=1
tar -cf "$out_dir/unsigned-aarch64-apple-darwin.tar" -C "$(dirname "$arm_binary")" pessimal-agent
tar -cf "$out_dir/unsigned-x86_64-apple-darwin.tar" -C "$(dirname "$intel_binary")" pessimal-agent
tar -cf "$out_dir/unsigned-Pessimal.app.tar" -C "$app_build_dir" Pessimal.app
for slice in aarch64 x86_64; do
  listing="$(tar -tf "$out_dir/unsigned-$slice-apple-darwin.tar")"
  say "checked unsigned-$slice-apple-darwin.tar, found '$(printf '%s' "$listing" | tr '\n' ' ')', expected 'pessimal-agent'"
  [[ "$listing" == "pessimal-agent" ]] || die "unsigned-$slice-apple-darwin.tar does not hold exactly pessimal-agent"
done
app_roots="$(tar -tf "$out_dir/unsigned-Pessimal.app.tar" | awk -F/ '{print $1}' | LC_ALL=C sort -u)"
say "checked unsigned-Pessimal.app.tar, found top-level '$(printf '%s' "$app_roots" | tr '\n' ' ')', expected 'Pessimal.app'"
[[ "$app_roots" == "Pessimal.app" ]] || die "unsigned-Pessimal.app.tar does not hold exactly Pessimal.app at its top"
tar -tf "$out_dir/unsigned-Pessimal.app.tar" | grep -x 'Pessimal.app/Contents/MacOS/Pessimal' >/dev/null \
  || die "unsigned-Pessimal.app.tar has no Pessimal.app/Contents/MacOS/Pessimal"

phase "done"
rm -rf "$work_dir"
say "unsigned macOS release inputs for $version in $out_dir:"
ls -l "$out_dir"
