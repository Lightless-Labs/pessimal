#!/usr/bin/env bash
# Proves the agent's OTLP export reaches a real collector, over gRPC and over HTTP/protobuf.
#
# Usage: scripts/ci-otlp-export-smoke.sh
#
# Needs ./target/debug/pessimal-agent, so run `cargo build -p pessimal_agent_host` first: the script
# does not build it, and refuses to start without it. Runs from any directory, in the
# `:satellite: OTLP export smoke` Buildkite step on the ci-linux-arm64 queue, inside the guest's
# rsync'd checkout; it needs no .git.
#
# Both protocols, because they fail in different ways: a 404 on the HTTP path and a missing gRPC
# runtime are different bugs, and neither shows up in the other's run. The assertion itself is
# scripts/ci-export-smoke.sh's -- the SDK's own per-export result line -- run once per protocol. Both
# runs happen even when the first fails, so one red build names both problems.
#
# The collector is a pinned otelcol-contrib release binary rather than a container, because the Tart
# guests cannot run Docker (no nested virtualisation). 0.160.0 is the version
# scripts/otlp-trace-probe.py was measured against. Each digest is the release's own published
# `<asset>.sha256`, read with `gh api` on 2026-09-14 and confirmed by hashing the downloaded bytes; a
# replaced or truncated asset fails the comparison before anything is extracted or run. The
# darwin_arm64 entry exists so the whole script can be run on a Mac, not for CI.
#
# It starts the collector with dev/otelcol/config.yaml, the config developers use, so CI and a laptop
# agree on what "a collector" is: OTLP on 4317 and 4318, a debug exporter, nothing stored.
#
# It refuses to start if 4317 or 4318 already accepts connections: the port wait would otherwise pass
# against somebody else's listener while this collector died on "address already in use". And the
# collector is killed on every exit path. A background process in a non-interactive shell ignores the
# terminal's SIGINT, and the SSH session a guest step runs in delivers SIGHUP when it is cancelled, so
# nothing but the trap below would stop it.
set -euo pipefail

guest_image="ci-linux-arm64-rust-bazel"
collector_version="0.160.0"
ports="4317 4318"

die() {
  printf 'error: %s\n' "$1" >&2
  exit "${2:-1}"
}

usage() {
  awk 'NR > 1 && /^#/ { sub(/^# ?/, ""); print; next } NR > 1 { exit }' "$0"
}

while [ $# -gt 0 ]; do
  case "$1" in
    -h|--help) usage; exit 0 ;;
    *) die "unexpected argument $1 (see --help)" 2 ;;
  esac
done

platform="$(uname -s)-$(uname -m)"
case "$platform" in
  Linux-aarch64|Linux-arm64)
    asset="otelcol-contrib_${collector_version}_linux_arm64.tar.gz"
    asset_sha256="bff414e6a287309dfd0c51350c1e13c96c4ab807127b4320bd684f605e781326"
    # How long the collector takes to listen in the guest was never measured. The wait ends the moment
    # the port accepts, so a generous deadline costs nothing when the collector is healthy.
    listen_deadline=300
    ;;
  Darwin-arm64)
    asset="otelcol-contrib_${collector_version}_darwin_arm64.tar.gz"
    asset_sha256="ceb5309ba16f2587dbef765d54e15c803354d038b0495b0b691e1eb9876d17c9"
    # Measured 2026-09-14 on the Apple silicon Mac this was written on: 266s and 391s from exec until
    # the collector listened, in two timed launches, and one of those spent 373s before its first log
    # line with about one second of CPU used -- the process is held before it runs rather than slow
    # once running. The cause was not determined.
    listen_deadline=600
    ;;
  *)
    die "no pinned otelcol-contrib for $platform; this runs on the ci-linux-arm64 queue ($guest_image)" 2
    ;;
esac

# timeout(1) is what scripts/ci-export-smoke.sh bounds each agent run with.
for tool in curl tar awk grep tail mktemp timeout; do
  command -v "$tool" >/dev/null 2>&1 \
    || die "$tool is not on PATH; the $guest_image guest image is expected to provide it" 2
done
if command -v sha256sum >/dev/null 2>&1; then
  sha256_of() { sha256sum "$1" | awk '{ print $1 }'; }
