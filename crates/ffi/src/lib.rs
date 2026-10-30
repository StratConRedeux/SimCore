//! C API for SimCore. Implements `include/simcore.h` exactly; see `docs/ffi-contract.md`.
//!
//! Every export:
//! - accepts a NULL handle and returns a safe default (NULL, 0, `false`, `UINT32_MAX` for a link
//!   delay or a standing order's stock, or an empty [`ScFrame`], [`ScEngagements`],
//!   [`ScLaunches`], [`ScStandingOrders`], [`ScStatus`] or [`ScBytes`]);
//! - catches unwinding, so no panic crosses the boundary. Release builds use `panic = "abort"`,
//!   where a panic aborts the process instead; that is also defined behaviour.
//!
//! Handles are `Box<ScWorld>` turned into raw pointers. The caller must use each handle from one
//! thread at a time and must not use it after `sc_world_destroy`.

use std::panic::{AssertUnwindSafe, catch_unwind};

pub use simcore_abi::{
    ABI_VERSION, MAX_STANDING_ORDERS, ScBytes, ScCommand, ScEngagement, ScEngagements, ScFrame,
    ScLaunch, ScLaunches, ScLinkRules, ScOutcome, ScRules, ScSensor, ScStandingOrder,
    ScStandingOrderView, ScStandingOrders, ScStatus, ScTerrain, ScUnitSpec, ScUnitView,
    TICKS_PER_SECOND, command, engagement, kind, spawn,
};
use simcore_engine::{
    EmitterCycle, EmsProfile, LinkRules, MAX_DUMMY_UNITS, Order, OutcomeTable, Position, Pursuer,
    Rules, Sensor, SensorParams, Sim, StableId, StandingOrder, Terrain, UnitSpec, Velocity,
};

