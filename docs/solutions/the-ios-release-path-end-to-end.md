# What the first iOS release needed

**Shipped:** 2026-09-11. Build `0.1.0.26` of `com.lightless-labs.pessimal.ios`. The self-hosted mini
uploaded it, and App Store Connect processed it.

The release needed eight attempts. No single fix was difficult. This record is useful because of the
types of failure. **Five failures came from the configuration of this repo. Two came from the Apple
setup. One came from my design mistake, and two more attempts did not find it.**

## The failures, in the sequence that they occurred

| # | Failure | Cause |
|---|---|---|
| 1 | `rbenv init - bash` → `complete: command not found` | The guest runs commands in zsh. |
| 2 | `rbenv: version '3.4.5' is not installed` | `.ruby-version` names the Ruby of the development machine. rbenv obeys that file before our checks run. |
| 3 | `BUILDKITE_BUILD_NUMBER: parameter not set` | The tart-ci plugin sends only the variables in its `env` allowlist. |
| 4 | `Doppler returned no value for APPLE_TEAM_ID` | A team ID is not a secret, and `BUILD.bazel` already contained it. |
| 5 | `No matching provisioning profile found` | `sigh`. A direct App Store Connect call replaced it. |
| 6–7 | `Unable to find an identity ... matching the ones in ...mobileprovision` | **Two guesses.** Each guess cost a full build. |
| 8 | Five App Store validation errors at the same time | The app had no icon, and three Info.plist keys were missing. |

## The important lesson

Failures 6 and 7 were one failure that occurred two times. `rules_apple` signs at the end of the build.
Thus, each hypothesis cost about 180 seconds of Bazel. This was the error message:

    ERROR: Unable to find an identity on the system matching the ones in ...mobileprovision

This message covers three different conditions, and it does not tell you which condition occurred:

- No identity is visible.
- An identity is visible, but the profile does not accept it.
- An identity is visible, but it has no private key that codesign can use.

We tried two keychain theories. The second theory was that a Bazel sandbox cannot read `/var/folders`.
A commit added a comment that stated this theory as a fact. Nobody demonstrated it, and it was not the
cause.

A diagnostic script found the answer in one attempt:

```
profile 'com.lightless-labs.pessimal.ios' accepts 1 certificate(s):
  764C077A58A9FB589B8F2847FBF53C7B5637661A  'Apple Distribution: Thomas Leger (PKPPLFK854)'
identities visible: 1
  24583EF1D58CEAEE35ABF2023C529D8184D25BA7  'Apple Distribution: Thomas Léger (PKPPLFK854)'
NO MATCH.
```

The team had two Apple Distribution certificates. Their names had one difference: an accent. No change
to the keychain could fix that.

**The rule:** a tool can give one error for different conditions. When this occurs, do not write the
next hypothesis. Write the check that shows which condition occurred. This method was already
successful two times earlier in the same session. A list of the secret *names* in Doppler found the
missing `APPLE_TEAM_ID` at its first run. When `asc.py` prints all visible profiles, the output shows
failure 5 immediately.

## The components now

- [`scripts/asc.py`](../../scripts/asc.py): a read-only App Store Connect client. It uses `openssl` to
  sign its ES256 JWTs. Thus, it needs only the system Python 3.9.
- [`scripts/signing-diagnostics.py`](../../scripts/signing-diagnostics.py): prints the certificates that
  the profile accepts, the visible identities, and the identities that are in the two lists. It runs
  before Bazel.
- [`scripts/release-ios-testflight-buildkite.sh`](../../scripts/release-ios-testflight-buildkite.sh):
  reads the secrets through the Doppler REST API and removes the service token. Since 2026-09-14 it also
  signs, builds and uploads without fastlane.
- [`tools/appicon/render-app-icon.swift`](../../tools/appicon/render-app-icon.swift): draws the icon at
  each declared size. Thus, you can compare and rebuild the committed PNG files.

The band's cookbook has the general version of all this, in §6.1–6.4 of
`docs/solutions/ci-cd-patterns/2026-09-10-buildkite-self-hosted-ios-cicd-cookbook.md`.

## Not yet proved

App Store Connect processed this TestFlight build. That does not prove that a person installed it.
Tester access, installation and behaviour on a device each need their own evidence.
