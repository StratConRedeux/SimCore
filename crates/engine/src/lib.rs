//! Headless, deterministic simulation engine for SimCore.
//!
//! The world lives in a `bevy_ecs` [`World`] driven by a single-threaded schedule at a fixed
//! rate of [`TICKS_PER_SECOND`]. The engine never reads the wall clock: callers decide how many
//! ticks to run with [`Sim::step`] and read the result with [`Sim::frame`].
//!
//! Each tick runs, in order: command delivery, emitter duty cycles, observation, exposure, the
//! detected flag, pursuit steering, move orders, movement, then engagement resolution. See
//! `docs/rules.md`.

mod components;
mod gameplay;
mod grid;
mod orders;
mod rng;
mod snapshot;
mod terrain;

use components::Gone;
pub use components::{
    Affiliation, CommandNode, Detected, EmitterCycle, EmsProfile, Exposure, Position, Pursuer,
    Sensor, SensorParams, StableId, UnitKind, Velocity,
};
pub use gameplay::{
    EmitterView, EngagementEvent, Heatmap, HeatmapSpec, OutcomeTable, Rules, SensorView, opposing,
    sensor_observes,
};
pub use orders::{CommandError, Emcon, LinkRules, Order};
pub use rng::Pcg32;
pub use simcore_abi::{ScStatus, ScUnitView, TICKS_PER_SECOND};
pub use snapshot::{SNAPSHOT_FORMAT_VERSION, SnapshotError};
pub use terrain::Terrain;

use bevy_ecs::prelude::*;
use bevy_ecs::schedule::SingleThreadedExecutor;
use gameplay::{Engagements, IdIndex, Scratch};
use orders::{CommandQueue, JammerQuery, Moves, Pending};
use simcore_abi::{affiliation, flags};
use simcore_rules::to_local_f32;

/// Fixed timestep in seconds.
pub const DT: f64 = 1.0 / TICKS_PER_SECOND as f64;

/// Unit type codes, defined with the C API since revision 1.4.
pub use simcore_abi::kind;

/// The largest stable id a world can save and restore (`2^24 - 1`). [`Sim::spawn`] refuses to
/// go past it.
pub const MAX_UNIT_ID: u64 = (1 << 24) - 1;

/// Why [`Sim::spawn`] rejected a unit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpawnError {
    /// A non-finite position or velocity, a negative or non-finite power, an unknown
    /// affiliation, or the command node's kind.
    InvalidUnit,
    /// A pursuer speed that is not positive and finite, a negative or non-finite turn rate or
    /// engagement radius, or a lead outside `[0, 1]`.
    InvalidPursuer,
    /// The pursuer's target does not exist.
    UnknownTarget,
    /// Every id up to [`MAX_UNIT_ID`] has been used.
    IdsExhausted,
}

/// Simulation time, in ticks since creation.
#[derive(Resource, Clone, Copy, Debug, Default)]
pub struct SimClock {
    pub tick: u64,
}

/// Everything needed to spawn one unit.
#[derive(Clone, Copy, Debug)]
pub struct UnitSpec {
    pub kind: u16,
    pub affiliation: u8,
    pub position: Position,
    pub velocity: Velocity,
    pub ems: EmsProfile,
    pub cycle: Option<EmitterCycle>,
}

type FrameQuery = (
    &'static StableId,
    &'static Position,
    &'static Velocity,
    &'static UnitKind,
    &'static Affiliation,
    &'static EmsProfile,
    Option<&'static Detected>,
    Has<CommandNode>,
    &'static Gone,
);

type HeatQuery = (
    &'static Position,
    &'static Affiliation,
    &'static EmsProfile,
    &'static Gone,
);

type StatusQuery = (
    &'static Affiliation,
    &'static EmsProfile,
    &'static Detected,
    &'static Gone,
);

/// One simulated world.
pub struct Sim {
    world: World,
    schedule: Schedule,
    command_node: Entity,
    next_id: u64,
    frame_query: QueryState<FrameQuery>,
    frame: Vec<ScUnitView>,
    engagements: Engagements,
    heat_query: QueryState<HeatQuery>,
    heatmap: Heatmap,
    commands: CommandQueue,
    jammer_query: JammerQuery,
    status_query: QueryState<StatusQuery>,
    snapshot: Vec<u8>,
}