/// Opaque world handle (`ScWorld` in the header). C only ever sees a pointer to it.
pub struct ScWorld {
    sim: Sim,
    /// The latest step's engagements in C layout, rebuilt by `sc_world_engagements`.
    engagements: Vec<ScEngagement>,
    /// The latest step's standing-order launches in C layout, rebuilt by `sc_world_launches`.
    launches: Vec<ScLaunch>,
    /// The active standing orders in C layout, rebuilt by `sc_world_standing_orders`.
    standing_orders: Vec<ScStandingOrderView>,
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
    Box::into_raw(Box::new(ScWorld {
        sim,
        engagements: Vec::new(),
        launches: Vec::new(),
        standing_orders: Vec::new(),
    }))
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
/// Returns NULL when `unit_count` exceeds the ids a world has (`MAX_DUMMY_UNITS`): its
/// snapshot could not be restored.
#[unsafe(no_mangle)]
pub extern "C" fn sc_world_create_dummy(seed: u64, unit_count: u32) -> *mut ScWorld {
    if unit_count > MAX_DUMMY_UNITS {
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

/// The engagements returned on a NULL handle or a failure: null pointer, count 0.
pub const fn empty_engagements() -> ScEngagements {
    ScEngagements {
        events: std::ptr::null(),
        count: 0,
        _reserved: 0,
    }
}

/// `ScEngagements sc_world_engagements(ScWorld *world);` Returns [`empty_engagements`] on NULL
/// or failure.
///
/// The engagements resolved during the most recent `sc_world_step`, in resolution order. The
/// returned `events` points into a buffer the handle reuses and stays valid until the next
/// `sc_world_engagements` or `sc_world_destroy` on this handle.
///
/// # Safety
/// `world` is NULL or a live handle, not used concurrently from another thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sc_world_engagements(world: *mut ScWorld) -> ScEngagements {
    // SAFETY: NULL or a live, exclusively-used handle (caller contract).
    let Some(w) = (unsafe { world.as_mut() }) else {
        return empty_engagements();
    };
    guard(empty_engagements(), || {
        w.engagements.clear();
        w.engagements
            .extend(w.sim.engagement_events().iter().map(|e| ScEngagement {
                tick: e.tick,
                pursuer_id: e.pursuer.0,
                target_id: e.target.0,
                x: e.x,
                y: e.y,
                z: e.z,
                target_kind: e.target_kind,
                target_affiliation: e.target_affiliation,
                flags: (u8::from(e.success) * engagement::SUCCESS)
                    | (u8::from(e.target_removed) * engagement::TARGET_REMOVED)
                    | (u8::from(e.command_node_hit) * engagement::COMMAND_NODE_HIT),
            }));
        let Ok(count) = u32::try_from(w.engagements.len()) else {
            return empty_engagements();
        };
        ScEngagements {
            // An empty slice's pointer is dangling; the contract says NULL for "none".
            events: if count == 0 {
                std::ptr::null()
            } else {
                w.engagements.as_ptr()
            },
            count,
            _reserved: 0,
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

/// `bool sc_world_order(ScWorld *world, const ScCommand *command);` Applies the command at
/// once to a live unit of any affiliation (revision 1.7). `false` if it is rejected (the type
/// and value checks of `sc_world_command`), either handle is NULL, or the call fails.
///
/// # Safety
/// `world` is NULL or a live handle, not used concurrently from another thread; `command` is
/// NULL or points to a readable `ScCommand`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sc_world_order(world: *mut ScWorld, command: *const ScCommand) -> bool {
    // SAFETY: NULL or a live, exclusively-used handle (caller contract).
    let Some(w) = (unsafe { world.as_mut() }) else {
        return false;
    };
    // SAFETY: NULL or a readable ScCommand (caller contract). Copied out at once.
    let Some(c) = (unsafe { command.as_ref() }).copied() else {
        return false;
    };
    guard(false, || {
        order(&w.sim, &c).is_some_and(|o| w.sim.order(StableId(c.unit_id), o).is_ok())
    })
}

/// `bool sc_world_set_tag(ScWorld *world, uint64_t unit_id, uint64_t tag);` Sets the host's
/// tag on a live unit; 0 clears it (revision 1.7). `false` for an unknown or removed unit, a
/// NULL handle, or a failure.
///
/// # Safety
/// `world` is NULL or a live handle, not used concurrently from another thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sc_world_set_tag(world: *mut ScWorld, unit_id: u64, tag: u64) -> bool {
    // SAFETY: NULL or a live, exclusively-used handle (caller contract).
    let Some(w) = (unsafe { world.as_mut() }) else {
        return false;
    };
    guard(false, || w.sim.set_tag(StableId(unit_id), tag))
}

/// `uint64_t sc_world_tag(const ScWorld *world, uint64_t unit_id);` The host's tag on a unit:
/// 0 if unset, the unit is unknown or removed, the handle is NULL, or the call fails.
///
/// # Safety
/// `world` is NULL or a live handle, not mutated concurrently from another thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sc_world_tag(world: *const ScWorld, unit_id: u64) -> u64 {
    // SAFETY: NULL or a live handle that nobody mutates during this call (caller contract).
    let Some(w) = (unsafe { world.as_ref() }) else {
        return 0;
    };
    guard(0, || w.sim.tag(StableId(unit_id)))
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

/// Finite and non-negative: what every distance, power, rate and chance in the setup structs
/// must be.
fn non_negative(x: f64) -> bool {
    x >= 0.0 && x.is_finite()
}

fn chance(x: f64) -> bool {
    (0.0..=1.0).contains(&x)
}

/// The command node's world position, which setup coordinates are relative to.
fn node(sim: &Sim) -> Position {
    sim.position(StableId(1)).unwrap_or_default()
}

/// Checks and converts C rules. `None` if any value is out of range, a reserved field is set,
/// or a (class, kind) pair repeats.
fn rules(r: &ScRules, outcomes: &[ScOutcome]) -> Option<Rules> {
    let values_ok = r._reserved == 0
        && non_negative(r.detection_max_range_m)
        && non_negative(r.sensor_base_range_m)
        && non_negative(r.sensor_reference_power_w)
        && chance(r.default_chance);
    if !values_ok {
        return None;
    }
    let mut table = OutcomeTable::new(r.default_chance);
    for o in outcomes {
        if o._reserved != 0 || !chance(o.chance) {
            return None;
        }
        table.set(o.outcome_class, o.target_kind, o.chance);
    }
    // `set` replaces repeats, so a repeat shows as a shorter table.
    if table.entries().len() != outcomes.len() {
        return None;
    }
    Some(Rules {
        detection_max_range_m: r.detection_max_range_m,
        sensor: SensorParams {
            base_range_m: r.sensor_base_range_m,
            reference_power_w: r.sensor_reference_power_w,
        },
        exposure_threshold_ticks: r.exposure_threshold_ticks,
        exposure_cap_ticks: r.exposure_cap_ticks,
        exposure_decay_per_tick: r.exposure_decay_per_tick,
        outcomes: table,
    })
}

/// `bool sc_world_set_rules(ScWorld *world, const ScRules *rules, const ScOutcome *outcomes,
/// uint32_t outcome_count);` False, with nothing changed, on NULL, bad values or failure.
///
/// # Safety
/// `world` is NULL or a live handle, not used concurrently from another thread; `rules` is
/// NULL or points to a readable `ScRules`; `outcomes` is NULL or points to `outcome_count`
/// readable `ScOutcome`s.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sc_world_set_rules(
    world: *mut ScWorld,
    rules: *const ScRules,
    outcomes: *const ScOutcome,
    outcome_count: u32,
) -> bool {
    // SAFETY: NULL or a live, exclusively-used handle (caller contract).
    let Some(w) = (unsafe { world.as_mut() }) else {
        return false;
    };
    // SAFETY: NULL or a readable ScRules (caller contract). Copied out at once.
    let Some(r) = (unsafe { rules.as_ref() }).copied() else {
        return false;
    };
    let outcomes: &[ScOutcome] = match (outcomes.is_null(), outcome_count) {
        (_, 0) => &[],
        (true, _) => return false,
        // SAFETY: non-null and `outcome_count` readable entries (caller contract); at most
        // 2^32 * 16 bytes, within `isize::MAX` on 64-bit targets.
        (false, n) => unsafe { std::slice::from_raw_parts(outcomes, n as usize) },
    };
    guard(false, || match self::rules(&r, outcomes) {
        Some(rules) => {
            w.sim.set_rules(rules);
            true
        }
        None => false,
    })
}

/// `bool sc_world_set_link_rules(ScWorld *world, const ScLinkRules *rules);` False, with
/// nothing changed, on NULL, bad values or failure.
///
/// # Safety
/// `world` is NULL or a live handle, not used concurrently from another thread; `rules` is
/// NULL or points to a readable `ScLinkRules`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sc_world_set_link_rules(
    world: *mut ScWorld,
    rules: *const ScLinkRules,
) -> bool {
    // SAFETY: NULL or a live, exclusively-used handle (caller contract).
    let Some(w) = (unsafe { world.as_mut() }) else {
        return false;
    };
    // SAFETY: NULL or a readable ScLinkRules (caller contract). Copied out at once.
    let Some(r) = (unsafe { rules.as_ref() }).copied() else {
        return false;
    };
    if r._reserved != 0 || !non_negative(r.delay_ticks_per_km) || !non_negative(r.jam_radius_m) {
        return false;
    }
    guard(false, || {
        w.sim.set_link_rules(LinkRules {
            base_delay_ticks: r.base_delay_ticks,
            delay_ticks_per_km: r.delay_ticks_per_km,
            jam_radius_m: r.jam_radius_m,
            jam_delay_ticks: r.jam_delay_ticks,
            max_delay_ticks: r.max_delay_ticks,
        });
        true
    })
}

/// `bool sc_world_set_terrain(ScWorld *world, const ScTerrain *terrain, const float *heights,
/// uint64_t height_count);` NULL `terrain` sets flat terrain. False, with nothing changed, on a
/// NULL world, bad values or failure.
///
/// # Safety
/// `world` is NULL or a live handle, not used concurrently from another thread; `terrain` is
/// NULL or points to a readable `ScTerrain`; `heights` is NULL or points to `height_count`
/// readable floats that are not modified during the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sc_world_set_terrain(
    world: *mut ScWorld,
    terrain: *const ScTerrain,
    heights: *const f32,
    height_count: u64,
) -> bool {
    // SAFETY: NULL or a live, exclusively-used handle (caller contract).
    let Some(w) = (unsafe { world.as_mut() }) else {
        return false;
    };
    // SAFETY: NULL or a readable ScTerrain (caller contract). Copied out at once.
    let Some(t) = (unsafe { terrain.as_ref() }).copied() else {
        return guard(false, || {
            w.sim.set_terrain(Terrain::flat());
            true
        });
    };
    // The snapshot stores the sample count as a u32; nx * ny must match it anyway.
    // Checked before the copy below, so a wrong count never copies a huge buffer.
    if t._reserved != 0
        || heights.is_null()
        || height_count != u64::from(t.nx) * u64::from(t.ny)
        || height_count > u64::from(u32::MAX)
    {
        return false;
    }
    // SAFETY: non-null and `height_count` readable, unmodified floats (caller contract); at
    // most 2^32 * 4 bytes, within `isize::MAX` on 64-bit targets.
    let heights = unsafe { std::slice::from_raw_parts(heights, height_count as usize) };
    guard(false, || {
        let o = node(&w.sim);
        let terrain = Terrain::heightfield(
            o.x + t.origin_x,
            o.y + t.origin_y,
            t.spacing_m,
            t.nx,
            t.ny,
            heights.to_vec(),
            t.los_samples,
        );
        match terrain {
            Some(terrain) => {
                w.sim.set_terrain(terrain);
                true
            }
            None => false,
        }
    })
}

/// Converts a C spawn spec, resolving its position relative to the command node. `None` for
/// unknown option bits, continuous emission with a duty cycle, or a set reserved field;
/// [`Sim::spawn`] checks the values.
fn unit_spec(sim: &Sim, s: &ScUnitSpec) -> Option<(UnitSpec, Option<Pursuer>)> {
    const KNOWN: u8 = spawn::EMITTING | spawn::CYCLE | spawn::PURSUER | spawn::SENSOR;
    let has = |bit: u8| s.options & bit != 0;
    if s._reserved != 0 || s.options & !KNOWN != 0 || (has(spawn::EMITTING) && has(spawn::CYCLE)) {
        return None;
    }
    let o = node(sim);
    let spec = UnitSpec {
        kind: s.kind,
        affiliation: s.affiliation,
        position: Position {
            x: o.x + s.x,
            y: o.y + s.y,
            z: o.z + s.z,
        },
        velocity: Velocity {
            x: s.vx,
            y: s.vy,
            z: s.vz,
        },
        ems: EmsProfile {
            emitting: has(spawn::EMITTING),
            erp_w: s.erp_w,
        },
        cycle: has(spawn::CYCLE).then_some(EmitterCycle {
            on_ticks: s.cycle_on_ticks,
            off_ticks: s.cycle_off_ticks,
            phase: s.cycle_phase,
        }),
    };
    let pursuer = has(spawn::PURSUER).then_some(Pursuer {
        target: StableId(s.target_id),
        speed: s.speed_mps,
        max_turn_rate: s.max_turn_rate_rad_s,
        lead: s.lead,
        engagement_radius_m: s.engagement_radius_m,
        outcome_class: s.outcome_class,
    });
    Some((spec, pursuer))
}

/// `uint64_t sc_world_spawn(ScWorld *world, const ScUnitSpec *spec);` Returns the new unit's
/// id, or 0, with nothing changed, on NULL, a rejected spec, exhausted ids or failure.
///
/// # Safety
/// `world` is NULL or a live handle, not used concurrently from another thread; `spec` is
/// NULL or points to a readable `ScUnitSpec`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sc_world_spawn(world: *mut ScWorld, spec: *const ScUnitSpec) -> u64 {
    // SAFETY: NULL or a live, exclusively-used handle (caller contract).
    let Some(w) = (unsafe { world.as_mut() }) else {
        return 0;
    };
    // SAFETY: NULL or a readable ScUnitSpec (caller contract). Copied out at once.
    let Some(s) = (unsafe { spec.as_ref() }).copied() else {
        return 0;
    };
    guard(0, || {
        let Some((spec, pursuer)) = unit_spec(&w.sim, &s) else {
            return 0;
        };
        let Ok(id) = w.sim.spawn(spec, pursuer) else {
            return 0;
        };
        if s.options & spawn::SENSOR != 0 {
            w.sim.add_sensor(id, Sensor::default());
        }
        id.0
    })
}

