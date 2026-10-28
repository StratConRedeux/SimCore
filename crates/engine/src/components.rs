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

/// Steers toward another unit and engages it when the swept approach comes within
/// `engagement_radius_m`. Consumed by the engagement, whatever the outcome.
#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub struct Pursuer {
    pub target: StableId,
    /// Constant speed in m/s.
    pub speed: f64,
    /// Maximum heading change in rad/s.
    pub max_turn_rate: f64,
    /// Lead factor in `[0, 1]`: 0 is pure pursuit.
    pub lead: f64,
    /// Engagement radius in m.
    pub engagement_radius_m: f64,
    /// Row of the outcome table (see [`crate::Rules`]).
    pub outcome_class: u16,
}

/// Passive listening parameters: an emitter of `reference_power_w` is heard out to
/// `base_range_m`, scaling with the square root of power (see [`simcore_rules::detection_range`]).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SensorParams {
    pub base_range_m: f64,
    pub reference_power_w: f64,
}

/// A passive sensor. It observes emitting units of the opposing affiliation in range and in
/// line of sight. `None` uses the shared [`crate::Rules::sensor`] parameters.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq)]
pub struct Sensor {
    pub params: Option<SensorParams>,
}

/// How long a unit has been observed by opposing sensors.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Exposure {
    /// Accumulated exposure in ticks: +1 per observed tick (capped), decaying otherwise.
    pub ticks: u32,
    /// Whether at least one opposing sensor observed the unit this tick.
    pub observed: bool,
}

/// Whether the opposing side currently has the unit detected (exposure at the threshold).
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Detected(pub bool);

/// Set when an engagement removes the unit. The entity is despawned later, outside
/// `Sim::step`, so removals do not allocate mid-tick; until then every system skips it and it
/// is gone from the id index, the frame and the heatmap.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Gone(pub bool);
