# M9: iCloud settings sync

**Created:** 2026-09-14
**Status:** Stages 0-4 are being built. Stages 5-6 (signing) wait for the Apple developer account
steps in [Owner steps](#owner-steps).

## Goal

The environment, the poll interval and the alert rules a person edits on one device appear on their
other devices: the iOS app and the macOS menu bar app, signed in to one Apple Account.

## Answers

### Is CloudKit needed?

No. The iCloud key-value store (`NSUbiquitousKeyValueStore`) is the API Apple provides to "store
settings, configuration information, and app-specific data in a person's iCloud account and share it
among instances of your app"
(<https://developer.apple.com/documentation/foundation/nsubiquitouskeyvaluestore>). Apple's design
guide says to use CloudKit only "in situations where key-value storage and document storage are
insufficient"
(<https://developer.apple.com/library/archive/documentation/General/Conceptual/iCloudDesignGuide/Chapters/iCloudFundametals.html>).
Pessimal syncs two values and a short rule list, so it uses one key-value entry. CloudKit would not
resolve conflicts for us either: it reports them and the app merges
(<https://developer.apple.com/documentation/cloudkit/ckerror/serverrecordchanged>).

One condition. The class reference says the app must ship through the App Store or Mac App Store, but
Apple's macOS capability table lists iCloud key-value storage for Developer ID apps
(<https://developer.apple.com/help/account/reference/supported-capabilities-macos>). The Mac app ships
as a Developer ID zip. So the spike in owner step 7 must pass before any signing change merges. If it
fails, the Mac side uses the [CloudKit fallback](#cloudkit-fallback) behind the same Swift port, and
the Rust document and merge do not change.

### How is the merge deterministic and idempotent?

Every synced value and every alert rule carries a stamp `[milliseconds, counter]`. When two copies
meet, the larger entry under one fixed order is kept: stamp first, then the exact bytes of the value.
A deleted rule stays as a stamped tombstone, and a tombstone beats a live copy of the same rule.
Taking the maximum under a total order is commutative, associative and idempotent, so every device
ends with the same bytes whatever the arrival order, repeats or grouping (Almeida, Shoker, Baquero,
arXiv:1603.01529, section 2 and figs. 3-4).

A new stamp is always larger than every stamp the device holds, so an edit beats every value it was
made on top of, whatever the clocks say. Clocks decide only between edits that did not see each
other, and those break ties the same way on every device.

The merge is a pure Rust function in `pessimal_client_core`, tested exhaustively. iCloud only carries
the bytes: each device keeps its own merged copy, merges what iCloud holds, and writes back only when
the result differs. A write iCloud drops is written again on the next round, as Apple advises
(<https://developer.apple.com/library/archive/documentation/General/Conceptual/iCloudDesignGuide/Chapters/DesigningForKey-ValueDataIniCloud.html>,
"Resolving Key-Value Conflicts").

## What syncs

- **Environment** (`pessimal.environment`), register `environment`. A synced change does not re-mint
  existing rule ids or clear `fleet-state.json`, the same as a local change today.
- **Poll interval** in whole seconds (`pessimal.pollIntervalSeconds`), register
  `poll_interval_seconds`. The receiving device validates it against core's default tuning first.
- **Alert rules**, one entry per rule keyed by its full URN. Adds and deletes, and later any edit, as
  whole rules. A delete is a stamped tombstone, so an offline device cannot bring a deleted rule back.
- **Content this build cannot use** is carried byte for byte and never applied: unknown register
  names, and rule bodies this build cannot decode.

## What stays on the device

- The backend base URL and the API key. They only work together, and the key is a secret: Apple says
  the key-value store "stores the information on disk in an unencrypted format". Syncing them needs a
  synchronizable data-protection keychain item and is a later phase.
- The usage-reporting opt-out. It is per-install consent under M8.
- `fleet-state.json`. Each device folds its own polls.
- `pessimal.sync.replica`, this device's merged copy of the document, and
  `pessimal.sync.replacedRemoteDigest`, the FNV-1a 64-bit hex digest of the last unreadable iCloud
  value this device replaced.
- `overviewMetrics`, `detailMetrics`, focus, the other seven `PollTuning` fields, the liveness policy,
  and all in-memory state.

## The document, format 1

iCloud holds one key, `pessimal.settings`, in `NSUbiquitousKeyValueStore.default`. Its value is a
string of compact UTF-8 JSON. From stages 5-6, both apps claim
`com.apple.developer.ubiquity-kvstore-identifier = PKPPLFK854.com.lightless-labs.pessimal.ios`. The
same JSON is stored locally in `pessimal.sync.replica`.

```json
{"format":1,
 "settings":{"environment":{"at":[1789430400123,0],"value":"\"production\""},
             "poll_interval_seconds":{"at":[0,0],"value":"60"}},
 "rules":{"pessimal::production::alerts::rule::01994f3a-6c1e-7d3b-9a2f-4b8c1d2e3f40":{"at":[1789430455000,0],"rule":"{\"id\":\"pessimal::production::alerts::rule::01994f3a-...\",...}"},
          "pessimal::production::alerts::rule::01994f3b-0a4d-7e21-8c55-6f7a8b9c0d1e":{"at":[1789430500000,1],"rule":null}}}
```

(Line breaks added here. The stored form has no whitespace.)

- `format` is 1. It changes only when the stamp shape, the merge order or the entry shape changes. A
  new synced setting is a new name in `settings` and needs no change. A new top-level key or entry
  key needs a new format.
- `settings` maps a register name to `{"at", "value"}`. `value` is the setting's JSON text as a
  string, or `null` for cleared. A missing name means never set.
- `rules` maps a rule URN to `{"at", "rule"}`. `rule` is the compact JSON of one `AlertRule` as a
  string, or `null` for a tombstone. A body whose `id` differs from its key is kept but never applied.
- `at` is `[ms, seq]`: `ms` is a `u64` no larger than `MAX_MS = 253402300799999`
  (9999-12-31T23:59:59.999Z), `seq` is a `u32`. `[0,0]` is the bootstrap stamp.
- Payloads are JSON text inside strings, so the merge copies them and never re-encodes a rule. Maps
  are `BTreeMap`, so equal documents encode to equal bytes. No Cargo change is needed.
- Decoding has two stages. First only `format` is read: not an object, or a missing, non-integer or
  negative format, is **foreign**; a format above 1 is **newer** and is never merged or overwritten.
  Then format 1 is decoded, ignoring unknown keys: a known key with the wrong type, a malformed `at`,
  or `ms > MAX_MS` is **invalid**. A missing format-1 key (`settings`, `rules`, `at`, `value`,
  `rule`), an entry that is not an object, or format 0 is also invalid.
- Budget: 64 KiB encoded. Over budget, nothing is published, local settings keep working, and the
  status line says so.

## Merge rule

All of it lives in `pessimal_client_core::settings_sync`, with no I/O and no clock read.

| Type | Fields | Order key |
|---|---|---|
| `Stamp` | `ms: u64, seq: u32` | `(ms, seq)` |
| `Register` | `at: Stamp, value: Option<String>` | `(at, value)`, `None < Some`, strings by UTF-8 bytes |
| `RuleEntry` | `at: Stamp, rule: Option<String>` | `(rule.is_none(), at, rule)`, so every tombstone beats every live entry |
| `Document` | `settings`, `rules`: `BTreeMap<String, _>` | per key; a missing key is below every entry |

- **join** keeps the entry with the larger order key at every key of the union of both maps. Each key
  order is total, so this is `max`, which is commutative, associative and idempotent with the missing
  key as identity. A product of such maps has the same laws.
- **tick** mints the stamp for one Save. `top` is the largest stamp in the document, `n` is now
  clamped to `0..=MAX_MS`. If `n > top.ms` the stamp is `[n, 0]`; else if `top.seq < u32::MAX` it is
  `[top.ms, top.seq + 1]`; else if `top.ms < MAX_MS` it is `[top.ms + 1, 0]`; else it returns
  `StampSpaceExhausted`. Stamps from a device with a fast clock are never refused.
- **bootstrap** builds a document from the three stored values at `[0,0]`. If the stored rules do not
  decode, sync stays off on this device (`BootstrapDeferred`) until "Discard the Unreadable Rules"
  and Save fix them.
- **record_edits** is a local Save. It validates the edited values with core first, then stamps only
  what differs from the base: the settings screen's `committed` values. A rule in the base and not in
  the edit gets a tombstone. When the base rules are unreadable there is no base, and no tombstone is
  written. Fields the user did not change get no stamp, so a remote change that arrived while the
  window was open survives the Save.
- **step** is one sync round. It joins the local replica with the iCloud value and publishes only when
  the result differs from what iCloud holds. A newer format pauses sync. Foreign or invalid content
  is replaced once per distinct digest.
- **project** decides what the app applies: values core accepts, and rules that decode, validate and
  match their key, in UUIDv7 creation order. It counts the rules it hides.

Deleting a rule beats a concurrent edit of it (owner decision 0c). The alternative, last stamp wins,
is the order key `(at, rule.is_none(), rule)`.

## When sync runs

Each round runs on the main actor, with no `await` between reading and writing the replica.

- At launch, before `fleet.start()`. Then `synchronize()`; when it returns `false` the status line
  says iCloud is unavailable.
- On an iCloud change notification. On an initial-sync change, run without publishing, then again
  10 s later with publishing. On an account change, clear the replica and the digest, then run.
- On iOS foreground, on Refresh Now (macOS) and Refresh (iOS), and after every successful Save.

A Save calls `SettingsSync.recordEdits` first. It returns the config to save and persists nothing. The
screen calls `SettingsSync.persist` only once the Save goes ahead: on a first run just before the
settings are stored, otherwise just after core accepts the config. `persist` joins the edit with the
current replica, because a round can run while core judges the config.

## Without entitlements

Stages 0-4 add no entitlements. Development builds, the CI builds and today's releases therefore
behave as they do now: the key-value store does not sync, the settings screens save, nothing crashes,
and the status line may say iCloud is unavailable.

Three things still differ from today:

- On macOS, every launch logs a `com.apple.kvs` fault ("BUG IN CLIENT OF KVS") until stages 5-6 add
  the entitlement.
- On a first run, the replica takes the Save just before the settings are stored. If that Save fails
  after this point, the next sync round still applies it. A later Save that fails before core accepts
  its config leaves nothing to sync.
- The behaviour of the iOS app without the entitlement has not been measured.

## Stages

| Stage | Files | State |
|---|---|---|
| 0 docs | this plan, `docs/runbooks/verifying-settings-sync.md`, `docs/HANDOFF.md`, `CLAUDE.md` | being built |
| 1 core | `pessimal_client_core/src/settings_sync.rs`, `tests/settings_sync_laws.rs`, `tests/settings_sync_simulation.rs` | being built |
| 2 FFI | `pessimal_ffi/src/settings_sync_records.rs`, regenerated bindings | being built |
| 3 Swift platform | `SettingsMailbox.swift`, `SettingsReplicaStore.swift`, `PlatformStores.swift`, `SettingsStore.swift` (comments) | being built |
| 4 Swift composition | `SettingsSync.swift`, both `PessimalServices.swift`, `PessimalApp.swift` and `SettingsView.swift`, `scripts/swift-smoke.sh` | being built |
| 5 iOS signing | distribution entitlements file, `clients/apple/ios/BUILD.bazel`, `signing-diagnostics.py`, the TestFlight script | waits for owner steps 2-3 |
| 6 macOS signing | `Signing/Pessimal.entitlements`, `Signing/embedded.provisionprofile`, `notarize-macos-app.sh`, `ci-check-macos-bundle.sh` | waits for owner steps 4-7 |

Merge order: stages 0-4 add no entitlement, and without it the apps behave as today, so they may
merge before the owner steps. Stages 5-6 merge after owner steps 2-7. If the spike fails, only the
Mac mailbox adapter changes.

## Verification

1. `cargo test -p pessimal_client_core --test settings_sync_laws`, `--test settings_sync_simulation`,
   and `--lib settings_sync`.
2. `cargo clippy -p pessimal_client_core --all-targets -- -D warnings`, the same for `pessimal_ffi`,
   and `cargo fmt --all -- --check`.
3. `git diff --exit-code Cargo.lock MODULE.bazel.lock clients/common/pessimal_client_core/Cargo.toml`.
4. `cargo test -p pessimal_ffi settings_sync`, `tools/uniffi/regen.sh`, `scripts/ci-check-bindings.sh`,
   and the Swift name collision check in HANDOFF.
5. `scripts/swift-smoke.sh`: two in-memory devices through the real FFI converge.
6. Stages 5-6 only: the unsigned macOS bundle claims no entitlements; the decoded profiles authorise
   the kvstore identifier; the TestFlight and notarize scripts assert the signed entitlements.
7. Real hardware only: [`../runbooks/verifying-settings-sync.md`](../runbooks/verifying-settings-sync.md).

## Owner steps

Step 0 is a decision. Steps 1-5 and 10 are in the Apple Developer portal and need the Account Holder
or an Admin. Steps 7 and 9 need a physical Mac and an iPhone. No step asks you to run a command.

0. **Product calls.** Accept or overrule each; each is a one-line change. (a) The backend URL and API
   key stay per device. (b) The poll interval syncs. (c) Deleting a rule beats a concurrent edit.
   (d) The usage opt-out stays per device. (e) Tombstones are kept forever in format 1.
1. **Only if step 2 will not save without a container:** Certificates, Identifiers & Profiles >
   Identifiers > + > iCloud Containers > `iCloud.com.lightless-labs.pessimal`. A container can never
   be deleted or renamed, so do not create one in advance
   (<https://developer.apple.com/help/account/identifiers/create-an-icloud-container>).
2. Identifiers > App IDs > `com.lightless-labs.pessimal.ios` > Edit > tick iCloud > "Compatible with
   Xcode 5" (key-value storage, no CloudKit) > Save > Confirm
   (<https://developer.apple.com/help/account/identifiers/enable-app-capabilities>). The App Store
   profile is now invalid, and a push to main that releases fails at `scripts/asc.py` ("is INVALID,
   not ACTIVE") until step 3.
3. Straight after step 2: Profiles > `com.lightless-labs.pessimal.ios` (App Store) > Edit > Generate.
   Keep that exact name. Download it. Any push to main between steps 2 and 3 needs `[skip release]`
   on its subject line.
4. Identifiers > + > App IDs > App > explicit bundle ID `com.lightless-labs.pessimal.macos`, platform
   macOS (edit it if it exists) > tick iCloud > "Compatible with Xcode 5" > Register or Save.
5. Profiles > + > Distribution > Developer ID > App ID `com.lightless-labs.pessimal.macos` > the
   Developer ID Application certificate whose `.p12` is in Doppler `prd_macos_notarisation` > name
   `com.lightless-labs.pessimal.macos Developer ID` > Generate > Download. If the portal lists more
   than one such certificate, ask which one the release uses before you choose.
6. Hand over both profile files. They are decoded for you, and the result decides the next step. If
   the macOS profile authorises `PKPPLFK854.*` or `PKPPLFK854.com.lightless-labs.pessimal.ios`, go on.
   If it authorises only `PKPPLFK854.com.lightless-labs.pessimal.macos`, the apps cannot share one
   store: stop and take the CloudKit fallback.
7. **Gating spike**, on a physical Mac (not the Tart mini) and an iPhone, both signed in to one Apple
   Account with iCloud on. You receive an uncommitted spike build with a debug item that writes and
   shows a counter under key `pessimal.spike`. The Mac build is Developer ID signed and notarized, and
   its entitlements are checked before you get it; download it in a browser and open it. The iPhone
   build uses a development entitlements file and the explicit profile
   `iOS Team Provisioning Profile: com.lightless-labs.pessimal.ios`, installed from a Mac paired with
   the phone. **Pass** means all of:
   - the Mac build shows that `synchronize()` returned true;
   - a counter written on each device appears on the other within 5 minutes, in both directions,
     three times, one of them after quitting and reopening both apps.

   Also note whether it works without App Sandbox. If it fails, send the Console output for
   Pessimal; the Mac side then moves to the CloudKit fallback before stage 6 merges.
8. After the spike passes, the Developer ID profile is committed as
   `clients/apple/macos/Signing/embedded.provisionprofile`. The iOS app gets sync on the push to main
   that lands stage 5; the menu bar app on the first `v*` release after stage 6.
9. Before the first release that ships sync, and before any release that changes
   `settings_sync.rs`, run [`../runbooks/verifying-settings-sync.md`](../runbooks/verifying-settings-sync.md).
10. Whenever the Developer ID Application certificate is replaced, repeat step 5 and hand over the new
    profile before the next `v*` tag. An embedded profile that does not list the signing certificate
    stops the app from launching (<https://developer.apple.com/support/developer-id/>).

## CloudKit fallback

Not built. Follow it only if step 6 or step 7 fails.

- (a) Register container `iCloud.com.lightless-labs.pessimal`.
- (b) Switch both App IDs to iCloud with "Include CloudKit support", assign the container, and
  regenerate both profiles. Use `[skip release]` between the switch and the regeneration.
- (c) In both entitlements files, add `com.apple.developer.icloud-services = [CloudKit]`,
  `com.apple.developer.icloud-container-identifiers = [iCloud.com.lightless-labs.pessimal]` and
  `com.apple.developer.icloud-container-environment = Production`. Remove the kvstore key.
- (d) In CloudKit Console, create record type `SettingsDocument` with one field, `document`
  (Encrypted Bytes), in Development. Deploy it to Production before the entitlements commit reaches
  main. Production fields can never be deleted
  (<https://developer.apple.com/documentation/cloudkit/deploying-an-icloud-container-s-schema>).
- (e) Write `CloudKitSettingsMailbox`, implementing `SettingsMailbox` with one record in the private
  database under a fixed `recordName` (a UUIDv7 constant). Save with `.ifServerRecordUnchanged`, with
  no `CKSyncEngine` and no push. Fetch on launch, foreground and Refresh Now. On
  `serverRecordChanged`, run the step against the server copy.
- (f) Create `CKContainer` only when an Info.plist flag is set, because it raises when the entitlement
  is missing.

The Rust module, the FFI and the views do not change.

## Risks

- Apple states the Developer ID answer both ways. Steps 6 and 7 settle it before any signing change.
- A custom iOS entitlements file replaces the profile's set and could drop `beta-reports-active` or
  `keychain-access-groups`. Stage 5 copies them from the shipped IPA and asserts them before upload.
- Two devices configured separately both bootstrap at `[0,0]`. Their rules are unioned, so the same
  rule made on both shows twice, and a conflicting environment or interval goes to the larger bytes.
  The user deletes the duplicates once.
- Concurrent edits of one key within sync latency are ordered by device clocks; one is lost.
- An account change rebuilds the replica at `[0,0]`, so the new account's data wins.
- Tombstones are never removed in format 1. Years of churn could reach the budget; publishing then
  stops and the status line says so.
- An older Mac build meeting a newer format pauses until it updates.
- Sync is eventual: seconds to minutes. iCloud limits writes to "several times per minute"
  (<https://developer.apple.com/documentation/foundation/nsubiquitouskeyvaluestore/synchronize()>).
- The key-value store is unencrypted on disk. Environment names, rule names and host ids reach
  iCloud. No secret is written there.
- Development builds never sync. Sync is only exercised on TestFlight and notarized builds, or a spike
  build.

## Out of scope

- Syncing the backend URL, the API key, the usage opt-out, `fleet-state.json`, focus, metric sets,
  the liveness policy or the other `PollTuning` fields.
- CloudKit, except as the fallback above. iCloud Drive documents.
- A rule editor, an enable toggle, field-level merge inside a rule, tombstone collection, a conflict
  review screen, or a "Reset iCloud copy" button.
- Background refresh or silent push on iOS.
- New dependencies, or the `serde_json` features `float_roundtrip` and `preserve_order`.
- Automated iCloud tests in CI. CI guests cannot stay signed in to iCloud (see the runbook).
