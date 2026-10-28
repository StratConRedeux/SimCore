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
/// Position and velocity are `f32` metres, relative to the player's command node,
/// in a local east-north-up frame (x east, y north, z up). The engine keeps `f64`
/// world coordinates internally and converts when it builds a frame.
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

const _: () = {
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
