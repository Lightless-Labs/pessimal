# Run uniffi-bindgen as a Bazel genrule

Generated Swift bindings are currently produced by `tools/uniffi/regen.sh` and checked in, with CI
verifying they are not stale. That matches how phil-connors and kumbaya do it, and it keeps the
Xcode-side build simple.

The better end state is a Bazel genrule that runs `//tools/uniffi:pessimal-uniffi-bindgen` over the
built cdylib and feeds the outputs straight into `swift_library` / `objc_library`, so the bindings
cannot go stale at all. Neither sibling project has done this — the monorepo's `tools/uniffi/`
is still a stub — so it needs working out from scratch.

Not blocking. Revisit once M4 is landed and the binding surface has stopped moving.
