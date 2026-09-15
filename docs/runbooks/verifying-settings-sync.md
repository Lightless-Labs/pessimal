# Verifying settings sync

A manual check that settings sync between two devices on one Apple Account. It runs on real hardware
only. The design is in
[`../plans/2026-09-14-m9-icloud-settings-sync.md`](../plans/2026-09-14-m9-icloud-settings-sync.md).

Run it before the first release that ships sync, and before any release that changes
`clients/common/pessimal_client_core/src/settings_sync.rs`.

## Why CI cannot run it

Apple supports iCloud in macOS 15 virtual machines, but ties it to the VM's identity. A copy of a VM
is a clone. A clone gets a new identity when it starts while another clone of the same VM is running,
and a VM gets a new identity when it moves to another Mac and restarts. After either change, iCloud
needs a new sign-in
(<https://developer.apple.com/documentation/virtualization/using-icloud-with-macos-virtual-machines>).
Buildkite's Tart guests are fresh clones with no Apple Account signed in, so CI cannot run this check.
The development mini is itself a Tart guest.

## Before you start

- A **physical Mac**, not the Tart mini, and an **iPhone**. Both are signed in to **one Apple
  Account** with iCloud turned on.
- On the Mac: `Pessimal-<version>-macos.zip` from the GitHub release, downloaded in a browser and
  opened. On the iPhone: the TestFlight build from the same commit or later.
- Both builds include stages 5 and 6 of the plan. Development builds have no iCloud entitlement and
  never sync.
- Both apps are connected to the same backend. The backend URL and API key do not sync, so enter them
  on each device.
- Open Settings on each device and read the status line at the foot of the screen. It must not say
  iCloud is unavailable. If it does, stop: check that the device is signed in to iCloud, then report
  the build number.

Sync can take from seconds to a few minutes. In each check, if a change has not arrived, press
**Refresh Now** in the Mac menu or **Refresh** on the iPhone, and wait up to 5 minutes before you
call it a failure.

## 1. A rule added on the iPhone appears on the Mac

1. On the iPhone, open Settings, add an alert rule named `sync check 1`, and tap **Save**.
2. On the Mac, press **Refresh Now** in the menu.
3. On the Mac, open **Settings…**.

**Pass:** `sync check 1` is in the Mac's alert rules, with the same metric and threshold.

## 2. An offline delete and an edit reach the same result

1. Make sure both devices list `sync check 1`.
2. On the iPhone, turn on **Airplane Mode**.
3. On the Mac, delete `sync check 1` and press **Save**.
4. On the iPhone, change the environment to a new name, for example `sync-check-2`, and tap **Save**.
5. On the iPhone, turn off Airplane Mode and bring Pessimal to the foreground. Tap **Refresh**.
6. On the Mac, press **Refresh Now**. Close and reopen **Settings…** on both devices.

**Pass**, on both devices:

- `sync check 1` is absent. It stays absent after another Refresh on each device.
- The environment is `sync-check-2`. The Mac menu header shows it too.
- No other rule and no poll interval changed.

## 3. An open Settings window does not undo a remote change

The Mac window must hold an unsaved edit when the iPhone's change arrives. A window with an unsaved
edit does not reload the stored settings, so it still shows the old environment when you press Save.
That Save must keep the iPhone's environment.

1. On the Mac, open **Settings…** and change the poll interval. Do not press **Save**.
2. On the iPhone, change the environment to `sync-check-3` and tap **Save**.
3. On the Mac, press **Refresh Now** in the menu. Wait until the menu header shows `sync-check-3`.
   Do not close the Settings window.
4. Confirm that the open Mac Settings window still shows the old environment and your new poll
   interval. If it does not, stop and record this check as failed.
5. In that window, press **Save**.
6. On the Mac, close and reopen **Settings…**. On the iPhone, tap **Refresh** and reopen Settings.

**Pass**, on both devices: the environment is `sync-check-3`, and the poll interval is the value set
on the Mac.

## 4. Two iPhones

Repeat checks 1 to 3 with two iPhones on the same Apple Account, in place of the Mac and the iPhone.

## Clean up

Delete any `sync check` rules that are left, set the environment and poll interval back to what they
were, and save.

## Record the result

Add the date, the Mac release version, the TestFlight build numbers, and pass or fail for each check to
the M9 entry in [`../HANDOFF.md`](../HANDOFF.md). For a failure, also record the status line text on
each device.