impl Sim {
    /// An empty world with the player's command node at the origin.
    pub fn new(seed: u64) -> Sim {
        let mut sim = Sim::without_units(seed);
        let command_node = sim
            .world
            .spawn((
                StableId(1),
                CommandNode,
                UnitKind(kind::COMMAND_NODE),
                Affiliation(affiliation::FRIENDLY),
                Position::default(),
                Velocity::default(),
                EmsProfile::default(),
                Exposure::default(),
                Detected::default(),
                Gone::default(),
                Emcon::default(),
            ))
            .id();
        sim.world
            .resource_mut::<IdIndex>()
            .insert(StableId(1), command_node);
        sim.command_node = command_node;
        sim.next_id = 2;
        sim
    }

    /// Everything but the units. `command_node` is a placeholder until one is spawned.
    fn without_units(seed: u64) -> Sim {
        let mut world = World::new();
        world.insert_resource(SimClock::default());
        world.insert_resource(Rules::default());
        world.insert_resource(LinkRules::default());
        world.insert_resource(Terrain::flat());
        world.insert_resource(IdIndex::default());
        world.insert_resource(Scratch::default());
        world.insert_resource(Moves::default());

        let mut schedule = Schedule::default();
        schedule.set_executor(SingleThreadedExecutor::new());
        schedule.add_systems(
            (
                update_emitters,
                gameplay::observe,
                gameplay::update_exposure,
                gameplay::update_detected,
                gameplay::steer_pursuers,
                orders::follow_moves,
                integrate_movement,
            )
                .chain(),
        );

        let frame_query = world.query::<FrameQuery>();
        let heat_query = world.query::<HeatQuery>();
        let jammer_query = world.query();
        let status_query = world.query::<StatusQuery>();
        // The seed drives engagement rolls.
        let engagements = Engagements::new(&mut world, seed);

        Sim {
            world,
            schedule,
            command_node: Entity::PLACEHOLDER,
            next_id: 1,
            frame_query,
            frame: Vec::new(),
            engagements,
            heat_query,
            heatmap: Heatmap::default(),
            commands: CommandQueue::default(),
            jammer_query,
            status_query,
            snapshot: Vec::new(),
        }
    }

    /// A world with the command node plus `unit_count` dummy units within about 40 km,
    /// with mixed affiliation, seeded velocities, and some emitters on a duty cycle.
    pub fn new_dummy(seed: u64, unit_count: u32) -> Sim {
        let mut sim = Sim::new(seed);
        let mut rng = Pcg32::new(seed, 0x51_4d_43_4f_52_45); // "SIMCORE"
        const KINDS: [u16; 5] = [
            kind::QUADCOPTER,
            kind::FIXED_WING_DRONE,
            kind::GROUND_RADAR,
            kind::GROUND_VEHICLE,
            kind::JAMMER,
        ];
        for _ in 0..unit_count {
            let k = KINDS[rng.below(KINDS.len() as u32) as usize];
            let (alt, speed, erp_w, emitter_chance) = match k {
                kind::QUADCOPTER => (
                    rng.range_f64(20.0, 300.0),
                    rng.range_f64(0.0, 20.0),
                    1.0,
                    30,
                ),
                kind::FIXED_WING_DRONE => (
                    rng.range_f64(500.0, 5000.0),
                    rng.range_f64(30.0, 80.0),
                    10.0,
                    40,
                ),
                kind::GROUND_RADAR => (0.0, 0.0, 50_000.0, 90),
                kind::GROUND_VEHICLE => (0.0, rng.range_f64(0.0, 15.0), 20.0, 20),
                _ => (0.0, rng.range_f64(0.0, 5.0), 2_000.0, 80),
            };
            // Uniform in a 40 km radius disc.
            let r = 40_000.0 * rng.next_f64().sqrt();
            let theta = rng.range_f64(0.0, core::f64::consts::TAU);
            let heading = rng.range_f64(0.0, core::f64::consts::TAU);
            let emitter = rng.below(100) < emitter_chance;
            let cycle = emitter.then(|| EmitterCycle {
                on_ticks: 15 + rng.below(150),
                off_ticks: 15 + rng.below(300),
                phase: rng.below(1000),
            });
            sim.spawn_unit(UnitSpec {
                kind: k,
                affiliation: 1 + rng.below(3) as u8,
                position: Position {
                    x: r * theta.cos(),
                    y: r * theta.sin(),
                    z: alt,
                },
                velocity: Velocity {
                    x: speed * heading.cos(),
                    y: speed * heading.sin(),
                    z: 0.0,
                },
                ems: EmsProfile {
                    emitting: cycle.is_some_and(|c| c.is_on(0)),
                    erp_w: erp_w as f32,
                },
                cycle,
            });
        }
        sim
    }

