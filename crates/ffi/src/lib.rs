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
    EmitterCycle, EmsProfile, LinkRules, Order, OutcomeTable, Position, Pursuer, Rules, Sensor,
    SensorParams, Sim, StableId, StandingOrder, Terrain, UnitSpec, Velocity,
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
    if t._reserved != 0 || heights.is_null() || height_count > u64::from(u32::MAX) {
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
            let e = sc_world_engagements(std::ptr::null_mut());
            assert!(e.events.is_null());
            assert_eq!(e.count, 0);
            let l = sc_world_launches(std::ptr::null_mut());
            assert!(l.launches.is_null());
            assert_eq!(l.count, 0);
            let s = sc_world_standing_orders(std::ptr::null_mut());
            assert!(s.orders.is_null());
            assert_eq!(s.count, 0);
            assert_eq!(
                sc_world_add_standing_order(std::ptr::null_mut(), std::ptr::null()),
                0
            );
            assert!(!sc_world_cancel_standing_order(std::ptr::null_mut(), 1));
            assert_eq!(
                sc_world_standing_order_stock(std::ptr::null_mut(), 1),
                u32::MAX
            );
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

    fn unit_at(x: f64, y: f64, aff: u8, kind: u16) -> ScUnitSpec {
        ScUnitSpec {
            x,
            y,
            erp_w: 100.0,
            kind,
            affiliation: aff,
            ..ScUnitSpec::default()
        }
    }

    fn engagements_of(w: *mut ScWorld) -> Vec<ScEngagement> {
        // SAFETY: `w` is a live handle on this thread; the events are copied out before any
        // other call on it, and only read when there are some (NULL otherwise).
        unsafe {
            let e = sc_world_engagements(w);
            if e.count == 0 {
                assert!(e.events.is_null());
                return Vec::new();
            }
            std::slice::from_raw_parts(e.events, e.count as usize).to_vec()
        }
    }

    fn frame_of(w: *mut ScWorld) -> Vec<ScUnitView> {
        // SAFETY: `w` is a live handle on this thread; the units are copied out before any
        // other call on it.
        unsafe {
            let f = sc_world_frame(w);
            let mut v = std::slice::from_raw_parts(f.units, f.count as usize).to_vec();
            v.sort_by_key(|u| u.id);
            v
        }
    }

    #[test]
    fn setup_calls_reject_bad_input_and_change_nothing() {
        use simcore_abi::affiliation::{FRIENDLY, HOSTILE};
        use simcore_engine::kind;
        let w = sc_world_create(1);
        // SAFETY: `w` is a live handle used only on this thread and destroyed once; every
        // pointer passed points to a live local of the right type and count.
        unsafe {
            let null = std::ptr::null_mut();
            let good_rules = ScRules {
                detection_max_range_m: 5000.0,
                default_chance: 0.5,
                ..ScRules::default()
            };
            assert!(!sc_world_set_rules(null, &good_rules, std::ptr::null(), 0));
            assert!(!sc_world_set_rules(
                w,
                std::ptr::null(),
                std::ptr::null(),
                0
            ));
            assert!(!sc_world_set_rules(w, &good_rules, std::ptr::null(), 1));
            for bad in [
                ScRules {
                    _reserved: 1,
                    ..good_rules
                },
                ScRules {
                    default_chance: 1.5,
                    ..good_rules
                },
                ScRules {
                    detection_max_range_m: -1.0,
                    ..good_rules
                },
                ScRules {
                    sensor_base_range_m: f64::INFINITY,
                    ..good_rules
                },
                ScRules {
                    sensor_reference_power_w: f64::NAN,
                    ..good_rules
                },
            ] {
                assert!(!sc_world_set_rules(w, &bad, std::ptr::null(), 0), "{bad:?}");
            }
            let o = ScOutcome {
                outcome_class: 1,
                target_kind: 4,
                _reserved: 0,
                chance: 1.0,
            };
            for bad in [
                [o, o],
                [
                    o,
                    ScOutcome {
                        chance: -0.1,
                        target_kind: 5,
                        ..o
                    },
                ],
                [
                    o,
                    ScOutcome {
                        _reserved: 1,
                        target_kind: 5,
                        ..o
                    },
                ],
            ] {
                assert!(
                    !sc_world_set_rules(w, &good_rules, bad.as_ptr(), 2),
                    "{bad:?}"
                );
            }
            assert_eq!(
                (*w).sim.rules(),
                &Rules::default(),
                "rejections change nothing"
            );
            let two = [
                o,
                ScOutcome {
                    target_kind: 5,
                    chance: 0.0,
                    ..o
                },
            ];
            assert!(sc_world_set_rules(w, &good_rules, two.as_ptr(), 2));
            let r = (*w).sim.rules();
            assert_eq!(r.outcomes.chance(1, 4), 1.0);
            assert_eq!(r.outcomes.chance(1, 5), 0.0);
            assert_eq!(r.outcomes.chance(9, 9), 0.5);

            let link = ScLinkRules {
                base_delay_ticks: 3,
                max_delay_ticks: 100,
                ..ScLinkRules::default()
            };
            assert!(!sc_world_set_link_rules(w, std::ptr::null()));
            assert!(!sc_world_set_link_rules(
                w,
                &ScLinkRules {
                    jam_radius_m: -1.0,
                    ..link
                }
            ));
            assert!(!sc_world_set_link_rules(
                w,
                &ScLinkRules {
                    delay_ticks_per_km: f64::NAN,
                    ..link
                }
            ));
            assert!(!sc_world_set_link_rules(
                w,
                &ScLinkRules {
                    _reserved: 7,
                    ..link
                }
            ));
            assert_eq!(sc_world_link_delay(w, 1), 0);
            assert!(sc_world_set_link_rules(w, &link));
            assert_eq!(sc_world_link_delay(w, 1), 3);

            let t = ScTerrain {
                spacing_m: 100.0,
                nx: 2,
                ny: 2,
                los_samples: 8,
                ..ScTerrain::default()
            };
            let h = [0.0f32, 10.0, 20.0, 30.0];
            assert!(!sc_world_set_terrain(w, &t, std::ptr::null(), 4));
            assert!(
                !sc_world_set_terrain(w, &t, h.as_ptr(), 3),
                "count must be nx * ny"
            );
            assert!(!sc_world_set_terrain(
                w,
                &ScTerrain {
                    los_samples: 1025,
                    ..t
                },
                h.as_ptr(),
                4
            ));
            assert!(!sc_world_set_terrain(
                w,
                &ScTerrain { _reserved: 1, ..t },
                h.as_ptr(),
                4
            ));
            let nan = [0.0f32, f32::NAN, 0.0, 0.0];
            assert!(!sc_world_set_terrain(w, &t, nan.as_ptr(), 4));
            assert!((*w).sim.terrain().is_flat());
            assert!(sc_world_set_terrain(w, &t, h.as_ptr(), 4));
            assert_eq!((*w).sim.terrain().height_at(100.0, 100.0), 30.0);
            assert!(sc_world_set_terrain(
                w,
                std::ptr::null(),
                std::ptr::null(),
                0
            ));
            assert!((*w).sim.terrain().is_flat());

            let radar = unit_at(500.0, 0.0, HOSTILE, kind::GROUND_RADAR);
            let strike = ScUnitSpec {
                options: spawn::PURSUER,
                target_id: 2,
                speed_mps: 50.0,
                max_turn_rate_rad_s: 1.0,
                engagement_radius_m: 5.0,
                ..unit_at(0.0, 0.0, FRIENDLY, kind::FIXED_WING_DRONE)
            };
            assert_eq!(sc_world_spawn(null, &radar), 0);
            assert_eq!(sc_world_spawn(w, std::ptr::null()), 0);
            assert_eq!(sc_world_spawn(w, &strike), 0, "target 2 does not exist yet");
            for bad in [
                ScUnitSpec {
                    x: f64::NAN,
                    ..radar
                },
                ScUnitSpec {
                    vz: f64::INFINITY,
                    ..radar
                },
                ScUnitSpec {
                    erp_w: -1.0,
                    ..radar
                },
                ScUnitSpec {
                    affiliation: 4,
                    ..radar
                },
                ScUnitSpec {
                    kind: kind::COMMAND_NODE,
                    ..radar
                },
                ScUnitSpec {
                    options: 1 << 4,
                    ..radar
                },
                ScUnitSpec {
                    options: spawn::EMITTING | spawn::CYCLE,
                    ..radar
                },
                ScUnitSpec {
                    _reserved: 1,
                    ..radar
                },
            ] {
                assert_eq!(sc_world_spawn(w, &bad), 0, "{bad:?}");
            }
            assert_eq!(sc_world_status(w).unit_count, 1, "rejections spawn nothing");
            assert_eq!(sc_world_spawn(w, &radar), 2);
            for bad in [
                ScUnitSpec {
                    speed_mps: 0.0,
                    ..strike
                },
                ScUnitSpec {
                    lead: 1.5,
                    ..strike
                },
                ScUnitSpec {
                    max_turn_rate_rad_s: -1.0,
                    ..strike
                },
                ScUnitSpec {
                    engagement_radius_m: f64::NAN,
                    ..strike
                },
                ScUnitSpec {
                    target_id: 99,
                    ..strike
                },
            ] {
                assert_eq!(sc_world_spawn(w, &bad), 0, "{bad:?}");
            }
            assert_eq!(
                sc_world_spawn(w, &strike),
                3,
                "ids are not used up by rejections"
            );

            let s = ScSensor {
                base_range_m: 1000.0,
                reference_power_w: 1.0,
            };
            assert!(!sc_world_add_sensor(null, 1, &s));
            assert!(!sc_world_add_sensor(w, 99, &s));
            assert!(!sc_world_add_sensor(
                w,
                1,
                &ScSensor {
                    base_range_m: -1.0,
                    ..s
                }
            ));
            assert!(sc_world_add_sensor(w, 1, &s));
            assert!(sc_world_add_sensor(w, 2, std::ptr::null()));
            sc_world_destroy(w);
        }
    }

    #[test]
    fn scenario_built_over_the_c_api_plays_and_survives_save_restore() {
        use simcore_abi::affiliation::{FRIENDLY, HOSTILE};
        let w = sc_world_create(42);
        // SAFETY: `w` and `r` are live handles used only on this thread, each destroyed once;
        // pointers point to live locals; snapshot bytes are copied before the next call on `w`.
        unsafe {
            let rules = ScRules {
                detection_max_range_m: 20_000.0,
                sensor_base_range_m: 2_000.0,
                sensor_reference_power_w: 1.0,
                default_chance: 0.0,
                exposure_threshold_ticks: 3,
                exposure_cap_ticks: 30,
                exposure_decay_per_tick: 1,
                _reserved: 0,
            };
            let hit = [ScOutcome {
                outcome_class: 7,
                target_kind: kind::GROUND_RADAR,
                _reserved: 0,
                chance: 1.0,
            }];
            assert!(sc_world_set_rules(w, &rules, hit.as_ptr(), 1));
            let link = ScLinkRules {
                base_delay_ticks: 2,
                delay_ticks_per_km: 1.0,
                max_delay_ticks: 60,
                ..ScLinkRules::default()
            };
            assert!(sc_world_set_link_rules(w, &link));
            assert!(sc_world_add_sensor(w, 1, std::ptr::null()));
            // A hostile radar 3 km east, always on: 50 kW is heard out to 2 km * sqrt(50000).
            let radar = sc_world_spawn(
                w,
                &ScUnitSpec {
                    erp_w: 50_000.0,
                    options: spawn::EMITTING,
                    ..unit_at(3_000.0, 0.0, HOSTILE, kind::GROUND_RADAR)
                },
            );
            // A friendly quadcopter on a duty cycle, with its own sensor.
            let drone = sc_world_spawn(
                w,
                &ScUnitSpec {
                    z: 100.0,
                    erp_w: 1.0,
                    options: spawn::CYCLE | spawn::SENSOR,
                    cycle_on_ticks: 5,
                    cycle_off_ticks: 10,
                    ..unit_at(-200.0, 0.0, FRIENDLY, kind::QUADCOPTER)
                },
            );
            // A strike on the radar, launched eastward from the node.
            let strike = sc_world_spawn(
                w,
                &ScUnitSpec {
                    vx: 1.0,
                    options: spawn::PURSUER,
                    target_id: radar,
                    outcome_class: 7,
                    speed_mps: 100.0,
                    max_turn_rate_rad_s: 1.0,
                    engagement_radius_m: 10.0,
                    ..unit_at(0.0, 0.0, FRIENDLY, kind::FIXED_WING_DRONE)
                },
            );
            assert_eq!((radar, drone, strike), (2, 3, 4));
            let f = frame_of(w);
            assert_eq!(f.len(), 4);
            assert_eq!(
                (f[1].x, f[1].kind, f[1].affiliation),
                (3_000.0, kind::GROUND_RADAR, HOSTILE)
            );
            assert!(f[1].flags & simcore_abi::flags::EMITTING != 0);
            assert!(
                f[2].flags & simcore_abi::flags::EMITTING != 0,
                "cycle on at tick 0"
            );
            // 2 base + ceil(3 km * 1/km).
            assert_eq!(sc_world_link_delay(w, radar), 5);
            assert!(
                sc_world_command(
                    w,
                    &ScCommand {
                        unit_id: drone,
                        x: -200.0,
                        y: 300.0,
                        z: 100.0,
                        speed_mps: 10.0,
                        type_: command::MOVE,
                        ..ScCommand::default()
                    }
                ) > 0
            );

            sc_world_step(w, 10);
            assert_eq!(
                sc_world_status(w).hostile_detected,
                1,
                "the node hears the radar"
            );
            let saved = {
                let b = sc_world_save(w);
                std::slice::from_raw_parts(b.data, b.len as usize).to_vec()
            };
            let r = sc_world_restore(saved.as_ptr(), saved.len() as u64);
            assert!(!r.is_null());
            assert!(engagements_of(w).is_empty(), "nothing has engaged yet");
            // 3 km at 100 m/s: about 30 s, 450 ticks. Both worlds play it identically.
            let mut events = Vec::new();
            for _ in 0..60 {
                sc_world_step(w, 10);
                sc_world_step(r, 10);
                assert_eq!(frame_of(w), frame_of(r));
                assert_eq!(sc_world_status(w), sc_world_status(r));
                assert_eq!(engagements_of(w), engagements_of(r));
                events.extend(engagements_of(w));
            }
            // The strike's one engagement: a kill, reported where the radar stood.
            let [e] = events[..] else {
                panic!("one engagement: {events:?}");
            };
            assert_eq!(
                (
                    e.pursuer_id,
                    e.target_id,
                    e.target_kind,
                    e.target_affiliation
                ),
                (strike, radar, kind::GROUND_RADAR, HOSTILE)
            );
            assert_eq!(e.flags, engagement::SUCCESS | engagement::TARGET_REMOVED);
            assert_eq!((e.x, e.y, e.z), (3_000.0, 0.0, 0.0));
            assert!((400..=500).contains(&e.tick), "engaged at tick {}", e.tick);
            let s = sc_world_status(w);
            assert_eq!(
                s.unit_count, 2,
                "the strike removed the radar and itself: {s:?}"
            );
            let ids: Vec<u64> = frame_of(w).iter().map(|u| u.id).collect();
            assert_eq!(ids, [1, drone]);
            let d = frame_of(w)[1];
            assert!(
                (d.y - 300.0).abs() < 0.5 && d.vy == 0.0,
                "the drone arrived: {d:?}"
            );
            // Ids are never reused.
            assert_eq!(
                sc_world_spawn(w, &unit_at(0.0, 0.0, FRIENDLY, kind::QUADCOPTER)),
                5
            );
            sc_world_destroy(w);
            sc_world_destroy(r);
        }
    }

    #[test]
    fn setup_coordinates_are_relative_to_the_command_node() {
        let w = sc_world_create(1);
        // SAFETY: `w` is a live handle used only on this thread and destroyed once.
        unsafe {
            (*w).sim.set_command_node_position(Position {
                x: 1e6,
                y: -2e6,
                z: 50.0,
            });
            let id = sc_world_spawn(w, &unit_at(10.0, 20.0, 1, simcore_engine::kind::QUADCOPTER));
            let p = (*w).sim.position(StableId(id)).unwrap();
            assert_eq!((p.x, p.y, p.z), (1e6 + 10.0, -2e6 + 20.0, 50.0));
            let t = ScTerrain {
                origin_x: -5.0,
                origin_y: 5.0,
                spacing_m: 1.0,
                nx: 1,
                ny: 1,
                ..ScTerrain::default()
            };
            assert!(sc_world_set_terrain(w, &t, [7.0f32].as_ptr(), 1));
            let (ox, oy, ..) = (*w).sim.terrain().parts().unwrap();
            assert_eq!((ox, oy), (1e6 - 5.0, -2e6 + 5.0));
            sc_world_destroy(w);
        }
    }

    fn launches_of(w: *mut ScWorld) -> Vec<ScLaunch> {
        // SAFETY: `w` is a live handle on this thread; the launches are copied out before any
        // other call on it, and only read when there are some (NULL otherwise).
        unsafe {
            let l = sc_world_launches(w);
            if l.count == 0 {
                assert!(l.launches.is_null());
                return Vec::new();
            }
            std::slice::from_raw_parts(l.launches, l.count as usize).to_vec()
        }
    }

    fn sop(x: f64, y: f64) -> ScStandingOrder {
        ScStandingOrder {
            x,
            y,
            radius_m: 300.0,
            kinds: 0,
            stock: 3,
            cooldown_ticks: 15,
            _reserved: 0,
            launch: ScUnitSpec {
                z: 5.0,
                erp_w: 1.0,
                kind: kind::FIXED_WING_DRONE,
                affiliation: 1,
                options: spawn::PURSUER | spawn::EMITTING,
                outcome_class: 1,
                speed_mps: 80.0,
                max_turn_rate_rad_s: 1.0,
                engagement_radius_m: 10.0,
                ..ScUnitSpec::default()
            },
        }
    }

    #[test]
    fn standing_orders_over_the_c_api() {
        let w = sc_world_create(4);
        let rules = ScRules {
            detection_max_range_m: 20_000.0,
            sensor_base_range_m: 20_000.0,
            sensor_reference_power_w: 1.0,
            exposure_threshold_ticks: 1,
            exposure_cap_ticks: 1,
            exposure_decay_per_tick: 1,
            ..ScRules::default()
        };
        // SAFETY: `w` (and the restored `r`) are live handles used only on this thread and
        // destroyed once; every pointer passed is to a live local.
        unsafe {
            assert!(sc_world_set_rules(w, &rules, std::ptr::null(), 0));
            assert!(sc_world_add_sensor(w, 1, std::ptr::null()));
            let node = Position {
                x: 50_000.0,
                y: 0.0,
                z: 0.0,
            };
            (*w).sim.set_command_node_position(node);
            let mut radar = unit_at(2_000.0, 0.0, 2, kind::GROUND_RADAR);
            radar.options = spawn::EMITTING;
            let target = sc_world_spawn(w, &radar);

            // Rejected without changing anything: no pursuit, a sensor launch, a hostile
            // launch, reserved bits, the stock sentinel, a bad pursuit, a bad zone.
            let mut bad = Vec::new();
            let cases: [fn(&mut ScStandingOrder); 8] = [
                |o| o.launch.options = spawn::EMITTING,
                |o| o.launch.options |= spawn::SENSOR,
                |o| o.launch.affiliation = 2,
                |o| o._reserved = 1,
                |o| o.launch._reserved = 1,
                |o| o.stock = u32::MAX,
                |o| o.launch.speed_mps = 0.0,
                |o| o.radius_m = f64::NAN,
            ];
            for f in cases {
                let mut o = sop(2_000.0, 0.0);
                f(&mut o);
                bad.push(sc_world_add_standing_order(w, &o));
            }
            assert_eq!(bad, [0; 8]);
            assert_eq!((*w).sim.standing_orders().count(), 0);

            let id = sc_world_add_standing_order(w, &sop(2_000.0, 0.0));
            assert_eq!(id, 1);
            // The zone was fixed in the world when the order was added; the launch point
            // follows the node.
            let (_, o) = (*w).sim.standing_orders().next().unwrap();
            assert_eq!((o.center_x, o.center_y), (52_000.0, 0.0));
            (*w).sim.set_command_node_position(Position {
                x: 51_000.0,
                ..node
            });
            sc_world_step(w, 1);
            let l = launches_of(w);
            assert_eq!(l.len(), 1);
            assert_eq!((l[0].tick, l[0].order_id, l[0].target_id), (1, id, target));
            let p = (*w).sim.position(StableId(l[0].pursuer_id)).unwrap();
            assert_eq!((p.x, p.y, p.z), (51_000.0, 0.0, 5.0));
            assert_eq!(sc_world_standing_order_stock(w, id), 2);
            assert_eq!(launches_of(w), l, "calling again returns the same launches");

            // Saved with the world: the restored one carries on identically.
            let b = sc_world_save(w);
            let r = sc_world_restore(b.data, b.len);
            assert!(!r.is_null());
            assert_eq!(sc_world_standing_order_stock(r, id), 2);
            assert!(launches_of(r).is_empty(), "empty after a restore");
            for _ in 0..300 {
                sc_world_step(w, 1);
                sc_world_step(r, 1);
                assert_eq!(launches_of(w), launches_of(r));
                assert_eq!(engagements_of(w), engagements_of(r));
            }
            assert!(sc_world_cancel_standing_order(w, id));
            assert!(!sc_world_cancel_standing_order(w, id));
            assert_eq!(sc_world_standing_order_stock(w, id), u32::MAX);
            sc_world_destroy(r);
            sc_world_destroy(w);
        }
    }

    fn standing_orders_of(w: *mut ScWorld) -> Vec<ScStandingOrderView> {
        // SAFETY: `w` is a live handle on this thread; the orders are copied out before any
        // other call on it, and only read when there are some (NULL otherwise).
        unsafe {
            let s = sc_world_standing_orders(w);
            if s.count == 0 {
                assert!(s.orders.is_null());
                return Vec::new();
            }
            std::slice::from_raw_parts(s.orders, s.count as usize).to_vec()
        }
    }

    #[test]
    fn standing_orders_are_listed_relative_to_the_command_node() {
        let w = sc_world_create(4);
        assert!(standing_orders_of(w).is_empty(), "none in a new world");
        let rules = ScRules {
            detection_max_range_m: 20_000.0,
            sensor_base_range_m: 20_000.0,
            sensor_reference_power_w: 1.0,
            exposure_threshold_ticks: 1,
            exposure_cap_ticks: 1,
            exposure_decay_per_tick: 1,
            ..ScRules::default()
        };
        // SAFETY: `w` (and the restored `r`) are live handles used only on this thread and
        // destroyed once; every pointer passed is to a live local.
        unsafe {
            assert!(sc_world_set_rules(w, &rules, std::ptr::null(), 0));
            assert!(sc_world_add_sensor(w, 1, std::ptr::null()));
            let node = Position {
                x: 50_000.0,
                y: 0.0,
                z: 0.0,
            };
            (*w).sim.set_command_node_position(node);
            let mut radar = unit_at(2_000.0, 0.0, 2, kind::GROUND_RADAR);
            radar.options = spawn::EMITTING;
            let target = sc_world_spawn(w, &radar);

            let id = sc_world_add_standing_order(w, &sop(2_000.0, 0.0));
            let mut idle = sop(-500.0, 250.0);
            (idle.stock, idle.kinds, idle.cooldown_ticks) = (0, 1 << kind::JAMMER, 40);
            let empty = sc_world_add_standing_order(w, &idle);
            assert_eq!((id, empty), (1, 2));
            let view = |id, x, y, radius_m, kinds, remaining, stock, ready_tick, cooldown_ticks| {
                ScStandingOrderView {
                    id,
                    x,
                    y,
                    radius_m,
                    kinds,
                    remaining,
                    stock,
                    ready_tick,
                    cooldown_ticks,
                    _reserved: 0,
                }
            };
            let idle_at = |x, y| view(empty, x, y, 300.0, 1 << kind::JAMMER, 0, 0, 0, 40);
            assert_eq!(
                standing_orders_of(w),
                [
                    view(id, 2_000.0, 0.0, 300.0, 0, 3, 3, 0, 15),
                    idle_at(-500.0, 250.0)
                ],
                "in id order, the out-of-stock one included"
            );

            // The zones stay fixed in the world, so their centres move in the frame as the
            // node moves, exactly as the frame's units do.
            (*w).sim.set_command_node_position(Position {
                x: 51_000.0,
                y: -100.0,
                ..node
            });
            sc_world_step(w, 1);
            let l = launches_of(w);
            assert_eq!((l[0].order_id, l[0].target_id), (id, target));
            let listed = standing_orders_of(w);
            assert_eq!(
                listed,
                [
                    view(id, 1_000.0, 100.0, 300.0, 0, 2, 3, 16, 15),
                    idle_at(-1_500.0, 350.0)
                ]
            );
            let f = sc_world_frame(w);
            let frame = std::slice::from_raw_parts(f.units, f.count as usize);
            let radar_view = frame.iter().find(|u| u.id == target).unwrap();
            assert_eq!((radar_view.x, radar_view.y), (listed[0].x, listed[0].y));
            assert_eq!(
                standing_orders_of(w),
                listed,
                "calling again lists the same"
            );

            // Saved with the world: the restored one lists the same orders.
            let b = sc_world_save(w);
            let r = sc_world_restore(b.data, b.len);
            assert!(!r.is_null());
            assert_eq!(standing_orders_of(r), listed);

            assert!(sc_world_cancel_standing_order(w, id));
            assert_eq!(standing_orders_of(w), [idle_at(-1_500.0, 350.0)]);
            assert!(sc_world_cancel_standing_order(w, empty));
            assert!(standing_orders_of(w).is_empty(), "NULL once none are left");
            assert_eq!(standing_orders_of(r).len(), 2, "each world its own");
            sc_world_destroy(r);
            sc_world_destroy(w);
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
