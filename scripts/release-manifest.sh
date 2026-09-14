#!/usr/bin/env bash
# The single source of the release asset names. Nothing else is allowed to spell them.
#
# Usage:
#   scripts/release-manifest.sh <version> [all|agents|app|sums|linux|macos]
#   scripts/release-manifest.sh <version> triple <rust-target-triple>
#
# One name per line, LC_ALL=C sorted, so a caller can pipe it straight into `comm -3` without
# sorting it again. `linux` and `macos` are the sets the `release-linux` and `release-macos` Buildkite
# steps each produce; together with `sums` they are exactly `all`, which is what `release-publish`
# asserts it holds before it touches GitHub.
#
# The names are load-bearing, not cosmetic. mise's `github:` backend and standalone `ubi` both
# resolve an asset by scoring its *filename* against the host triple, so a stray underscore or a
# dropped `.gz` does not produce a clear error -- it produces "no release found", or worse, a
# confident match on the wrong file. That is why every name below is a literal in a case arm
# rather than a string assembled from the triple: a typo in an assembly rule silently renames an
# asset, while a typo in a literal is visible on the line it is written.
#
# Why BOTH macOS agent tarballs must always be present: the scoring is a partial match, not an
# exact one. Drop `pessimal-agent-<v>-x86_64-apple-darwin.tar.gz` and on an Intel Mac the
# highest-scoring remaining candidate in both mise's github backend and standalone ubi becomes
# `Pessimal-<v>-macos.zip` -- so a user asking for the telemetry agent silently gets the menu bar
# app installed as one. A five-asset release is therefore not "a release missing one file", it is a
# release that mis-installs. `release-publish` rejects five as loudly as seven.
#
# There is no Windows asset, deliberately, and not by omission. A `cargo zigbuild --target
# x86_64-pc-windows-gnu` probe ran for over twenty minutes without finishing, and the Buildkite
# cluster has no Windows machine that could ever execute the result -- so it would be the one asset
# nothing verifies. scripts/release-notes.sh tells users the same thing. Adding it back means a line
# here, an archive branch in scripts/package-agent-release.sh, and a verifier that can run it.
set -euo pipefail

die() { printf 'release-manifest: %s\n' "$1" >&2; exit 1; }

version="${1:-}"
group="${2:-all}"

[ -n "$version" ] || die "usage: release-manifest.sh <version> [all|agents|app|sums|linux|macos|triple TRIPLE]"

# Deliberately strict, and deliberately the same shape as the release steps' Buildkite condition
# (`build.tag =~ /^v[0-9]+\.[0-9]+\.[0-9]+$/`) and `release-guard`'s assertion. A caller that passes
# the *tag* by mistake ("v0.1.0") is the realistic error, and it must fail here rather than mint six
# asset names with a stray `v` in them that then fail to match anything on the release.
case "$version" in
  v*) die "pass the bare version, not the tag: got '$version', want '${version#v}'" ;;
esac
printf '%s' "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$' \
  || die "not a release version: '$version' (want MAJOR.MINOR.PATCH)"

# Every triple the release ships an agent for, grouped by the Buildkite step that builds it. This
# list is the source: `release-publish` counts what it prints, and the build scripts assert they
# produced exactly their half of it.
linux_triples='aarch64-unknown-linux-gnu
x86_64-unknown-linux-gnu'
macos_triples='aarch64-apple-darwin
x86_64-apple-darwin'

# The triple -> asset name map. `.tar.gz` for every shipped triple.
asset_for_triple() {
  case "$1" in
    aarch64-apple-darwin)        printf 'pessimal-agent-%s-aarch64-apple-darwin.tar.gz\n' "$version" ;;
    x86_64-apple-darwin)         printf 'pessimal-agent-%s-x86_64-apple-darwin.tar.gz\n' "$version" ;;
    x86_64-unknown-linux-gnu)    printf 'pessimal-agent-%s-x86_64-unknown-linux-gnu.tar.gz\n' "$version" ;;
    aarch64-unknown-linux-gnu)   printf 'pessimal-agent-%s-aarch64-unknown-linux-gnu.tar.gz\n' "$version" ;;
    *-windows-*) die "no release asset is defined for '$1': Windows is deferred from the first release (see this script's header)" ;;
    *) die "no release asset is defined for triple '$1'" ;;
  esac
}

emit_triples() { printf '%s\n' "$1" | while read -r t; do asset_for_triple "$t"; done; }
emit_app()     { printf 'Pessimal-%s-macos.zip\n' "$version"; }
emit_sums()    { printf 'SHA256SUMS\n'; }

case "$group" in
  all)    { emit_triples "$linux_triples"; emit_triples "$macos_triples"; emit_app; emit_sums; } ;;
  agents) { emit_triples "$linux_triples"; emit_triples "$macos_triples"; } ;;
  app)    emit_app ;;
  sums)   emit_sums ;;
  linux)  emit_triples "$linux_triples" ;;
  macos)  { emit_triples "$macos_triples"; emit_app; } ;;
  # The packager asks for one name by triple rather than deriving it, so the archive it writes and
  # the name `release-publish` expects cannot drift apart.
  triple) asset_for_triple "${3:?usage: release-manifest.sh <version> triple <target-triple>}" ;;
  *) die "unknown group '$group' (want all, agents, app, sums, linux, macos, or 'triple TRIPLE')" ;;
esac | LC_ALL=C sort