    /// Spawns a unit and returns its stable id.
    pub fn spawn_unit(&mut self, spec: UnitSpec) -> StableId {
        self.spawn_with(spec, ())
    }

    /// Spawns a unit that pursues `pursuer.target` and returns its stable id.
    ///
    /// The target need not exist; a pursuer without a live target flies straight. Any unit can
    /// be targeted, but the command node is never removed: a successful engagement against it
    /// consumes the pursuer, reports `command_node_hit: true` and `target_removed: false`, and
    /// counts in [`Sim::command_node_hits`].
    pub fn spawn_pursuer(&mut self, spec: UnitSpec, pursuer: Pursuer) -> StableId {
        // Each pursuer engages at most once: size the engagement buffers now, not mid-tick.
        self.engagements.add_pursuer(&mut self.world);
        self.spawn_with(spec, pursuer)
    }

    /// Spawns a unit, or a pursuer, after checking it, and returns its stable id.
    ///
    /// Unlike [`Sim::spawn_unit`] and [`Sim::spawn_pursuer`], this rejects values the rules
    /// would only tolerate (see [`SpawnError`]), a pursuer whose target does not exist, and ids
    /// beyond [`MAX_UNIT_ID`], so every world built with it can be saved and restored. With a
    /// duty cycle, `spec.ems.emitting` is ignored: the cycle decides, from the current tick.
    pub fn spawn(
        &mut self,
        mut spec: UnitSpec,
        pursuer: Option<Pursuer>,
    ) -> Result<StableId, SpawnError> {
        let finite = |v: [f64; 3]| v.iter().all(|c| c.is_finite());
        let p = spec.position;
        let v = spec.velocity;
        let unit_ok = finite([p.x, p.y, p.z])
            && finite([v.x, v.y, v.z])
            && spec.ems.erp_w >= 0.0
            && spec.ems.erp_w.is_finite()
            && spec.affiliation <= affiliation::NEUTRAL
            && spec.kind != kind::COMMAND_NODE;
        if !unit_ok {
            return Err(SpawnError::InvalidUnit);
        }
        if let Some(cycle) = spec.cycle {
            spec.ems.emitting = cycle.is_on(self.tick());
        }
        if let Some(q) = pursuer {
            let non_negative = |x: f64| x >= 0.0 && x.is_finite();
            let ok = q.speed > 0.0
                && q.speed.is_finite()
                && non_negative(q.max_turn_rate)
                && non_negative(q.engagement_radius_m)
                && (0.0..=1.0).contains(&q.lead);
            if !ok {
                return Err(SpawnError::InvalidPursuer);
            }
            if !self.contains(q.target) {
                return Err(SpawnError::UnknownTarget);
            }
        }
        if self.next_id > MAX_UNIT_ID {
            return Err(SpawnError::IdsExhausted);
        }
        Ok(match pursuer {
            Some(q) => self.spawn_pursuer(spec, q),
            None => self.spawn_unit(spec),
        })
    }

    /// Despawns the entities of units removed by engagements, releasing their storage.
    ///
    /// Removed units leave the id index, the frame and the heatmap at once; their entities
    /// stay in the world until this runs, so that `step` itself never allocates for a removal.
    /// Spawning calls it too, so calling it is only needed to reclaim memory sooner.
    pub fn despawn_removed(&mut self) {
        self.engagements.despawn_removed(&mut self.world);
    }

