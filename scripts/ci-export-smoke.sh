#!/usr/bin/env bash
# Runs the agent against a collector for long enough to export, and fails if the
# exporter reported an error.
#
# Usage: scripts/ci-export-smoke.sh <protocol> <endpoint>
#
# Export failures are not fatal to the agent by design — it keeps beating and
# retries — so the exit code alone proves nothing. The SDK's internal logs are
# what tell us whether anything actually landed.
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

if grep -qiE "NetworkError|export failed|Failed to shutdown" "$log"; then
  echo "::error::$protocol export reported a failure"
  cat "$log"
  exit 1
fi

if ! grep -q "PeriodReaderInvokedExport" "$log"; then
  echo "::error::$protocol export never ran; the agent produced no export at all"
  cat "$log"
  exit 1
fi

echo "$protocol export succeeded"