/// `bool sc_world_add_sensor(ScWorld *world, uint64_t unit_id, const ScSensor *sensor);` NULL
/// `sensor` uses the shared rules' parameters. False, with nothing changed, on a NULL world,
/// an unknown unit, bad values or failure.
///
/// # Safety
/// `world` is NULL or a live handle, not used concurrently from another thread; `sensor` is
/// NULL or points to a readable `ScSensor`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sc_world_add_sensor(
    world: *mut ScWorld,
    unit_id: u64,
    sensor: *const ScSensor,
) -> bool {
    // SAFETY: NULL or a live, exclusively-used handle (caller contract).
    let Some(w) = (unsafe { world.as_mut() }) else {
        return false;
    };
    // SAFETY: NULL or a readable ScSensor (caller contract). Copied out at once.
    let params = (unsafe { sensor.as_ref() }).copied();
    if params.is_some_and(|p| !non_negative(p.base_range_m) || !non_negative(p.reference_power_w)) {
        return false;
    }
    let sensor = Sensor {
        params: params.map(|p| SensorParams {
            base_range_m: p.base_range_m,
            reference_power_w: p.reference_power_w,
        }),
    };
    guard(false, || w.sim.add_sensor(StableId(unit_id), sensor))
}

/// Converts a C standing order, resolving the zone centre relative to the command node. `None`
/// for a set reserved field, or a launch that is not a pursuer, is a sensor or fails
/// [`unit_spec`]; [`Sim::add_standing_order`] checks the values.
fn standing_order(sim: &Sim, o: &ScStandingOrder) -> Option<StandingOrder> {
    let l = &o.launch;
    if o._reserved != 0 || l.options & spawn::PURSUER == 0 || l.options & spawn::SENSOR != 0 {
        return None;
    }
    let (mut launch, pursuer) = unit_spec(sim, l)?;
    // Relative to the node at each launch, not now: undo the conversion `unit_spec` applies.
    launch.position = Position {
        x: l.x,
        y: l.y,
        z: l.z,
    };
    let n = node(sim);
    Some(StandingOrder {
        center_x: n.x + o.x,
        center_y: n.y + o.y,
        radius_m: o.radius_m,
        kinds: o.kinds,
        launch,
        pursuer: pursuer?,
        stock: o.stock,
        cooldown_ticks: o.cooldown_ticks,
    })
}

