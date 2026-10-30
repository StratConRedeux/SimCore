//! A shared rich world for determinism and save/restore tests, built through the public API
//! only. Everything that draws on or carries hidden state is in it: duty-cycled and continuous
//! emitters, sensors (shared and own parameters), pursuers on both sides with several outcome
//! classes whose chances are strictly between 0 and 1 (so every engagement draws from the
//! RNG), standing orders (one with a duty-cycled launch unit), link delay with jamming, terrain
//! and a script of moves, halts, retargets and emission changes, as commands and as direct
//! orders to any side, plus host tags. All numbers are fixtures.

#![allow(dead_code)]

use simcore_abi::ScUnitView;
use simcore_abi::affiliation::{FRIENDLY, HOSTILE, NEUTRAL};
use simcore_engine::{
    EmitterCycle, EmsProfile, LinkRules, Order, OutcomeTable, Pcg32, Position, Pursuer, Rules,
    Sensor, SensorParams, Sim, StableId, StandingOrder, Terrain, UnitSpec, Velocity, kind,
};

pub fn p(x: f64, y: f64, z: f64) -> Position {
    Position { x, y, z }
}

pub fn spec(
    affiliation: u8,
    k: u16,
    position: Position,
    velocity: Velocity,
    power_w: f32,
) -> UnitSpec {
    UnitSpec {
        kind: k,
        affiliation,
        position,
        velocity,
        ems: EmsProfile {
            emitting: power_w > 0.0,
            erp_w: power_w,
        },
        cycle: None,
    }
}

/// Every field of every unit as raw bits, in id order (frame order is unspecified).
pub fn bits(frame: &[ScUnitView]) -> Vec<[u64; 5]> {
    let mut v: Vec<_> = frame
        .iter()
        .map(|u| {
            [
                u.id,
                (u64::from(u.x.to_bits()) << 32) | u64::from(u.y.to_bits()),
                (u64::from(u.z.to_bits()) << 32) | u64::from(u.vx.to_bits()),
                (u64::from(u.vy.to_bits()) << 32) | u64::from(u.vz.to_bits()),
                (u64::from(u.kind) << 48)
                    | (u64::from(u.affiliation) << 40)
                    | (u64::from(u.flags) << 32)
                    | u64::from(u.ems_power_w.to_bits()),
            ]
        })
        .collect();
    v.sort_unstable_by_key(|u| u[0]);
    v
}

/// Rules, link rules and terrain for an active world.
pub fn activate(sim: &mut Sim, seed: u64) {
    let mut rng = Pcg32::new(seed, 0x52_49_43_48); // "RICH"
    let heights = (0..16 * 16)
        .map(|_| rng.range_f64(0.0, 60.0) as f32)
        .collect();
    sim.set_terrain(
        Terrain::heightfield(-12_000.0, -12_000.0, 1600.0, 16, 16, heights, 6).unwrap(),
    );
    sim.set_rules(Rules {
        detection_max_range_m: 9000.0,
        sensor: SensorParams {
            base_range_m: 2500.0,
            reference_power_w: 50.0,
        },
        exposure_threshold_ticks: 6,
        exposure_cap_ticks: 30,
        exposure_decay_per_tick: 2,
        outcomes: OutcomeTable::new(0.5)
            .with(1, kind::GROUND_RADAR, 0.7)
            .with(1, kind::JAMMER, 0.4)
            .with(2, kind::QUADCOPTER, 0.35)
            .with(2, kind::COMMAND_NODE, 0.6)
            .with(3, kind::GROUND_VEHICLE, 0.55),
    });
    sim.set_link_rules(LinkRules {
        base_delay_ticks: 2,
        delay_ticks_per_km: 0.8,
        jam_radius_m: 2500.0,
        jam_delay_ticks: 9,
        max_delay_ticks: 60,
    });
}

