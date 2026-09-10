fastlane documentation
----

# Installation

Make sure you have the latest version of the Xcode command line tools installed:

```sh
xcode-select --install
```

For _fastlane_ installation instructions, see [Installing _fastlane_](https://docs.fastlane.tools/#installing-fastlane)

# Available Actions

## iOS

### ios pessimal_fetch_signing

```sh
[bundle exec] fastlane ios pessimal_fetch_signing
```

Check that the distribution certificate and provisioning profile resolve

### ios pessimal_build

```sh
[bundle exec] fastlane ios pessimal_build
```

Build the Pessimal ipa for a device

### ios pessimal_upload_testflight

```sh
[bundle exec] fastlane ios pessimal_upload_testflight
```

Upload an already-built Pessimal ipa to TestFlight

### ios pessimal_beta_testflight

```sh
[bundle exec] fastlane ios pessimal_beta_testflight
```

Sign, build and upload a Pessimal beta to TestFlight

----

This README.md is auto-generated and will be re-generated every time [_fastlane_](https://fastlane.tools) is run.

More information about _fastlane_ can be found on [fastlane.tools](https://fastlane.tools).

The documentation of _fastlane_ can be found on [docs.fastlane.tools](https://docs.fastlane.tools).