    fn spawn_with(&mut self, spec: UnitSpec, extra: impl Bundle) -> StableId {
        self.despawn_removed();
        let id = StableId(self.next_id);
        self.next_id += 1;
        let mut e = self.world.spawn((
            id,
            UnitKind(spec.kind),
            Affiliation(spec.affiliation),
            spec.position,
            spec.velocity,
            spec.ems,
            Exposure::default(),
            Detected::default(),
            Gone::default(),
            Emcon::default(),
            extra,
        ));
        if let Some(cycle) = spec.cycle {
            e.insert(cycle);
        }
        let e = e.id();
        self.world.resource_mut::<IdIndex>().insert(id, e);
        id
    }

    /// Makes unit `id` a passive sensor (replacing any previous one). Returns `false` if the
    /// unit does not exist. Parameters are taken as given: see
    /// [`simcore_rules::detection_range`] for how non-positive values behave.
    pub fn add_sensor(&mut self, id: StableId, sensor: Sensor) -> bool {
        let Some(e) = self.entity(id) else {
            return false;
        };
        self.world.entity_mut(e).insert(sensor);
        true
    }

    /// Replaces the gameplay rules resource.
    pub fn set_rules(&mut self, rules: Rules) {
        self.world.insert_resource(rules);
    }

    pub fn rules(&self) -> &Rules {
        self.world.resource::<Rules>()
    }

    /// Replaces the link-delay rules for commands issued from now on.
    pub fn set_link_rules(&mut self, rules: LinkRules) {
        self.world.insert_resource(rules);
    }

    pub fn link_rules(&self) -> &LinkRules {
        self.world.resource::<LinkRules>()
    }

    /// Queues `order` for unit `unit`, to be applied after the link delay. Returns the command
    /// id (from 1, in issue order).
    ///
    /// A command issued now, at tick `t`, with delay `d`, is applied at the start of the tick
    /// that begins at `t + d`: with no delay, on the next tick run. It is checked now and again
    /// when it arrives; if its recipient (or a retarget's target) is gone by then it is dropped
    /// and counted in [`ScStatus::dropped_commands`].
    pub fn command(&mut self, unit: StableId, order: Order) -> Result<u64, CommandError> {
        orders::validate(&self.world, unit, &order)?;
        let delay = self.link_delay(unit).ok_or(CommandError::UnknownUnit)?;
        // Neither can overflow in practice (2^64 commands or ticks); saturating keeps a
        // hand-made world from panicking in debug builds.
        self.commands.last_seq = self.commands.last_seq.saturating_add(1);
        let seq = self.commands.last_seq;
        self.commands.push(Pending {
            deliver_tick: self.tick().saturating_add(u64::from(delay)),
            seq,
            unit,
            order,
        });
        Ok(seq)
    }

    /// The link delay, in ticks, of a command issued now to unit `id`, if it exists.
    pub fn link_delay(&mut self, id: StableId) -> Option<u32> {
        let at = self.position(id)?;
        let node = self.command_node_position();
        Some(orders::link_delay(
            &self.world,
            &mut self.jammer_query,
            node,
            at,
        ))
    }

    /// Commands queued and not yet delivered.
    pub fn pending_commands(&self) -> u32 {
        u32::try_from(self.commands.pending.len()).unwrap_or(u32::MAX)
    }

    /// A summary for the HUD: counts are of live units, and the link delay is that of a
    /// command to the command node itself (the base delay plus any jamming there).
    pub fn status(&mut self) -> ScStatus {
        let mut s = ScStatus {
            tick: self.tick(),
            unit_count: self.unit_count(),
            link_delay_ticks: self.link_delay(StableId(1)).unwrap_or(0),
            pending_commands: self.pending_commands(),
            dropped_commands: self.commands.dropped,
            command_node_hits: self.command_node_hits(),
            awaiting_cleanup: self.engagements.awaiting_despawn(),
            ..ScStatus::default()
        };
        for (a, ems, detected, gone) in self.status_query.iter(&self.world) {
            if gone.0 {
                continue;
            }
            match a.0 {
                affiliation::FRIENDLY => {
                    s.friendly_emitting += u32::from(ems.current_power_w() > 0.0);
                    s.friendly_detected += u32::from(detected.0);
                }
                affiliation::HOSTILE => s.hostile_detected += u32::from(detected.0),
                _ => {}
            }
        }
        s
    }

    /// Replaces the terrain used for line of sight.
    pub fn set_terrain(&mut self, terrain: Terrain) {
        self.world.insert_resource(terrain);
    }

    pub fn terrain(&self) -> &Terrain {
        self.world.resource::<Terrain>()
    }

