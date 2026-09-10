# The iOS TestFlight upload has never been run

The portal prerequisites now exist and the Buildkite release path is wired. What is recorded below is
what has actually been observed, job by job; the upload itself is the last unproven step.

## What the pipeline has proven

Buildkite pipeline `la-bande-a-bonnot/pessimal`, self-hosted Apple silicon, tart-ci v0.2.4:

- **Build #4 green end to end.** Rust workspace (fmt, clippy, `cargo test --locked`) on the Linux
  guest; the iOS app through Bazel with the uniffi symbols asserted in the bundle; the macOS menu bar
  app plus the Swift smoke test.
- The webhook fires on push, the concurrency group holds the macOS work to one guest at a time, and
  tag builds correctly skip every non-release step.
- GitHub Actions stayed green across the same commits, so `--define ci=true` and the raised CI memory
  did not cost the hosted matrix anything.

Three failures worth not repeating are recorded in
[`docs/solutions/a-green-bazel-job-says-nothing-about-cargo.md`](../docs/solutions/a-green-bazel-job-says-nothing-about-cargo.md)
and in the commit log: a transient upstream 500 on a cold repository cache (now retried), cargo
refusing the workspace for being below MSRV while Bazel passed on the same guest, and `rbenv init -
bash` killing a job under the guest's zsh.

## What is verified locally

- `--config=ci` builds an unsigned simulator ipa, and `--embed_label=0.1.0.77` reaches
  `CFBundleVersion` through `apple_bundle_version`.
- `--config=beta --config=ios_device` resolves `:distribution_profile` (confirmed by `bazel cquery`)
  and fails at *execution* with `no provisioning profile was found named
  'com.lightless-labs.pessimal.ios'` — which is the correct failure for a machine with no profile
  installed, and is the gap `fastlane sigh` fills.
- All four lanes load under `bundle exec fastlane lanes`.

## The portal prerequisites, done

All three were set up on 2026-09-10:

1. `com.lightless-labs.pessimal.ios` registered as an **App ID**.
2. An **App Store distribution** profile whose portal *Name* is exactly
   `com.lightless-labs.pessimal.ios`. `local_provisioning_profile` matches the Name field, not the
   filename, and the string has to agree in three places — the portal, `profile_name` in
   `clients/apple/ios/BUILD.bazel`, and `IOS_APPS[:pessimal][:profile_name]` in `fastlane/Fastfile`.
3. The app record in App Store Connect.

The hand-off between fastlane and Bazel rests on one thing that is easy to miss: `sigh` installs the
downloaded profile into `~/Library/MobileDevice/Provisioning Profiles` unless `skip_install` is set,
and that directory is the only place Bazel's finder looks. Do not add `skip_install`.

Then, from Doppler project `lightless-labs-pessimal`:

```sh
doppler run -p lightless-labs-pessimal -c prd_ios_deployment -- \
  bundle exec fastlane pessimal_beta_testflight version:0.1.0 build_number:1
```

The lane creates its own keychain for the distribution certificate and deletes it on the way out, so
this needs nothing installed in the login keychain and leaves nothing behind. It does need the Apple
WWDR intermediate certificate, which macOS usually already has; if codesign reports an untrusted
identity, that is the missing piece and `security import AppleWWDRCA.cer` is the fix.

## Unverified beyond that

- **The upload itself.** Everything up to it now runs on the guest; whether App Store Connect accepts
  the build is the one thing no local check can answer.
- **Every push to `main` spawns macOS jobs that queue against a release** for the single macOS slot,
  including docs-only commits. A dynamic upload script that skips the Apple jobs when no Swift, Rust
  or Bazel file changed would pay for itself; Pocket Companion's `upload-pipeline.sh` is the shape.
- **Privacy manifest.** `PessimalKit`'s `UserDefaultsSettingsStore` uses `UserDefaults`, a
  required-reason API. Neither kumbaya nor phil-connors ships a `PrivacyInfo.xcprivacy`, so the band's
  convention is to go without — but if the first upload is rejected with **ITMS-91053**, that is why,
  and the fix is a manifest declaring reason `CA92.1`.
- `bazel run //:xcodeproj` is defined and still never executed. `:xcode_profile` exists for it.
