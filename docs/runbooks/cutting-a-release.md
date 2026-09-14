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
| `release-macos` | macOS | `DOPPLER_PESSIMAL_PRD_MACOS_NOTARISATION` | Signs and notarizes the agents, staples the app, and packages them. It builds and runs nothing. |
| `release-publish` | Linux | `DOPPLER_PESSIMAL_PRD_GITHUB_RELEASE` | Uploads everything to a draft release, downloads it back, checks the hashes, writes `SHA256SUMS`, and publishes a prerelease. |
| `release-verify-linux` | Linux | none | Downloads the release without a token, checks it, and runs the arm64 Linux agent. |
| `release-verify-macos` | macOS | none | The same, plus signatures, notarization, the app's stapled ticket, and the macOS agents. |
| `release-promote` | Linux | `DOPPLER_PESSIMAL_PRD_GITHUB_RELEASE` | Marks the release as latest and updates the Homebrew formula. |

A release starts as a draft, becomes a prerelease, and becomes the latest release only after both verify
steps pass. Nothing in the pipeline moves a release back.

`scripts/release-manifest.sh <version> all` lists the files in a release. There is no Windows build.

## One-time setup

Do these steps in this order, before the first tag. The ruleset must exist before the signing
credential does.

### 1. Protect release tags

```sh
gh api -X POST repos/Lightless-Labs/pessimal/rulesets --input - <<'JSON'
{
  "name": "release tags",
  "target": "tag",
  "enforcement": "active",
  "conditions": { "ref_name": { "include": ["refs/tags/v*"], "exclude": [] } },
  "rules": [ { "type": "creation" }, { "type": "update" }, { "type": "deletion" } ],
  "bypass_actors": [ { "actor_id": 1, "actor_type": "OrganizationAdmin", "bypass_mode": "always" } ]
}
JSON
```

Only organisation admins can then create, move or delete a `v*` tag. Put all the fields in the JSON
body: `gh api` sends `-f` fields as query parameters when you use `--input`.

Check it:

```sh
gh api repos/Lightless-Labs/pessimal/rulesets --jq '.[] | "\(.id) \(.name) \(.target) \(.enforcement)"'
```

### 2. Create a GitHub token

Create a fine-grained token at <https://github.com/settings/personal-access-tokens/new>:

- Resource owner: `Lightless-Labs`.
- Repositories: `pessimal` and `homebrew-tap` only.
- Permissions: Contents, read and write. Nothing else.

The organisation must allow fine-grained tokens. If it needs approval, approve the token.

Check that the token can write to both repositories. This command writes nothing:

```sh
read -rs GH_TOKEN && export GH_TOKEN      # paste the token and press return
for repo in pessimal homebrew-tap; do
  gh api -X POST "repos/Lightless-Labs/$repo/releases/generate-notes" \
    -f tag_name=v0.0.0-token-check -f target_commitish=main --jq .name
done
unset GH_TOKEN
```

Each line prints `v0.0.0-token-check`. A 403 means the token cannot write. A 404 means it cannot see the
repository.

### 3. Store the token in Doppler

```sh
doppler configs create prd_github_release --project lightless-labs-pessimal --environment prd

pbpaste | tr -d '\n' | doppler secrets set GITHUB_TOKEN --silent \
  --project lightless-labs-pessimal --config prd_github_release
pbcopy </dev/null

doppler secrets --only-names --project lightless-labs-pessimal --config prd_github_release
```

The last command must list only `GITHUB_TOKEN` and Doppler's own `DOPPLER_*` names. If it lists more,
they come from the root `prd` config. Move them, or this token can read them too.

Check that the macOS config has its five secrets:

```sh
doppler secrets --only-names --project lightless-labs-pessimal --config prd_macos_notarisation
# MACOS_DEVELOPER_ID_CERT_P12_BASE64  MACOS_DEVELOPER_ID_CERT_PASSWORD
# APPLE_NOTARY_KEY_ID  APPLE_NOTARY_ISSUER_ID  APPLE_NOTARY_KEY_P8_BASE64
```

### 4. Create two read-only Doppler tokens

