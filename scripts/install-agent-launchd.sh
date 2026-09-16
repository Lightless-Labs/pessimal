#!/usr/bin/env bash
# Installs the host agent as a per-user launchd service on macOS.
#
# Usage:
#   scripts/install-agent-launchd.sh --environment infrastructure \
#       --preset signoz --endpoint https://ingest.eu2.signoz.cloud:443 \
#       --key-file ~/.config/pessimal/ingestion-key
#   scripts/install-agent-launchd.sh --tarball ~/Downloads/pessimal-agent-0.2.0-aarch64-apple-darwin.tar.gz \
#       --endpoint https://ingest.eu2.signoz.cloud:443 --preset signoz \
#       --key-file ~/.config/pessimal/ingestion-key
#   scripts/install-agent-launchd.sh --uninstall
#
# Other flags: --protocol, --interval, --no-verify (skip the install-time export check), and
# --clear-quarantine (see below).
#
# Where the binary comes from, in this order:
#   --tarball PATH   a release archive, pessimal-agent-<version>-<triple>.tar.gz, for this Mac's
#                    architecture. It is checked against a SHA256SUMS lying beside it when there is
#                    one, unpacked into a temporary directory, and its binary must report the version
#                    the archive is named for.
#   --binary PATH    an agent binary you already have.
#   otherwise        ./target/release/pessimal-agent if it exists, else a `cargo build --release`.
#
# The agent is an ordinary user process: it reads /proc-equivalents through sysinfo and needs no
# elevation, so this installs a LaunchAgent under the calling user rather than a root LaunchDaemon.
#
# The API key is deliberately *not* written to the config file. It goes into the plist's
# EnvironmentVariables as PESSIMAL_API_KEY and the plist is chmod 600, so the key never appears in
# a process listing (as it would on argv) and never in a file meant to be shared or committed.
#
# A --tarball or --binary carrying com.apple.quarantine is refused. macOS sets that attribute on a
# file that arrived through a browser (or Mail, or AirDrop) and keeps it through Finder's expansion
# and through `cp`, `install` and `ditto`. A quarantined agent under launchd has no window in which
# Gatekeeper could ask about it, so it would fail to start and say nothing -- unmeasured, and not
# measurable on a SIP-disabled guest (packaging/macos/GATEKEEPER.md has the whole story). Stripping the attribute silently would make a Gatekeeper decision the user
# never saw, so the refusal names the file and prints the command that removes it; --clear-quarantine
# is for a user who has decided, and clears the attribute on the installed copy only. The archive is
# checked, not just the binary inside it: `tar` does not carry the attribute onto what it unpacks
# (measured, bsdtar 3.5.3 on macOS 26.2), so checking only the unpacked binary would let a browser
# download through unexamined. Fetching with curl and unpacking with tar never sets it at all.
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
source_tarball=""
clear_quarantine="no"
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
    --tarball) source_tarball="${2:?--tarball needs a path}"; shift 2 ;;
    --clear-quarantine) clear_quarantine="yes"; shift ;;
    --uninstall) uninstall="yes"; shift ;;
    --no-verify) verify="no"; shift ;;
    -h|--help) awk 'NR > 1 && /^#/ { sub(/^# ?/, ""); print; next } NR > 1 { exit }' "$0"; exit 0 ;;
    *) die "unknown argument $1" ;;
  esac
done
[ -z "$source_binary" ] || [ -z "$source_tarball" ] || die "--binary and --tarball are two answers to one question; pass one"

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

# A downloaded source is checked, and a tarball unpacked, before anything under $HOME is written, so a
# refusal leaves the existing installation exactly as it was.
has_quarantine() { xattr -p com.apple.quarantine "$1" >/dev/null 2>&1; }

refuse_quarantined() { # refuse_quarantined PATH archive|binary
  if [ "$clear_quarantine" = "yes" ]; then
    echo "note: $1 carries com.apple.quarantine; installing from it anyway, as --clear-quarantine asks"
    return 0
  fi
  {
    printf 'error: %s carries com.apple.quarantine, so nothing was installed.\n\n' "$1"
    if [ "$2" = "archive" ]; then
      printf 'macOS attached that attribute because this archive was downloaded through a browser, Mail or\n'
      printf 'AirDrop. Unpacking it with tar here would quietly drop the attribute from the binary inside,\n'
      printf 'and that decision is yours rather than this script'"'"'s.\n'
    else
      printf 'macOS attached that attribute because this binary arrived through a browser, Mail or AirDrop;\n'
      printf 'a browser download expanded in Finder is the usual way.\n'
    fi
    printf 'A quarantined agent under launchd has no window in which Gatekeeper could ask about it, so it\n'
    printf 'would fail to start and say nothing.\n'
    printf '\nIf you trust it -- check a release archive against SHA256SUMS from the same release first --\n'
    printf 'remove the attribute and run this again:\n\n'
    printf '  xattr -d com.apple.quarantine %q\n\n' "$1"
    printf 'or run this again with --clear-quarantine, which clears it on the installed copy only.\n'
    printf 'Downloading with curl and unpacking with tar xzf never sets the attribute at all.\n'
  } >&2
  exit 1
}

if [ -n "$source_binary$source_tarball" ]; then
  command -v xattr >/dev/null 2>&1 || die "xattr is required to check a downloaded agent for com.apple.quarantine, and it is not on PATH"
fi

if [ -n "$source_binary" ] && has_quarantine "$source_binary"; then
  refuse_quarantined "$source_binary" binary
fi