/// `uint64_t sc_world_add_standing_order(ScWorld *world, const ScStandingOrder *order);`
/// Returns the order's id (from 1), or 0, with nothing changed, on NULL, a rejected order, a
/// full list or failure.
///
/// # Safety
/// `world` is NULL or a live handle, not used concurrently from another thread; `order` is
/// NULL or points to a readable `ScStandingOrder`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sc_world_add_standing_order(
    world: *mut ScWorld,
    order: *const ScStandingOrder,
) -> u64 {
    // SAFETY: NULL or a live, exclusively-used handle (caller contract).
    let Some(w) = (unsafe { world.as_mut() }) else {
        return 0;
    };
    // SAFETY: NULL or a readable ScStandingOrder (caller contract). Copied out at once.
    let Some(o) = (unsafe { order.as_ref() }).copied() else {
        return 0;
    };
    guard(0, || {
        standing_order(&w.sim, &o)
            .and_then(|o| w.sim.add_standing_order(o).ok())
            .unwrap_or(0)
    })
}

/// `bool sc_world_cancel_standing_order(ScWorld *world, uint64_t order_id);` False on NULL, an
/// unknown order or failure.
///
/// # Safety
/// `world` is NULL or a live handle, not used concurrently from another thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sc_world_cancel_standing_order(
    world: *mut ScWorld,
    order_id: u64,
) -> bool {
    // SAFETY: NULL or a live, exclusively-used handle (caller contract).
    let Some(w) = (unsafe { world.as_mut() }) else {
        return false;
    };
    guard(false, || w.sim.cancel_standing_order(order_id))
}

