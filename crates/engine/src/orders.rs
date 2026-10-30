//! Player commands, delivered after a link delay, and the move orders they start.
//!
//! A command is checked when it is issued, queued for `issue tick + link delay`, and applied at
//! the start of that tick, before any system runs. The link delay is computed once, at issue,
//! from the [`LinkRules`] resource. See `docs/rules.md`.

use bevy_ecs::prelude::*;
use simcore_abi::affiliation;

use crate::gameplay::IdIndex;
use crate::{
    Affiliation, DT, EmitterCycle, EmsProfile, Gone, Position, Pursuer, StableId, UnitKind,
    Velocity, kind,
};

/// Caller-supplied link-delay rules. The default delivers every command on the next tick.
#[derive(Resource, Clone, Copy, Debug, PartialEq)]
pub struct LinkRules {
    /// Delay of every command, in ticks.
    pub base_delay_ticks: u32,
    /// Added per km of distance from the command node to the recipient, rounded up.
    /// Non-positive or NaN adds nothing.
    pub delay_ticks_per_km: f64,
    /// A recipient within this distance (3D, m) of an emitting hostile jammer is jammed.
    /// Non-positive or NaN disables jamming.
    pub jam_radius_m: f64,
    /// Added while the recipient is jammed, in ticks.
    pub jam_delay_ticks: u32,
    /// No delay exceeds this, in ticks.
    pub max_delay_ticks: u32,
}

impl Default for LinkRules {
    fn default() -> LinkRules {
        LinkRules {
            base_delay_ticks: 0,
            delay_ticks_per_km: 0.0,
            jam_radius_m: 0.0,
            jam_delay_ticks: 0,
            max_delay_ticks: u32::MAX,
        }
    }
}

/// What a command tells its recipient to do.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Order {
    /// Travel in a straight line to `destination` (world m) at `speed_mps`, then stop.
    /// Replaces any earlier move. Not for pursuers.
    Move {
        destination: Position,
        speed_mps: f64,
    },
    /// Cancel any move and stop. Not for pursuers.
    Halt,
    /// Pursue `target` instead. Pursuers only.
    Retarget { target: StableId },
    /// Radiate (follow the duty cycle, or emit if there is none) or stay silent.
    Emissions { radiate: bool },
}

/// Why [`crate::Sim::command`] rejected a command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandError {
    /// The recipient does not exist.
    UnknownUnit,
    /// Only friendly units take commands.
    NotFriendly,
    /// Move and halt are for non-pursuers, retarget for pursuers.
    WrongUnitType,
    /// A non-finite destination, a non-positive or non-finite speed, or a retarget at a unit
    /// that does not exist or at the pursuer itself.
    InvalidOrder,
}

/// A queued command.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Pending {
    /// Applied at the start of the tick that begins at this [`crate::Sim::tick`].
    pub deliver_tick: u64,
    /// Command id: issue order, from 1.
    pub seq: u64,
    pub unit: StableId,
    pub order: Order,
}

/// A move in progress.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Move {
    pub unit: StableId,
    pub destination: Position,
    pub speed_mps: f64,
    /// The unit reaches the destination this tick and stops on the next.
    pub arriving: bool,
}

/// Moves in progress, in delivery order. Only these units are steered, so a world of
/// stationary units pays nothing per tick.
#[derive(Resource, Debug, Default)]
pub(crate) struct Moves(pub Vec<Move>);

/// Emission control: a silent unit stays off whatever its duty cycle says.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Emcon {
    pub silent: bool,
}

/// The command queue and its counters.
#[derive(Debug, Default)]
pub(crate) struct CommandQueue {
    /// Sorted by `(deliver_tick, seq)` descending, so the next delivery is at the end.
    pub pending: Vec<Pending>,
    /// Last command id handed out.
    pub last_seq: u64,
    /// Commands whose recipient (or retarget target) was gone at delivery, saturating.
    pub dropped: u32,
}

impl CommandQueue {
    pub fn push(&mut self, p: Pending) {
        let key = (p.deliver_tick, p.seq);
        // Descending order: insert after every entry with a larger key.
        let i = self
            .pending
            .partition_point(|q| (q.deliver_tick, q.seq) > key);
        self.pending.insert(i, p);
    }