/// Adds the units and standing orders of the rich world to `sim`.
pub fn populate(sim: &mut Sim, seed: u64) {
    let mut rng = Pcg32::new(seed, 0x50_4f_50); // "POP"
    let kinds = [
        kind::QUADCOPTER,
        kind::FIXED_WING_DRONE,
        kind::GROUND_RADAR,
        kind::GROUND_VEHICLE,
        kind::JAMMER,
    ];
    let mut ids = vec![StableId(1)];
    for i in 0..90u32 {
        let k = kinds[rng.below(kinds.len() as u32) as usize];
        let affiliation = [FRIENDLY, HOSTILE, HOSTILE, NEUTRAL][(i % 4) as usize];
        let r = 9000.0 * rng.next_f64().sqrt();
        let a = rng.range_f64(0.0, core::f64::consts::TAU);
        let h = rng.range_f64(0.0, core::f64::consts::TAU);
        let speed = if k == kind::GROUND_RADAR {
            0.0
        } else {
            rng.range_f64(0.0, 25.0)
        };
        let mut s = spec(
            affiliation,
            k,
            p(r * a.cos(), r * a.sin(), rng.range_f64(0.0, 120.0)),
            Velocity {
                x: speed * h.cos(),
                y: speed * h.sin(),
                z: 0.0,
            },
            10f64.powf(rng.range_f64(0.0, 3.0)) as f32,
        );
        match i % 3 {
            0 => {
                s.cycle = Some(EmitterCycle {
                    on_ticks: 3 + rng.below(20),
                    off_ticks: 2 + rng.below(30),
                    phase: rng.below(50),
                })
            }
            1 => s.ems.emitting = rng.below(3) != 0,
            _ => {}
        }
        let id = sim.spawn(s, None).unwrap();
        if i % 4 != 3 && rng.below(3) == 0 {
            let own = (rng.below(2) == 0).then(|| SensorParams {
                base_range_m: rng.range_f64(1500.0, 4000.0),
                reference_power_w: rng.range_f64(20.0, 200.0),
            });
            assert!(sim.add_sensor(id, Sensor { params: own }));
        }
        ids.push(id);
    }
    assert!(sim.add_sensor(StableId(1), Sensor::default()));

    // Pursuers on both sides, close enough to engage within a few hundred ticks; some go for
    // the command node.
    for i in 0..40u32 {
        let target = if i % 9 == 0 {
            StableId(1)
        } else {
            ids[1 + rng.below(ids.len() as u32 - 1) as usize]
        };
        let t = sim.position(target).unwrap();
        let a = rng.range_f64(0.0, core::f64::consts::TAU);
        let d = rng.range_f64(300.0, 1500.0);
        let mut s = spec(
            if i % 2 == 0 { FRIENDLY } else { HOSTILE },
            kind::FIXED_WING_DRONE,
            p(t.x + d * a.cos(), t.y + d * a.sin(), t.z + 80.0),
            Velocity {
                x: -a.cos(),
                y: -a.sin(),
                z: 0.0,
            },
            if i % 3 == 0 { 3.0 } else { 0.0 },
        );
        if i % 5 == 0 {
            s.cycle = Some(EmitterCycle {
                on_ticks: 2,
                off_ticks: 5,
                phase: i,
            });
        }
        sim.spawn(
            s,
            Some(Pursuer {
                target,
                speed: rng.range_f64(60.0, 140.0),
                max_turn_rate: rng.range_f64(0.5, 1.5),
                lead: rng.range_f64(0.0, 1.0),
                engagement_radius_m: rng.range_f64(8.0, 20.0),
                outcome_class: (i % 4) as u16,
            }),
        )
        .unwrap();
    }

    for (i, (x, y)) in [(-4000.0, 2000.0), (3000.0, -3000.0), (0.0, 5000.0)]
        .into_iter()
        .enumerate()
    {
        let mut launch = spec(
            FRIENDLY,
            kind::QUADCOPTER,
            p(0.0, 0.0, 30.0),
            Velocity::default(),
            1.5,
        );
        if i == 1 {
            launch.cycle = Some(EmitterCycle {
                on_ticks: 4,
                off_ticks: 7,
                phase: 3,
            });
        }
        sim.add_standing_order(StandingOrder {
            center_x: x,
            center_y: y,
            radius_m: 6000.0,
            kinds: if i == 2 {
                1 << kind::GROUND_RADAR | 1 << kind::JAMMER
            } else {
                0
            },
            launch,
            pursuer: Pursuer {
                target: StableId(0),
                speed: 120.0 + 10.0 * i as f64,
                max_turn_rate: 1.2,
                lead: 0.3 * i as f64,
                engagement_radius_m: 15.0,
                outcome_class: 1 + i as u16,
            },
            stock: 25,
            cooldown_ticks: 5 + 4 * i as u32,
        })
        .unwrap();
    }
}

