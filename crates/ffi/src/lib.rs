//! C API for SimCore. Implements `include/simcore.h` exactly; see `docs/ffi-contract.md`.
//!
//! Every export:
//! - accepts a NULL handle and returns a safe default (NULL, 0, or an empty [`ScFrame`]);
//! - catches unwinding, so no panic crosses the boundary. Release builds use `panic = "abort"`,
//!   where a panic aborts the process instead; that is also defined behaviour.
//!
//! Handles are `Box<ScWorld>` turned into raw pointers. The caller must use each handle from one
//! thread at a time and must not use it after `sc_world_destroy`.

use std::panic::{AssertUnwindSafe, catch_unwind};

pub use simcore_abi::{ABI_VERSION, ScFrame, ScUnitView, TICKS_PER_SECOND};
use simcore_engine::Sim;

/// Opaque world handle (`ScWorld` in the header). C only ever sees a pointer to it.
pub struct ScWorld {
    sim: Sim,
}

/// The frame returned on a NULL handle or a failure: null pointer, count 0, tick 0.
pub const fn empty_frame() -> ScFrame {
    ScFrame {
        units: std::ptr::null(),
        count: 0,
        _reserved: 0,
        tick: 0,
    }
}

/// Runs `f`, returning `default` if it panics. The panic payload is leaked rather than
/// dropped, because a payload's destructor could itself panic and escape.
fn guard<R>(default: R, f: impl FnOnce() -> R) -> R {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(r) => r,
        Err(payload) => {
            std::mem::forget(payload);
            default
        }
    }
}

fn into_handle(sim: Sim) -> *mut ScWorld {
    Box::into_raw(Box::new(ScWorld { sim }))
}

/// `uint32_t sc_abi_version(void);`
#[unsafe(no_mangle)]
pub extern "C" fn sc_abi_version() -> u32 {
    ABI_VERSION
}

/// `ScWorld *sc_world_create(uint64_t seed);` Returns NULL on failure.
#[unsafe(no_mangle)]
pub extern "C" fn sc_world_create(seed: u64) -> *mut ScWorld {
    guard(std::ptr::null_mut(), || into_handle(Sim::new(seed)))
}

/// `ScWorld *sc_world_create_dummy(uint64_t seed, uint32_t unit_count);`
///
/// Returns NULL when `unit_count == UINT32_MAX`, because the frame (units plus the command
/// node) would not fit `ScFrame.count`.
#[unsafe(no_mangle)]
pub extern "C" fn sc_world_create_dummy(seed: u64, unit_count: u32) -> *mut ScWorld {
    if unit_count == u32::MAX {
        return std::ptr::null_mut();
    }
    guard(std::ptr::null_mut(), || {
        into_handle(Sim::new_dummy(seed, unit_count))
    })
}

/// `void sc_world_destroy(ScWorld *world);` Safe to call with NULL.
///
/// # Safety
/// `world` is NULL or a live handle from `sc_world_create*` that is not used again.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sc_world_destroy(world: *mut ScWorld) {
    if world.is_null() {
        return;
    }
    guard((), || {
        // SAFETY: non-null, and by the caller's contract it came from `Box::into_raw` in
        // `into_handle` and ownership is handed back exactly once.
        drop(unsafe { Box::from_raw(world) });
    });
}

/// `void sc_world_step(ScWorld *world, uint32_t ticks);` No-op on NULL.
///
/// # Safety
/// `world` is NULL or a live handle, not used concurrently from another thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sc_world_step(world: *mut ScWorld, ticks: u32) {
    // SAFETY: NULL or a live, exclusively-used handle (caller contract), so a unique borrow
    // for the duration of this call is sound.
    let Some(w) = (unsafe { world.as_mut() }) else {
        return;
    };
    guard((), || w.sim.step(ticks));
}

/// `uint64_t sc_world_tick(const ScWorld *world);` Returns 0 on NULL.
///
/// # Safety
/// `world` is NULL or a live handle, not mutated concurrently from another thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sc_world_tick(world: *const ScWorld) -> u64 {
    // SAFETY: NULL or a live handle that nobody mutates during this call (caller contract).
    let Some(w) = (unsafe { world.as_ref() }) else {
        return 0;
    };
    guard(0, || w.sim.tick())
}