    /// Applies every command due at the start of the tick beginning at `tick`.
    pub fn deliver(&mut self, world: &mut World, tick: u64) {
        while let Some(&p) = self.pending.last() {
            if p.deliver_tick > tick {
                break;
            }
            self.pending.pop();
            if !apply(world, p.unit, p.order) {
                self.dropped = self.dropped.saturating_add(1);
            }
        }
    }
}

/// Checks a command against the recipient's current state: the affiliation gate (only friendly
/// units take commands), then [`validate_order`].
pub(crate) fn validate(world: &World, unit: StableId, order: &Order) -> Result<(), CommandError> {
    let e = world
        .resource::<IdIndex>()
        .get(unit)
        .ok_or(CommandError::UnknownUnit)?;
    if world.entity(e).get::<Affiliation>().map(|a| a.0) != Some(affiliation::FRIENDLY) {
        return Err(CommandError::NotFriendly);
    }
    validate_order(world, unit, order)
}

/// Checks an order's type and values against a live unit of any affiliation: move and halt for
/// non-pursuers, retarget for pursuers (at another existing unit), emissions for any unit.
pub(crate) fn validate_order(
    world: &World,
    unit: StableId,
    order: &Order,
) -> Result<(), CommandError> {
    let index = world.resource::<IdIndex>();
    let e = index.get(unit).ok_or(CommandError::UnknownUnit)?;
    let pursuer = world.entity(e).contains::<Pursuer>();
    match *order {
        Order::Move {
            destination: d,
            speed_mps,
        } => {
            if pursuer {
                return Err(CommandError::WrongUnitType);
            }
            let finite = d.x.is_finite() && d.y.is_finite() && d.z.is_finite();
            if !finite || !(speed_mps > 0.0 && speed_mps.is_finite()) {
                return Err(CommandError::InvalidOrder);
            }
        }
        Order::Halt if pursuer => return Err(CommandError::WrongUnitType),
        Order::Halt => {}
        Order::Retarget { target } => {
            if !pursuer {
                return Err(CommandError::WrongUnitType);
            }
            if target == unit || index.get(target).is_none() {
                return Err(CommandError::InvalidOrder);
            }
        }
        Order::Emissions { .. } => {}
    }
    Ok(())
}

/// Applies one delivered command, or a direct order ([`crate::Sim::order`]). Returns `false`
/// if it had to be dropped.
pub(crate) fn apply(world: &mut World, unit: StableId, order: Order) -> bool {
    let index = world.resource::<IdIndex>();
    let Some(e) = index.get(unit) else {
        return false;
    };
    match order {
        Order::Move {
            destination,
            speed_mps,
        } => {
            let mut moves = world.resource_mut::<Moves>();
            let m = Move {
                unit,
                destination,
                speed_mps,
                arriving: false,
            };
            match moves.0.iter_mut().find(|m| m.unit == unit) {
                Some(existing) => *existing = m,
                None => moves.0.push(m),
            }
        }
        Order::Halt => {
            world.resource_mut::<Moves>().0.retain(|m| m.unit != unit);
            if let Some(mut v) = world.get_mut::<Velocity>(e) {
                *v = Velocity::default();
            }
        }
        Order::Retarget { target } => {
            if world.resource::<IdIndex>().get(target).is_none() {
                return false;
            }
            if let Some(mut p) = world.get_mut::<Pursuer>(e) {
                p.target = target;
            }
        }
        Order::Emissions { radiate } => {
            if let Some(mut c) = world.get_mut::<Emcon>(e) {
                c.silent = !radiate;
            }
            // A duty-cycled emitter is set by `update_emitters` this tick; any other follows
            // the order directly.
            if !world.entity(e).contains::<EmitterCycle>()
                && let Some(mut ems) = world.get_mut::<EmsProfile>(e)
            {
                ems.emitting = radiate;
            }
        }
    }
    true
}

