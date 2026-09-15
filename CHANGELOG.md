# Changelog
All notable changes to this project will be documented in this file. See [conventional commits](https://www.conventionalcommits.org/) for commit guidelines.

- - -
## v0.1.0 - 2026-09-15
#### Features
- (**agent**) install as a launchd service, and document running it - (aa06568) - El-Fitz, *Claude Opus 5*
- (**agent**) host telemetry agent exporting OTLP - (deaad25) - El-Fitz, *Claude Opus 5 (1M context)*
- (**apps**) sync settings through iCloud key-value storage - (9d54805) - El-Fitz, *Claude Opus 5*
- (**apps**) show the bytes under the percentage on both clients - (c020c72) - El-Fitz, *Claude Opus 5*
- (**client**) a deterministic merge for synced settings - (f6afea4) - El-Fitz, *Claude Opus 5*
- (**client**) derive used-of-total bytes for memory and every filesystem - (68b2271) - El-Fitz, *Claude Opus 5*
- (**clients**) an opt-out switch, and an honest list of what is sent - (1f6cb0e) - El-Fitz, *Claude Opus 5 (1M context)*
- (**clients**) the iOS app, built end to end by Bazel - (a323ae5) - El-Fitz, *Claude Opus 5 (1M context)*
- (**clients**) the macOS menu bar app - (880b3af) - El-Fitz, *Claude Opus 5 (1M context)*
- (**clients**) implement the pessimal_ffi UniFFI bridge - (25fd273) - El-Fitz, *Claude Opus 5 (1M context)*
- (**clients**) scaffold pessimal_ffi and verify the tokio reactor from Swift - (2bc8c68) - El-Fitz, *Claude Opus 5 (1M context)*
- (**clients**) implement pessimal_client_core as plan-gather-fold - (3746666) - El-Fitz, *Claude Opus 5 (1M context)*
- (**clients**) SigNoz query adapter - (2747c41) - El-Fitz, *Claude Opus 5 (1M context)*
- (**core**) metric model, liveness policy, and alert evaluation - (3145c44) - El-Fitz, *Claude Opus 5 (1M context)*
- (**ffi**) expose the settings sync step and record_edits - (8c827dc) - El-Fitz, *Claude Opus 5*
- (**ffi**) report one span per poll, batched, and never on the poll's path - (3aa5ad1) - El-Fitz, *Claude Opus 5 (1M context)*
- (**ios**) give the app an icon, and satisfy the rest of App Store validation - (71b5bd3) - El-Fitz, *Claude Opus 5 (1M context)*
- (**ios**) sign and version the iOS app for distribution - (c454220) - El-Fitz, *Claude Opus 5 (1M context)*
- (**release**) print both sides of the signing identity match before building - (a0026ae) - El-Fitz, *Claude Opus 5 (1M context)*
- (**release**) talk to App Store Connect directly instead of through sigh - (7054be5) - El-Fitz, *Claude Opus 5 (1M context)*
- (**release**) read the iOS secrets from Doppler on the guest, and trust the identity - (f0510b8) - El-Fitz, *Claude Opus 5 (1M context)*
- (**release**) add the fastlane lanes that sign and ship the iOS app - (368e837) - El-Fitz, *Claude Opus 5 (1M context)*
- (**release**) wire macOS signing and notarization to Doppler - (ae5363f) - El-Fitz, *Claude Opus 5 (1M context)*
- (**usage**) encode and ship Pessimal's own traces, with the allowlist in the types - (4ddd371) - El-Fitz, *Claude Opus 5 (1M context)*
#### Bug Fixes
- (**agent**) an empty PESSIMAL_* override counts as unset - (a07eb97) - El-Fitz, *Claude Opus 5*
- (**agent**) supply TLS roots, or gRPC export fails against every cloud backend - (82ec9ce) - El-Fitz, *Claude Opus 5 (1M context)*
- (**agent**) accept the same spellings in config files and env overrides - (57b3e60) - El-Fitz, *Claude Opus 5 (1M context)*
- (**build**) use the bzlmod repo name for the apple_support constraints - (26431e1) - El-Fitz, *Claude Opus 5 (1M context)*
- (**ci**) honour the release opt-out on the subject line only - (5e25f76) - El-Fitz, *Claude Opus 5 (1M context)*
- (**ci**) the release gate never fired; cut it back to conditions that work - (9cc176c) - El-Fitz, *Claude Opus 5 (1M context)*
- (**ci**) forward BUILDKITE_BUILD_NUMBER into the release guest - (2126a6f) - El-Fitz, *Claude Opus 5 (1M context)*
- (**ci**) select a Ruby the guest actually has, rather than the one .ruby-version names - (e27d96c) - El-Fitz, *Claude Opus 5 (1M context)*
- (**ci**) put rbenv's shims on PATH instead of sourcing its bash init - (f337f13) - El-Fitz, *Claude Opus 5 (1M context)*
- (**ci**) only install a toolchain when the guest's is behind MSRV - (443d0cb) - El-Fitz, *Claude Opus 5 (1M context)*
- (**clients**) stop a lagging backend silently disabling every alert - (0521efb) - El-Fitz, *Claude Opus 5 (1M context)*
- (**clients**) pick a TLS stack the clients can actually ship everywhere - (ab5193f) - El-Fitz, *Claude Opus 5 (1M context)*
- (**clients**) budget for the backend's ingestion lag - (b9ccb69) - El-Fitz, *Claude Opus 5 (1M context)*
- (**clients**) query each metric with the temporality SigNoz stored it under - (76aecfd) - El-Fitz, *Claude Opus 5 (1M context)*
- (**clients**) parse the real SigNoz response envelope, not the inner half - (59852d2) - El-Fitz, *Claude Opus 5 (1M context)*
- (**clients**) break dominant_at ties by attributes, not by response order - (4df3074) - El-Fitz, *Claude Opus 5 (1M context)*
- (**clients**) time SigNoz queries out, and tie the step to the liveness policy - (fdad897) - El-Fitz, *Claude Opus 5 (1M context)*
- (**clients**) stop the SigNoz API key reaching logs or another host - (d0c9928) - El-Fitz, *Claude Opus 5 (1M context)*
- (**clients**) query hosts at the heartbeat interval, not the window length - (200adaf) - El-Fitz, *Claude Opus 5 (1M context)*
- (**core**) hold deserialisation to the same invariants the constructors enforce - (572b418) - El-Fitz, *Claude Opus 5 (1M context)*
- (**core**) stop alerts firing on samples nobody is refreshing - (a65d4ce) - El-Fitz, *Claude Opus 5 (1M context)*
- (**release**) give release-cut a git identity cog can read - (b3527a4) - El-Fitz, *Claude Opus 5*
- (**release**) name the script that signs the macOS release [skip release] - (767721f) - El-Fitz, *Claude Opus 5*
- (**release**) put the signing keychain where the Bazel sandbox can read it - (afdd034) - El-Fitz, *Claude Opus 5 (1M context)*
- (**release**) stop asking the vault for a team ID it does not hold - (6f683be) - El-Fitz, *Claude Opus 5 (1M context)*
- (**release**) leave the host's keychains exactly as the lane found them - (8ccae6e) - El-Fitz, *Claude Opus 5 (1M context)*
- (**release**) create the keychain the signing lane imports into - (ca73af8) - El-Fitz, *Claude Opus 5 (1M context)*
- (**usage**) strip a signal path the endpoint already carries - (189f93b) - El-Fitz, *Claude Opus 5 (1M context)*
- stop reachable durations panicking in chrono arithmetic - (76cc61b) - El-Fitz, *Claude Opus 5 (1M context)*
#### Documentation
- (**plans**) plan M8, opt-out usage reporting to our own backend - (0d27166) - El-Fitz, *Claude Opus 5 (1M context)*
- (**plans**) design pessimal_client_core as plan-gather-fold - (9a7d7fc) - El-Fitz, *Claude Opus 5 (1M context)*
- (**plans**) record the SigNoz contract and what still blocks M3 - (6e1024d) - El-Fitz, *Claude Opus 5 (1M context)*
- (**solutions**) record the measured backend ingestion lag and what it breaks - (b1f1f2e) - El-Fitz, *Claude Opus 5 (1M context)*
- (**todo**) replace the iOS signing todo with what is actually left - (139b540) - El-Fitz, *Claude Opus 5 (1M context)*
- (**todos**) record the client platform list and what does not match it yet - (213b98a) - El-Fitz, *Claude Opus 5 (1M context)*
- (**todos**) correct the record — Bazel works, and provides Rust 1.95 - (887cc20) - El-Fitz, *Claude Opus 5 (1M context)*
- (**todos**) record that Bazel could not be run, and what was done instead - (e1602ed) - El-Fitz, *Claude Opus 5 (1M context)*
- (**todos**) file the alert-evidence gap the lag fix leaves behind - (08bc5ca) - El-Fitz, *Claude Opus 5 (1M context)*
- (**todos**) record the Bazel toolchain constraint before M4 trips over it - (b65136c) - El-Fitz, *Claude Opus 5 (1M context)*
- plan M9, iCloud settings sync - (12da8fd) - El-Fitz, *Claude Opus 5*
- add todo 007 for the TestFlight token exposure [skip release] - (c82b2e2) - El-Fitz, *Claude Opus 5*
- move the older todos to the NNN-status-priority format [skip release] - (1cfa43d) - El-Fitz, *Claude Opus 5*
- add a todo to protect release tags [skip release] - (3dbd7e1) - El-Fitz, *Claude Opus 5*
- fix a paragraph break in HANDOFF [skip release] - (d0de8be) - El-Fitz, *Claude Opus 5*
- TestFlight without fastlane works, and Actions is disabled [skip release] - (d7088fa) - El-Fitz, *Claude Opus 5*
- rewrite the README and guides in plain language - (e673c26) - El-Fitz, *Claude Opus 5*
- plan how releases get distributed, and fix the repository URL - (6466a32) - El-Fitz, *Claude Opus 5*
- M8 is verified live, and the environment does not filter the roster [skip release] - (4b04860) - El-Fitz, *Claude Opus 5*
- the subject-line release opt-out is confirmed working [skip release] - (3862e9d) - El-Fitz, *Claude Opus 5*
- both SigNoz keys are dead, so the M8 live check is blocked [skip release] - (e2dbc32) - El-Fitz, *Claude Opus 5*
- M8 phase 1 is built; the live check is what remains - (adbb355) - El-Fitz, *Claude Opus 5 (1M context)*
- record the M8 spike, and four facts about this machine - (3411ef1) - El-Fitz, *Claude Opus 5 (1M context)*
- Pessimal is on TestFlight - (bc8ee4f) - El-Fitz, *Claude Opus 5 (1M context)*
- the release path is blocked on a certificate decision, not a bug - (ee5136e) - El-Fitz, *Claude Opus 5 (1M context)*
- drop the app-runtime Doppler config that has no consumer - (54e4092) - El-Fitz, *Claude Opus 5 (1M context)*
- record the real Doppler coordinates and the iOS release state - (74af91d) - El-Fitz, *Claude Opus 5 (1M context)*
- make the README true again - (7d0e70c) - El-Fitz, *Claude Opus 5 (1M context)*
- correct the stale Bazel claims and record M6 - (59869ca) - El-Fitz, *Claude Opus 5 (1M context)*
- record what live verification found, and that it is done - (ea8ac27) - El-Fitz, *Claude Opus 5 (1M context)*
- mark M3 complete and record what M4 must not forget - (390de36) - El-Fitz, *Claude Opus 5 (1M context)*
- stop documenting commands that do not exist yet - (172a16a) - El-Fitz, *Claude Opus 5 (1M context)*
#### Tests
- (**client**) check the derived totals against a real backend [skip release] - (8a2d4cb) - El-Fitz, *Claude Opus 5*
- (**clients**) verify the whole read path against a real backend - (5e5a3f2) - El-Fitz, *Claude Opus 5 (1M context)*
- (**ffi**) stop a usage test failing on a chance "500" in timestamps - (526439c) - El-Fitz, *Claude Opus 5*
- (**usage**) a trace probe that distinguishes accepted from stored - (e287468) - El-Fitz, *Claude Opus 5 (1M context)*
#### Build system
- (**ios**) inject the usage destination, and declare what it collects - (5e02535) - El-Fitz, *Claude Opus 5 (1M context)*
- commit MODULE.bazel.lock - (1332e4f) - El-Fitz, *Claude Opus 5 (1M context)*
- Bazel targets for the shared Swift, the iOS app, and the Xcode project - (1b62635) - El-Fitz, *Claude Opus 5 (1M context)*
- add the Bazel workspace, and share the Swift model between the apps - (06ec246) - El-Fitz, *Claude Opus 5 (1M context)*
#### CI/CD
- cut agent releases automatically on main - (72c7480) - El-Fitz, *Claude Opus 5*
- read Doppler through DOPPLER_SERVICE_ACCOUNT_TOKEN everywhere - (c080235) - El-Fitz, *Claude Opus 5*
- use the existing Doppler secret for the release steps [skip release] - (1d03b62) - El-Fitz, *Claude Opus 5*
- replace fastlane and Ruby with a shell TestFlight script - (ad915b4) - El-Fitz, *Claude Opus 5*
- run CI and the agent release on Buildkite, and delete the GitHub Actions workflow - (32a24dc) - El-Fitz, *Claude Opus 5*
- ship every green push to main, and make the release depend on being green - (1ae0888) - El-Fitz, *Claude Opus 5 (1M context)*
- install a toolchain that meets MSRV on the Tart guests - (7910028) - El-Fitz, *Claude Opus 5 (1M context)*
- retry transient fetches and stop capping CI memory at the dev VM's share - (afb2af6) - El-Fitz, *Claude Opus 5 (1M context)*
- add the Buildkite pipeline for the self-hosted mini - (355f25f) - El-Fitz, *Claude Opus 5 (1M context)*
- build the iOS app, and check the Rust is actually linked into it - (96aeb88) - El-Fitz, *Claude Opus 5 (1M context)*
- build the macOS app bundle and assert it is shippable - (e1867ac) - El-Fitz, *Claude Opus 5 (1M context)*
- fail on stale Swift bindings, and drive the boundary from Swift - (3f5eddc) - El-Fitz, *Claude Opus 5 (1M context)*
- assert on the SDK's export result rather than known error strings - (9a0be24) - El-Fitz, *Claude Opus 5 (1M context)*
- build and test on all three agent platforms, with an OTLP smoke test - (6f06d97) - El-Fitz, *Claude Opus 5 (1M context)*
#### Refactoring
- (**usage**) take the credential route the siblings already ship - (de7facb) - El-Fitz, *Claude Opus 5 (1M context)*
#### Chores
- (**clients**) scaffold the pessimal_client_core crate - (2db001a) - El-Fitz, *Claude Opus 5 (1M context)*
- drop the dead reqwest workspace dependency - (9b0216f) - El-Fitz, *Claude Opus 5 (1M context)*
- ignore the report fastlane rewrites on every run - (9f78939) - El-Fitz, *Claude Opus 5 (1M context)*
- license under AGPL-3.0-or-later, matching the repository - (47e434a) - El-Fitz, *Claude Opus 5 (1M context)*
- scaffold Pessimal monorepo - (0ee9804) - El-Fitz, *Claude Opus 5 (1M context)*
#### Style
- (**tools**) backtick UniFFI in the bindgen doc comment - (f3b098c) - El-Fitz, *Claude Opus 5 (1M context)*

- - -

Changelog generated by [cocogitto](https://github.com/cocogitto/cocogitto).