    /// The ECS entity for `id`, or `None` once the unit has been removed.
    pub fn entity(&self, id: StableId) -> Option<Entity> {
        self.world.resource::<IdIndex>().get(id)
    }

    /// Whether unit `id` still exists.
    pub fn contains(&self, id: StableId) -> bool {
        self.entity(id).is_some()
    }

    /// Unit `id`'s exposure, if it exists.
    pub fn exposure(&self, id: StableId) -> Option<Exposure> {
        self.world.get::<Exposure>(self.entity(id)?).copied()
    }

    /// Whether unit `id` is detected, if it exists.
    pub fn is_detected(&self, id: StableId) -> Option<bool> {
        self.world.get::<Detected>(self.entity(id)?).map(|d| d.0)
    }

    /// Unit `id`'s world position, if it exists.
    pub fn position(&self, id: StableId) -> Option<Position> {
        self.world.get::<Position>(self.entity(id)?).copied()
    }

    /// Engagements resolved during the most recent [`Sim::step`] call, in resolution order.
    /// The buffer is reused across calls.
    pub fn engagement_events(&self) -> &[EngagementEvent] {
        &self.engagements.events
    }

    /// Successful engagements against the command node since creation, saturating at
    /// `u32::MAX`. The engine only counts them; the caller's rules decide what they mean.
    pub fn command_node_hits(&self) -> u32 {
        self.engagements.command_node_hits
    }

    /// Rebuilds the signal heatmap around the command node from currently emitting units of
    /// `affiliation`, and returns it. An invalid spec gives an empty map.
    ///
    /// The map's buffers are reused: rebuilding with the same spec does not allocate once the
    /// emitter count is stable.
    pub fn build_heatmap(&mut self, spec: HeatmapSpec, affiliation: u8) -> &Heatmap {
        let origin = self.command_node_position();
        let units = self.heat_query.iter(&self.world);
        self.heatmap.build(spec, origin, units, affiliation);
        &self.heatmap
    }

    /// Moves the command node (and therefore the frame origin) to `position`.
    pub fn set_command_node_position(&mut self, position: Position) {
        if let Some(mut p) = self.world.get_mut::<Position>(self.command_node) {
            *p = position;
        }
    }

    /// The command node's world position. Engagements never remove the command node, so it
    /// always exists; the default is only a fallback.
    fn command_node_position(&self) -> Position {
        self.world
            .get::<Position>(self.command_node)
            .copied()
            .unwrap_or_default()
    }

    /// Advances the world by `ticks` fixed steps of [`DT`] seconds. `step(0)` runs no ticks.
    ///
    /// Clears the engagement events, then collects those of every tick run.
    pub fn step(&mut self, ticks: u32) {
        self.engagements.events.clear();
        for _ in 0..ticks {
            let now = self.world.resource::<SimClock>().tick;
            self.commands.deliver(&mut self.world, now);
            self.schedule.run(&mut self.world);
            let tick = self.world.resource::<SimClock>().tick + 1;
            // Engagement resolution removes units, so it runs with exclusive world access as
            // the last stage of the tick.
            let origin = self.command_node_position();
            self.engagements.resolve(&mut self.world, tick, origin);
            self.world.resource_mut::<SimClock>().tick = tick;
            // Despawns (in `despawn_removed`) log removed components in double-buffered queues
            // that only this clears. Once per tick keeps them bounded (and their capacity
            // reused).
            self.world.clear_trackers();
        }
        if ticks > 0 {
            // Keeps change-detection ticks from wrapping during long sessions. Cheap: it
            // returns early unless enough change ticks have passed. Our schedule lives outside
            // the world's `Schedules` resource, so it has to be told separately.
            if let Some(check) = self.world.check_change_ticks() {
                self.schedule.check_change_ticks(check);
            }
        }
    }

    /// Ticks run since creation.
    pub fn tick(&self) -> u64 {
        self.world.resource::<SimClock>().tick
    }

