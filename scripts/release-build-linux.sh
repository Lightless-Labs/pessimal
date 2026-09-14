#!/usr/bin/env bash
# Builds, checks and packages both Linux agent tarballs: the whole of the `release-linux` Buildkite step.
#
# Usage:
#   scripts/release-build-linux.sh [--out DIR]      # default DIR: .build/release/dist
#
# Runs in the ci-linux-arm64-rust-bazel Tart guest, on a tag build only: BUILDKITE_TAG=vX.Y.Z must be
# set and must agree with [workspace.package] version. It holds no secret and needs none. In order:
#   1. cargo test --workspace --locked, so a release never assumes the push build was green;
#   2. installs a PINNED zig and cargo-zigbuild inside the guest;
#   3. builds x86_64-unknown-linux-gnu.2.28 and aarch64-unknown-linux-gnu.2.28, BOTH through zigbuild;
#   4. scripts/check-glibc-floor.sh on both, against 2.28;
#   5. EXECUTES the aarch64 binary (this guest is arm64) and asserts `--version` names this version;
#   6. packages both with scripts/package-agent-release.sh into DIR, which must start empty and ends
#      holding exactly `release-manifest.sh <version> linux` plus a `.sha256` beside each.
# DIR is what the step's `artifact_paths` uploads. Buildkite's own agent is not reachable from inside
# the guest, so point --out into the shared checkout (or rsync DIR back) for the upload to find it.
#
# Why the aarch64 binary goes through zigbuild on an arm64 guest that could build it natively: a plain
# `cargo build` links against whatever glibc the guest image carries, so the floor would silently float
# up to that image's version the day it is rebuilt. The `.2.28` target suffix is what pins it, and the
# floor check is what proves it. Do not "simplify" the native architecture back to plain cargo.
#
# Why zig comes from the `ziglang` PyPI wheel, fetched with curl and checked against a pinned SHA-256
# rather than `pip install`ed: whether this image has pip at all, and whether its system Python refuses
# installs as externally managed, was never measured. A wheel is a zip, and python3 -- which the floor
# check needs anyway -- can unpack it with the standard library. The pinned pair is the one that built
# the measured binaries on the release Mac: zig 0.15.2 with cargo-zigbuild 0.22.1.
#
# Nothing here reads a credential, and the build deliberately does not set RUSTFLAGS=-D warnings: a
# new upstream warning must not block a release that has nothing wrong with it.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
IMAGE="ci-linux-arm64-rust-bazel"
MSRV="1.95"
GLIBC_FLOOR="2.28"
ZIG_VERSION="0.15.2"
# The Linux aarch64 wheel for the version above. PyPI file URLs are content-addressed and
# permanent; the hash is the one PyPI publishes for this file, and was re-computed over a download of
# this exact URL on 2026-09-13.
ZIG_WHEEL_URL="https://files.pythonhosted.org/packages/53/7d/8c277208250ffa72f12a10f52dfc1d45850f08244093065b40c5f4628260/ziglang-0.15.2-py3-none-manylinux_2_17_aarch64.manylinux2014_aarch64.musllinux_1_1_aarch64.whl"
ZIG_WHEEL_SHA256="edc0aa60ec964a4cf462d40f68d7de242ddf37fd9a80f2afaee6397059463230"
CARGO_ZIGBUILD_VERSION="0.22.1"
LINUX_TRIPLES="aarch64-unknown-linux-gnu x86_64-unknown-linux-gnu"

say() { printf '%s\n' "$*"; }
die() { printf 'release-build-linux: error: %s\n' "$*" >&2; exit 1; }
phase() { printf -- '--- %s (t+%ss)\n' "$*" "$SECONDS"; }

out_dir="$ROOT_DIR/.build/release/dist"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --out) out_dir="${2:?--out needs a directory}"; shift 2 ;;
    -h|--help) sed -n '2,17p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) die "unknown argument: $1" ;;
  esac
