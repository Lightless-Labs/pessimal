# Distribution: where a release goes and how someone installs it

**Status:** phase 1 built, not yet run.
**Phase 1 built:** 2026-09-13, on Buildkite. No `v*` tag has been pushed, so no release exists and the
release has never run end to end.
**Revised:** 2026-09-14: what phase 1 was built as, and corrections to decisions 3, 5 and 6, each marked
where it stands.
**Context:** the agent had no release artefacts at all, and the macOS app had no home.

## What phase 1 became

- **Buildkite, not GitHub Actions.** A GitHub Actions release workflow was designed first, and the
  repository owner ruled it out with the words "Not *a single* use of Github Actions. Anywhere." So
  the release runs as seven tag-only steps in [`.buildkite/pipeline.yml`](../../.buildkite/pipeline.yml),
  on the same self-hosted cluster and Tart guests that already ship the iOS app, copying the release
  shape of the sibling project Descartes. The project's GitHub Actions CI was deleted in the same
  change, after each of its checks was ported to Buildkite. How to cut, watch and wipe a release is in
  [`../runbooks/cutting-a-release.md`](../runbooks/cutting-a-release.md).
- **The Homebrew formula moved into phase 1.** `Lightless-Labs/homebrew-tap` already existed (it
  carries `descartes` and `middens`), so the formula needed no new repository. Its source is
  [`packaging/homebrew/pessimal-agent.rb.template`](../../packaging/homebrew/pessimal-agent.rb.template),
  and the release's last step renders it and commits it to the tap. The cask for the app stays in
  phase 2.
- **Windows is deferred.** A `cargo zigbuild --target x86_64-pc-windows-gnu` probe ran for over twenty
  minutes without finishing, and the cluster has no Windows machine that could ever execute the result,
  so it would have been the one asset nobody had run. No Windows asset is published; the release notes
  say so and why.
- **Forward only.** A tag produces a draft, then a prerelease that `/releases/latest` does not point at,
  and only after anonymous download-and-execute checks on Linux and macOS does it become latest. If a
  release goes wrong, a human deletes the release and the tag and cuts it again; the pipeline itself
  never un-publishes.

## Where things stand

| Thing | Built by | Published to |
|---|---|---|
| iOS app | Buildkite, every green push to `main` | TestFlight. Works. |
| macOS menu bar app | Buildkite `release-macos`, on a `vX.Y.Z` tag → `Pessimal-<version>-macos.zip`, signed, notarized, stapled | GitHub Releases, once a tag is pushed. None has been. |
| Host agent | Buildkite `release-linux` and `release-macos`, on the same tag → four tarballs | GitHub Releases and `lightless-labs/tap/pessimal-agent`, once a tag is pushed. None has been. |

So the honest answer to "where do I get it" is still, today: you build it. The release that changes
that is built and has never run. `scripts/install-agent-launchd.sh` installs from `target/release`,
builds on the spot, or (with `--tarball`) takes a downloaded release archive.

## Decisions

**1. GitHub Releases is the distribution point.** A `vX.Y.Z` tag triggers the release build; the
artefacts and a `SHA256SUMS` land on the release. Everything downstream — Homebrew, mise, nix, a curl installer —
reads from there rather than from its own build. The repo is already public on GitHub, so this adds no
infrastructure and no account anyone has to trust.

**2. Asset names follow the `ubi` convention**, which is what buys mise support for free:

```
pessimal-agent-<version>-aarch64-apple-darwin.tar.gz
pessimal-agent-<version>-x86_64-apple-darwin.tar.gz
pessimal-agent-<version>-x86_64-unknown-linux-gnu.tar.gz
pessimal-agent-<version>-aarch64-unknown-linux-gnu.tar.gz
Pessimal-<version>-macos.zip          # the menu bar app, notarized and stapled
SHA256SUMS
```

That is exactly what [`scripts/release-manifest.sh`](../../scripts/release-manifest.sh) lists, and
nothing else may spell an asset name. There is no Windows asset (see "What phase 1 became").

Each tarball carries the binary, `LICENSE`, `pessimal.example.toml`, a `README.md`, and on Linux the
systemd unit. The licence is not decoration: AGPL-3.0 means distributing a binary
carries an obligation to offer the corresponding source, and shipping the licence plus a public repo
is how that is met.

**3. The macOS agent binary has to be signed, and this is the part that will bite.** Apple Silicon
will not execute an unsigned Mach-O at all — not a Gatekeeper prompt, a kill. Apple's own linker
ad-hoc signs arm64 output, so a build *on* a Mac is fine by accident; a cross-build whose linker does
not (lld, depending on version and flags) produces a binary that is dead on arrival on exactly the
machines most likely to install it. Ad-hoc is enough to *run*; Developer ID plus notarization is what
gets something a browser downloaded past Gatekeeper. Which applies depends on the channel, so do both
and stop guessing. The Developer ID certificate and notary key are already in Doppler for the app
(`prd_macos_notarisation`), so the agent signs with the same identity. Verify by downloading the
published artefact on a machine that has never seen the source, which is the only test that counts.

*Corrected 2026-09-14.* Three things above were wrong or incomplete:

- **Notarisation does not clear the quarantine flag.** `com.apple.quarantine` persists; Gatekeeper
  evaluates the file and records its approval in the attribute's flag bits. Homebrew's
  `inherit_user_approval!` depends on exactly that persistence, which is why the signing identity must
  stay stable across releases.
- **The tarball is not notarized, and cannot carry a ticket.** The binary *inside* it is signed and
  notarized: `notarytool` accepts only disk images, flat installer packages and zips, so the binary is
  zipped purely to be submitted. `stapler` has nowhere to put a ticket in a bare Mach-O, so the agent
  ships unstapled and Gatekeeper fetches the ticket over the network on a quarantined copy's first
  launch. `Pessimal.app`, a bundle, *is* stapled. The whole story is in
  [`packaging/macos/GATEKEEPER.md`](../../packaging/macos/GATEKEEPER.md).
