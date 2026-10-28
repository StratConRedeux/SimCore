//! ECS components. World coordinates are `f64` metres, local east-north-up.

use bevy_ecs::prelude::*;

/// World position in metres (x east, y north, z up).
#[derive(Component, Clone, Copy, Debug, Default, PartialEq)]
pub struct Position {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

/// World velocity in metres per second.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq)]
pub struct Velocity {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

/// Electromagnetic signature.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq)]
pub struct EmsProfile {
    pub emitting: bool,
    /// Effective radiated power in watts while emitting.
    pub erp_w: f32,
}

impl EmsProfile {
    /// Power currently radiated: `erp_w` when emitting, otherwise 0.
    #[inline]
    pub fn current_power_w(&self) -> f32 {
        if self.emitting { self.erp_w } else { 0.0 }
    }
}

/// Fixed duty cycle for an emitter: on for `on_ticks`, off for `off_ticks`, offset by `phase`.
#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub struct EmitterCycle {
    pub on_ticks: u32,
    pub off_ticks: u32,
    pub phase: u32,
}

impl EmitterCycle {
    /// Whether the emitter is on at `tick`.
    #[inline]
    pub fn is_on(&self, tick: u64) -> bool {
        let period = u64::from(self.on_ticks) + u64::from(self.off_ticks);
        period != 0 && (tick + u64::from(self.phase)) % period < u64::from(self.on_ticks)
    }
}

/// Stable for the entity's lifetime and across save/restore. Never reused.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StableId(pub u64);

/// Affiliation code from [`simcore_abi::affiliation`].
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Affiliation(pub u8);

/// Unit type code from [`crate::kind`].
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnitKind(pub u16);

/// Marks the player's command node: exactly one per world, and the frame origin.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct CommandNode;
