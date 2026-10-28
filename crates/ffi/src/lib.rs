//! C API for SimCore. Implements `include/simcore.h` exactly; see `docs/ffi-contract.md`.
//!
//! Every export:
//! - accepts a NULL handle and returns a safe default (NULL, 0, `UINT32_MAX` for a link delay,
//!   or an empty [`ScFrame`], [`ScStatus`] or [`ScBytes`]);
//! - catches unwinding, so no panic crosses the boundary. Release builds use `panic = "abort"`,
//!   where a panic aborts the process instead; that is also defined behaviour.
//!
//! Handles are `Box<ScWorld>` turned into raw pointers. The caller must use each handle from one
//! thread at a time and must not use it after `sc_world_destroy`.

use std::panic::{AssertUnwindSafe, catch_unwind};

pub use simcore_abi::{
    ABI_VERSION, ScBytes, ScCommand, ScFrame, ScStatus, ScUnitView, TICKS_PER_SECOND, command,
};
use simcore_engine::{Order, Position, Sim, StableId};

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
/// until the next `sc_world_frame` or `sc_world_destroy` on this handle.
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

/// `void sc_world_despawn_removed(ScWorld *world);` No-op on NULL.
///
/// # Safety
/// `world` is NULL or a live handle, not used concurrently from another thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sc_world_despawn_removed(world: *mut ScWorld) {
    // SAFETY: NULL or a live, exclusively-used handle (caller contract).
    let Some(w) = (unsafe { world.as_mut() }) else {
        return;
    };
    guard((), || w.sim.despawn_removed());
}

/// `ScStatus sc_world_status(ScWorld *world);` All zeros on NULL or failure.
///
/// # Safety
/// `world` is NULL or a live handle, not used concurrently from another thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sc_world_status(world: *mut ScWorld) -> ScStatus {
    // SAFETY: NULL or a live, exclusively-used handle (caller contract).
    let Some(w) = (unsafe { world.as_mut() }) else {
        return ScStatus::default();
    };
    guard(ScStatus::default(), || w.sim.status())
}

/// `uint32_t sc_world_link_delay(ScWorld *world, uint64_t unit_id);` `UINT32_MAX` on NULL, an
/// unknown unit, or failure; real delays are capped at `UINT32_MAX - 1`.
///
/// # Safety
/// `world` is NULL or a live handle, not used concurrently from another thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sc_world_link_delay(world: *mut ScWorld, unit_id: u64) -> u32 {
    // SAFETY: NULL or a live, exclusively-used handle (caller contract).
    let Some(w) = (unsafe { world.as_mut() }) else {
        return u32::MAX;
    };
    guard(u32::MAX, || {
        w.sim.link_delay(StableId(unit_id)).unwrap_or(u32::MAX)
    })
}

/// Converts a C command to an engine order, resolving positions relative to the command node.
/// `None` for an unknown type, non-zero reserved fields, or a `radiate` other than 0 or 1.
fn order(sim: &Sim, c: &ScCommand) -> Option<Order> {
    if c._reserved0 != 0 || c._reserved1 != 0 {
        return None;
    }
    Some(match c.type_ {
        command::MOVE => {
            let node = sim.position(StableId(1)).unwrap_or_default();
            Order::Move {
                destination: Position {
                    x: node.x + f64::from(c.x),
                    y: node.y + f64::from(c.y),
                    z: node.z + f64::from(c.z),
                },
                speed_mps: f64::from(c.speed_mps),
            }
        }
        command::HALT => Order::Halt,
        command::RETARGET => Order::Retarget {
            target: StableId(c.target_id),
        },
        command::EMISSIONS if c.radiate <= 1 => Order::Emissions {
            radiate: c.radiate == 1,
        },
        _ => return None,
    })
}

/// `uint64_t sc_world_command(ScWorld *world, const ScCommand *command);` Returns the command
/// id (from 1), or 0 if the command is rejected, either handle is NULL, or the call fails.
///
/// # Safety
/// `world` is NULL or a live handle, not used concurrently from another thread; `command` is
/// NULL or points to a readable `ScCommand`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sc_world_command(world: *mut ScWorld, command: *const ScCommand) -> u64 {
    // SAFETY: NULL or a live, exclusively-used handle (caller contract).
    let Some(w) = (unsafe { world.as_mut() }) else {
        return 0;
    };
    // SAFETY: NULL or a readable ScCommand (caller contract). Copied out at once.
    let Some(c) = (unsafe { command.as_ref() }).copied() else {
        return 0;
    };
    guard(0, || {
        order(&w.sim, &c)
            .and_then(|o| w.sim.command(StableId(c.unit_id), o).ok())
            .unwrap_or(0)
    })
}

const fn empty_bytes() -> ScBytes {
    ScBytes {
        data: std::ptr::null(),
        len: 0,
    }
}

/// `ScBytes sc_world_save(ScWorld *world);` Empty (NULL, 0) on NULL or failure.
///
/// `data` points into the engine's reused snapshot buffer and stays valid until the next
/// `sc_world_save` or `sc_world_destroy` on this handle.
///
/// # Safety
/// `world` is NULL or a live handle, not used concurrently from another thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sc_world_save(world: *mut ScWorld) -> ScBytes {
    // SAFETY: NULL or a live, exclusively-used handle (caller contract).
    let Some(w) = (unsafe { world.as_mut() }) else {
        return empty_bytes();
    };
    guard(empty_bytes(), || {
        let bytes = w.sim.save();
        ScBytes {
            data: bytes.as_ptr(),
            len: bytes.len() as u64,
        }
    })
}