- **The lld hazard does not arise.** Both macOS slices are built on Apple silicon macOS with Apple's
  linker: arm64 natively, x86_64 with `--target x86_64-apple-darwin` (measured working for this
  dependency graph). But the cross-built x86_64 slice comes out of the linker *not signed at all*,
  unlike the arm64 one, so the release signs both slices explicitly and checks each.

**4. Homebrew: one tap, a formula and a cask.** *(2026-09-14: the tap already existed, so the formula
is built in phase 1; the cask waits for phase 2.)*

```sh
brew install lightless-labs/tap/pessimal-agent   # the agent, with a service block
brew install --cask lightless-labs/tap/pessimal  # the menu bar app
brew services start pessimal-agent
```

A tap rather than homebrew-core: core wants notability we do not have and builds from source by
policy, while a tap may ship the prebuilt, signed artefact that decision 3 requires. The formula's
`service do` block is the idiomatic macOS service story, and it supersedes
`scripts/install-agent-launchd.sh` for brew users. **Both use a LaunchAgent, and two of them would
report the same host twice**, so the formula's caveats must say which one the machine already has —
and the installer script should notice a brew-managed service rather than quietly adding a second.

**5. mise needs nothing from us** once decision 2 holds:

```sh
mise use -g github:Lightless-Labs/pessimal
```

mise's `github:` backend resolves GitHub release assets by platform. This is the cheapest channel we
have and it is a naming convention, not a package. *(Corrected 2026-09-14: this said
`ubi:Lightless-Labs/pessimal[exe=pessimal-agent]`. mise's `ubi` backend is deprecated, and `exe` is a
ubi-only option that does not exist on the `github:` backend.)*

**6. Linux: `gnu` targets first, not `musl`.** The agent pulls `aws-lc-rs` through reqwest's default
rustls provider, and a static musl build of it is a C-toolchain fight for a benefit nobody has asked
for yet. `x86_64-unknown-linux-gnu` and `aarch64-unknown-linux-gnu` built against an old glibc cover
most of the Linux hosts anyone runs. `.deb`/`.rpm` via `nfpm` is a later phase; the tarball plus the
unit file is the whole of it for now. If musl ever matters, the lever is the agent's TLS features —
`ring` instead of `aws-lc-rs` — not a heroic build.

*Corrected and achieved, 2026-09-14.* This decision named "Debian 11+, Ubuntu 20.04+" without saying
which glibc; the floor as built is **glibc 2.28**, which is Debian 10+, Ubuntu 18.10+ and RHEL 8+. Both
Linux binaries are built with `cargo zigbuild` for the `.2.28` target suffix, the aarch64 one included
even though the arm64 guest could build it natively, because a native build would link against
whatever glibc the guest image carries. `scripts/check-glibc-floor.sh` reads each binary's versioned
symbol needs and fails the release if either asks for anything newer; measured, both need at most
`GLIBC_2.28`. musl's real cost also turned out to be semantic rather than a toolchain fight: a static
musl binary resolves names without nsswitch, which is a behaviour change, so it stays deferred.

**7. Nix: a flake in this repo, not nixpkgs.** `nix run github:Lightless-Labs/pessimal#pessimal-agent`,
plus a NixOS module that wires the systemd service and takes the key from a path rather than the Nix
store — a key in the store is world-readable. Upstreaming to nixpkgs is a maintenance commitment
worth making only once someone other than us wants it. **Check first** that nixpkgs' default Rust is
at least our MSRV of 1.95; if it is not, the flake pins a toolchain and says so.

**8. Publish the agent to crates.io.** `cargo install pessimal_agent_host` is a one-line install for
the audience most likely to be reading Rust source anyway, and it costs a `cargo publish`. It is also
the only channel that needs no signing: the user's own toolchain builds it.

**9. No `curl | sh`.** It would be the most convenient row in the table and the one that teaches
people to pipe a URL into a shell. The tarball and the package managers above cover every platform we
target.

## Phases

1. **The release.** *Built 2026-09-13 on Buildkite; not yet run.* Tag `vX.Y.Z` → build the Linux and
   macOS agents and the app → sign and notarize the macOS artefacts → publish a prerelease with
   `SHA256SUMS` computed from the bytes GitHub serves → verify by anonymous download and execution →
   promote to latest → render the Homebrew formula into the tap. `cog bump` now moves the workspace
   version and pushes the tag. The app's signing path (`scripts/release-macos-artifacts.sh`, through
   the same library as `scripts/release-macos-app.sh`) runs in CI for the first time. Ends with mise
   and Homebrew working. Windows is deferred.
2. **The cask.** A cask for the app in `Lightless-Labs/homebrew-tap`. It needs the app to be universal
   or to declare `depends_on arch: :arm64`, since the app is Apple silicon only today. The formula, with
   decision 4's overlap caveat, is built in phase 1. `scripts/install-agent-launchd.sh` noticing a
   brew-managed service is not built.
3. **The flake**, with the NixOS module.
4. **`nfpm` packages**, `winget`/`scoop` if anyone asks, and Sparkle for in-app macOS updates.

## Open questions

- ~~Who owns the tap repo~~ — `Lightless-Labs/homebrew-tap`, which already existed. Whether it gets
  its own check that the formula installs is still open: nothing today installs the rendered formula
  before it is committed.
- Sign the Linux artefacts too (minisign or cosign), or let `SHA256SUMS` on a tagged release stand?
- The macOS app has no update mechanism at all once installed. A cask updates on `brew upgrade`; a
  hand-downloaded zip never does. Sparkle or accept it?
