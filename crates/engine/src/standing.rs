//! Standing orders: rules the command node carries out by itself at the end of every tick, with
//! no player input, so they also act during a long catch-up. See `docs/rules.md`.
//!
//! One kind for now: **engage detected hostiles in a zone**. While it has stock, an order
//! launches a pursuer from the command node at the nearest detected hostile inside its zone
//! that no friendly pursuer is already after, at most one per cooldown.

use bevy_ecs::prelude::*;
use simcore_abi::affiliation;

use crate::{
    Affiliation, Detected, Gone, Position, Pursuer, StableId, UnitKind, UnitSpec, Velocity,
};

/// No world holds more standing orders than this at once (`SC_MAX_STANDING_ORDERS`).
pub const MAX_STANDING_ORDERS: usize = simcore_abi::MAX_STANDING_ORDERS as usize;

/// What a standing order watches and what it launches. All values are caller data.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StandingOrder {
    /// Zone centre, world m. The zone is a horizontal disc: altitude does not matter.
    pub center_x: f64,
    pub center_y: f64,
    /// Zone radius, m; positive and finite.
    pub radius_m: f64,
    /// Bit `k` engages units of kind `k`; 0 engages every kind. Kinds from 32 up only match 0.
    pub kinds: u32,
    /// The unit launched. Its position is relative to the command node at launch, and its
    /// velocity is ignored: it leaves heading straight at the target. Friendly.
    pub launch: UnitSpec,
    /// The launched unit's pursuit; `target` is ignored and set to the contact.
    pub pursuer: Pursuer,
    /// Launches available, at most `u32::MAX - 1` (the C API's "no such order").
    pub stock: u32,
    /// Ticks after a launch before the order can launch again; 0 allows one every tick.
    pub cooldown_ticks: u32,
}

impl StandingOrder {
    pub(crate) fn engages(&self, kind: u16) -> bool {
        self.kinds == 0 || (kind < 32 && self.kinds & (1 << kind) != 0)
    }
}

/// Why [`crate::Sim::add_standing_order`] rejected an order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StandingOrderError {
    /// A non-finite centre, or a radius that is not positive and finite.
    InvalidZone,
    /// The launch unit fails [`crate::Sim::spawn`]'s unit checks or is not friendly.
    InvalidUnit,
    /// The pursuit fails [`crate::Sim::spawn`]'s pursuer checks.
    InvalidPursuer,
    /// Stock of `u32::MAX`.
    InvalidStock,
    /// [`MAX_STANDING_ORDERS`] are already active.
    TooMany,
}

/// A standing order's progress.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StandingOrderStatus {
    /// Launches left.
    pub remaining: u32,
    /// The order can launch again on the tick that ends at this [`crate::Sim::tick`].
    pub ready_tick: u64,
}

/// One launch by a standing order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LaunchEvent {
    /// [`crate::Sim::tick`] once the launch's tick completed.
    pub tick: u64,
    pub order: u64,
    /// The launched unit; its engagement reports it as the pursuer.
    pub pursuer: StableId,
    pub target: StableId,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Active {
    pub id: u64,
    pub order: StandingOrder,
    pub status: StandingOrderStatus,
}

