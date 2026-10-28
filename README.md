# SimCore

A deterministic, data-oriented simulation engine in Rust (`bevy_ecs`), with a C API and Swift bindings for iOS.

- `crates/abi`: plain-data types shared across the C boundary.
- `crates/engine`: the fixed-tick ECS simulation.
- `crates/ffi`: the C API (`include/simcore.h`).
- `bindings/swift`: `SimCoreKit`, a Swift package wrapping the XCFramework built by `scripts/build-xcframework.sh`.
- `docs/ffi-contract.md`: the rules for the boundary.

Releases are tagged with semantic versions. Consumers should pin to a tag, not a branch.

Released under the MIT License. See `LICENSE`.
