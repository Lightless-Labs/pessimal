---
status: pending
priority: p2
issue_id: "009"
tags: [ios, settings, usability]
dependencies: []
---

# One button in the iOS settings toolbar: Test, then Save

## Problem Statement

The owner set up a second iPhone, ran Test Connection, saw it succeed, left the screen, and then
wondered why no hosts appeared. Nothing was wrong with the configuration: it had never been saved.

A successful test reads as success. It is the loudest, most reassuring thing on the screen — a
report of a real round trip to the backend — and it is not the action that makes the settings take
effect. Save is a button at the bottom of a scrolling form, below the alert rules, out of sight at
the moment the test result appears.

## Findings

- `clients/apple/ios/Sources/Views/SettingsView.swift` has both actions well below the fold:
  Test Connection sits in the backend section (around :173), Save and Discard Changes in the last
  section (around :395 and :410).
- The screen already tracks everything the request needs:
  - `probeState: SettingsProbeState` — `.idle`, `.running`, `.finished(record)`, `.failed(message)`.
  - `hasUnsavedChanges`, from comparing `draft` against `committed`.
  - `canSave(candidate)` (:437), which requires unsaved changes, no save in flight, and a candidate
    configuration core accepted.
  - `probeState` is already reset to `.idle` when the connection changes (:596, :648), which is
    exactly the "until the config is changed" rule the request asks for.
- The footer already says "nothing here takes effect until it is saved" (:428). It did not help. A
  sentence under a button does not compete with a green tick above it.
- The macOS settings window has the same two actions and the same shape, so whatever is decided here
  should be considered for it, though a window shows more at once and the report is inline.

## Proposed Solutions

1. **One toolbar button that changes what it offers (the request).** Top trailing of the navigation
   bar. It reads **Test** while the draft has not been tested, and **Save** once a test has
   succeeded and nothing has changed since. Disabled exactly as the existing buttons are: Test is
   disabled until the required fields are filled and no probe is running; Save is disabled unless
   `canSave` is true. Any edit resets it to Test, because `probeState` already resets.
2. **Both buttons in the toolbar.** No mode, no state to explain — but two small buttons in a
   navigation bar, one of which is usually disabled.
3. **Warn on leaving with unsaved changes.** Solves the reported failure directly, but adds a
   dialog to every visit, including the ones where the user meant to leave.
4. **Make the test report say it.** The probe report gains a line: "Not saved yet." Cheapest, and
   the least likely to be read, since the whole problem is that a success report stops people
   reading.

## Recommended Action

Solution 1, as asked, with solution 4's line inside the probe report as well, since it costs one
`Text` and covers the user who never looks at the toolbar.

Open question for the owner: what should the toolbar read when a test has **failed**? Staying on
Test invites a retry, which is probably right — but a configuration can be perfectly saveable while
the backend is unreachable, and Save must stay reachable in that case rather than being hidden
behind a test that cannot pass.

## Technical Details

- The button lives in `.toolbar { ToolbarItem(placement: .topBarTrailing) { … } }` on the existing
  `Form`.
- Mode is derived, never stored: a second source of truth for "has this been tested" would be a
  second thing that can disagree with `probeState`.
- "Required config has been filled" must mean what the rest of the screen means by it. Do not invent
  a new rule: derive it from the same `candidate` that `canSave` uses, so the toolbar cannot say a
  configuration is testable while the form says it is not saveable.
- A save leaves the screen showing a saved draft, so the mode after a successful save is Test again
  with nothing to do — check it does not read as an invitation to test something already applied.

## Acceptance Criteria

- The toolbar button reads Test until a test succeeds, then Save, and returns to Test on any edit.
- Disabled states match the existing buttons exactly, including while a probe or a save is running.
- A successful test followed by leaving the screen without saving is not possible without the user
  being told, either by the toolbar or by the probe report.
- The macOS settings window is considered, and either changed the same way or explicitly left alone
  with the reason recorded.

## Work Log

- 2026-09-16: raised by the owner after a second iPhone was configured, tested successfully, and
  never saved.

## Resources

- `clients/apple/ios/Sources/Views/SettingsView.swift`
- `clients/apple/ios/Sources/Views/SettingsProbeReportView.swift`
- `clients/apple/macos/Sources/Views/SettingsView.swift`
