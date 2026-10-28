//! Plain-data types that cross the C boundary. Must match `include/simcore.h` byte for byte.
//!
//! Changing any layout here is an ABI break: bump `ABI_VERSION` and update the header.

#![no_std]

/// Bumped on every layout or signature change.
pub const ABI_VERSION: u32 = 1;

/// Simulation rate. The engine advances in fixed ticks of `1 / TICKS_PER_SECOND` seconds.
pub const TICKS_PER_SECOND: u32 = 15;

/// Affiliation codes (`ScUnitView::affiliation`).
pub mod affiliation {
    pub const UNKNOWN: u8 = 0;
    pub const FRIENDLY: u8 = 1;
    pub const HOSTILE: u8 = 2;
    pub const NEUTRAL: u8 = 3;
}

/// Bit flags (`ScUnitView::flags`).
pub mod flags {
    /// The unit is radiating (radar, datalink, jammer, weapon guidance).
    pub const EMITTING: u8 = 1 << 0;
    /// The unit is the player's command node, which is the frame's origin.
    pub const COMMAND_NODE: u8 = 1 << 1;
    /// The unit is currently detected by the opposing side's sensors.
    pub const DETECTED: u8 = 1 << 2;
}

/// One unit as the frontend sees it.
///
/// Position is `f32` metres relative to the player's command node; velocity is `f32` m/s in
/// the world frame, not relative to the node's motion. Both are local east-north-up (x east,
/// y north, z up). The engine keeps `f64` world coordinates internally and converts when it
/// builds a frame.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ScUnitView {
    /// Stable for the entity's lifetime and across save/restore.
    pub id: u64,
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub vx: f32,
    pub vy: f32,
    pub vz: f32,
    /// Unit type code. The engine defines the table.
    pub kind: u16,
    pub affiliation: u8,
    pub flags: u8,
    /// Current effective radiated power in watts; 0 when not emitting.
    pub ems_power_w: f32,
}

/// A read-only view of the latest frame.
///
/// `units` points into engine-owned memory and is valid until the next `sc_world_frame` or
/// `sc_world_destroy` on the same world.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct ScFrame {
    pub units: *const ScUnitView,
    pub count: u32,
    pub _reserved: u32,
    pub tick: u64,
}

/// Command types (`ScCommand::type_`, `type` in C).
pub mod command {
    /// Travel straight to `(x, y, z)` at `speed_mps`, then stop.
    pub const MOVE: u16 = 1;
    /// Cancel any move and stop.
    pub const HALT: u16 = 2;
    /// A pursuer pursues `target_id` instead.
    pub const RETARGET: u16 = 3;
    /// `radiate` 1: follow the duty cycle (or emit); 0: stay silent.
    pub const EMISSIONS: u16 = 4;
}

/// One command from the app to a friendly unit, applied after the link delay.
///
/// Positions are `f32` metres relative to the command node at the moment the command is
/// issued (the same frame as [`ScUnitView`]). Fields a type does not use are ignored; the
/// reserved fields must be zero.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ScCommand {
    /// Recipient.
    pub unit_id: u64,
    /// `RETARGET`: the new target.
    pub target_id: u64,
    /// `MOVE`: destination.
    pub x: f32,
    pub y: f32,
    pub z: f32,
    /// `MOVE`: speed in m/s, positive.
    pub speed_mps: f32,
    /// One of [`command`].
    pub type_: u16,
    /// `EMISSIONS`: 1 radiate, 0 silent.
    pub radiate: u8,
    pub _reserved0: u8,
    pub _reserved1: u32,
}

/// A summary of the world for the HUD. Counts are of live units.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScStatus {
    pub tick: u64,
    /// Units, including the command node.
    pub unit_count: u32,
    /// Ticks a command to the command node itself would take now: the base delay plus any
    /// jamming at the node.
    pub link_delay_ticks: u32,
    /// Friendly units radiating now.
    pub friendly_emitting: u32,
    /// Friendly units the hostile side has detected.
    pub friendly_detected: u32,
    /// Hostile units the friendly side has detected.
    pub hostile_detected: u32,
    /// Commands issued and not yet delivered.
    pub pending_commands: u32,
    /// Commands dropped at delivery because their unit was gone (saturating).
    pub dropped_commands: u32,
    /// Successful engagements against the command node (saturating).
    pub command_node_hits: u32,
    /// Removed units whose storage is not yet released (see `sc_world_despawn_removed`).
    pub awaiting_cleanup: u32,
    pub _reserved: u32,
}

