# Distribution: where a release goes and how someone installs it

**Status:** plan, nothing built yet.
**Context:** the agent has no release artefacts at all, and the macOS app has no home.

## Where things stand

| Thing | Built by | Published to |
|---|---|---|
| iOS app | Buildkite, every green push to `main` | TestFlight. Works. |
| macOS menu bar app | `scripts/release-macos-app.sh` → a **signed, notarized, stapled `Pessimal.app.zip`** | Nowhere. CI never runs that script. |
| Host agent | `cargo build --release -p pessimal_agent_host` | Nowhere. Build it yourself. |

So the honest answer to "where do I get it" is today: you build it. `scripts/install-agent-launchd.sh`
assumes exactly that — it installs from `target/release` or builds on the spot.

## Decisions

**1. GitHub Releases is the distribution point.** A `v*` tag triggers a build matrix; the artefacts and
a `SHA256SUMS` land on the release. Everything downstream — Homebrew, mise, nix, a curl installer —
reads from there rather than from its own build. The repo is already public on GitHub, so this adds no
infrastructure and no account anyone has to trust.

**2. Asset names follow the `ubi` convention**, which is what buys mise support for free:

```
pessimal-agent-<version>-aarch64-apple-darwin.tar.gz
pessimal-agent-<version>-x86_64-apple-darwin.tar.gz
pessimal-agent-<version>-x86_64-unknown-linux-gnu.tar.gz
pessimal-agent-<version>-aarch64-unknown-linux-gnu.tar.gz
pessimal-agent-<version>-x86_64-pc-windows-msvc.zip
Pessimal-<version>-macos.zip          # the menu bar app, notarized and stapled
SHA256SUMS
```

Each tarball carries the binary, `LICENSE`, `pessimal.example.toml`, and the platform's service file
— the systemd unit on Linux. The licence is not decoration: AGPL-3.0 means distributing a binary
carries an obligation to offer the corresponding source, and shipping the licence plus a public repo
is how that is met.

**3. The macOS agent binary has to be signed, and this is the part that will bite.** Apple Silicon
refuses to execute an unsigned Mach-O at all — not a Gatekeeper prompt, a kill. A binary
cross-compiled on a Linux runner and dropped into a tarball is therefore dead on arrival on exactly
the machines most likely to install it. The Developer ID certificate and notary key are already in
Doppler for the app (`prd_macos_notarisation`), so the agent signs with the same identity and the
tarball gets notarized alongside. Verify by downloading the published artefact on a machine that has
never seen the source, which is the only test that counts.

**4. Homebrew: one tap, two casks-and-formulas.**

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
mise use -g "ubi:Lightless-Labs/pessimal[exe=pessimal-agent]"
```

mise's `ubi` backend resolves GitHub release assets by platform. This is the cheapest channel we have
and it is a naming convention, not a package.

**6. Linux: `gnu` targets first, not `musl`.** The agent pulls `aws-lc-rs` through reqwest's default
rustls provider, and a static musl build of it is a C-toolchain fight for a benefit nobody has asked
for yet. `x86_64-unknown-linux-gnu` and `aarch64-unknown-linux-gnu` built against an old glibc cover
Debian 11+, Ubuntu 20.04+, and every current RHEL derivative. `.deb`/`.rpm` via `nfpm` is a later
phase; the tarball plus the unit file is the whole of it for now. If musl ever matters, the lever is
the agent's TLS features — `ring` instead of `aws-lc-rs` — not a heroic build.

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

1. **Release workflow.** Tag `v*` → build the matrix → sign and notarize the macOS artefacts → upload
   assets and `SHA256SUMS`. Teaches `cog bump` to tag, and makes the existing
   `scripts/release-macos-app.sh` run in CI for the first time. Ends with mise working.
2. **The tap.** A `Lightless-Labs/homebrew-tap` repo, a formula with a service block, a cask for the
   app, and the overlap caveat from decision 4.
3. **The flake**, with the NixOS module.
4. **`nfpm` packages**, `winget`/`scoop` if anyone asks, and Sparkle for in-app macOS updates.

## Open questions

- Who owns the tap repo, and does it get its own CI to verify the formula installs?
- Sign the Linux artefacts too (minisign or cosign), or let `SHA256SUMS` on a tagged release stand?
- The macOS app has no update mechanism at all once installed. A cask updates on `brew upgrade`; a
  hand-downloaded zip never does. Sparkle or accept it?