done

# ---- preconditions: everything that can be known before a byte is downloaded or compiled ----------

[[ "$(uname -s)" == "Linux" ]] || die "this builds the Linux release on the $IMAGE guest; this host is $(uname -s)"
case "$(uname -m)" in
  aarch64|arm64) ;;
  *) die "this must run on an arm64 Linux guest ($IMAGE, queue ci-linux-arm64): it executes the aarch64 binary natively, and this host is $(uname -m)" ;;
esac

# The contents of the guest image were never inventoried, so every tool is named here, at the top,
# rather than discovered as a "command not found" twenty minutes into a build. That includes what
# the children need -- scripts/package-agent-release.sh (tar, gzip, install, mktemp) and
# scripts/check-glibc-floor.sh (python3) check for themselves too, but only after the build -- and
# `cc`, which zig does not replace: build scripts and cargo-zigbuild itself are linked for the host.
missing=""
for tool in cargo rustc rustup cc curl tar gzip install mktemp awk sort grep; do
  command -v "$tool" >/dev/null 2>&1 || missing="$missing $tool"
done
# Asked to run, not merely looked up: it unpacks zig, reads the ELF headers and parses cargo metadata,
# so an interpreter that exists but cannot import the standard library is as missing as none.
python3 -c 'import json, struct, zipfile' >/dev/null 2>&1 || missing="$missing python3(a-working-one)"
if ! command -v sha256sum >/dev/null 2>&1 && ! command -v shasum >/dev/null 2>&1; then
  missing="$missing sha256sum-or-shasum"
fi
[[ -z "$missing" ]] || die "missing from PATH in the $IMAGE guest:$missing"

# Same fallback as the pipeline's existing Linux step: the tart-ci guest can start the command with
# HOME unset or "/", and cargo, rustup and zig all write caches under it.
if [[ -z "${HOME:-}" || "$HOME" == "/" ]]; then
  export HOME=/home/admin
fi

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | awk '{print $1}'
  else shasum -a 256 "$1" | awk '{print $1}'; fi
}

