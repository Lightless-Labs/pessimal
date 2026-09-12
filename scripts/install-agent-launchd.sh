#!/usr/bin/env bash
# Installs the host agent as a per-user launchd service on macOS.
#
# Usage:
#   scripts/install-agent-launchd.sh --environment infrastructure \
#       --preset signoz --endpoint https://ingest.eu2.signoz.cloud:443 \
#       --key-file ~/.config/pessimal/ingestion-key
#   scripts/install-agent-launchd.sh --uninstall
#
# Other flags: --protocol, --interval, --binary, --no-verify (skip the install-time export check).
#
# The agent is an ordinary user process: it reads /proc-equivalents through sysinfo and needs no
# elevation, so this installs a LaunchAgent under the calling user rather than a root LaunchDaemon.
#
# The API key is deliberately *not* written to the config file. It goes into the plist's
# EnvironmentVariables as PESSIMAL_API_KEY and the plist is chmod 600, so the key never appears in
# a process listing (as it would on argv) and never in a file meant to be shared or committed.
set -euo pipefail

label="com.lightless-labs.pessimal.agent"
bin_dir="$HOME/.local/bin"
binary="$bin_dir/pessimal-agent"
support_dir="$HOME/Library/Application Support/pessimal"
config="$support_dir/pessimal.toml"
plist="$HOME/Library/LaunchAgents/$label.plist"
log_dir="$HOME/Library/Logs/pessimal"

preset="otlp"
endpoint=""
protocol="grpc"
environment="default"
interval="30"
key_file=""
source_binary=""
uninstall="no"
verify="yes"

die() { printf 'error: %s\n' "$1" >&2; exit 1; }

while [ $# -gt 0 ]; do
  case "$1" in
    --preset) preset="${2:?--preset needs a value}"; shift 2 ;;
    --endpoint) endpoint="${2:?--endpoint needs a value}"; shift 2 ;;
    --protocol) protocol="${2:?--protocol needs a value}"; shift 2 ;;
    --environment) environment="${2:?--environment needs a value}"; shift 2 ;;
    --interval) interval="${2:?--interval needs a value}"; shift 2 ;;
    --key-file) key_file="${2:?--key-file needs a path}"; shift 2 ;;
    --binary) source_binary="${2:?--binary needs a path}"; shift 2 ;;
    --uninstall) uninstall="yes"; shift ;;
    --no-verify) verify="no"; shift ;;
    -h|--help) sed -n '2,17p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) die "unknown argument $1" ;;
  esac
done

[ "$(uname -s)" = "Darwin" ] || die "this installs a launchd service; on Linux run the agent under systemd instead"

if [ "$uninstall" = "yes" ]; then
  launchctl bootout "gui/$(id -u)/$label" 2>/dev/null || true
  rm -f "$plist"
  echo "removed $label"
  echo "the binary, config, and logs were left in place:"
  echo "  $binary"
  echo "  $config"
  echo "  $log_dir"
  exit 0
fi

# launchd reads the plist literally, and an unescaped & or < would make it unparseable. Rejecting
# those is better than escaping them: none of these fields has a legitimate use for one.
for value in "$preset" "$endpoint" "$protocol" "$environment" "$interval"; do
  case "$value" in
    *'<'*|*'>'*|*'&'*) die "XML metacharacters are not allowed in a config value: $value" ;;
  esac
done
case "$environment" in *'::'*) die "environment must not contain '::', the URN separator" ;; esac

api_key=""
if [ -n "$key_file" ]; then
  [ -r "$key_file" ] || die "cannot read key file $key_file"
  api_key="$(tr -d ' \t\n\r' < "$key_file")"
  [ -n "$api_key" ] || die "key file $key_file is empty"
elif [ -n "${PESSIMAL_API_KEY:-}" ]; then
  api_key="$PESSIMAL_API_KEY"
fi
case "$api_key" in
  *'<'*|*'>'*|*'&'*) die "the API key contains an XML metacharacter; launchd cannot carry it" ;;
esac

mkdir -p "$bin_dir" "$support_dir" "$log_dir" "$HOME/Library/LaunchAgents"

if [ -n "$source_binary" ]; then
  [ -x "$source_binary" ] || die "$source_binary is not an executable"
  install -m 755 "$source_binary" "$binary"
elif [ -x "./target/release/pessimal-agent" ]; then
  install -m 755 ./target/release/pessimal-agent "$binary"
else
  command -v cargo >/dev/null || die "no prebuilt binary and no cargo; pass --binary PATH"
  echo "building the agent (release)..."
  cargo build --release -p pessimal_agent_host
  install -m 755 ./target/release/pessimal-agent "$binary"
fi
echo "installed $("$binary" --version) at $binary"