/// Link delay in ticks for a recipient at `at`, with the command node at `node`.
pub(crate) fn link_delay(
    world: &World,
    jammers: &mut JammerQuery,
    node: Position,
    at: Position,
) -> u32 {
    let rules = *world.resource::<LinkRules>();
    let mut delay = u64::from(rules.base_delay_ticks);
    if rules.delay_ticks_per_km > 0.0 {
        let km = distance(node, at) / 1000.0;
        // Zero distance adds nothing, even at an infinite rate (inf * 0 is NaN). Otherwise the
        // float-to-int cast saturates: an infinite product, or an unmeasurably far recipient
        // (NaN distance), reaches the cap below.
        if km != 0.0 {
            let ticks = km * rules.delay_ticks_per_km;
            delay = delay.saturating_add(if ticks.is_nan() {
                u64::MAX
            } else {
                ticks.ceil() as u64
            });
        }
    }
    if rules.jam_delay_ticks > 0 && jammed(world, jammers, at, rules.jam_radius_m) {
        delay = delay.saturating_add(u64::from(rules.jam_delay_ticks));
    }
    // UINT32_MAX means "no such unit" over the C API, so no real delay reaches it.
    delay
        .min(u64::from(rules.max_delay_ticks))
        .min(u64::from(u32::MAX - 1)) as u32
}

/// Euclidean distance without overflow for any finite inputs: scaled by the largest component.
/// NaN if a coordinate difference is not finite.
fn distance(a: Position, b: Position) -> f64 {
    let (dx, dy, dz) = (b.x - a.x, b.y - a.y, b.z - a.z);
    let m = dx.abs().max(dy.abs()).max(dz.abs());
    if m == 0.0 {
        return 0.0;
    }
    if !m.is_finite() {
        return f64::NAN;
    }
    let (x, y, z) = (dx / m, dy / m, dz / m);
    m * (x * x + y * y + z * z).sqrt()
}

pub(crate) type JammerQuery = QueryState<(
    &'static Position,
    &'static Affiliation,
    &'static UnitKind,
    &'static EmsProfile,
    &'static Gone,
)>;

/// Whether an emitting hostile jammer is within `radius` of `at`.
fn jammed(world: &World, jammers: &mut JammerQuery, at: Position, radius: f64) -> bool {
    if radius.is_nan() || radius <= 0.0 {
        return false;
    }
    let r2 = radius * radius;
    jammers.iter(world).any(|(p, a, k, ems, gone)| {
        let (dx, dy, dz) = (p.x - at.x, p.y - at.y, p.z - at.z);
        !gone.0
            && k.0 == kind::JAMMER
            && a.0 == affiliation::HOSTILE
            && ems.current_power_w() > 0.0
            && dx * dx + dy * dy + dz * dz <= r2
    })
}

/// Steers each moving unit straight at its destination, arriving exactly at the end of a tick
/// and stopping on the next. Runs after pursuit steering and before movement.
pub(crate) fn follow_moves(
    index: Res<IdIndex>,
    mut moves: ResMut<Moves>,
    mut units: Query<(&Position, &mut Velocity, &Gone)>,
) {
    let mut i = 0;
    while i < moves.0.len() {
        let m = moves.0[i];
        let unit = index.get(m.unit).and_then(|e| units.get_mut(e).ok());
        // A removed unit's move ends; order within the list does not matter.
        let Some((p, mut v, _)) = unit.filter(|(_, _, gone)| !gone.0) else {
            moves.0.swap_remove(i);
            continue;
        };
        if m.arriving {
            *v = Velocity::default();
            moves.0.swap_remove(i);
            continue;
        }
        let (dx, dy, dz) = (
            m.destination.x - p.x,
            m.destination.y - p.y,
            m.destination.z - p.z,
        );
        let dist = distance(*p, m.destination);
        if dist.is_nan() {
            // Too far apart to represent the way there: the move ends where the unit stands.
            *v = Velocity::default();
            moves.0.swap_remove(i);
            continue;
        }
        let next = if dist <= m.speed_mps * DT {
            moves.0[i].arriving = true;
            Velocity {
                x: dx / DT,
                y: dy / DT,
                z: dz / DT,
            }
        } else {
            let k = m.speed_mps / dist;
            Velocity {
                x: dx * k,
                y: dy * k,
                z: dz * k,
            }
        };
        if *v != next {
            *v = next;
        }
        i += 1;
    }
}
