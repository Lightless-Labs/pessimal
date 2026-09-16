#!/usr/bin/env bash
# Renders clients/apple/macos/Resources/AppIcon.icns from tools/appicon/render-app-icon.swift.
#
# Usage:
#   scripts/render-macos-app-icon.sh [--check]
#
# The committed .icns is what scripts/build-macos-app.sh copies into the bundle, the same way the iOS
# app's committed PNGs are what Bazel globs. Run this after changing the generator, and commit the
# result. `--check` renders to a temporary file and compares instead of writing, which is what a CI
# step would run to catch a generator change that was never re-rendered.
#
# macOS only: `swift` interprets the generator, and `iconutil` builds the .icns. Both ship with the
# Xcode command line tools.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
GENERATOR="$ROOT_DIR/tools/appicon/render-app-icon.swift"
ICON_PATH="$ROOT_DIR/clients/apple/macos/Resources/AppIcon.icns"

check_only="no"
case "${1:-}" in
  --check) check_only="yes" ;;
  "") ;;
  *) echo "usage: $(basename "$0") [--check]" >&2; exit 2 ;;
esac

for tool in swift iconutil; do
  command -v "$tool" >/dev/null || { echo "error: $tool is required and is not on PATH" >&2; exit 1; }
done
[[ -f "$GENERATOR" ]] || { echo "error: missing $GENERATOR" >&2; exit 1; }

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# iconutil takes the directory's name as the iconset's, and refuses any other suffix.
swift "$GENERATOR" --macos "$work/AppIcon.iconset" >/dev/null
iconutil --convert icns --output "$work/AppIcon.icns" "$work/AppIcon.iconset"

if [[ "$check_only" == "yes" ]]; then
  if [[ ! -f "$ICON_PATH" ]]; then
    echo "error: $ICON_PATH does not exist; run $(basename "$0")" >&2
    exit 1
  fi
  if ! cmp -s "$work/AppIcon.icns" "$ICON_PATH"; then
    echo "error: $ICON_PATH is not what the generator produces; run $(basename "$0") and commit it" >&2
    exit 1
  fi
  echo "ok    AppIcon.icns matches the generator"
  exit 0
fi

mkdir -p "$(dirname "$ICON_PATH")"
cp "$work/AppIcon.icns" "$ICON_PATH"
echo "wrote ${ICON_PATH#"$ROOT_DIR"/} ($(wc -c < "$ICON_PATH" | tr -d ' ') bytes)"
