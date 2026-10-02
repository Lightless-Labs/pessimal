# M11: iOS widgets, home and lock screen

**Created:** 2026-10-02
**Status:** planned. Stage 0 (the memory spike) gates the design; stage 5 waits for owner steps.

## Goal

A widget a person configures — the whole fleet, or one host, and which metric — on the home screen
and the lock screen. The iOS app is rarely running, so a glance that does not need it open is most
of what a fleet monitor is for on a phone.

## What it shows

| Family | Where | Content |
|---|---|---|
| `systemSmall` | Home | Fleet: worst severity, hosts and alive. One host: its liveness and one metric. |
| `systemMedium` | Home | Fleet: the first few hosts in core's order, each with its liveness. One host: three metrics. |
| `systemLarge` | Home | Fleet: up to eight hosts with liveness and one metric each. |
| `accessoryInline` | Lock screen | "3/3 alive", or one host's liveness and one value. |
| `accessoryCircular` | Lock screen | A gauge: alive over total, or one host's metric as a ratio. |
| `accessoryRectangular` | Lock screen | One host or the fleet, in three short lines. |

Configuration is an App Intent (`AppIntentConfiguration`, iOS 17; the app's minimum is already 17.0):
**Scope** — the fleet, or one host chosen from the roster; **Metric** — chosen from the kinds the
app already charts. Temperature is offered only when a host reports it, and shows the hottest
sensor.

The rules from the apps carry over unchanged: the tallies show only what is not zero
(`FleetTally`), a host's severity is core's, and absence renders as absence.

## Where the data comes from

**The widget polls the backend itself**, through the same Rust core the app uses, in its
`TimelineProvider`. Not a snapshot written by the app: the app runs when someone opens it, so a
snapshot is stale exactly when the widget is the only thing being looked at.

It needs the backend address, the environment and the API key. All three move to a **shared
keychain item** under a team-prefixed access group, `PKPPLFK854.com.lightless-labs.pessimal.shared`.

Why the keychain and not an App Group:

- Keychain sharing needs no portal capability. Every explicit App ID's profile already carries
  `keychain-access-groups = PKPPLFK854.*`, so any team-prefixed group is allowed; the only reason
  Xcode shows it as a capability is to edit the list
  ([DTS, forum thread 653372](https://developer.apple.com/forums/thread/653372),
  [thread 809012](https://developer.apple.com/forums/thread/809012)).
- An App Group must be registered in the portal and enabled on both App IDs, and both profiles
  regenerated. That is more owner steps for a container this design does not need.
- The key is a secret and already lives in the keychain. Moving the address and environment beside
  it keeps one store, not two.

The app's existing item (service `com.lightless-labs.pessimal.macos`, no access group, so in the
app's private group) is migrated once into the shared group on launch, and the old item deleted
after the copy is read back.

## The risk that gates the design: 30 MB

A widget extension is killed at **30 MB** resident
([forum thread 713561](https://developer.apple.com/forums/thread/713561),
[733347](https://developer.apple.com/forums/thread/733347)). The Rust core brings tokio, reqwest and
rustls. Nobody has measured what one poll costs in an extension process.

**Stage 0 measures it before anything else is built.** A minimal extension links `pessimal_ffi`,
runs one real poll in its timeline provider on a physical iPhone, and reports its peak footprint.
The simulator does not enforce the limit, so its number is a floor, not an answer.

- Under ~20 MB: build as designed.
- Close to 30: a narrower FFI entry point for the widget — a single-tier poll with no detail
  queries, a current-thread runtime instead of the multi-threaded one, and no usage reporting.
- Over: the widget reads a snapshot from an App Group container that the app writes, and gains an
  owner step. Its staleness is then shown, never hidden.

## Signing: what it needs

| Item | State | Who |
|---|---|---|
| Extension App ID `com.lightless-labs.pessimal.ios.widget` | does not exist | owner, or CI with an Admin key |
| App Store profile for it, named `com.lightless-labs.pessimal.ios.widget` | does not exist | owner, or CI with an Admin key |
| Keychain group | allowed by every existing profile | nobody |
| App Group | not needed unless stage 0 fails | — |

Two cautions already in CLAUDE.md apply directly:

- **An entitlements file replaces the set rules_apple takes from the profile.** Adding
  `keychain-access-groups` to the app means the app gains its first entitlements file, and every key
  the app needs must then be in it. M9 stage 5 (the iCloud key-value store) adds a key to the same
  file. The two must be designed together, keyed exactly like `provisioning_profile`.
- The extension's profile name must match in three places, as the app's does: the portal, the
  `local_provisioning_profile` target, and `scripts/release-ios-testflight-buildkite.sh`.

## Stages

| Stage | What | Gate |
|---|---|---|
| 0 | Memory spike: a bare extension, one poll, peak footprint on a device | — |
| 1 | The shared keychain item and the one-time migration, with tests | stage 0 passes |
| 2 | The widget's FFI entry point, sized by stage 0 | stage 0 |
| 3 | The extension: timeline provider, App Intent configuration, the six families | 1, 2 |
| 4 | Bazel: `ios_extension`, the app embeds it, the simulator build in CI | 3 |
| 5 | Signing: the extension's profile in the TestFlight script; entitlements for both | owner steps |

Stages 1 to 4 build and run in the simulator with no signing at all (`//:ci_build` signs nothing).
Only stage 5 needs the portal.

## Verification

1. `cargo test --workspace`, including the widget entry point's tests.
2. `scripts/swift-smoke.sh`: the migration, and whatever of the widget's view logic is pure.
3. `bazelisk build --config=ci //clients/apple/ios:Pessimal` with the extension embedded.
4. The simulator: add each family, configure it for the fleet and for one host, and confirm a
   refresh fetches.
5. A physical iPhone, for the one thing the simulator cannot show: the extension staying under
   30 MB across real polls.

## Out of scope

- Live Activities and interactive widgets (buttons inside a widget).
- A macOS widget. The menu bar already is one.
- Alerts pushed to the widget. A widget refreshes on WidgetKit's budget, not on events.