elif command -v shasum >/dev/null 2>&1; then
  sha256_of() { shasum -a 256 "$1" | awk '{ print $1 }'; }
else
  die "neither sha256sum nor shasum is on PATH; the $guest_image guest image is expected to provide sha256sum" 2
fi

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"
config="dev/otelcol/config.yaml"
smoke="scripts/ci-export-smoke.sh"
agent="target/debug/pessimal-agent"

[ -f "$config" ] || die "no collector config at $root/$config"
[ -x "$smoke" ] || die "$root/$smoke is missing or not executable"
[ -x "$agent" ] || die "no agent at $root/$agent; run \`cargo build -p pessimal_agent_host\` before this script"

port_open() {
  # bash's /dev/tcp, so the guest needs neither nc nor ss. A refused connect returns at once.
  (exec 3<>"/dev/tcp/127.0.0.1/$1") 2>/dev/null
}

for port in $ports; do
  if port_open "$port"; then
    die "something already accepts connections on 127.0.0.1:$port; stop it first, or this would test that instead of the collector it starts"
  fi
done

work="$(mktemp -d "${TMPDIR:-/tmp}/ci-otlp-export-smoke.XXXXXX")"
collector_pid=""

cleanup() {
  local status=$?
  if [ -n "$collector_pid" ] && kill -0 "$collector_pid" 2>/dev/null; then
    kill "$collector_pid" 2>/dev/null || true
    local waited=0
    while kill -0 "$collector_pid" 2>/dev/null && [ "$waited" -lt 20 ]; do
      sleep 0.5
      waited=$((waited + 1))
    done
    if kill -0 "$collector_pid" 2>/dev/null; then
      kill -9 "$collector_pid" 2>/dev/null || true
    fi
    wait "$collector_pid" 2>/dev/null || true
    echo "stopped the collector (pid $collector_pid)"
  fi
  if [ "$status" -ne 0 ] && [ -s "$work/otelcol.log" ]; then
    echo "last 40 lines of the collector's log:" >&2
    tail -n 40 "$work/otelcol.log" >&2 || true
  fi
  rm -rf "$work"
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

url="https://github.com/open-telemetry/opentelemetry-collector-releases/releases/download/v${collector_version}/${asset}"
echo "downloading $asset"
curl --proto '=https' --tlsv1.2 -fsSL --retry 3 -o "$work/$asset" "$url"
actual_sha256="$(sha256_of "$work/$asset")"
if [ "$actual_sha256" != "$asset_sha256" ]; then
  die "sha256 mismatch for $asset: expected $asset_sha256, got $actual_sha256. Not extracting it."
fi
echo "ok: $asset sha256 $actual_sha256"
tar -xzf "$work/$asset" -C "$work" otelcol-contrib
rm -f "$work/$asset"
[ -x "$work/otelcol-contrib" ] || die "the $asset tarball held no executable otelcol-contrib"

"$work/otelcol-contrib" --config "$root/$config" > "$work/otelcol.log" 2>&1 &
collector_pid=$!
echo "started otelcol-contrib $collector_version (pid $collector_pid) with $config"

started=$SECONDS
next_note=$((started + 30))
for port in $ports; do
  until port_open "$port"; do
    kill -0 "$collector_pid" 2>/dev/null \
      || die "the collector exited before it listened on $port"
    [ "$((SECONDS - started))" -lt "$listen_deadline" ] \
      || die "the collector did not listen on 127.0.0.1:$port within ${listen_deadline}s"
    if [ "$SECONDS" -ge "$next_note" ]; then
      echo "still waiting for the collector to listen on 127.0.0.1:$port ($((SECONDS - started))s)"
      next_note=$((SECONDS + 30))
    fi
    sleep 0.5
  done
  echo "ok: the collector listens on 127.0.0.1:$port ($((SECONDS - started))s after starting it)"
done

failed=""
"$smoke" grpc http://localhost:4317 || failed="$failed grpc"
"$smoke" http/protobuf http://localhost:4318 || failed="$failed http/protobuf"

kill -0 "$collector_pid" 2>/dev/null || die "the collector died during the smoke test"

if [ -n "$failed" ]; then
  die "OTLP export failed over:$failed"
fi
echo "ok: the agent exported to otelcol-contrib $collector_version over grpc and http/protobuf"
