---
status: pending
priority: p3
issue_id: "008"
tags: [release, macos, packaging, gatekeeper]
dependencies: []
---

# A stapled .pkg for the macOS agent

## Problem Statement

The macOS `pessimal-agent` binary is signed and notarized but carries no stapled notarization ticket,
because a bare Mach-O has nowhere to hold one. On a copy that macOS marked with `com.apple.quarantine`,
Gatekeeper asks Apple over the network the first time the binary runs. A host that cannot reach Apple
may then fail to start the agent, and under launchd there is no message.

This affects one install path only: a release archive downloaded through a browser (or Mail, or
AirDrop) and unpacked with Finder, Archive Utility, `ditto` or `unzip`. Homebrew, mise and `curl` +
`tar xzf` never set the attribute, so Gatekeeper never assesses the binary. `brew install
lightless-labs/tap/pessimal-agent` was checked against v0.1.2 on 2026-09-16: the installed binary has
no extended attributes at all.

## Findings

- Apple DTS: "Stapling only works for bundled code (typically apps), installer packages, and disk
  images", and "my advice is that you package your tool into a container that supports stapling,
  notarise that container, and then staple that."
  <https://developer.apple.com/forums/thread/736973>
- Apple DTS, on a tool installed by a package: "When the user goes to install the package, Gatekeeper
  checks it. Assuming that check passes, Gatekeeper does no further checks on the content it
  installed." <https://developer.apple.com/forums/thread/706379>
- The same page records a Gatekeeper bug (r. 58097824) that blocks a command-line tool double-clicked
  in Finder however it is signed, and names an installer package as the way around it.
- Signing a flat package needs a **Developer ID Installer** certificate. The release has a Developer ID
  *Application* certificate. Whether the `.p12` in `lightless-labs-pessimal/prd_macos_notarisation`
  also holds an Installer certificate is not known.
- Only the Account Holder can create a Developer ID certificate in the Apple Developer portal.
- The claim in `scripts/install-agent-launchd.sh` and `packaging/macos/GATEKEEPER.md` that a quarantined
  agent under launchd "fails to start and says nothing" has never been measured. An attempt on the
  development VM on 2026-09-15 proved nothing: that guest has SIP disabled and developer mode enabled,
  so an ad-hoc signed control binary with the same quarantine attribute ran as well. Measuring it needs
  a Mac with SIP on, a browser download, and no route to Apple.

## Proposed Solutions

1. **A signed, notarized, stapled `.pkg` (recommended).** `pkgbuild` a universal `pessimal-agent` into
   `/usr/local/bin`, sign with Developer ID Installer, notarize the package, staple it, and publish it
   beside the tarballs. Gatekeeper checks the package once, offline, from its stapled ticket, and never
   checks the installed binary. Needs the certificate above.
2. **Embed the agent in `Pessimal.app`** and register it with `SMAppService`. The app is stapled
   already, and Gatekeeper's approval of the app covers code inside it, so no new certificate is
   needed. But the app is Apple silicon only, and it would put the viewer on every monitored Mac.
3. **A stapled `.dmg`.** No new certificate, but the ticket sits on the disk image, not on the binary a
   user copies out of it, so the offline case is unproven.
4. **Leave it.** Point browser downloads at Homebrew or at `curl` + `tar`, which never quarantine, and
   keep `--clear-quarantine` for the rest.

## Recommended Action

Solution 1, once the owner confirms a Developer ID Installer certificate exists (or creates one) and
says where it lives in Doppler. Until then, solution 4 is what ships, and the release is unaffected:
every other install path is clean.

## Technical Details

Build (in `release-macos`, after the binaries are signed):

```sh
lipo -create -output "$stage/usr/local/bin/pessimal-agent" "$arm64_binary" "$x86_64_binary"
pkgbuild --root "$stage" --identifier com.lightless-labs.pessimal.agent --version "$version" \
  --install-location / "$unsigned_pkg"
productsign --sign "Developer ID Installer: ..." "$unsigned_pkg" "$pkg"
xcrun notarytool submit "$pkg" --wait ...     # must report Accepted
xcrun stapler staple "$pkg"
```

Verify (in `release-verify-macos`, against the bytes GitHub serves):

```sh
pkgutil --check-signature "$pkg"
xcrun stapler validate "$pkg"
spctl --assess --type install --verbose=4 "$pkg"   # expect: source=Notarized Developer ID
```

`scripts/release-manifest.sh` gains the package, `SHA256SUMS` covers it, and the tarballs stay for
Homebrew, mise and `curl`.

## Acceptance Criteria

- A `pessimal-agent-<version>-macos.pkg` on the release, signed, notarized and stapled.
- `stapler validate` and `spctl --assess --type install` pass in the macOS verify step on the
  downloaded package.
- Installing it puts a working universal `pessimal-agent` on `PATH` with no quarantine attribute.
- The Gatekeeper documentation says which install paths involve Gatekeeper at all, and which do not.

## Work Log

- 2026-09-15: raised after the first published release (v0.1.2). Apple's own guidance read and quoted,
  the local measurement attempt found to be void on a SIP-disabled guest, and the certificate question
  put to the owner.

## Resources

- [`packaging/macos/GATEKEEPER.md`](../packaging/macos/GATEKEEPER.md)
- [`docs/plans/2026-09-12-distribution.md`](../docs/plans/2026-09-12-distribution.md)
- Apple DTS, [Notarize: the staple and validate](https://developer.apple.com/forums/thread/736973)
- Apple DTS, [Resolving Gatekeeper problems](https://developer.apple.com/forums/thread/706379)
