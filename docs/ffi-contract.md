# FFI contract (ABI version 1, header revision 1.1)

The only interface between the Rust engine and the iOS app. The app does no game logic: it reads flat structs and sends commands.

## Sources of truth
- `crates/abi/src/lib.rs`: `#[repr(C)]` types and constants, with compile-time size and offset checks.
- `include/simcore.h`: the same layout for C and Swift. Keep the two identical; any change bumps `ABI_VERSION`.

## Rules
- **Coordinates.** The engine stores `f64` metres in a local east-north-up world frame. Frames carry `f32` metres relative to the player's command node (floating origin), so values near the player stay precise.
- **Time.** Fixed ticks at 15 per second. The engine never reads the wall clock; the caller decides how many ticks to run.
- **Determinism.** Same seed and same inputs give the same state, on the same build. Single-threaded schedule, seeded RNG, no hash-map iteration order in simulation logic.
- **Memory ownership.** The engine owns all frame memory. `ScFrame.units` is valid until the next mutating call. The app copies what it needs to keep, or reads it within the same frame.
- **Threads.** One thread at a time per `ScWorld`. The app calls from a single serial queue.
- **Errors.** Constructors return `NULL` on failure. No panic may cross the boundary: every exported function catches unwinding, and release builds use `panic = "abort"`.
- **No allocation per frame** once the unit count is stable. The frame buffer is reused.

## Revision history
- **1.1** (no layout change): typed `static const` constants instead of `#define`s; nullability annotations (`_Nullable` on constructor returns, `sc_world_destroy`'s argument and `ScFrame.units`, everything else non-null); `SC_FLAG_DETECTED` (bit 2) added; documented that `sc_world_create_dummy(seed, UINT32_MAX)` returns NULL.
- **1.0**: initial contract.

## Not yet in the contract (later phases)
Commands from the app (orders, SOPs, strikes), state save and restore (FlatBuffers), fast-forward on resume, and events for the Dynamic Island and Live Activities.