if [ -e "$config" ]; then
  echo "keeping the existing config at $config"
  echo "  (delete it and re-run to regenerate, or edit it and restart the service)"
else
  [ -n "$endpoint" ] || die "--endpoint is required when there is no config at $config"
  cat > "$config" <<TOML
# Written by scripts/install-agent-launchd.sh. Safe to edit; restart the service afterwards:
#   launchctl kickstart -k gui/\$(id -u)/$label
#
# The API key is *not* here. It is carried by the launchd plist's environment so that it stays out
# of any file you might copy or commit. Every value below can also be overridden by a PESSIMAL_*
# variable in that same plist.

[export]
preset = "$preset"
endpoint = "$endpoint"
protocol = "$protocol"
interval_seconds = $interval
timeout_seconds = 10

[resource]
service_name = "pessimal-agent"
environment = "$environment"

[collection]
filesystems = ["/"]
per_interface_network = false
TOML
  echo "wrote $config"
fi

# Prove an export actually succeeds *before* installing a service that would otherwise fail
# silently. The service itself runs at info, where the SDK's per-export line is not emitted -- a
# daemon logging four lines every cycle forever is its own problem -- so this is where that line
# gets read. `timeout(1)` is not on a stock macOS, hence the poll-and-kill.
if [ "$verify" = "yes" ]; then
  echo "checking that an export succeeds..."
  verify_log="$(mktemp -t pessimal-verify)"
  # Not `PESSIMAL_API_KEY="$api_key"` unconditionally: an empty value is still a *set* variable,
  # and older agents read one as a real (empty) key.
  if [ -n "$api_key" ]; then
    PESSIMAL_API_KEY="$api_key" PESSIMAL_INTERVAL_SECONDS=15 \
      PESSIMAL_LOG=info,opentelemetry_sdk=debug \
      "$binary" --config "$config" > "$verify_log" 2>&1 &
  else
    PESSIMAL_INTERVAL_SECONDS=15 PESSIMAL_LOG=info,opentelemetry_sdk=debug \
      "$binary" --config "$config" > "$verify_log" 2>&1 &
  fi
  verify_pid=$!
  for _ in $(seq 1 40); do
    grep -q 'export_result' "$verify_log" && break
    kill -0 "$verify_pid" 2>/dev/null || break
    sleep 1
  done
  kill "$verify_pid" 2>/dev/null || true
  wait "$verify_pid" 2>/dev/null || true

  if grep -q 'export_result="Ok' "$verify_log"; then
    echo "  an export succeeded; the backend is accepting this agent"
    rm -f "$verify_log"
  elif grep -q 'export_result="Err' "$verify_log"; then
    echo "  WARNING: the backend rejected the export --"
    grep -o 'export_result="Err([^)]*' "$verify_log" | head -1 | sed 's/^/    /'
    echo "  Installing anyway: the agent treats this as transient and retries every cycle."
    echo "  Full log: $verify_log"
  else
    echo "  WARNING: no export completed in 40s. Full log: $verify_log"
  fi
fi

# A brand-new plist must not inherit a stale loaded copy.
launchctl bootout "gui/$(id -u)/$label" 2>/dev/null || true

umask 077
cat > "$plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>$label</string>
	<key>ProgramArguments</key>
	<array>
		<string>$binary</string>
		<string>--config</string>
		<string>$config</string>
	</array>
	<key>EnvironmentVariables</key>
	<dict>
		<key>PESSIMAL_LOG</key>
		<string>info</string>
$([ -n "$api_key" ] && printf '\t\t<key>PESSIMAL_API_KEY</key>\n\t\t<string>%s</string>\n' "$api_key")
	</dict>
	<key>RunAtLoad</key>
	<true/>
	<key>KeepAlive</key>
	<true/>
	<key>ProcessType</key>
	<string>Background</string>
	<key>StandardOutPath</key>
	<string>$log_dir/agent.log</string>
	<key>StandardErrorPath</key>
	<string>$log_dir/agent.log</string>
</dict>
</plist>
PLIST
chmod 600 "$plist"
plutil -lint "$plist" >/dev/null || die "generated an unparseable plist at $plist"
if [ -n "$api_key" ]; then
  echo "wrote $plist (mode 600; carries PESSIMAL_API_KEY)"
else
  echo "wrote $plist (mode 600; no API key — fine for an unauthenticated collector)"
fi

launchctl bootstrap "gui/$(id -u)" "$plist"
echo
echo "$label is loaded. It starts at login and launchd restarts it if it dies."
echo
echo "  follow the log     tail -f '$log_dir/agent.log'"
echo "  re-check exports   re-run this script; it verifies one export before reinstalling"
echo "  restart            launchctl kickstart -k gui/\$(id -u)/$label"
echo "  stop and remove    scripts/install-agent-launchd.sh --uninstall"
