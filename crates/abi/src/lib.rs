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
/// `units` points into engine-owned memory and is valid until the next call that
/// mutates the world (`sc_world_step`, `sc_world_frame`, `sc_world_destroy`).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct ScFrame {
    pub units: *const ScUnitView,
    pub count: u32,
    pub _reserved: u32,
    pub tick: u64,
}

const _: () = {
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
