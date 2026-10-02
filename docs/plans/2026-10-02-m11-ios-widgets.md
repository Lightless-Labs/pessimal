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
| `systemSmall` | Home, square | Fleet: worst severity, hosts and alive. One host: its liveness and one metric. |
| `systemMedium` | Home, wide | Fleet: the first few hosts in core's order, each with its liveness and the chosen metric. One host: three metrics. |
| `systemLarge` | Home, large square | Fleet: up to eight hosts with liveness and two metrics each. One host: every overview metric. |

Three home sizes, by the owner's call (2026-10-02): a square, a wide rectangle and a large square,
with more information in each.

**Density follows the fleet size** (also the owner's call). A fleet widget with two hosts has room to
show both in full; one with thirty has room for a summary and the hosts that need attention. So the
layout is chosen from the number of hosts:

| Hosts | Square | Wide | Large |
|---|---|---|---|
| 1 | The host: liveness, and the chosen metric large | The host: liveness and three metrics | The host: liveness and every overview metric |
| 2 – 4 | One row per host: liveness and name | One row per host: liveness, name, the chosen metric | One row per host: liveness, name, two metrics |
| 5 – 8 | The summary: worst severity and the tallies `FleetTally` shows | The summary on one side, the first hosts in core's order on the other | One row per host, as above |
| 9 or more | The summary | The summary and the first hosts | The summary on top, then the first hosts in core's order |

**Paging, in the square and the wide widget** (the owner's call, after Weather Up's widget). With
more than one host, the first page is the layout above, and buttons step through one page per host —
back, forward, and a "back to start" control (`arrow.uturn.backward`). Interactive widgets are
iOS 17, which is the app's minimum. The large widget does not page: it has room for the list.

- **A tap never fetches.** Each tap re-renders the widget; a fetch per tap would take seconds, cost
  memory, and spend WidgetKit's refresh budget. The timeline provider keeps the last poll in the
  extension's own container, and a page change renders from it. Fetching happens only on the
  refresh schedule.
- **The page index is the extension's own state**, keyed by the widget's configuration, so it needs
  no App Group. Two widgets configured identically page together; accepted.
- **Every control has a spoken label** — "Next host", "Previous host", "Back to start" — or
  VoiceOver reads an unlabelled button.

Core's order puts degraded hosts first, so "the first hosts" in a large fleet are the ones worth a
glance, with no second sort in the widget. The thresholds are how many rows fit at the default text
size; they are one pure function (`WidgetDensity`) with tests, not numbers scattered across views.
A widget configured for one host ignores all of this and shows that host.
| `accessoryInline` | Lock screen | "3/3 alive", or one host's liveness and one value. |
| `accessoryCircular` | Lock screen | A gauge: alive over total, or one host's metric as a ratio. |
| `accessoryRectangular` | Lock screen | One host or the fleet, in three short lines. |

Configuration is an App Intent (`AppIntentConfiguration`, iOS 17; the app's minimum is already 17.0):
**Scope** — the fleet, or one host chosen from the roster; **Metric** — chosen from the kinds the
app already charts. Temperature is offered only when a host reports it, and shows the hottest
sensor.

The rules from the apps carry over unchanged: the tallies show only what is not zero
(`FleetTally`), a host's severity is core's, and absence renders as absence.

## When there is nothing new to show

The app's rule carries over, because it is the right one: **a failed poll does not erase what the
last good one showed.** The app keeps its values on screen and qualifies them — an orange banner,
"Data is stale", with the age and core's own sentence for what failed, then a red "Not showing live
data" once the data is past the freshness budget. The widget does the same, and for the same
reason: the age of a value is information, and an error that replaces it throws that away.

So the widget persists core's exported state in its own container after every poll and restores it
into the next session, exactly as the app restores its cache. Core then computes the verdict —
`Fresh`, `Idle`, `Degraded`, `Unusable`, `Unattempted` — and the widget renders it, with no second
copy of the freshness rules in Swift. The same saved state is what paging renders from.

| State | Widget shows |
|---|---|
| Not set up: no shared connection | "Open Pessimal to connect", and a tap opens the app |
| Added, nothing polled yet (`Unattempted`) | The layout redacted, no error |
| Polling fine (`Fresh`) | The layout |
| Polls failing, recent data (`Degraded`) | The last values, the time they are from, and a warning mark |
| Polls failing, old or no data (`Unusable`) | The last values dimmed with their age, or "Can't reach your backend" if there never were any |
| Key rejected (core's unauthorised failure) | "Your key was refused — open Pessimal", and a tap opens Settings |

No network, a backend that is down and a DNS failure are all the same state to the widget: core
reports them as an unreachable failure, and the widget shows the age of what it last saw.

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

The app keeps its own stores exactly as they are, and **mirrors** the connection into the shared
item whenever it changes and on launch. A mirror, not a migration: nothing is deleted, the app's
source of truth does not move, and a widget that cannot read the shared item fails on its own
without taking the app's key with it.

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

What an entitlements file does under rules_apple 4.3.3, read from its source
(`tools/plisttool/plisttool.py`, `update_plist`): it copies **only** `application-identifier` and
`get-task-allow` in from the profile when the file leaves them out. Nothing else is copied. So:

- The app keeps its application identifier with or without listing it.
- The profile's `keychain-access-groups = PKPPLFK854.*` is **not** copied, so the file must list the
  shared group by name — and once it does, that group becomes the app's *default* access group for
  keychain items written without one. Every write here names its group explicitly for that reason.
- M9 stage 5 (the iCloud key-value store) adds a key to the same file. The two are one file, keyed
  exactly like `provisioning_profile`.
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

- Live Activities.
- Buttons that change anything on a host or in the configuration. The only interaction is paging.
- A macOS widget. The menu bar already is one.
- Alerts pushed to the widget. A widget refreshes on WidgetKit's budget, not on events.
