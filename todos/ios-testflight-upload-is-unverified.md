# The iOS TestFlight upload has never been run

The Bazel and fastlane wiring is in place and verified as far as this machine can verify it. What is
left needs the Apple Developer portal and a real upload, neither of which can be faked locally.

## What is verified

- `--config=ci` builds an unsigned simulator ipa, and `--embed_label=0.1.0.77` reaches
  `CFBundleVersion` through `apple_bundle_version`.
- `--config=beta --config=ios_device` resolves `:distribution_profile` (confirmed by `bazel cquery`)
  and fails at *execution* with `no provisioning profile was found named
  'com.lightless-labs.pessimal.ios'` — which is the correct failure for a machine with no profile
  installed, and is the gap `fastlane sigh` fills.
- All four lanes load under `bundle exec fastlane lanes`.

## What the portal needs first

`get_provisioning_profile(readonly: true)` downloads; it never creates. So before the first run:

1. `com.lightless-labs.pessimal.ios` registered as an **App ID**.
2. An **App Store distribution** provisioning profile whose portal *Name* is exactly
   `com.lightless-labs.pessimal.ios`. `local_provisioning_profile` matches the Name field, not the
   filename, and the string has to agree in three places — the portal, `profile_name` in
   `clients/apple/ios/BUILD.bazel`, and `IOS_APPS[:pessimal][:profile_name]` in `fastlane/Fastfile`.
3. The app record created in App Store Connect, or `upload_to_testflight` has nothing to upload to.

Then, from Doppler project `lightless-labs-pessimal`:

```sh
doppler run -p lightless-labs-pessimal -c prd_ios_deployment -- \
  bundle exec fastlane pessimal_beta_testflight version:0.1.0 build_number:1
```

## Unverified beyond that

- **No CI workflow drives this yet.** Pocket Companion's `pocket-companion-beta.yml` is the model:
  `ruby/setup-ruby` pinned to the `.ruby-version`, Doppler injection, then the lane. It is not
  written, because a workflow that has never succeeded is worth less than the manual run that proves
  the lane first.
- **Privacy manifest.** `PessimalKit`'s `UserDefaultsSettingsStore` uses `UserDefaults`, a
  required-reason API. Neither kumbaya nor phil-connors ships a `PrivacyInfo.xcprivacy`, so the band's
  convention is to go without — but if the first upload is rejected with **ITMS-91053**, that is why,
  and the fix is a manifest declaring reason `CA92.1`.
- `bazel run //:xcodeproj` is defined and still never executed. `:xcode_profile` exists for it.
