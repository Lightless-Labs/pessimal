#!/usr/bin/env bash
# Runs the agent against a collector for long enough to export, and fails if any export failed.
#
# Usage: scripts/ci-export-smoke.sh <protocol> <endpoint>
#
# Export failures are not fatal to the agent by design -- it keeps beating and retries -- so the
# exit code alone proves nothing. The assertion is the SDK's own per-export result line, which is
# protocol-agnostic: grepping for particular error strings would only catch the failure modes we
# happen to have seen, and gRPC's are not among them.
set -euo pipefail

protocol="${1:?protocol required, e.g. grpc or http/protobuf}"
endpoint="${2:?endpoint required, e.g. http://localhost:4317}"
log="$(mktemp)"

cat > "$log.toml" <<TOML
[export]
preset = "otlp"
endpoint = "$endpoint"
protocol = "$protocol"
interval_seconds = 2
timeout_seconds = 1
TOML

PESSIMAL_LOG=debug timeout 12 ./target/debug/pessimal-agent --config "$log.toml" > "$log" 2>&1 || true

ok=$(grep -c 'export_result="Ok' "$log" || true)
err=$(grep -c 'export_result="Err' "$log" || true)

if [ "$err" -ne 0 ]; then
  echo "::error::$protocol: $err export(s) failed"
  cat "$log"
  exit 1
fi

if [ "$ok" -eq 0 ]; then
  echo "::error::$protocol: no export ran at all in 12s"
  cat "$log"
  exit 1
fi

echo "$protocol: $ok export(s) succeeded, 0 failed"