```sh
doppler configs tokens create buildkite-release-macos --access read --plain \
  --project lightless-labs-pessimal --config prd_macos_notarisation | pbcopy
# paste it into the Buildkite secret in step 5, then:
pbcopy </dev/null

doppler configs tokens create buildkite-release-github --access read --plain \
  --project lightless-labs-pessimal --config prd_github_release | pbcopy
# paste it into the Buildkite secret in step 5, then:
pbcopy </dev/null
```

### 5. Add two Buildkite secrets

In Buildkite, go to **Agents → your cluster → Secrets → New Secret**. Use these exact names:

| Secret | Value | Used by |
|---|---|---|
| `DOPPLER_PESSIMAL_PRD_MACOS_NOTARISATION` | the `buildkite-release-macos` token | `release-macos` |
| `DOPPLER_PESSIMAL_PRD_GITHUB_RELEASE` | the `buildkite-release-github` token | `release-publish`, `release-promote` |

On each secret's **Access** tab, restrict it to tag builds of this pipeline. A possible policy:

```yaml
- pipeline_slug: pessimal
  build_branch: "v*"
  cluster_queue_key: ci-macos-apple-silicon   # ci-linux-arm64 for the GitHub secret
```

Check the syntax in Buildkite's documentation first. Nobody has checked what `build_branch` contains on
a tag build. `v*` also matches a branch whose name starts with `v`, and the tag ruleset does not protect
branches, so do not create branches that start with `v`. After the first release, check that a push to
`main` and a push to a branch named `v-probe` cannot read either secret.

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
| `configured doppler_token_secret is missing from host env and Buildkite secrets: …` | The Buildkite secret is missing, or its access policy blocks this build. The two look the same. It can come from `release-macos`, `release-publish` or `release-promote`. | Fix the secret or policy, then Retry. |
| `Doppler read failed for … HTTP 401` | The Doppler token is revoked or wrong | Make a new token, replace the Buildkite secret, Retry. |
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

**Two credentials, in two Doppler configs.** `prd_macos_notarisation` holds the Apple signing secrets.
Only `release-macos` reads it. `prd_github_release` holds only `GITHUB_TOKEN`. Only `release-publish` and
`release-promote` read it.

**The signing step runs no code that was built.** A build runs code from every dependency's `build.rs`,
and any process of the same user can read its parent's starting environment with `ps -E`. So
`release-macos-build` builds with no token, and `release-macos` signs with the token and runs only
Apple's tools.

**The signing token is the dangerous one.** It can sign any code as Lightless Labs, and Apple will
notarize it. If it leaks, you must revoke the certificate. That can also stop copies of `Pessimal.app`
that people already have from opening.

**The GitHub token can still get code signed.** A fine-grained token acts as the account that made it.
Today the organisation has one member, and that member is the admin who bypasses the tag ruleset. So
a leaked release token can push a commit to `main`, which has no ruleset, push a `v*` tag on it, and
the pipeline signs that code. To close this, make the token from a separate machine account that has
write access but no admin role, and add a ruleset on `main` that only the owner can bypass. Until then,
the two Doppler configs keep the signing secrets away from the GitHub token, but not the reverse.

**tart-ci writes the Doppler token to a file on the host** while a step runs, and deletes it when the
step ends. If a Buildkite agent crashes during a credentialed step, check the host for leftover
`tart-ci/*/command.sh` files and `buildkite-tart-ci-*` VMs, and delete them.

**Rotate** the two Doppler tokens and the GitHub token every three months, and at once if one may have
leaked. Make the new token, put it in Buildkite or Doppler, and only then revoke the old one:

```sh
doppler configs tokens --project lightless-labs-pessimal --config prd_macos_notarisation
doppler configs tokens revoke --slug <slug> --project lightless-labs-pessimal --config prd_macos_notarisation
```

Delete old GitHub tokens at <https://github.com/settings/personal-access-tokens>.

Work that is not in this release (Windows, musl, `.deb`, crates.io, a cask for the app) is listed in
[`docs/plans/2026-09-12-distribution.md`](../plans/2026-09-12-distribution.md).