/// `ScFrame sc_world_frame(ScWorld *world);` Returns [`empty_frame`] on NULL or failure.
///
/// The returned `units` points into the engine's reused frame buffer (no copy) and stays valid
/// until the next `sc_world_step`, `sc_world_frame` or `sc_world_destroy` on this handle.
///
/// # Safety
/// `world` is NULL or a live handle, not used concurrently from another thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sc_world_frame(world: *mut ScWorld) -> ScFrame {
    // SAFETY: NULL or a live, exclusively-used handle (caller contract).
    let Some(w) = (unsafe { world.as_mut() }) else {
        return empty_frame();
    };
    guard(empty_frame(), || {
        let tick = w.sim.tick();
        let units = w.sim.frame();
        let Ok(count) = u32::try_from(units.len()) else {
            return empty_frame();
        };
        ScFrame {
            // An empty slice's pointer is dangling; the contract says NULL for "no units".
            units: if count == 0 {
                std::ptr::null()
            } else {
                units.as_ptr()
            },
            count,
            _reserved: 0,
            tick,
        }
    })
}

/// The exports with the exact C signatures from `include/simcore.h`. If a signature drifts,
/// this stops compiling.
const _: () = {
    let _: extern "C" fn() -> u32 = sc_abi_version;
    let _: extern "C" fn(u64) -> *mut ScWorld = sc_world_create;
    let _: extern "C" fn(u64, u32) -> *mut ScWorld = sc_world_create_dummy;
    let _: unsafe extern "C" fn(*mut ScWorld) = sc_world_destroy;
    let _: unsafe extern "C" fn(*mut ScWorld, u32) = sc_world_step;
    let _: unsafe extern "C" fn(*const ScWorld) -> u64 = sc_world_tick;
    let _: unsafe extern "C" fn(*mut ScWorld) -> ScFrame = sc_world_frame;
};

#[cfg(test)]
mod tests {
    use super::*;
    use simcore_abi::flags;

    #[test]
    fn null_handles_are_safe() {
        // SAFETY: NULL is an accepted input for every handle-taking function.
        unsafe {
            sc_world_destroy(std::ptr::null_mut());
            sc_world_step(std::ptr::null_mut(), 10);
            assert_eq!(sc_world_tick(std::ptr::null()), 0);
            let f = sc_world_frame(std::ptr::null_mut());
            assert!(f.units.is_null());
            assert_eq!((f.count, f.tick), (0, 0));
        }
    }

    #[test]
    fn frame_is_engine_buffer_without_copy() {
        let w = sc_world_create_dummy(7, 1000);
        assert!(!w.is_null());
        // SAFETY: `w` is a live handle used only on this thread, destroyed once at the end;
        // `f.units` is read only before the next mutating call.
        unsafe {
            sc_world_step(w, 3);
            assert_eq!(sc_world_tick(w), 3);
            let f = sc_world_frame(w);
            assert_eq!(f.count, 1001);
            assert_eq!(f.tick, 3);
            let units = std::slice::from_raw_parts(f.units, f.count as usize);
            let cmd: Vec<_> = units
                .iter()
                .filter(|u| u.flags & flags::COMMAND_NODE != 0)
                .collect();
            assert_eq!(cmd.len(), 1);
            assert_eq!((cmd[0].x, cmd[0].y, cmd[0].z), (0.0, 0.0, 0.0));
            // Done reading `units`. The engine buffer is reused, so a rebuilt frame (through the
            // engine or the C API) comes back at the same address: no copy is made.
            assert_eq!(
                (*w).sim.frame().as_ptr(),
                f.units,
                "points into the engine buffer"
            );
            assert_eq!(sc_world_frame(w).units, f.units);
            sc_world_destroy(w);
        }
    }

    #[test]
    fn unrepresentable_unit_count_is_null() {
        assert!(sc_world_create_dummy(1, u32::MAX).is_null());
    }

    #[cfg(panic = "unwind")]
    #[test]
    fn guard_catches_panics() {
        assert_eq!(guard(5, || -> i32 { panic!("boom") }), 5);
        let f = guard(empty_frame(), || -> ScFrame { panic!("boom") });
        assert!(f.units.is_null());
        assert_eq!(guard(1, || 2), 2);
    }
}
