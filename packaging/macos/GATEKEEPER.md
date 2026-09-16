# The macOS agent and Gatekeeper

The macOS `pessimal-agent` binary is signed and notarized, but it has no stapled ticket. A plain binary
cannot hold one. So the first time macOS checks a downloaded copy, it asks Apple over the network.

On a host that cannot reach Apple, that first start can fail. Under launchd there would be no message:
the agent would not start, and its metrics would not arrive. **Nobody has measured this.** An attempt on
the development VM on 2026-09-15 proved nothing, because that guest has SIP disabled and developer mode
enabled: an ad-hoc signed binary with the same quarantine attribute, which Gatekeeper must reject, ran
as well. A real measurement needs a Mac with SIP on, a browser download, and no route to Apple.

This applies to one install path: a browser download. Homebrew, mise and `curl` + `tar xzf` never mark
a file as quarantined, so Gatekeeper never looks at the binary. Checked with `brew install
lightless-labs/tap/pessimal-agent` against v0.1.2 on 2026-09-16: the installed binary has no extended
attributes at all.

`Pessimal.app` is different. It is stapled, so it opens without network access.

## Why the binary cannot be stapled

`notarytool` accepts only `.dmg`, `.pkg` and `.zip` files. `scripts/notarize-macos-binary.sh` puts the
binary in a zip, submits the zip, and deletes it.

`stapler` can attach a ticket only to a disk image, a signed bundle or a flat package. A plain Mach-O
binary has no place for a ticket. Do not add a `stapler staple` step for the binary. It fails.

## Check a binary

This command rejects every plain command-line tool, even a correctly notarized one:

```
spctl --assess --type execute pessimal-agent
pessimal-agent: rejected (the code is valid but does not seem to be an app)
```

Use this command instead:

```sh
spctl --assess --type open --context context:primary-signature --verbose=4 pessimal-agent
# pessimal-agent: accepted
# source=Notarized Developer ID
```

`codesign -dvv pessimal-agent` must show `Authority=Developer ID Application: …`, `runtime` in `flags=`,
and a `Timestamp=` line. Like the first start, `spctl` can need network access.

## When macOS marks a file as quarantined

macOS adds `com.apple.quarantine` only when a file comes from a browser and you open it with Finder,
Archive Utility, `ditto` or `unzip`.

| Download | Unpack | Quarantined |
|---|---|---|
| Browser | Finder, Archive Utility, `ditto`, `unzip` | yes |
| `curl`, `wget` | `tar xzf` | no |
| `mise`, `ubi` | their own unpack | no |

Notarization does not remove this mark. macOS records its approval inside the mark.

A test with `curl` and `tar` does not test Gatekeeper. To test it, download the release in a browser on a
real Mac and open it in Finder.

`scripts/install-agent-launchd.sh` refuses a quarantined binary and prints the command to remove the
mark:

```sh
xattr -d com.apple.quarantine <path>
```

Or run the installer with `--clear-quarantine`.

## What would fix it

An installer package. Apple's Developer Technical Support states that "stapling only works for bundled
code (typically apps), installer packages, and disk images", and advises packaging a command-line tool
in a container that supports stapling ([thread 736973](https://developer.apple.com/forums/thread/736973)).
For a tool the package is the best of the three: "When the user goes to install the package, Gatekeeper
checks it. Assuming that check passes, Gatekeeper does no further checks on the content it installed"
([thread 706379](https://developer.apple.com/forums/thread/706379)). The same page records a Gatekeeper
bug that blocks any tool double-clicked in Finder, and names a package as the way around it.

A `.pkg` needs a Developer ID Installer certificate. The release uses a Developer ID Application
certificate. It is not known if the `.p12` in `lightless-labs-pessimal/prd_macos_notarisation` also has
an Installer certificate, and only the Account Holder can create one.

The plan, the alternatives (the agent inside `Pessimal.app`, a stapled `.dmg`) and the commands are in
[`todos/008-pending-p3-stapled-pkg-for-the-macos-agent.md`](../../todos/008-pending-p3-stapled-pkg-for-the-macos-agent.md).

## The app

`Pessimal-<version>-macos.zip` contains the stapled `Pessimal.app`. `scripts/notarize-macos-app.sh`
signs, notarizes and staples the bundle, checks it, and then zips it.

`Pessimal.app` runs on Apple silicon only. The agent binary ships for both Apple silicon and Intel.