/// A read-only byte buffer owned by the engine.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct ScBytes {
    pub data: *const u8,
    pub len: u64,
}

/// Gameplay rules for `sc_world_set_rules`. Every `f64` must be finite and non-negative, and
/// chances at most 1. The outcome table's entries are passed separately as [`ScOutcome`]s.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ScRules {
    /// Hard cap on any detection range, in m. 0 disables observation.
    pub detection_max_range_m: f64,
    /// Parameters for sensors without their own (see [`ScSensor`]).
    pub sensor_base_range_m: f64,
    pub sensor_reference_power_w: f64,
    /// Success chance for (class, kind) pairs not in the outcome table.
    pub default_chance: f64,
    /// Exposure ticks at which a unit becomes detected. 0 is treated as 1.
    pub exposure_threshold_ticks: u32,
    /// Exposure never exceeds this (nor drops below the threshold as a cap).
    pub exposure_cap_ticks: u32,
    /// Exposure lost per tick while not observed.
    pub exposure_decay_per_tick: u32,
    pub _reserved: u32,
}

/// One outcome table entry: a pursuer of `outcome_class` engaging a target of `target_kind`
/// succeeds with `chance`, in `[0, 1]`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ScOutcome {
    pub outcome_class: u16,
    pub target_kind: u16,
    pub _reserved: u32,
    pub chance: f64,
}

/// Link-delay rules for `sc_world_set_link_rules`. The `f64`s must be finite and non-negative.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ScLinkRules {
    /// Added per km from the command node to the recipient, rounded up.
    pub delay_ticks_per_km: f64,
    /// A recipient within this distance (3D, m) of an emitting hostile jammer is jammed.
    pub jam_radius_m: f64,
    pub base_delay_ticks: u32,
    /// Added while the recipient is jammed.
    pub jam_delay_ticks: u32,
    /// No delay exceeds this.
    pub max_delay_ticks: u32,
    pub _reserved: u32,
}

/// A heightfield for `sc_world_set_terrain`: `nx * ny` samples `spacing_m` apart, row-major,
/// sample `(i, j)` at `(origin_x + i * spacing_m, origin_y + j * spacing_m)`. The origin is in
/// metres relative to the command node when the call is made.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ScTerrain {
    pub origin_x: f64,
    pub origin_y: f64,
    /// Positive.
    pub spacing_m: f64,
    pub nx: u32,
    pub ny: u32,
    /// Line-of-sight samples per check, at most 1024.
    pub los_samples: u32,
    pub _reserved: u32,
}

/// Spawn options (`ScUnitSpec::options`).
pub mod spawn {
    /// Radiates continuously. Not with `CYCLE`.
    pub const EMITTING: u8 = 1 << 0;
    /// Radiates on the duty cycle in `cycle_*`.
    pub const CYCLE: u8 = 1 << 1;
    /// A pursuer: `target_id`, `outcome_class` and the pursuit fields apply.
    pub const PURSUER: u8 = 1 << 2;
    /// A passive sensor with the shared rules' parameters.
    pub const SENSOR: u8 = 1 << 3;
}

/// One unit for `sc_world_spawn`.
///
/// Position is `f64` metres relative to the command node when the call is made; velocity is
/// m/s in the world frame, as in [`ScUnitView`]. Both east-north-up. A pursuer's velocity is only its initial heading: pursuit
/// steering sets its speed. Fields an option does not use are ignored; reserved fields and
/// unknown option bits must be zero.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ScUnitSpec {
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub vx: f64,
    pub vy: f64,
    pub vz: f64,
    /// Effective radiated power in watts while emitting; finite, non-negative.
    pub erp_w: f32,
    /// Unit type code; not the command node's.
    pub kind: u16,
    pub affiliation: u8,
    /// [`spawn`] bits.
    pub options: u8,
    /// `CYCLE`: on for `cycle_on_ticks`, off for `cycle_off_ticks`, offset by `cycle_phase`.
    pub cycle_on_ticks: u32,
    pub cycle_off_ticks: u32,
    pub cycle_phase: u32,
    /// `PURSUER`: row of the outcome table.
    pub outcome_class: u16,
    pub _reserved: u16,
    /// `PURSUER`: an existing unit.
    pub target_id: u64,
    /// `PURSUER`: constant speed, positive.
    pub speed_mps: f64,
    /// `PURSUER`: maximum heading change, non-negative.
    pub max_turn_rate_rad_s: f64,
    /// `PURSUER`: lead factor in `[0, 1]`; 0 is pure pursuit.
    pub lead: f64,
    /// `PURSUER`: engages when the swept approach comes this close, non-negative.
    pub engagement_radius_m: f64,
}