impl Active {
    fn ready(&self, tick: u64) -> bool {
        self.status.remaining > 0 && self.status.ready_tick <= tick
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Contact {
    id: StableId,
    position: Position,
    kind: u16,
}

pub(crate) type ContactQuery = QueryState<(
    &'static StableId,
    &'static Position,
    &'static UnitKind,
    &'static Affiliation,
    &'static Detected,
    &'static Gone,
)>;

pub(crate) type PursuitQuery = QueryState<(&'static Pursuer, &'static Affiliation, &'static Gone)>;

/// The active orders, in id order, and the buffers their pass reuses.
#[derive(Debug, Default)]
pub(crate) struct StandingOrders {
    pub active: Vec<Active>,
    /// Last order id handed out.
    pub last_id: u64,
    /// Launches during the most recent `step` call.
    pub launches: Vec<LaunchEvent>,
    contacts: Vec<Contact>,
    /// Units a friendly pursuer is after, sorted.
    engaged: Vec<StableId>,
}

/// What one order launches this tick.
pub(crate) struct Launch {
    pub order: u64,
    /// The order's status before this launch was charged, to restore if the spawn fails.
    pub before: StandingOrderStatus,
    pub spec: UnitSpec,
    pub pursuer: Pursuer,
}

impl StandingOrders {
    /// Whether any order could launch at the end of the tick ending at `tick`. False costs no
    /// scan of the units.
    pub fn any_ready(&self, tick: u64) -> bool {
        self.active.iter().any(|a| a.ready(tick))
    }

    /// Picks this tick's launches, in order id order, and charges them to their orders. `node`
    /// is the command node's position. At most one launch per order, and never two at one
    /// target. Deterministic: candidates are ranked by (distance, id), never by ECS order.
    pub fn plan(
        &mut self,
        world: &World,
        contacts: &mut ContactQuery,
        pursuits: &mut PursuitQuery,
        tick: u64,
        node: Position,
        out: &mut Vec<Launch>,
    ) {
        self.contacts.clear();
        self.contacts.extend(
            contacts
                .iter(world)
                .filter(|(_, _, _, a, d, g)| a.0 == affiliation::HOSTILE && d.0 && !g.0)
                .map(|(id, p, k, ..)| Contact {
                    id: *id,
                    position: *p,
                    kind: k.0,
                }),
        );
        if self.contacts.is_empty() {
            return;
        }
        self.engaged.clear();
        self.engaged.extend(
            pursuits
                .iter(world)
                .filter(|(_, a, g)| a.0 == affiliation::FRIENDLY && !g.0)
                .map(|(p, ..)| p.target),
        );
        self.engaged.sort_unstable();
        self.engaged.dedup();

        for a in &mut self.active {
            if !a.ready(tick) {
                continue;
            }
            let o = &a.order;
            let r2 = o.radius_m * o.radius_m;
            let engaged = &self.engaged;
            let pick = self
                .contacts
                .iter()
                .filter(|c| o.engages(c.kind) && engaged.binary_search(&c.id).is_err())
                .map(|c| {
                    let (dx, dy) = (c.position.x - o.center_x, c.position.y - o.center_y);
                    (dx * dx + dy * dy, c)
                })
                .filter(|(d2, _)| *d2 <= r2)
                .min_by(|(d1, c1), (d2, c2)| d1.total_cmp(d2).then(c1.id.cmp(&c2.id)));
            let Some((_, c)) = pick else {
                continue;
            };
            let mut spec = o.launch;
            spec.position = Position {
                x: node.x + spec.position.x,
                y: node.y + spec.position.y,
                z: node.z + spec.position.z,
            };
            spec.velocity = heading(spec.position, c.position, o.pursuer.speed);
            let mut pursuer = o.pursuer;
            pursuer.target = c.id;
            out.push(Launch {
                order: a.id,
                before: a.status,
                spec,
                pursuer,
            });
            a.status.remaining -= 1;
            a.status.ready_tick = tick.saturating_add(u64::from(o.cooldown_ticks.max(1)));
            let i = self.engaged.partition_point(|&e| e < c.id);
            self.engaged.insert(i, c.id);
        }
    }
}

/// Straight from `from` to `to` at `speed`; zero if they coincide or the way is not
/// representable (pursuit steering then points the pursuer at its target). Always finite for
/// a finite speed: each unit-vector component is at most 1 in magnitude.
fn heading(from: Position, to: Position, speed: f64) -> Velocity {
    let (dx, dy, dz) = (to.x - from.x, to.y - from.y, to.z - from.z);
    let d = (dx * dx + dy * dy + dz * dz).sqrt();
    if !(d > 0.0 && d.is_finite()) {
        return Velocity::default();
    }
    Velocity {
        x: (dx / d).clamp(-1.0, 1.0) * speed,
        y: (dy / d).clamp(-1.0, 1.0) * speed,
        z: (dz / d).clamp(-1.0, 1.0) * speed,
    }
}
