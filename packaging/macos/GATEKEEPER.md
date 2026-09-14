# The macOS agent and Gatekeeper

The macOS `pessimal-agent` binary is signed and notarized, but it has no stapled ticket. A plain binary
cannot hold one. So the first time macOS checks a downloaded copy, it asks Apple over the network.

On a host that cannot reach Apple, that first start can fail. Under launchd there is no message: the
agent does not start, and its metrics do not arrive.

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

A signed, notarized and stapled `.pkg` or `.dmg` can start without network access. A `.pkg` needs a
Developer ID Installer certificate. The release uses a Developer ID Application certificate. It is not
known if the `.p12` in `lightless-labs-pessimal/prd_macos_notarisation` also has an Installer
certificate.

## The app

`Pessimal-<version>-macos.zip` contains the stapled `Pessimal.app`. `scripts/notarize-macos-app.sh`
signs, notarizes and staples the bundle, checks it, and then zips it.

`Pessimal.app` runs on Apple silicon only. The agent binary ships for both Apple silicon and Intel.