    /// Rebuilds the frame relative to the command node and returns it.
    ///
    /// The buffer is reused: once the unit count is stable this does not allocate.
    pub fn frame(&mut self) -> &[ScUnitView] {
        let origin = self.command_node_position();
        self.frame.clear();
        let units = self.frame_query.iter(&self.world);
        // Exact size on first build; a no-op while the unit count is stable.
        self.frame.reserve(units.len());
        for (id, pos, vel, kind, aff, ems, detected, is_cmd, gone) in units {
            if gone.0 {
                continue;
            }
            let mut f = 0;
            if ems.emitting {
                f |= flags::EMITTING;
            }
            if is_cmd {
                f |= flags::COMMAND_NODE;
            }
            if detected.is_some_and(|d| d.0) {
                f |= flags::DETECTED;
            }
            self.frame.push(ScUnitView {
                id: id.0,
                // Floating origin: subtract in f64, then narrow, so precision is relative to
                // the command node.
                x: to_local_f32(pos.x, origin.x),
                y: to_local_f32(pos.y, origin.y),
                z: to_local_f32(pos.z, origin.z),
                vx: vel.x as f32,
                vy: vel.y as f32,
                vz: vel.z as f32,
                kind: kind.0,
                affiliation: aff.0,
                flags: f,
                ems_power_w: ems.current_power_w(),
            });
        }
        &self.frame
    }

    /// Capacity of the reused frame buffer, in units.
    pub fn frame_capacity(&self) -> usize {
        self.frame.capacity()
    }

    /// Number of units, including the command node.
    pub fn unit_count(&self) -> u32 {
        // Live units in the id index. The ECS entity count is not a substitute: bevy_ecs also
        // stores resources as entities.
        self.world.resource::<IdIndex>().live()
    }

    /// Read-only access to the ECS world, for diagnostics and tests.
    pub fn world(&self) -> &World {
        &self.world
    }
}

/// Applies each emitter's duty cycle for the current tick. A silent unit ([`Emcon`]) stays off.
///
/// Removed ([`Gone`]) units keep cycling: every reader of `emitting` skips them, and filtering
/// here would make this pass about 2.5x slower.
fn update_emitters(clock: Res<SimClock>, mut q: Query<(&EmitterCycle, &Emcon, &mut EmsProfile)>) {
    let tick = clock.tick;
    for (cycle, emcon, mut ems) in &mut q {
        let on = !emcon.silent && cycle.is_on(tick);
        // Only touch the component when it changes, so change detection stays meaningful.
        if ems.emitting != on {
            ems.emitting = on;
        }
    }
}

/// Explicit Euler integration over one fixed tick.
fn integrate_movement(mut q: Query<(&mut Position, &Velocity)>) {
    for (mut pos, vel) in &mut q {
        if vel.x == 0.0 && vel.y == 0.0 && vel.z == 0.0 {
            continue;
        }
        pos.x += vel.x * DT;
        pos.y += vel.y * DT;
        pos.z += vel.z * DT;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quad() -> UnitSpec {
        UnitSpec {
            kind: kind::QUADCOPTER,
            affiliation: affiliation::FRIENDLY,
            position: Position::default(),
            velocity: Velocity::default(),
            ems: EmsProfile::default(),
            cycle: None,
        }
    }

    #[test]
    fn spawn_stops_at_the_largest_restorable_id() {
        let mut sim = Sim::new(1);
        sim.next_id = MAX_UNIT_ID;
        assert_eq!(sim.spawn(quad(), None), Ok(StableId(MAX_UNIT_ID)));
        assert_eq!(sim.spawn(quad(), None), Err(SpawnError::IdsExhausted));
        assert_eq!(sim.unit_count(), 2);
        // The fullest world it allows still round-trips.
        let saved = sim.save().to_vec();
        let restored = Sim::restore(&saved).expect("restores");
        assert!(restored.contains(StableId(MAX_UNIT_ID)));
    }

    #[test]
    fn a_duty_cycle_sets_emitting_from_the_current_tick() {
        let mut sim = Sim::new(1);
        sim.step(3);
        let cycle = EmitterCycle {
            on_ticks: 2,
            off_ticks: 2,
            phase: 0,
        };
        let mut spec = quad();
        spec.ems.emitting = true;
        spec.cycle = Some(cycle);
        // Tick 3 of an on-2, off-2 cycle is off.
        let id = sim.spawn(spec, None).unwrap();
        let e = sim.entity(id).unwrap();
        assert!(!sim.world().get::<EmsProfile>(e).unwrap().emitting);
    }
}