# The exact parser the pipeline's TestFlight step and scripts/set-workspace-version.sh use, so the
# version a release builds cannot be read differently from the version everything else reads.
cargo_version="$(awk '/^\[workspace\.package\]/{f=1; next} /^\[/{f=0} f && /^version[[:space:]]*=/{gsub(/["[:space:]]/,"",$0); sub(/^version=/,"",$0); print; exit}' "$ROOT_DIR/Cargo.toml")"
[[ "$cargo_version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] \
  || die "could not read a MAJOR.MINOR.PATCH version from [workspace.package] in Cargo.toml (got '$cargo_version')"

# The tag is the whole of the intent. There is no untagged mode: a release that goes wrong is fixed by
# deleting the tag and cutting it again, so nothing here needs to run without one. release-guard has
# already compared the tag with Cargo.toml; it is compared again because this is the step whose
# binaries carry the version, and a guard that was skipped or edited must not be the only check.
tag="${BUILDKITE_TAG:-}"
[[ -n "$tag" ]] \
  || die "BUILDKITE_TAG is empty. This builds releases from tags only. On a tag build, BUILDKITE_TAG is probably missing from the tart-ci plugin's env: allowlist; the guest only sees listed variables."
[[ "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "BUILDKITE_TAG '$tag' is not a vX.Y.Z release tag"
version="${tag#v}"
[[ "$version" == "$cargo_version" ]] \
  || die "tag $tag says $version but [workspace.package] version in Cargo.toml says $cargo_version"
say "release build for $tag (Cargo.toml agrees: $cargo_version)"

# The upload directory must start empty. Anything already in it would be uploaded next to this run's
# assets with nothing to say it is stale.
mkdir -p "$out_dir"
out_dir="$(cd "$out_dir" && pwd)"
if [[ -n "$(ls -A "$out_dir")" ]]; then
  die "$out_dir is not empty; refusing to mix this run's assets with whatever is already there: $(ls -A "$out_dir" | tr '\n' ' ')"
fi

# ---- toolchain ---------------------------------------------------------------------------------

phase "rust toolchain"
# Mirrors the pipeline's MSRV gate, plus the one thing a release adds: the x86_64 standard library.
# The guest's rustup lives in a root-owned /opt/rustup that the job user cannot write (measured on
# the push build, where an unconditional install failed with EACCES), so `rustup target add` there is
# not an option; a private toolchain under $HOME is. Stable rather than a pin, as the push build does.
rust_target_installed() { rustup target list --installed 2>/dev/null | grep -x "$1" >/dev/null; }
cargo_meets_msrv() {
  local have
  have="$(cargo -V | awk '{print $2}')"
  [[ "$(printf '%s\n%s\n' "$MSRV" "$have" | sort -V | head -1)" == "$MSRV" ]]
}
say "cargo reports $(cargo -V | awk '{print $2}'); MSRV is $MSRV"
if ! cargo_meets_msrv || ! rust_target_installed x86_64-unknown-linux-gnu; then
  if cargo_meets_msrv && [[ -w "${RUSTUP_HOME:-$HOME/.rustup}" ]]; then
    say "adding the x86_64-unknown-linux-gnu standard library to the current toolchain"
    rustup target add x86_64-unknown-linux-gnu
  else
    if [[ ! -w "${RUSTUP_HOME:-$HOME/.rustup}" ]]; then
      export RUSTUP_HOME="$HOME/.rustup-ci"
    fi
    say "installing stable with the x86_64-unknown-linux-gnu target into ${RUSTUP_HOME:-$HOME/.rustup}"
    rustup toolchain install stable --profile minimal --target x86_64-unknown-linux-gnu
    rustup default stable
  fi
fi
cargo_meets_msrv || die "cargo is still below MSRV $MSRV after the toolchain step: $(cargo -V)"
rust_target_installed x86_64-unknown-linux-gnu \
  || die "the x86_64-unknown-linux-gnu standard library is still not installed after the toolchain step"
say "$(cargo -V); targets: $(rustup target list --installed | tr '\n' ' ')"

# ---- tests -------------------------------------------------------------------------------------

phase "cargo test --workspace --locked"
(cd "$ROOT_DIR" && cargo test --workspace --locked)

# ---- pinned zig and cargo-zigbuild --------------------------------------------------------------

tools_dir="$HOME/pessimal-release-tools"
rm -rf "$tools_dir"
mkdir -p "$tools_dir"

phase "zig $ZIG_VERSION from the pinned ziglang wheel"
wheel="$tools_dir/ziglang-$ZIG_VERSION.whl"
curl --proto '=https' --tlsv1.2 -fsSL --retry 3 --retry-delay 5 -o "$wheel" "$ZIG_WHEEL_URL" \
  || die "could not download $ZIG_WHEEL_URL"
wheel_sha="$(sha256_of "$wheel")"
say "checked the wheel's SHA-256, found $wheel_sha, expected $ZIG_WHEEL_SHA256"
[[ "$wheel_sha" == "$ZIG_WHEEL_SHA256" ]] || die "the ziglang wheel does not match its pinned hash; refusing to run it"

zig_root="$tools_dir/ziglang-$ZIG_VERSION"
mkdir -p "$zig_root"
# zipfile.extract does not restore Unix modes, and `ziglang/zig` is useless without its exec bit, so
# the mode recorded in each entry is re-applied. Members that would land outside the destination are
# refused rather than trusted, hash or no hash.
python3 - "$wheel" "$zig_root" <<'PY'
import os
import sys
import zipfile

wheel, dest = sys.argv[1], os.path.realpath(sys.argv[2])
with zipfile.ZipFile(wheel) as archive:
    for info in archive.infolist():
        target = os.path.realpath(os.path.join(dest, info.filename))
        if not target.startswith(dest + os.sep):
            sys.exit("refusing a wheel member outside %s: %s" % (dest, info.filename))
        archive.extract(info, dest)
        mode = (info.external_attr >> 16) & 0o777
        if mode and not info.is_dir():
            os.chmod(target, mode)
PY
rm -f "$wheel"

# cargo-zigbuild 0.22.1 looks for `python3 -m ziglang` FIRST and a `zig` binary second (src/zig.rs,
# find_zig). Both are pointed at the pinned copy -- PYTHONPATH wins over any ziglang the image's own
# site-packages might hold, and CARGO_ZIGBUILD_ZIG_PATH covers the fallback -- and both are asserted,
# so neither discovery route can reach a zig this script did not install.
export PYTHONPATH="$zig_root${PYTHONPATH:+:$PYTHONPATH}"
export CARGO_ZIGBUILD_PYTHON_PATH
CARGO_ZIGBUILD_PYTHON_PATH="$(command -v python3)"
export CARGO_ZIGBUILD_ZIG_PATH="$zig_root/ziglang/zig"
zig_via_python="$(python3 -m ziglang version 2>&1)" || die "python3 -m ziglang version failed: $zig_via_python"
zig_via_path="$("$CARGO_ZIGBUILD_ZIG_PATH" version 2>&1)" || die "$CARGO_ZIGBUILD_ZIG_PATH version failed: $zig_via_path"
say "checked zig, found '$zig_via_python' via python3 -m ziglang and '$zig_via_path' via CARGO_ZIGBUILD_ZIG_PATH, expected $ZIG_VERSION"
[[ "$zig_via_python" == "$ZIG_VERSION" && "$zig_via_path" == "$ZIG_VERSION" ]] \
  || die "zig is not the pinned $ZIG_VERSION"

phase "cargo-zigbuild $CARGO_ZIGBUILD_VERSION"
# --root keeps it out of CARGO_HOME/bin, which may be as root-owned as the rustup directory. It is
# then invoked by absolute path, not as `cargo zigbuild`: cargo searches CARGO_HOME/bin before PATH
# for subcommands, so a copy baked into the image would otherwise win silently. Run from $tools_dir so
# no .cargo/config.toml of the checkout applies to installing a tool.
(cd "$tools_dir" && cargo install cargo-zigbuild --locked --version "$CARGO_ZIGBUILD_VERSION" --root "$tools_dir/cargo-zigbuild")
zigbuild="$tools_dir/cargo-zigbuild/bin/cargo-zigbuild"
zigbuild_version="$("$zigbuild" --version 2>&1)" || die "$zigbuild --version failed: $zigbuild_version"
say "checked cargo-zigbuild, found '$zigbuild_version', expected 'cargo-zigbuild $CARGO_ZIGBUILD_VERSION'"
[[ "$zigbuild_version" == "cargo-zigbuild $CARGO_ZIGBUILD_VERSION" ]] || die "cargo-zigbuild is not the pinned $CARGO_ZIGBUILD_VERSION"

# ---- build ---------------------------------------------------------------------------------------

# Where cargo puts binaries, from cargo itself: CARGO_TARGET_DIR or a config file can move it, and a
# guessed path that happens to hold an old binary would pass every check below.
target_dir="$(cd "$ROOT_DIR" && cargo metadata --format-version 1 --no-deps --locked \
  | python3 -c 'import json, sys; print(json.load(sys.stdin)["target_directory"])')"
[[ -n "$target_dir" ]] || die "cargo metadata reported no target_directory"

for triple in $LINUX_TRIPLES; do
  phase "cargo zigbuild --release --locked -p pessimal_agent_host --target $triple.$GLIBC_FLOOR"
  binary="$target_dir/$triple/release/pessimal-agent"
  rm -f "$binary"
  (cd "$ROOT_DIR" && "$zigbuild" zigbuild --release --locked -p pessimal_agent_host --target "$triple.$GLIBC_FLOOR")
  [[ -s "$binary" ]] || die "cargo-zigbuild reported success but wrote no binary at $binary"
done

# ---- prove the binaries ------------------------------------------------------------------------

# The ELF machine field, read rather than inferred from the path. It is the only check the x86_64
# binary gets that the aarch64 one does not need: nothing on this cluster can execute it, so this is
# what stops two copies of the same architecture shipping under two names.
elf_machine() {
  python3 - "$1" <<'PY'
import struct
import sys

with open(sys.argv[1], "rb") as fh:
    head = fh.read(20)
if head[:4] != b"\x7fELF":
    print("not-elf")
else:
    endian = "<" if head[5] == 1 else ">"
    (machine,) = struct.unpack_from(endian + "H", head, 18)
    print({62: "x86_64", 183: "aarch64"}.get(machine, "e_machine=%d" % machine))
PY
}

for triple in $LINUX_TRIPLES; do
  binary="$target_dir/$triple/release/pessimal-agent"
  phase "glibc floor and architecture: $triple"
  machine="$(elf_machine "$binary")"
  say "checked the ELF machine of $binary, found $machine, expected ${triple%%-*}"
  [[ "$machine" == "${triple%%-*}" ]] || die "$binary is $machine, not ${triple%%-*}"
  "$ROOT_DIR/scripts/check-glibc-floor.sh" "$binary" "$GLIBC_FLOOR"
done

phase "execute the aarch64 binary"
aarch64_binary="$target_dir/aarch64-unknown-linux-gnu/release/pessimal-agent"
reported="$("$aarch64_binary" --version 2>&1)" || die "$aarch64_binary --version exited non-zero: $reported"
say "checked '$aarch64_binary --version', found '$reported', expected 'pessimal-agent $version'"
[[ "$reported" == "pessimal-agent $version" ]] || die "the aarch64 binary does not report version $version"
say "NOT EXECUTED: the x86_64-unknown-linux-gnu binary. This cluster has no x86_64 Linux machine; it is"
say "verified only by its ELF machine field, its glibc floor and, after publishing, its checksum."

# ---- package -----------------------------------------------------------------------------------

for triple in $LINUX_TRIPLES; do
  phase "package $triple"
  "$ROOT_DIR/scripts/package-agent-release.sh" \
    --binary "$target_dir/$triple/release/pessimal-agent" --triple "$triple" --version "$version" --out "$out_dir" >/dev/null
done

# Exactly the manifest's Linux half and a hash beside each. Missing and extra both fail: an upload
# directory that holds one tarball too few is the green-with-no-artefact shape this project has
# already paid for, and one too many is a stale file shipping by accident.
expected="$("$ROOT_DIR/scripts/release-manifest.sh" "$version" linux | while read -r asset; do
  printf '%s\n%s.sha256\n' "$asset" "$asset"; done | LC_ALL=C sort)"
actual="$(ls -A "$out_dir" | LC_ALL=C sort)"
if [[ "$expected" != "$actual" ]]; then
  die "$out_dir does not hold exactly the Linux release set. Expected: $(printf '%s' "$expected" | tr '\n' ' ') Found: $(printf '%s' "$actual" | tr '\n' ' ')"
fi
for asset in $("$ROOT_DIR/scripts/release-manifest.sh" "$version" linux); do
  [[ -s "$out_dir/$asset" && -s "$out_dir/$asset.sha256" ]] || die "$out_dir/$asset or its .sha256 is empty"
  (cd "$out_dir" && if command -v sha256sum >/dev/null 2>&1; then sha256sum -c "$asset.sha256"; else shasum -a 256 -c "$asset.sha256"; fi) >/dev/null \
    || die "$asset does not match its own .sha256"
done

phase "done"
say "Linux release assets for $version in $out_dir:"
ls -l "$out_dir"