/// `uint32_t sc_world_standing_order_stock(ScWorld *world, uint64_t order_id);` `UINT32_MAX` on
/// NULL, an unknown order or failure; stock never reaches it.
///
/// # Safety
/// `world` is NULL or a live handle, not used concurrently from another thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sc_world_standing_order_stock(world: *mut ScWorld, order_id: u64) -> u32 {
    // SAFETY: NULL or a live, exclusively-used handle (caller contract).
    let Some(w) = (unsafe { world.as_mut() }) else {
        return u32::MAX;
    };
    guard(u32::MAX, || {
        w.sim
            .standing_order(order_id)
            .map_or(u32::MAX, |s| s.remaining)
    })
}

/// The launches returned on a NULL handle or a failure: null pointer, count 0.
pub const fn empty_launches() -> ScLaunches {
    ScLaunches {
        launches: std::ptr::null(),
        count: 0,
        _reserved: 0,
    }
}

/// `ScLaunches sc_world_launches(ScWorld *world);` Returns [`empty_launches`] on NULL or
/// failure.
///
/// The standing-order launches during the most recent `sc_world_step`, in order. The returned
/// `launches` points into a buffer the handle reuses and stays valid until the next
/// `sc_world_launches` or `sc_world_destroy` on this handle.
///
/// # Safety
/// `world` is NULL or a live handle, not used concurrently from another thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sc_world_launches(world: *mut ScWorld) -> ScLaunches {
    // SAFETY: NULL or a live, exclusively-used handle (caller contract).
    let Some(w) = (unsafe { world.as_mut() }) else {
        return empty_launches();
    };
    guard(empty_launches(), || {
        w.launches.clear();
        w.launches
            .extend(w.sim.standing_order_launches().iter().map(|l| ScLaunch {
                tick: l.tick,
                order_id: l.order,
                pursuer_id: l.pursuer.0,
                target_id: l.target.0,
            }));
        let Ok(count) = u32::try_from(w.launches.len()) else {
            return empty_launches();
        };
        ScLaunches {
            // An empty slice's pointer is dangling; the contract says NULL for "none".
            launches: if count == 0 {
                std::ptr::null()
            } else {
                w.launches.as_ptr()
            },
            count,
            _reserved: 0,
        }
    })
}

