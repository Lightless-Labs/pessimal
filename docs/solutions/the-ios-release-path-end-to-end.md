# What the first iOS release actually needed

**Shipped:** 2026-09-11. Build `0.1.0.26` of `com.lightless-labs.pessimal.ios`, uploaded from the
self-hosted mini and processed by App Store Connect.

Eight release attempts. Worth recording not because any single fix was hard, but because of what they
were: **five were this repo's own configuration, two were Apple-side setup, and one was a design
mistake of mine that two further attempts failed to find.**

## The order they arrived in, and what each cost

| # | Failure | Where it came from |
|---|---|---|
| 1 | `rbenv init - bash` → `complete: command not found` | the guest runs commands under zsh |
| 2 | `rbenv: version '3.4.5' is not installed` | `.ruby-version` names the dev machine's Ruby; rbenv obeys it before any check of ours |
| 3 | `BUILDKITE_BUILD_NUMBER: parameter not set` | the tart-ci plugin forwards only its `env` allowlist |
| 4 | `Doppler returned no value for APPLE_TEAM_ID` | a team ID is not a secret and was already in `BUILD.bazel` |
| 5 | `No matching provisioning profile found` | `sigh`; replaced by a direct App Store Connect call |
| 6–7 | `Unable to find an identity ... matching the ones in ...mobileprovision` | **two guesses**, each costing a full build |
| 8 | Five App Store validation complaints at once | the app had no icon, and three Info.plist keys were missing |

## The one worth learning from

Failures 6 and 7 were the same failure twice. `rules_apple` signs at the very end, so each hypothesis
cost ~180 seconds of Bazel, and its message

    ERROR: Unable to find an identity on the system matching the ones in ...mobileprovision

covers three unrelated situations without naming any: nothing visible, something visible that the
profile does not accept, or an identity with no usable private key. Two keychain theories were tried —
the second, that a Bazel sandbox cannot read `/var/folders`, was committed as a comment asserting it as
fact. It was never demonstrated and it was not the cause.

Writing the diagnostic instead took one attempt and answered it outright:

```
profile 'com.lightless-labs.pessimal.ios' accepts 1 certificate(s):
  764C077A58A9FB589B8F2847FBF53C7B5637661A  'Apple Distribution: Thomas Leger (PKPPLFK854)'
identities visible: 1
  24583EF1D58CEAEE35ABF2023C529D8184D25BA7  'Apple Distribution: Thomas Léger (PKPPLFK854)'
NO MATCH.
```

Two Apple Distribution certificates in one team, differing by an accent. No keychain work could have
fixed it.

**The rule:** when a tool's error cannot distinguish the situations it reports, the next thing to write
is the thing that distinguishes them — not the next hypothesis. The same move had already paid off
twice in this session before it was applied here: listing Doppler's secret *names* found the missing
`APPLE_TEAM_ID` on its first run, and `asc.py` printing every visible profile would have found
failure 5 immediately.

## What the pieces are now

- [`scripts/asc.py`](../../scripts/asc.py) — read-only App Store Connect client; ES256 JWTs signed by
  shelling out to `openssl`, so it needs nothing beyond the system Python 3.9.
- [`scripts/signing-diagnostics.py`](../../scripts/signing-diagnostics.py) — prints the profile's
  accepted certificates, the visible identities, and the intersection; runs before Bazel.
- [`scripts/release-ios-testflight-buildkite.sh`](../../scripts/release-ios-testflight-buildkite.sh) —
  reads the six secrets over Doppler's REST API and unsets the service token before fastlane starts.
- [`tools/appicon/render-app-icon.swift`](../../tools/appicon/render-app-icon.swift) — draws the icon at
  every declared size, so the committed PNGs are diffable and rebuildable.

The band's cookbook carries the general version of all of this in §6.1–6.4 of
`docs/solutions/ci-cd-patterns/2026-09-10-buildkite-self-hosted-ios-cicd-cookbook.md`.

## Still unproven

A TestFlight build that processes is not a build anyone has installed. Tester access, installation and
on-device behaviour each need their own evidence.
