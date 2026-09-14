# Cutting a release

A release is a `vX.Y.Z` tag on `main`. `cog bump` makes the tag and pushes it. The tag starts a Buildkite
build that builds, signs, publishes and checks the release. No release has run yet, so the first tag is
also the first test of this pipeline.

- [One-time setup](#one-time-setup)
- [Cut a release](#cut-a-release)
- [Watch the build](#watch-the-build)
- [Check the release](#check-the-release)
- [Fix a failed release](#fix-a-failed-release)
- [Security](#security)

## The release steps

```
release-guard ─┬─► release-linux ─────────────────────────┬─► release-publish ─┬─► release-verify-linux ─┬─► release-promote
               └─► release-macos-build ─► release-macos ─┘                    └─► release-verify-macos ─┘
```

| Step | Runs on | Credential | Does |
|---|---|---|---|
| `release-guard` | Linux | none | Stops unless the tag matches `[workspace.package] version`, the commit is on `main`, and no published release has this tag. |
| `release-linux` | Linux | none | Runs `cargo test`, builds both Linux agents for glibc 2.28, runs the arm64 one, and packages both. |
| `release-macos-build` | macOS | none | Builds both macOS agents and `Pessimal.app`, runs them, and uploads them unsigned. |
| `release-macos` | macOS | Doppler | Signs and notarizes the agents, staples the app, and packages them. It builds and runs nothing. |
| `release-publish` | Linux | Doppler | Uploads everything to a draft release, downloads it back, checks the hashes, writes `SHA256SUMS`, and publishes a prerelease. |
| `release-verify-linux` | Linux | none | Downloads the release without a token, checks it, and runs the arm64 Linux agent. |
| `release-verify-macos` | macOS | none | The same, plus signatures, notarization, the app's stapled ticket, and the macOS agents. |
| `release-promote` | Linux | Doppler | Marks the release as latest and updates the Homebrew formula. |

A release starts as a draft, becomes a prerelease, and becomes the latest release only after both verify
steps pass. Nothing in the pipeline moves a release back.

`scripts/release-manifest.sh <version> all` lists the files in a release. There is no Windows build.

## One-time setup

Do these steps before the first tag. Every step that reads Doppler uses the Buildkite secret
`DOPPLER_SERVICE_ACCOUNT_TOKEN`.

### 1. The GitHub token

The release reads `LL_CLI_RELEASE_GH_TOKEN` from `prd_macos_notarisation`, which inherits it. It needs
Contents read and write on `pessimal` and `homebrew-tap`.

### 2. Check the Apple secrets

The release reads these from `prd_macos_notarisation`. Nothing has read that config yet:

```sh
doppler secrets --only-names --project lightless-labs-pessimal --config prd_macos_notarisation
# expect: MACOS_DEVELOPER_ID_CERT_P12_BASE64  MACOS_DEVELOPER_ID_CERT_PASSWORD
#         APPLE_NOTARY_KEY_ID  APPLE_NOTARY_ISSUER_ID  APPLE_NOTARY_KEY_P8_BASE64  LL_CLI_RELEASE_GH_TOKEN
```

## Cut a release

1. Start from a clean, current `main`:

   ```sh
   export SSH_AUTH_SOCK=~/.ssh/agent.sock && ssh-add --apple-load-keychain
   git switch main && git pull --ff-only && git status --short
   ```

2. Make sure the Buildkite build for this commit passed. The tag build does not run fmt, clippy, the
   OTLP smoke test, the bindings check or the iOS build.

   ```sh
   gh api "repos/Lightless-Labs/pessimal/commits/$(git rev-parse HEAD)/status" \
     --jq '.statuses[] | select(.context == "buildkite/pessimal") | "\(.state) \(.target_url)"'
   ```

3. See which version `cog` will make:

   ```sh
   cog bump --dry-run --auto
   ```

4. Cut it:

   ```sh
   cog bump --auto
   ```

`cog bump` sets the version in `Cargo.toml`, updates `Cargo.lock` and `CHANGELOG.md`, commits, tags,
and pushes `main` and then the tag. If a push fails, push both yourself, `main` first:

```sh
git push origin main && git push origin vX.Y.Z
```

## Watch the build

The two pushes start two builds:

| Build | Runs |
|---|---|
| `main` | The CI steps, then TestFlight with the new version number. |
| `vX.Y.Z` | The eight release steps. |

The cluster runs one macOS VM at a time, and two Linux VMs. The release steps wait for the `main` build's
VMs. A step that waits for a VM is in a queue, not stuck.

Notarization in `release-macos` prints nothing while Apple works. Building the x86_64 Linux agent takes
about 20 minutes.

## Check the release

The release is done when `release-promote` passes. Then check it:

```sh
gh release view vX.Y.Z --repo Lightless-Labs/pessimal --json isDraft,isPrerelease,assets \
  --jq '{isDraft, isPrerelease, assets: [.assets[].name]}'
scripts/verify-release.sh vX.Y.Z --channels      # needs mise and ubi
```

Then, on a real Mac, download `Pessimal-X.Y.Z-macos.zip` in a browser, open it in Finder, and start the
app. No pipeline step can test this. See [`packaging/macos/GATEKEEPER.md`](../../packaging/macos/GATEKEEPER.md).

## Fix a failed release

First, find how far the release got.

- **It failed before `release-publish` published it.** Nothing is public. If the cause was outside the
  code (a VM, the network, a secret), fix the cause and **Retry** the failed step. `release-publish`
  reuses an existing draft. If the code must change, wipe the release and cut it again.
- **It is a prerelease, and a verify step failed.** If the verify VM was the problem, **Retry** that
  step. If the release is bad, wipe it and cut it again.
- **It is the latest release.** Do not wipe it. Fix the code and cut the next version.

Retry the failed step. Do not start a new build for the tag: `release-guard` stops it when a release
already exists.

### Common errors

| Error | Cause | Do |
|---|---|---|
| `REFUSED: tag vX.Y.Z says X.Y.Z, but [workspace.package] version …` | `cog bump` did not make the tag | Wipe the tag. Use `cog bump --auto`. |
| `REFUSED: vX.Y.Z points at …, which is not an ancestor of main` | `main` was not pushed, or the tag is on another branch | `git push origin main`, then Retry. Otherwise wipe the tag. |
| `REFUSED: a published release already exists for vX.Y.Z` | The release already exists | See above. |
| `GitHub refused the anonymous GET with HTTP 403` | GitHub rate limit | Retry later. |
| `missing from PATH in the … guest:` | The VM image does not have a tool | Fix the image or the script. |
| `the ziglang wheel does not match its pinned hash` | A bad download | Retry once. If it happens again, find out why. |
| `check-glibc-floor: … needs glibc above the 2.28 floor:` | A dependency needs a newer glibc | Fix the dependency. Wipe and cut again. |
| `configured doppler_token_secret is missing from host env and Buildkite secrets: …` | Buildkite did not give the step `DOPPLER_SERVICE_ACCOUNT_TOKEN`. | Check the secret and its access policy, then Retry. |
| `Doppler read failed for … HTTP 401` | The Doppler token is revoked or wrong | Replace the token in `DOPPLER_SERVICE_ACCOUNT_TOKEN`, Retry. |
| `no Developer ID Application identity in the imported .p12` | Wrong certificate, or no private key | Fix `prd_macos_notarisation`, Retry. |
| `notarisation of … is Invalid, not Accepted` | Apple rejected it. The log that follows says why. | Fix the signing. Wipe and cut again. |
| `answered HTTP 403` or `HTTP 404` on a GitHub call | The GitHub token cannot write, or has expired | Fix the token, update it in Doppler, Retry. |
| `the artifacts under .build/release/dist are not exactly the release` | A build step uploaded too few or too many files | Retry the build step, then this step. |
| `FAIL  could not download …` | GitHub is slow to serve the new files | Retry the verify step. |
| `warning: Homebrew formula bump FAILED` | The release is out. Only the formula is old. | Retry `release-promote`. |

### Wipe a release

Do this only for a release that is not the latest.

```sh
tag=vX.Y.Z
export SSH_AUTH_SOCK=~/.ssh/agent.sock && ssh-add --apple-load-keychain

# 1. List every release with this tag, drafts too.
gh api --paginate repos/Lightless-Labs/pessimal/releases \
  --jq ".[] | select(.tag_name == \"$tag\") | \"\(.id) draft=\(.draft) prerelease=\(.prerelease)\""
```

Stop if a line says `draft=false prerelease=false`. That release is the latest. Cut the next version.

```sh
# 2. Delete each release by id.
gh api -X DELETE repos/Lightless-Labs/pessimal/releases/<id>

# 3. Delete the tag on GitHub, then locally.
git push origin ":refs/tags/$tag"
git tag -d "$tag"
```

The version commit stays on `main`. Tag again by hand, with a lightweight tag:

```sh
# Nothing in the repo changed: tag the same version commit.
sha="$(git log -1 --format=%H --grep="^chore(version): $tag\$")"
git tag "$tag" "$sha" && git push origin "$tag"

# You pushed a fix: wait for its build to pass, then tag main.
git tag "$tag" HEAD && git push origin "$tag"
```

Do not run `cog bump` again for the same version. It adds a second version commit and a second
changelog entry.

## Security

**Fork pull requests get no CI.** The cluster holds signing credentials, so code from forks must never
run on it. Keep Buildkite's "build pull requests from forks" setting off.

**One Doppler token.** `DOPPLER_SERVICE_ACCOUNT_TOKEN` is the Doppler service account. It reads the Apple
signing secrets and `LL_CLI_RELEASE_GH_TOKEN`. TestFlight, `release-macos`, `release-publish` and `release-promote`
get it. The guard, both builds and both verify steps do not.

**The signing step runs no code that was built.** A build runs code from every dependency's `build.rs`,
and any process of the same user can read its parent's starting environment with `ps -E`. So
`release-macos-build` builds with no token, and `release-macos` signs with the token and runs only
Apple's tools.

**That token can sign any code as Lightless Labs**, and Apple will notarize it. If it leaks, you must
revoke the certificate. That can also stop copies of `Pessimal.app` that people already have from
opening.

**TestFlight exposes it too.** The TestFlight step runs a Bazel build while its script holds the token
in its starting environment, so a `build.rs` in that build could read it with `ps -E`. That step runs on
every push to `main`.

**Anyone who can push a `v*` tag can get code signed.** There is no tag protection, because GitHub
rulesets need a paid plan here. Today only the owner can push. The GitHub release token can push too,
so a leaked `LL_CLI_RELEASE_GH_TOKEN` can push a commit and a tag, and the pipeline signs that commit.

**tart-ci writes the Doppler token to a file on the host** while a step runs, and deletes it when the
step ends. If a Buildkite agent crashes during a credentialed step, check the host for leftover
`tart-ci/*/command.sh` files and `buildkite-tart-ci-*` VMs, and delete them.

**Rotate** the Doppler token and the GitHub token every three months, and at once if one may have leaked.
Put the new value in place before you revoke the old one.

Delete old GitHub tokens at <https://github.com/settings/personal-access-tokens>.

Work that is not in this release (Windows, musl, `.deb`, crates.io, a cask for the app) is listed in
[`docs/plans/2026-09-12-distribution.md`](../plans/2026-09-12-distribution.md).