tarball_binary=""
if [ -n "$source_tarball" ]; then
  for tool in tar shasum mktemp awk grep; do
    command -v "$tool" >/dev/null 2>&1 || die "$tool is required to install from --tarball, and it is not on PATH"
  done
  { [ -f "$source_tarball" ] && [ -r "$source_tarball" ]; } || die "cannot read the archive $source_tarball"
  if has_quarantine "$source_tarball"; then
    refuse_quarantined "$source_tarball" archive
  fi

  case "$(uname -m)" in
    arm64) host_triple="aarch64-apple-darwin" ;;
    x86_64) host_triple="x86_64-apple-darwin" ;;
    *) die "no release archive is built for this Mac's architecture, $(uname -m)" ;;
  esac

  # scripts/package-agent-release.sh writes exactly one top-level directory,
  # pessimal-agent-<version>-<triple>/, so that name is where the version and triple are read from --
  # the file's own name may have gained a " (1)" in a Downloads folder.
  members="$(tar -tzf "$source_tarball" 2>/dev/null)" || die "$source_tarball is not a gzip-compressed tar archive"
  if grep -Eq '^/|(^|/)\.\.(/|$)' <<<"$members"; then
    die "$source_tarball has a member with an absolute path or a '..' component; refusing to unpack it"
  fi
  root="$(awk -F/ 'NF { print $1 }' <<<"$members" | LC_ALL=C sort -u)"
  root_re='^pessimal-agent-([0-9]+\.[0-9]+\.[0-9]+)-([a-z0-9_]+-[a-z0-9_-]+)$'
  if [ "$(grep -c . <<<"$root")" -ne 1 ] || ! [[ "$root" =~ $root_re ]]; then
    die "$source_tarball is not a pessimal-agent release archive: expected one top-level directory named pessimal-agent-<version>-<triple>, found: $(tr '\n' ' ' <<<"$root")"
  fi
  archive_version="${BASH_REMATCH[1]}"
  archive_triple="${BASH_REMATCH[2]}"
  case "$archive_triple" in
    "$host_triple") ;;
    *-linux-gnu) die "$source_tarball is the $archive_triple build. This installs a launchd service on macOS; on Linux, run the agent under systemd with the pessimal-agent.service in that archive (its README.md walks through it)" ;;
    *) die "$source_tarball is built for $archive_triple, and this Mac is $host_triple; download pessimal-agent-$archive_version-$host_triple.tar.gz from the same release" ;;
  esac

  # SHA256SUMS lists every asset of a release under its published name, which is the root directory's
  # name plus .tar.gz whatever this copy is called. Only a SHA256SUMS lying beside the archive is used:
  # one fetched from anywhere else proves nothing about these bytes.
  sums="$(dirname "$source_tarball")/SHA256SUMS"
  asset="$root.tar.gz"
  if [ -f "$sums" ]; then
    expected="$(awk -v name="$asset" '$2 == name || $2 == "*" name { print $1; exit }' "$sums")"
    [ -n "$expected" ] || die "$sums has no line for $asset, so it is not the SHA256SUMS of the release this archive comes from"
    actual="$(shasum -a 256 "$source_tarball" | awk '{ print $1 }')"
    [ "$actual" = "$expected" ] || die "$source_tarball does not match $sums: its SHA-256 is $actual, and $asset is listed as $expected. Download both again, from the same release"
    echo "checked $source_tarball against $sums: sha256 $actual"
  else
    echo "note: no SHA256SUMS beside $source_tarball, so the archive was not checksum-verified here."
    echo "  Put the release's SHA256SUMS in the same directory and run this again to have it checked."
  fi

  tarball_dir="$(mktemp -d "${TMPDIR:-/tmp}/pessimal-agent-tarball.XXXXXX")"
  trap 'rm -rf "$tarball_dir"' EXIT
  tar -xzf "$source_tarball" -C "$tarball_dir"
  tarball_binary="$tarball_dir/$root/pessimal-agent"
  { [ -f "$tarball_binary" ] && [ ! -L "$tarball_binary" ] && [ -x "$tarball_binary" ]; } \
    || die "$source_tarball holds no executable $root/pessimal-agent"
  reported="$("$tarball_binary" --version 2>&1)" || die "$root/pessimal-agent --version failed: $reported"
  [ "$reported" = "pessimal-agent $archive_version" ] \
    || die "the binary in $source_tarball reports '$reported', not 'pessimal-agent $archive_version' as its directory is named"
  echo "unpacked $root: $reported"
fi

mkdir -p "$bin_dir" "$support_dir" "$log_dir" "$HOME/Library/LaunchAgents"

if [ -n "$source_tarball" ]; then
  install -m 755 "$tarball_binary" "$binary"
elif [ -n "$source_binary" ]; then
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
# `install` carries extended attributes across (measured, as do `cp` and `ditto`), so a quarantined
# source is still quarantined here. Reaching this with the attribute and without --clear-quarantine
# would take a source that changed after it was checked, or a tar that restores an attribute recorded
# inside an archive -- the macOS one does not unless asked with --xattrs -- so it is asserted rather
# than assumed.
if [ -n "$source_binary$source_tarball" ] && has_quarantine "$binary"; then
  [ "$clear_quarantine" = "yes" ] \
    || die "the installed copy $binary carries com.apple.quarantine although its source did not when it was checked; remove it with: xattr -d com.apple.quarantine $binary"
  xattr -d com.apple.quarantine "$binary"
  echo "cleared com.apple.quarantine on the installed copy, $binary, as --clear-quarantine asked; the source keeps it"
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
