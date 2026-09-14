---
status: pending
priority: p2
issue_id: "007"
tags: [security, ios, ci]
dependencies: []
---

# The TestFlight build can read the Doppler token

## Problem Statement

The TestFlight step runs `scripts/release-ios-testflight-buildkite.sh` with `DOPPLER_TOKEN` in its
environment. The script reads its secrets and unsets the variable, but that does not clear the script's
starting environment. The Bazel build then runs in the same guest, as the same user, and any process in
it can read that starting environment with `ps -E`. Every dependency's build script runs in that build.

`DOPPLER_TOKEN` is `DOPPLER_SERVICE_ACCOUNT_TOKEN`, which reads every config, including the Developer ID
certificate, the notary key and `LL_CLI_RELEASE_GH_TOKEN`. This step runs on every push to `main`.

## Findings

- `export -n` and `unset` do not clear a process's starting environment. A review found this for the
  macOS release on 2026-09-14 and showed it with `ps -E -ww`.
- The macOS release was fixed by splitting it: `release-macos-build` builds with no token, and
  `release-macos` signs and runs nothing that was built.
- iOS is harder to split: `rules_apple` signs during `bazelisk build`, so the signing identity must be in
  the keychain while the build runs.

## Proposed Solutions

### Option A: Fetch the secrets in a separate process

In the pipeline command, run a small fetch script with the token, write the secrets the build needs to
files that only the build user can read, then start the build script with `env -u DOPPLER_TOKEN`, so no
ancestor of the build holds the token.

- **Pros:** small change. The Doppler token never reaches the build.
- **Cons:** the build can still read the distribution certificate and the App Store Connect key, which
  it needs. Those are narrower than the service account token.

### Option B: Build unsigned, then sign in a separate step

Build the ipa without signing in one step, with no credential. Re-sign it with `codesign` in a second
step that holds the token and runs nothing that was built, then upload.

- **Pros:** no credential is present while any build script runs, as in the macOS release.
- **Cons:** re-signing an ipa by hand must repeat what `rules_apple` does (entitlements, embedded
  profile, nested frameworks). More work, and a new way for signing to go wrong.

## Recommended Action

Option A first. Consider Option B if the build must not see the distribution certificate either.

## Technical Details

- `.buildkite/pipeline.yml`: the `:rocket: iOS TestFlight` step
- `scripts/release-ios-testflight-buildkite.sh`: reads the secrets, signs, builds, uploads
- `scripts/release-build-macos.sh`, `scripts/release-macos-artifacts.sh`: the macOS split, for reference

## Acceptance Criteria

- [ ] No process that runs during the Bazel build has `DOPPLER_TOKEN` in its own or an ancestor's
  starting environment.
- [ ] TestFlight still uploads on a push to `main`.

## Work Log

### 2026-09-14

- Found while fixing the same problem in the macOS release. The owner agreed to track it here.

## Resources

- `docs/runbooks/cutting-a-release.md`, Security
