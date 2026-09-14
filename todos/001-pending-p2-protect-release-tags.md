---
status: pending
priority: p2
issue_id: "001"
tags: [security, release, ci]
dependencies: []
---

# Protect release tags

## Problem Statement

A `vX.Y.Z` tag starts the agent release, and the release signs and notarizes the tagged commit with the
Lightless Labs Developer ID. Nothing controls who can push that tag. Anyone who can push a `v*` tag gets
their commit signed.

Today two things can push one: the owner, and `LL_CLI_RELEASE_GH_TOKEN`, which the release uses to
publish. So a leaked release token can push a commit to `main`, tag it, and get it signed.

## Findings

- GitHub rulesets need a paid plan for this organisation (the owner, 2026-09-14), so there is no tag
  ruleset and no ruleset on `main`.
- `scripts/release-guard.sh` checks that the tag matches the version in `Cargo.toml` and that the commit
  is on `main`. It does not check who made the tag.
- `DOPPLER_SERVICE_ACCOUNT_TOKEN` cannot be limited to tag builds with a Buildkite access policy,
  because the TestFlight step on `main` uses it too.

## Proposed Solutions

### Option A: Tag and branch rulesets

Add a ruleset that lets only the owner create `refs/tags/v*`, and one that protects `main`.

- **Pros:** GitHub enforces it before the tag exists.
- **Cons:** needs a paid GitHub plan. The token still acts as the account that made it, so make the token
  from an account that cannot bypass the rulesets.

### Option B: Require a signed tag in release-guard

Make the `release-cut` step create an annotated, signed tag, and make `release-guard` stop unless the tag
is signed by an allowed key (`git verify-tag` against a committed allowed-signers file).

- **Pros:** free. A leaked GitHub token alone cannot sign a tag.
- **Cons:** CI cuts releases, so the signing key must live in Doppler, beside `LL_CLI_RELEASE_GH_TOKEN`.
  A leaked `DOPPLER_SERVICE_ACCOUNT_TOKEN` gives both. `cog` makes lightweight tags today. Nobody has
  checked what Buildkite reports for an annotated tag.

### Option C: A separate account for the release token

Create `LL_CLI_RELEASE_GH_TOKEN` from a machine account with write access and no admin role.

- **Pros:** limits what a leaked token can do once rulesets exist.
- **Cons:** does nothing on its own while there are no rulesets.

## Recommended Action

Option B still helps against a leaked GitHub token, but not against a leaked Doppler token. Do it if
that difference is worth the work. Option A and C if the organisation moves to a paid plan.

## Technical Details

- `.buildkite/pipeline.yml`: the release steps (`if: build.tag != null && build.tag =~ /^v…$/`)
- `scripts/release-guard.sh`: the tag checks
- `scripts/release-cut.sh`: the CI step that runs `cog bump --auto` and pushes the tag
- `cog.toml`: `post_bump_hooks` push the tag
- `docs/runbooks/cutting-a-release.md`: the Security section and the re-cut commands

## Acceptance Criteria

- [ ] A `v*` tag pushed with `LL_CLI_RELEASE_GH_TOKEN` alone does not produce a signed release.
- [ ] A tag that `release-cut` makes still releases.
- [ ] The runbook's Security section says what protects release tags.

## Work Log

### 2026-09-14

- Tag ruleset dropped from the release setup: GitHub plan does not allow it.
- Gap written into the runbook's Security section and `docs/HANDOFF.md`.
- CI now cuts releases (`release-cut`), so Option B's signing key would have to live in Doppler.

## Resources

- `docs/runbooks/cutting-a-release.md`, Security
- <https://git-scm.com/docs/git-verify-tag>
