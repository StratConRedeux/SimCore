//! Headless, deterministic simulation engine for SimCore.
//!
//! The world lives in a `bevy_ecs` [`World`] driven by a single-threaded schedule at a fixed
//! rate of [`TICKS_PER_SECOND`]. The engine never reads the wall clock: callers decide how many
//! ticks to run with [`Sim::step`] and read the result with [`Sim::frame`].

mod components;
mod rng;

pub use components::{
    Affiliation, CommandNode, EmitterCycle, EmsProfile, Position, StableId, UnitKind, Velocity,
};
pub use rng::Pcg32;
pub use simcore_abi::{ScUnitView, TICKS_PER_SECOND};

use bevy_ecs::prelude::*;
use bevy_ecs::schedule::SingleThreadedExecutor;
use simcore_abi::{affiliation, flags};

/// Fixed timestep in seconds.
pub const DT: f64 = 1.0 / TICKS_PER_SECOND as f64;

/// Unit type codes (`ScUnitView::kind`).
pub mod kind {
    pub const UNKNOWN: u16 = 0;
    pub const COMMAND_NODE: u16 = 1;
    pub const QUADCOPTER: u16 = 2;
    pub const FIXED_WING_DRONE: u16 = 3;
    pub const GROUND_RADAR: u16 = 4;
    pub const GROUND_VEHICLE: u16 = 5;
    pub const JAMMER: u16 = 6;
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
    Has<CommandNode>,
);

/// One simulated world.
pub struct Sim {
    world: World,
    schedule: Schedule,
    command_node: Entity,
    next_id: u64,
    frame_query: QueryState<FrameQuery>,
    frame: Vec<ScUnitView>,
}

impl Sim {
    /// An empty world with the player's command node at the origin.
    pub fn new(seed: u64) -> Sim {
        let mut world = World::new();
        world.insert_resource(SimClock::default());

        let mut schedule = Schedule::default();
        schedule.set_executor(SingleThreadedExecutor::new());
        schedule.add_systems((update_emitters, integrate_movement).chain());

        let frame_query = world.query::<FrameQuery>();
        // The seed is reserved for rules that need randomness during simulation.
        let _ = seed;

        let command_node = world
            .spawn((
                StableId(1),
                CommandNode,
                UnitKind(kind::COMMAND_NODE),
                Affiliation(affiliation::FRIENDLY),
                Position::default(),
                Velocity::default(),
                EmsProfile::default(),
            ))
            .id();

        Sim {
            world,
            schedule,
            command_node,
            next_id: 2,
            frame_query,
            frame: Vec::new(),
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
        let id = StableId(self.next_id);
        self.next_id += 1;
        let mut e = self.world.spawn((
            id,
            UnitKind(spec.kind),
            Affiliation(spec.affiliation),
            spec.position,
            spec.velocity,
            spec.ems,
        ));
        if let Some(cycle) = spec.cycle {
            e.insert(cycle);
        }
        id
    }

    /// Moves the command node (and therefore the frame origin) to `position`.
    pub fn set_command_node_position(&mut self, position: Position) {
        *self
            .world
            .get_mut::<Position>(self.command_node)
            .expect("command node exists") = position;
    }

    /// Advances the world by `ticks` fixed steps of [`DT`] seconds. `step(0)` does nothing.
    pub fn step(&mut self, ticks: u32) {
        for _ in 0..ticks {
            self.schedule.run(&mut self.world);
            self.world.resource_mut::<SimClock>().tick += 1;
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
        let origin = *self
            .world
            .get::<Position>(self.command_node)
            .expect("command node exists");
        self.frame.clear();
        let units = self.frame_query.iter(&self.world);
        // Exact size on first build; a no-op while the unit count is stable.
        self.frame.reserve(units.len());
        for (id, pos, vel, kind, aff, ems, is_cmd) in units {
            let mut f = 0;
            if ems.emitting {
                f |= flags::EMITTING;
            }
            if is_cmd {
                f |= flags::COMMAND_NODE;
            }
            self.frame.push(ScUnitView {
                id: id.0,
                // Subtract in f64 first so precision is relative to the command node.
                x: (pos.x - origin.x) as f32,
                y: (pos.y - origin.y) as f32,
                z: (pos.z - origin.z) as f32,
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
        // Ids are allocated sequentially from 1 and units are never despawned (yet). The ECS
        // entity count is not a substitute: bevy_ecs also stores resources as entities.
        (self.next_id - 1) as u32
    }

    /// Read-only access to the ECS world, for diagnostics and tests.
    pub fn world(&self) -> &World {
        &self.world
    }
}

/// Applies each emitter's duty cycle for the current tick.
fn update_emitters(clock: Res<SimClock>, mut q: Query<(&EmitterCycle, &mut EmsProfile)>) {
    let tick = clock.tick;
    for (cycle, mut ems) in &mut q {
        let on = cycle.is_on(tick);
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