/// The rich world: a fresh world, activated and populated.
pub fn rich_world(seed: u64) -> Sim {
    let mut sim = Sim::new(seed);
    activate(&mut sim, seed);
    populate(&mut sim, seed);
    sim
}

/// A random order to a random unit of `ids`.
fn random_order(rng: &mut Pcg32, ids: &[u64]) -> (StableId, Order) {
    let id = StableId(ids[rng.below(ids.len() as u32) as usize]);
    let order = match rng.below(4) {
        0 => Order::Move {
            destination: p(
                rng.range_f64(-8000.0, 8000.0),
                rng.range_f64(-8000.0, 8000.0),
                0.0,
            ),
            speed_mps: rng.range_f64(5.0, 40.0),
        },
        1 => Order::Halt,
        2 => Order::Retarget {
            target: StableId(ids[rng.below(ids.len() as u32) as usize]),
        },
        _ => Order::Emissions {
            radiate: rng.below(2) == 0,
        },
    };
    (id, order)
}

/// Issues a deterministic batch of commands and direct orders to any side (rejections are
/// fine), sets and clears host tags (pursuers' included, so removals clear some) and, now and
/// then, spawns a new unit.
pub fn script(sim: &mut Sim, rng: &mut Pcg32) {
    let mut ids: Vec<u64> = sim.frame().iter().map(|u| u.id).collect();
    ids.sort_unstable();
    for _ in 0..25 {
        let (id, order) = random_order(rng, &ids);
        let _ = sim.command(id, order);
    }
    for _ in 0..10 {
        let (id, order) = random_order(rng, &ids);
        let _ = sim.order(id, order);
    }
    for _ in 0..8 {
        let id = StableId(ids[rng.below(ids.len() as u32) as usize]);
        let tag = match rng.below(4) {
            0 => 0,
            1 => u64::MAX,
            _ => (u64::from(rng.next_u32()) << 20) | 1,
        };
        assert!(sim.set_tag(id, tag), "ids come from the frame");
        assert_eq!(sim.tag(id), tag);
    }
    if rng.below(4) == 0 {
        let a = rng.range_f64(0.0, core::f64::consts::TAU);
        let mut s = spec(
            HOSTILE,
            kind::GROUND_RADAR,
            p(5000.0 * a.cos(), 5000.0 * a.sin(), 0.0),
            Velocity::default(),
            200.0,
        );
        s.cycle = Some(EmitterCycle {
            on_ticks: 5,
            off_ticks: 5,
            phase: rng.below(10),
        });
        sim.spawn(s, None).unwrap();
    }
}

/// Totals of what a run exercised, to check that a fixture does what a test relies on.
#[derive(Debug, Default)]
pub struct Exercised {
    pub engagements: usize,
    pub successes: usize,
    pub failures: usize,
    pub node_hits: usize,
    pub launches: usize,
}

impl Exercised {
    pub fn record(&mut self, sim: &Sim) {
        for e in sim.engagement_events() {
            self.engagements += 1;
            if e.success {
                self.successes += 1;
            } else {
                self.failures += 1;
            }
            self.node_hits += usize::from(e.command_node_hit);
        }
        self.launches += sim.standing_order_launches().len();
    }

    /// Every RNG-drawing outcome, a launch and both roll results happened.
    pub fn assert_rich(&self) {
        assert!(
            self.successes > 0 && self.failures > 0 && self.launches > 0,
            "the fixture exercises the rules: {self:?}"
        );
    }
}