/// The standing orders returned on a NULL handle or a failure: null pointer, count 0.
pub const fn empty_standing_orders() -> ScStandingOrders {
    ScStandingOrders {
        orders: std::ptr::null(),
        count: 0,
        _reserved: 0,
    }
}

/// `ScStandingOrders sc_world_standing_orders(ScWorld *world);` Returns
/// [`empty_standing_orders`] on NULL or failure.
///
/// The active standing orders in id order, out-of-stock ones included, each zone centre relative
/// to the command node now (as [`Sim::to_frame`] gives it, like the frame's units). The returned
/// `orders` points into a buffer the handle reuses and stays valid until the next
/// `sc_world_standing_orders` or `sc_world_destroy` on this handle.
///
/// # Safety
/// `world` is NULL or a live handle, not used concurrently from another thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sc_world_standing_orders(world: *mut ScWorld) -> ScStandingOrders {
    // SAFETY: NULL or a live, exclusively-used handle (caller contract).
    let Some(w) = (unsafe { world.as_mut() }) else {
        return empty_standing_orders();
    };
    guard(empty_standing_orders(), || {
        let sim = &w.sim;
        w.standing_orders.clear();
        w.standing_orders
            .extend(sim.standing_orders_with_status().map(|(id, o, s)| {
                let [x, y, _] = sim.to_frame(Position {
                    x: o.center_x,
                    y: o.center_y,
                    z: 0.0,
                });
                ScStandingOrderView {
                    id,
                    x,
                    y,
                    // Rounded to f32 like the centre: beyond its range, infinity or 0 (documented).
                    radius_m: o.radius_m as f32,
                    kinds: o.kinds,
                    remaining: s.remaining,
                    stock: o.stock,
                    ready_tick: s.ready_tick,
                    cooldown_ticks: o.cooldown_ticks,
                    _reserved: 0,
                }
            }));
        let Ok(count) = u32::try_from(w.standing_orders.len()) else {
            return empty_standing_orders();
        };
        ScStandingOrders {
            // An empty slice's pointer is dangling; the contract says NULL for "none".
            orders: if count == 0 {
                std::ptr::null()
            } else {
                w.standing_orders.as_ptr()
            },
            count,
            _reserved: 0,
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
    let _: unsafe extern "C" fn(*mut ScWorld) = sc_world_despawn_removed;
    let _: unsafe extern "C" fn(*mut ScWorld) -> ScStatus = sc_world_status;
    let _: unsafe extern "C" fn(*mut ScWorld, u64) -> u32 = sc_world_link_delay;
    let _: unsafe extern "C" fn(*mut ScWorld, *const ScCommand) -> u64 = sc_world_command;
    let _: unsafe extern "C" fn(*mut ScWorld) -> ScBytes = sc_world_save;
    let _: unsafe extern "C" fn(*const u8, u64) -> *mut ScWorld = sc_world_restore;
    let _: unsafe extern "C" fn(*mut ScWorld, *const ScRules, *const ScOutcome, u32) -> bool =
        sc_world_set_rules;
    let _: unsafe extern "C" fn(*mut ScWorld, *const ScLinkRules) -> bool = sc_world_set_link_rules;
    let _: unsafe extern "C" fn(*mut ScWorld, *const ScTerrain, *const f32, u64) -> bool =
        sc_world_set_terrain;
    let _: unsafe extern "C" fn(*mut ScWorld, *const ScUnitSpec) -> u64 = sc_world_spawn;
    let _: unsafe extern "C" fn(*mut ScWorld, u64, *const ScSensor) -> bool = sc_world_add_sensor;
    let _: unsafe extern "C" fn(*mut ScWorld) -> ScEngagements = sc_world_engagements;
    let _: unsafe extern "C" fn(*mut ScWorld, *const ScStandingOrder) -> u64 =
        sc_world_add_standing_order;
    let _: unsafe extern "C" fn(*mut ScWorld, u64) -> bool = sc_world_cancel_standing_order;
    let _: unsafe extern "C" fn(*mut ScWorld, u64) -> u32 = sc_world_standing_order_stock;
    let _: unsafe extern "C" fn(*mut ScWorld) -> ScLaunches = sc_world_launches;
    let _: unsafe extern "C" fn(*mut ScWorld) -> ScStandingOrders = sc_world_standing_orders;
    let _: unsafe extern "C" fn(*mut ScWorld, *const ScCommand) -> bool = sc_world_order;
    let _: unsafe extern "C" fn(*mut ScWorld, u64, u64) -> bool = sc_world_set_tag;
    let _: unsafe extern "C" fn(*const ScWorld, u64) -> u64 = sc_world_tag;
};

#[cfg(test)]
mod tests;