/// `ScWorld *sc_world_restore(const uint8_t *data, uint64_t len);` Returns NULL if `data` is
/// NULL, the bytes are not a snapshot this build reads, they are damaged, or the call fails.
///
/// # Safety
/// `data` is NULL or points to `len` readable bytes that are not modified during the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sc_world_restore(data: *const u8, len: u64) -> *mut ScWorld {
    let Ok(len) = usize::try_from(len) else {
        return std::ptr::null_mut();
    };
    if data.is_null() || len > isize::MAX as usize {
        return std::ptr::null_mut();
    }
    // SAFETY: non-null and `len` readable, unmodified bytes (caller contract), within
    // `isize::MAX`.
    let bytes = unsafe { std::slice::from_raw_parts(data, len) };
    guard(std::ptr::null_mut(), || match Sim::restore(bytes) {
        Ok(sim) => into_handle(sim),
        Err(_) => std::ptr::null_mut(),
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
    let _: unsafe extern "C" fn(*mut ScWorld) = sc_world_despawn_removed;
    let _: unsafe extern "C" fn(*mut ScWorld) -> ScStatus = sc_world_status;
    let _: unsafe extern "C" fn(*mut ScWorld, u64) -> u32 = sc_world_link_delay;
    let _: unsafe extern "C" fn(*mut ScWorld, *const ScCommand) -> u64 = sc_world_command;
    let _: unsafe extern "C" fn(*mut ScWorld) -> ScBytes = sc_world_save;
    let _: unsafe extern "C" fn(*const u8, u64) -> *mut ScWorld = sc_world_restore;
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

    #[test]
    fn new_calls_accept_null() {
        // SAFETY: NULL is an accepted input for every handle and pointer argument.
        unsafe {
            sc_world_despawn_removed(std::ptr::null_mut());
            assert_eq!(sc_world_status(std::ptr::null_mut()), ScStatus::default());
            assert_eq!(sc_world_link_delay(std::ptr::null_mut(), 1), u32::MAX);
            let c = ScCommand::default();
            assert_eq!(sc_world_command(std::ptr::null_mut(), &c), 0);
            let w = sc_world_create(1);
            assert_eq!(sc_world_command(w, std::ptr::null()), 0);
            let b = sc_world_save(std::ptr::null_mut());
            assert!(b.data.is_null() && b.len == 0);
            assert!(sc_world_restore(std::ptr::null(), 10).is_null());
            assert!(sc_world_restore(b"SCSN".as_ptr(), 4).is_null());
            sc_world_destroy(w);
        }
    }

    #[test]
    fn command_status_save_restore() {
        let w = sc_world_create_dummy(3, 300);
        // SAFETY: `w` and `r` are live handles used only on this thread, each destroyed once;
        // `saved` is copied out of the engine buffer before the next call on `w`.
        unsafe {
            let units = {
                let f = sc_world_frame(w);
                std::slice::from_raw_parts(f.units, f.count as usize).to_vec()
            };
            let friendly = units
                .iter()
                .find(|u| u.affiliation == simcore_abi::affiliation::FRIENDLY && u.id != 1)
                .unwrap();
            let hostile = units
                .iter()
                .find(|u| u.affiliation == simcore_abi::affiliation::HOSTILE)
                .unwrap();
            let mv = ScCommand {
                unit_id: friendly.id,
                x: friendly.x + 30.0,
                y: friendly.y,
                z: friendly.z,
                speed_mps: 10.0,
                type_: command::MOVE,
                ..ScCommand::default()
            };
            assert_eq!(sc_world_link_delay(w, friendly.id), 0);
            assert_eq!(sc_world_link_delay(w, 99_999), u32::MAX);
            assert_eq!(sc_world_command(w, &mv), 1);
            for bad in [
                ScCommand {
                    unit_id: hostile.id,
                    ..mv
                },
                ScCommand {
                    _reserved1: 1,
                    ..mv
                },
                ScCommand {
                    speed_mps: 0.0,
                    ..mv
                },
                ScCommand { type_: 99, ..mv },
                ScCommand {
                    type_: command::EMISSIONS,
                    radiate: 2,
                    ..mv
                },
            ] {
                assert_eq!(sc_world_command(w, &bad), 0, "{bad:?}");
            }
            assert_eq!(sc_world_status(w).pending_commands, 1);

            let b = sc_world_save(w);
            assert!(!b.data.is_null() && b.len > 0);
            let saved = std::slice::from_raw_parts(b.data, b.len as usize).to_vec();
            let r = sc_world_restore(saved.as_ptr(), saved.len() as u64);
            assert!(!r.is_null());
            assert_eq!(sc_world_status(r), sc_world_status(w));

            // Both deliver the move and travel the same 30 m: 45 ticks at 10 m/s.
            sc_world_step(w, 60);
            sc_world_step(r, 60);
            let pos = |h| {
                let f = sc_world_frame(h);
                let u = std::slice::from_raw_parts(f.units, f.count as usize);
                let u = u.iter().find(|u| u.id == friendly.id).unwrap();
                (u.x, u.y, u.vx)
            };
            let (pw, pr) = (pos(w), pos(r));
            assert_eq!(pw, pr);
            assert!(
                (pw.0 - mv.x).abs() < 0.5 && pw.2 == 0.0,
                "arrived and stopped: {pw:?}"
            );
            assert_eq!(sc_world_status(w), sc_world_status(r));
            sc_world_despawn_removed(w);
            sc_world_destroy(w);
            sc_world_destroy(r);
        }
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
