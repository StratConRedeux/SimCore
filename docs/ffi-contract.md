# FFI contract (ABI version 1, header revision 1.3)

The only interface between the Rust engine and the iOS app. The app does no game logic: it reads flat structs and sends commands.

## Sources of truth
- `crates/abi/src/lib.rs`: `#[repr(C)]` types and constants, with compile-time size and offset checks.
- `include/simcore.h`: the same layout for C and Swift. Keep the two identical. Changing an existing layout or signature bumps `ABI_VERSION`; additions (new types and functions) bump only the header revision.

## Rules
- **Coordinates.** The engine stores `f64` metres in a local east-north-up world frame. Frames carry `f32` metres relative to the player's command node (floating origin), so values near the player stay precise. Velocities are world-frame m/s, not relative to the node's motion. Commands and setup calls take positions in the same node-relative frame.
- **Time.** Fixed ticks at 15 per second. The engine never reads the wall clock; the caller decides how many ticks to run.
- **Determinism.** Same seed and same inputs give the same state, on the same build. Single-threaded schedule, seeded RNG, no hash-map iteration order in simulation logic.
- **Memory ownership.** The engine owns all frame and snapshot memory. `ScFrame.units` is valid until the next `sc_world_frame` or `sc_world_destroy`; `ScBytes.data` from `sc_world_save` until the next `sc_world_save` or `sc_world_destroy`. The app copies what it needs to keep.
- **Order.** Frame order is unspecified and changes after a restore. Identify units by `id`.
- **Commands and setup.** The app sends `ScCommand`s; it never moves units itself. It sets rules and spawns units with the setup calls (revision 1.3). Rejections return 0 or `false`, not an error code: the app checks its own input and treats them as "not accepted".
- **Threads.** One thread at a time per `ScWorld`. The app calls from a single serial queue.
- **Errors.** Constructors return `NULL` on failure. No panic may cross the boundary: every exported function catches unwinding, and release builds use `panic = "abort"`.
- **No allocation per frame** once the unit count is stable. The frame buffer is reused.

## Revision history
- **1.3** (additive): game setup. `ScRules` and `ScOutcome` with `sc_world_set_rules`; `ScLinkRules` with `sc_world_set_link_rules`; `ScTerrain` with `sc_world_set_terrain`; `ScUnitSpec` and `SC_SPAWN_*` with `sc_world_spawn`; `ScSensor` with `sc_world_add_sensor`. These return `bool` (`<stdbool.h>`) or an id, 0 if rejected, and change nothing when they reject. Setup coordinates are relative to the command node, like commands. Spawned ids stop at `2^24 - 1`, so every world can be saved and restored.
- **1.2** (additive; no existing layout or signature changed): `ScCommand` and `SC_COMMAND_*` with `sc_world_command` (move, halt, retarget, emissions) and `sc_world_link_delay`; `ScStatus` with `sc_world_status`; `sc_world_despawn_removed`; `ScBytes` with `sc_world_save` and `sc_world_restore`. `ScFrame.units` is documented as valid until the next `sc_world_frame` (stepping does not touch the buffer), and frame order as unspecified.
- **1.1** (no layout change): typed `static const` constants instead of `#define`s; nullability annotations (`_Nullable` on constructor returns, `sc_world_destroy`'s argument and `ScFrame.units`, everything else non-null); `SC_FLAG_DETECTED` (bit 2) added; documented that `sc_world_create_dummy(seed, UINT32_MAX)` returns NULL.
- **1.0**: initial contract.

## Not yet in the contract (later phases)
Standing orders (SOPs) and strikes; engagement events for the Dynamic Island and Live Activities; a coarse fast-forward mode for long absences.

The snapshot is a hand-written format, not FlatBuffers as first planned: its bytes never cross a language boundary (the app stores them opaquely), so FlatBuffers' cross-language zero-copy reading would buy nothing, and a dependency-free reader with a checksum and bounds checks was simpler to make robust against damaged files.