/// Passive sensor parameters for `sc_world_add_sensor`: an emitter of `reference_power_w` is
/// heard out to `base_range_m`, scaling with the square root of power. Finite, non-negative.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ScSensor {
    pub base_range_m: f64,
    pub reference_power_w: f64,
}

const _: () = {
    assert!(core::mem::size_of::<ScRules>() == 48);
    assert!(core::mem::offset_of!(ScRules, default_chance) == 24);
    assert!(core::mem::offset_of!(ScRules, exposure_threshold_ticks) == 32);
    assert!(core::mem::offset_of!(ScRules, _reserved) == 44);
    assert!(core::mem::size_of::<ScOutcome>() == 16);
    assert!(core::mem::offset_of!(ScOutcome, target_kind) == 2);
    assert!(core::mem::offset_of!(ScOutcome, chance) == 8);
    assert!(core::mem::size_of::<ScLinkRules>() == 32);
    assert!(core::mem::offset_of!(ScLinkRules, base_delay_ticks) == 16);
    assert!(core::mem::offset_of!(ScLinkRules, max_delay_ticks) == 24);
    assert!(core::mem::size_of::<ScTerrain>() == 40);
    assert!(core::mem::offset_of!(ScTerrain, nx) == 24);
    assert!(core::mem::offset_of!(ScTerrain, los_samples) == 32);
    assert!(core::mem::size_of::<ScUnitSpec>() == 112);
    assert!(core::mem::align_of::<ScUnitSpec>() == 8);
    assert!(core::mem::offset_of!(ScUnitSpec, vx) == 24);
    assert!(core::mem::offset_of!(ScUnitSpec, erp_w) == 48);
    assert!(core::mem::offset_of!(ScUnitSpec, kind) == 52);
    assert!(core::mem::offset_of!(ScUnitSpec, affiliation) == 54);
    assert!(core::mem::offset_of!(ScUnitSpec, options) == 55);
    assert!(core::mem::offset_of!(ScUnitSpec, cycle_on_ticks) == 56);
    assert!(core::mem::offset_of!(ScUnitSpec, cycle_phase) == 64);
    assert!(core::mem::offset_of!(ScUnitSpec, outcome_class) == 68);
    assert!(core::mem::offset_of!(ScUnitSpec, target_id) == 72);
    assert!(core::mem::offset_of!(ScUnitSpec, speed_mps) == 80);
    assert!(core::mem::offset_of!(ScUnitSpec, engagement_radius_m) == 104);
    assert!(core::mem::size_of::<ScSensor>() == 16);
    assert!(core::mem::size_of::<ScCommand>() == 40);
    assert!(core::mem::align_of::<ScCommand>() == 8);
    assert!(core::mem::offset_of!(ScCommand, target_id) == 8);
    assert!(core::mem::offset_of!(ScCommand, x) == 16);
    assert!(core::mem::offset_of!(ScCommand, speed_mps) == 28);
    assert!(core::mem::offset_of!(ScCommand, type_) == 32);
    assert!(core::mem::offset_of!(ScCommand, radiate) == 34);
    assert!(core::mem::offset_of!(ScCommand, _reserved1) == 36);
    assert!(core::mem::size_of::<ScStatus>() == 48);
    assert!(core::mem::offset_of!(ScStatus, unit_count) == 8);
    assert!(core::mem::offset_of!(ScStatus, awaiting_cleanup) == 40);
    assert!(core::mem::size_of::<ScBytes>() == 16);
    assert!(core::mem::size_of::<ScUnitView>() == 40);
    assert!(core::mem::align_of::<ScUnitView>() == 8);
    assert!(core::mem::offset_of!(ScUnitView, x) == 8);
    assert!(core::mem::offset_of!(ScUnitView, vx) == 20);
    assert!(core::mem::offset_of!(ScUnitView, kind) == 32);
    assert!(core::mem::offset_of!(ScUnitView, affiliation) == 34);
    assert!(core::mem::offset_of!(ScUnitView, flags) == 35);
    assert!(core::mem::offset_of!(ScUnitView, ems_power_w) == 36);
    assert!(core::mem::size_of::<ScFrame>() == 24);
};
