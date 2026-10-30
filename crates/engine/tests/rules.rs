//! Phase 2 gameplay rules through the engine: pursuit, engagement, observation, exposure,
//! line of sight, the spatial grid, determinism and the heatmap. All numbers are test fixtures.

use simcore_abi::affiliation::{FRIENDLY, HOSTILE, NEUTRAL};
use simcore_abi::{ScUnitView, flags};
use simcore_engine::{
    EmitterCycle, EmitterView, EmsProfile, HeatmapSpec, OutcomeTable, Pcg32, Position, Pursuer,
    Rules, Sensor, SensorParams, SensorView, Sim, StableId, Terrain, UnitSpec, Velocity, kind,
    sensor_observes,
};
use simcore_rules::to_local_f32;

fn p(x: f64, y: f64, z: f64) -> Position {
    Position { x, y, z }
}

fn v(x: f64, y: f64, z: f64) -> Velocity {
    Velocity { x, y, z }
}

fn unit(affiliation: u8, position: Position, velocity: Velocity, power_w: f32) -> UnitSpec {
    UnitSpec {
        kind: kind::GROUND_VEHICLE,
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

fn pursuer(target: StableId, speed: f64, max_turn_rate: f64, lead: f64, radius: f64) -> Pursuer {
    Pursuer {
        target,
        speed,
        max_turn_rate,
        lead,
        engagement_radius_m: radius,
        outcome_class: 1,
    }
}

/// Rules with one shared sensor: 1,000 m against a 10 W reference.
fn rules(max_range_m: f64, chance: f64) -> Rules {
    Rules {
        detection_max_range_m: max_range_m,
        sensor: SensorParams {
            base_range_m: 1000.0,
            reference_power_w: 10.0,
        },
        exposure_threshold_ticks: 1,
        exposure_cap_ticks: 1,
        exposure_decay_per_tick: 1,
        outcomes: OutcomeTable::new(chance),
    }
}

/// Runs until the first engagement event, up to `max_ticks`. Returns its tick and success.
fn run_until_engaged(sim: &mut Sim, max_ticks: u32) -> Option<(u64, bool)> {
    for _ in 0..max_ticks {
        sim.step(1);
        if let Some(e) = sim.engagement_events().first() {
            return Some((e.tick, e.success));
        }
    }
    None
}

#[test]
fn pursuer_behind_slower_target_closes_and_engages() {
    let mut sim = Sim::new(1);
    sim.set_rules(rules(0.0, 1.0));
    let target = sim.spawn_unit(unit(HOSTILE, p(1000.0, 0.0, 100.0), v(20.0, 0.0, 0.0), 0.0));
    let m = sim.spawn_pursuer(
        unit(FRIENDLY, p(0.0, 0.0, 100.0), v(50.0, 0.0, 0.0), 0.0),
        pursuer(target, 50.0, 1.0, 0.0, 5.0),
    );
    assert_eq!(sim.unit_count(), 3);
    let entities = [sim.entity(target).unwrap(), sim.entity(m).unwrap()];
    // Closing at 30 m/s from 1,000 m: about 33 s.
    let (tick, success) = run_until_engaged(&mut sim, 40 * 15).expect("engages");
    let e = sim.engagement_events()[0];
    assert!(success && e.target_removed && !e.command_node_hit);
    assert_eq!(sim.command_node_hits(), 0);
    assert!((480..=520).contains(&tick), "engaged at tick {tick}");
    assert!(!sim.contains(target) && !sim.contains(m));
    assert_eq!(sim.unit_count(), 1);
    assert_eq!(sim.frame().len(), 1);
    // The entities are despawned outside `step`.
    sim.step(5);
    assert_eq!(sim.frame().len(), 1);
    sim.despawn_removed();
    for e in entities {
        assert!(sim.world().get_entity(e).is_err());
    }
    assert_eq!(sim.frame().len(), 1);
}

#[test]
fn an_event_reports_the_target_where_it_was_relative_to_the_node() {
    let mut sim = Sim::new(1);
    sim.set_rules(rules(0.0, 1.0));
    // The node is off the world origin, so world and frame coordinates differ.
    sim.set_command_node_position(p(-300.0, 50.0, 0.0));
    let target = sim.spawn_unit(unit(HOSTILE, p(1000.0, 0.0, 100.0), v(20.0, 0.0, 0.0), 0.0));
    sim.spawn_pursuer(
        unit(FRIENDLY, p(0.0, 0.0, 100.0), v(50.0, 0.0, 0.0), 0.0),
        pursuer(target, 50.0, 1.0, 0.0, 5.0),
    );
    let (tick, _) = run_until_engaged(&mut sim, 40 * 15).expect("engages");
    let e = sim.engagement_events()[0];
    assert_eq!(
        (e.target_kind, e.target_affiliation),
        (kind::GROUND_VEHICLE, HOSTILE)
    );
    // The target at the end of the engagement's tick, less the node's position.
    let x = 1000.0 + 20.0 * tick as f64 / 15.0;
    // Movement accumulates per tick, so allow for rounding against the closed form.
    assert!((e.x - to_local_f32(x, -300.0)).abs() < 0.01, "x {}", e.x);
    assert_eq!((e.y, e.z), (-50.0, 100.0));
}

#[test]
fn command_node_is_never_removed() {
    let mut sim = Sim::new(1);
    sim.set_rules(rules(0.0, 1.0));
    let cmd = StableId(1);
    let m = sim.spawn_pursuer(
        unit(HOSTILE, p(100.0, 0.0, 0.0), v(0.0, 0.0, 0.0), 0.0),
        pursuer(cmd, 50.0, 1.0, 0.0, 5.0),
    );
    assert_eq!(sim.command_node_hits(), 0);
    let (_, success) = run_until_engaged(&mut sim, 60).expect("engages");
    // Exactly one event, and it is a command-node hit.
    assert_eq!(sim.engagement_events().len(), 1);
    let e = sim.engagement_events()[0];
    assert!(success && e.command_node_hit && !e.target_removed);
    assert_eq!((e.pursuer, e.target), (m, cmd));
    assert_eq!(
        (e.target_kind, e.target_affiliation),
        (kind::COMMAND_NODE, FRIENDLY)
    );
    assert_eq!((e.x, e.y, e.z), (0.0, 0.0, 0.0));
    assert_eq!(sim.command_node_hits(), 1);
    assert!(sim.contains(cmd) && !sim.contains(m));
    assert_eq!(sim.unit_count(), 1);
    // The count is a running total: it outlives the step's events.
    sim.step(30);
    assert!(sim.engagement_events().is_empty());
    assert_eq!(sim.command_node_hits(), 1);
    // Everything that reads the command node keeps working.
    sim.set_command_node_position(p(10.0, 0.0, 0.0));
    let frame = sim.frame();
    assert_eq!(frame.len(), 1);
    assert_eq!((frame[0].id, frame[0].x), (1, 0.0));
    assert_ne!(frame[0].flags & flags::COMMAND_NODE, 0);
    for theta in [HeatmapSpec::EXACT, HeatmapSpec::DEFAULT_THETA] {
        let spec = HeatmapSpec {
            cell_size_m: 100.0,
            cells_x: 8,
            cells_y: 8,
            d_min_m: 1.0,
            cutoff_m: 1000.0,
            theta,
        };
        let map = sim.build_heatmap(spec, HOSTILE);
        assert_eq!((map.values().len(), map.origin().x), (64, 10.0));
    }

    // A second pursuer counts again; a failed roll does not.
    let m2 = sim.spawn_pursuer(
        unit(HOSTILE, p(110.0, 0.0, 0.0), v(0.0, 0.0, 0.0), 0.0),
        pursuer(cmd, 50.0, 1.0, 0.0, 5.0),
    );
    run_until_engaged(&mut sim, 60).expect("engages");
    assert_eq!(sim.command_node_hits(), 2);
    assert!(sim.contains(cmd) && !sim.contains(m2));
    sim.set_rules(rules(0.0, 0.0));
    sim.spawn_pursuer(
        unit(HOSTILE, p(110.0, 0.0, 0.0), v(0.0, 0.0, 0.0), 0.0),
        pursuer(cmd, 50.0, 1.0, 0.0, 5.0),
    );
    let (_, success) = run_until_engaged(&mut sim, 60).expect("engages");
    let e = sim.engagement_events()[0];
    assert!(!success && !e.command_node_hit && !e.target_removed);
    assert_eq!(sim.command_node_hits(), 2);
    assert_eq!(sim.frame().len(), 1);
}

/// A target crossing 300 m ahead of a pursuer heading north at 100 m/s.
fn crossing(turn_rate: f64, lead: f64) -> Option<(u64, bool)> {
    let mut sim = Sim::new(2);
    sim.set_rules(rules(0.0, 1.0));
    let target = sim.spawn_unit(unit(HOSTILE, p(150.0, 300.0, 0.0), v(30.0, 0.0, 0.0), 0.0));
    sim.spawn_pursuer(
        unit(FRIENDLY, p(0.0, 0.0, 0.0), v(0.0, 100.0, 0.0), 0.0),
        pursuer(target, 100.0, turn_rate, lead, 3.0),
    );
    run_until_engaged(&mut sim, 20 * 15)
}

#[test]
fn turn_rate_too_low_overshoots_a_close_crossing_target() {
    assert_eq!(crossing(0.05, 0.0), None, "overshoots within 300 ticks");
    assert!(crossing(3.0, 0.0).is_some(), "an agile pursuer does engage");
}

#[test]
fn lead_engages_a_crossing_target_sooner() {
    let engage = |lead: f64| {
        let mut sim = Sim::new(3);
        sim.set_rules(rules(0.0, 1.0));
        let target = sim.spawn_unit(unit(
            HOSTILE,
            p(-1500.0, 2000.0, 0.0),
            v(60.0, 0.0, 0.0),
            0.0,
        ));
        sim.spawn_pursuer(
            unit(FRIENDLY, p(0.0, 0.0, 0.0), v(0.0, 100.0, 0.0), 0.0),
            pursuer(target, 100.0, 1.0, lead, 5.0),
        );
        run_until_engaged(&mut sim, 120 * 15).expect("engages").0
    };
    let (pure, lead) = (engage(0.0), engage(0.5));
    println!("crossing target engaged at tick {pure} (pure pursuit), {lead} (lead 0.5)");
    assert!(lead < pure, "lead {lead} vs pure {pure}");
}

#[test]
fn degenerate_pursuers_stay_finite() {
    let mut sim = Sim::new(4);
    sim.set_rules(rules(0.0, 0.0));
    let t = sim.spawn_unit(unit(HOSTILE, p(0.0, 0.0, 0.0), v(0.0, 0.0, 0.0), 0.0));
    // Same position, zero speed, target behind, a missing target, and itself.
    let specs = [
        (p(0.0, 0.0, 0.0), v(0.0, 0.0, 0.0), t, 50.0),
        (p(10.0, 0.0, 0.0), v(5.0, 0.0, 0.0), t, 0.0),
        (p(10.0, 0.0, 0.0), v(5.0, 0.0, 0.0), t, 50.0),
        (p(10.0, 0.0, 0.0), v(5.0, 0.0, 0.0), StableId(9999), 50.0),
        (p(10.0, 0.0, 0.0), v(5.0, 0.0, 0.0), StableId(6), 50.0),
    ];
    for (pos, vel, target, speed) in specs {
        sim.spawn_pursuer(
            unit(FRIENDLY, pos, vel, 0.0),
            pursuer(target, speed, 2.0, 0.5, -1.0),
        );
    }
    sim.step(60);
    for u in sim.frame() {
        for x in [u.x, u.y, u.z, u.vx, u.vy, u.vz] {
            assert!(x.is_finite(), "{u:?}");
        }
    }
}

#[test]
fn swept_check_catches_a_skip_past() {
    let run = |radius: f64| {
        let mut sim = Sim::new(5);
        sim.set_rules(rules(0.0, 1.0));
        let target = sim.spawn_unit(unit(HOSTILE, p(0.0, 0.0, 0.0), v(0.0, 0.0, 0.0), 0.0));
        // 3,000 m/s is 200 m per tick: from x = -150 it lands at +50, 50 m past the target.
        // No turning, so it flies straight 2 m to the side.
        sim.spawn_pursuer(
            unit(FRIENDLY, p(-150.0, 2.0, 0.0), v(3000.0, 0.0, 0.0), 0.0),
            pursuer(target, 3000.0, 0.0, 0.0, radius),
        );
        sim.step(1);
        sim.engagement_events().to_vec()
    };
    let hit = run(5.0);
    assert_eq!(hit.len(), 1);
    assert!(hit[0].success);
    assert_eq!(hit[0].tick, 1);
    assert!(run(1.0).is_empty(), "2 m miss is outside a 1 m radius");
}

#[test]
fn pursuer_with_lost_target_flies_straight() {
    let mut sim = Sim::new(6);
    sim.set_rules(rules(0.0, 1.0));
    let target = sim.spawn_unit(unit(HOSTILE, p(0.0, 0.0, 0.0), v(0.0, 0.0, 0.0), 0.0));
    sim.spawn_pursuer(
        unit(FRIENDLY, p(0.0, 0.0, 0.0), v(1.0, 0.0, 0.0), 0.0),
        pursuer(target, 10.0, 1.0, 0.0, 1.0),
    );
    let late = sim.spawn_pursuer(
        unit(FRIENDLY, p(5000.0, 0.0, 0.0), v(0.0, 50.0, 0.0), 0.0),
        pursuer(target, 50.0, 1.0, 0.0, 1.0),
    );
    sim.step(1);
    assert_eq!(sim.engagement_events().len(), 1);
    assert!(!sim.contains(target));
    let vel = sim
        .world()
        .get::<Velocity>(sim.entity(late).unwrap())
        .copied();
    let before = sim.position(late).unwrap();
    sim.step(30);
    assert!(sim.engagement_events().is_empty());
    assert_eq!(
        sim.world()
            .get::<Velocity>(sim.entity(late).unwrap())
            .copied(),
        vel
    );
    let after = sim.position(late).unwrap();
    let vel = vel.unwrap();
    assert!((after.y - before.y - vel.y * 2.0).abs() < 1e-9);
    assert!((after.x - before.x - vel.x * 2.0).abs() < 1e-9);
    assert_eq!(sim.unit_count(), 2);
}

/// One friendly sensor at the origin and one hostile emitter; true if observed after a tick.
fn observed(rules: Rules, terrain: Terrain, emitter: UnitSpec, sensor_side: u8) -> bool {
    let mut sim = Sim::new(7);
    sim.set_rules(rules);
    sim.set_terrain(terrain);
    let s = sim.spawn_unit(unit(sensor_side, p(0.0, 0.0, 2.0), v(0.0, 0.0, 0.0), 0.0));
    assert!(sim.add_sensor(s, Sensor::default()));
    let e = sim.spawn_unit(emitter);
    sim.step(1);
    sim.exposure(e).unwrap().observed
}

#[test]
fn detection_range_scales_and_clamps() {
    let at = |x: f64, w: f32| unit(HOSTILE, p(x, 0.0, 2.0), v(0.0, 0.0, 0.0), w);
    let r = || rules(10_000.0, 0.0);
    let flat = Terrain::flat;
    // 40 W is 4x the reference: range 2,000 m.
    assert!(observed(r(), flat(), at(1990.0, 40.0), FRIENDLY));
    assert!(!observed(r(), flat(), at(2010.0, 40.0), FRIENDLY));
    // 2.5 W is a quarter: 500 m.
    assert!(observed(r(), flat(), at(499.0, 2.5), FRIENDLY));
    assert!(!observed(r(), flat(), at(501.0, 2.5), FRIENDLY));
    // Huge power is clamped to the 10 km max range.
    assert!(observed(r(), flat(), at(9990.0, 1e12), FRIENDLY));
    assert!(!observed(r(), flat(), at(10_010.0, 1e12), FRIENDLY));
    // Not emitting, same side, neutral.
    assert!(!observed(r(), flat(), at(100.0, 0.0), FRIENDLY));
    assert!(!observed(r(), flat(), at(100.0, 40.0), HOSTILE));
    assert!(!observed(r(), flat(), at(100.0, 40.0), NEUTRAL));
    // A sensor's own parameters override the shared ones.
    let mut sim = Sim::new(8);
    sim.set_rules(r());
    let s = sim.spawn_unit(unit(FRIENDLY, p(0.0, 0.0, 0.0), v(0.0, 0.0, 0.0), 0.0));
    sim.add_sensor(
        s,
        Sensor {
            params: Some(SensorParams {
                base_range_m: 3000.0,
                reference_power_w: 10.0,
            }),
        },
    );
    let e = sim.spawn_unit(at(2900.0, 10.0));
    sim.step(1);
    assert!(sim.exposure(e).unwrap().observed);
    assert!(!sim.add_sensor(StableId(999), Sensor::default()));
}

#[test]
fn terrain_ridge_blocks_line_of_sight() {
    // 5 x 1 samples, 500 m apart: a 100 m ridge at x = 1,000.
    let ridge =
        Terrain::heightfield(0.0, 0.0, 500.0, 5, 1, vec![0.0, 0.0, 100.0, 0.0, 0.0], 16).unwrap();
    let em = unit(HOSTILE, p(2000.0, 0.0, 2.0), v(0.0, 0.0, 0.0), 40.0);
    assert!(observed(
        rules(10_000.0, 0.0),
        Terrain::flat(),
        em,
        FRIENDLY
    ));
    assert!(!observed(rules(10_000.0, 0.0), ridge.clone(), em, FRIENDLY));
    // Above the ridge it is visible again.
    // (More power, so the longer slant range is still within detection range.)
    let high = unit(HOSTILE, p(2000.0, 0.0, 300.0), v(0.0, 0.0, 0.0), 80.0);
    assert!(observed(rules(10_000.0, 0.0), ridge, high, FRIENDLY));
}

fn detected_flag(sim: &mut Sim, id: StableId) -> bool {
    let u = *sim.frame().iter().find(|u| u.id == id.0).unwrap();
    u.flags & flags::DETECTED != 0
}

#[test]
fn exposure_reaches_threshold_on_the_exact_tick() {
    let mut sim = Sim::new(9);
    sim.set_rules(Rules {
        exposure_threshold_ticks: 7,
        exposure_cap_ticks: 100,
        ..rules(10_000.0, 0.0)
    });
    let s = sim.spawn_unit(unit(FRIENDLY, p(0.0, 0.0, 0.0), v(0.0, 0.0, 0.0), 0.0));
    sim.add_sensor(s, Sensor::default());
    let e = sim.spawn_unit(unit(HOSTILE, p(500.0, 0.0, 0.0), v(0.0, 0.0, 0.0), 10.0));
    sim.step(6);
    assert_eq!(sim.exposure(e).unwrap().ticks, 6);
    assert_eq!(sim.is_detected(e), Some(false));
    assert!(!detected_flag(&mut sim, e));
    sim.step(1);
    assert_eq!(sim.exposure(e).unwrap().ticks, 7);
    assert_eq!(sim.is_detected(e), Some(true));
    assert!(detected_flag(&mut sim, e));
    assert!(!detected_flag(&mut sim, s), "the sensor is not emitting");
}

#[test]
fn emitter_off_decays_exposure_and_clears_detected() {
    let mut sim = Sim::new(10);
    sim.set_rules(Rules {
        exposure_threshold_ticks: 5,
        exposure_cap_ticks: 10,
        exposure_decay_per_tick: 1,
        ..rules(10_000.0, 0.0)
    });
    let s = sim.spawn_unit(unit(FRIENDLY, p(0.0, 0.0, 0.0), v(0.0, 0.0, 0.0), 0.0));
    sim.add_sensor(s, Sensor::default());
    let e = sim.spawn_unit(UnitSpec {
        cycle: Some(EmitterCycle {
            on_ticks: 12,
            off_ticks: 1000,
            phase: 0,
        }),
        ..unit(HOSTILE, p(500.0, 0.0, 0.0), v(0.0, 0.0, 0.0), 10.0)
    });
    sim.step(12);
    assert_eq!(sim.exposure(e).unwrap().ticks, 10, "capped");
    assert!(detected_flag(&mut sim, e));
    sim.step(5);
    assert_eq!(sim.exposure(e).unwrap().ticks, 5);
    assert!(detected_flag(&mut sim, e));
    sim.step(1);
    assert_eq!(sim.exposure(e).unwrap().ticks, 4);
    assert!(!detected_flag(&mut sim, e));
    sim.step(100);
    assert_eq!(sim.exposure(e).unwrap().ticks, 0);
}

/// `n` pursuers each sitting on its own target, all engaging on the first tick.
fn rolls(seed: u64, n: u32, table: OutcomeTable) -> Vec<bool> {
    let mut sim = Sim::new(seed);
    sim.set_rules(Rules {
        outcomes: table,
        ..rules(0.0, 0.0)
    });
    for i in 0..n {
        let at = p(f64::from(i) * 100.0, 0.0, 0.0);
        let t = sim.spawn_unit(unit(HOSTILE, at, v(0.0, 0.0, 0.0), 0.0));
        sim.spawn_pursuer(
            unit(FRIENDLY, at, v(1.0, 0.0, 0.0), 0.0),
            pursuer(t, 10.0, 1.0, 0.0, 1.0),
        );
    }
    sim.step(1);
    let events = sim.engagement_events();
    assert_eq!(events.len(), n as usize);
    let removed = events.iter().filter(|e| e.success).count() as u32;
    assert_eq!(sim.unit_count(), 1 + n - removed);
    events.iter().map(|e| e.success).collect()
}

#[test]
fn outcome_table() {
    assert!(rolls(1, 500, OutcomeTable::new(1.0)).iter().all(|&s| s));
    assert!(rolls(1, 500, OutcomeTable::new(0.0)).iter().all(|&s| !s));
    // The table overrides the default for its (class, kind) pair only.
    let t = OutcomeTable::new(0.0).with(1, kind::GROUND_VEHICLE, 1.0);
    assert!(rolls(1, 100, t.clone()).iter().all(|&s| s));
    assert_eq!(t.chance(2, kind::GROUND_VEHICLE), 0.0);
    assert_eq!(t.chance(1, kind::JAMMER), 0.0);
    assert_eq!(OutcomeTable::new(7.0).chance(0, 0), 1.0);
    assert_eq!(OutcomeTable::new(f64::NAN).chance(0, 0), 0.0);

    let a = rolls(42, 10_000, OutcomeTable::new(0.3));
    let rate = a.iter().filter(|&&s| s).count() as f64 / a.len() as f64;
    println!("success rate at 0.3 over 10,000 rolls: {rate:.4}");
    assert!((rate - 0.3).abs() < 0.02, "rate {rate}");
    assert_eq!(a, rolls(42, 10_000, OutcomeTable::new(0.3)), "same seed");
    assert_ne!(
        a,
        rolls(43, 10_000, OutcomeTable::new(0.3)),
        "different seed"
    );
}

#[test]
fn grid_observation_equals_brute_force() {
    const R: f64 = 3000.0;
    let mut rng = Pcg32::new(2024, 7);
    let terrain = {
        let heights = (0..32 * 32)
            .map(|_| rng.range_f64(0.0, 400.0) as f32)
            .collect();
        Terrain::heightfield(-16_000.0, -16_000.0, 1000.0, 32, 32, heights, 12).unwrap()
    };
    let rules = Rules {
        sensor: SensorParams {
            base_range_m: 1500.0,
            reference_power_w: 10.0,
        },
        ..rules(R, 0.0)
    };
    let mut sim = Sim::new(11);
    sim.set_rules(rules.clone());
    sim.set_terrain(terrain.clone());

    let mut sensors = Vec::new();
    let mut emitters = Vec::new();
    let mut add = |sim: &mut Sim, rng: &mut Pcg32, pos: Position, aff: u8, sensor: bool, w: f32| {
        let id = sim.spawn_unit(unit(aff, pos, v(0.0, 0.0, 0.0), w));
        if sensor {
            let own = rng.below(2) == 0;
            let params = own.then(|| SensorParams {
                base_range_m: rng.range_f64(500.0, 4000.0),
                reference_power_w: rng.range_f64(1.0, 100.0),
            });
            sim.add_sensor(id, Sensor { params });
            sensors.push(SensorView {
                position: [pos.x, pos.y, pos.z],
                affiliation: aff,
                params: params.unwrap_or(rules.sensor),
            });
        }
        emitters.push((
            id,
            EmitterView {
                position: [pos.x, pos.y, pos.z],
                affiliation: aff,
                power_w: f64::from(w),
            },
        ));
        id
    };

    let mut edge_pairs = Vec::new();
    for i in 0..2000 {
        let aff = [FRIENDLY, HOSTILE, NEUTRAL][rng.below(3) as usize];
        let (mut x, mut y) = (
            rng.range_f64(-15_000.0, 15_000.0),
            rng.range_f64(-15_000.0, 15_000.0),
        );
        // A fifth of the units sit exactly on cell edges (multiples of the max range).
        if i % 5 == 0 {
            x = (x / R).round() * R;
        }
        if i % 10 == 0 {
            y = (y / R).round() * R;
        }
        let z = terrain.height_at(x, y) + rng.range_f64(0.0, 600.0);
        let sensor = rng.below(10) < 3;
        let w = if rng.below(10) < 4 {
            10f64.powf(rng.range_f64(-1.0, 4.0)) as f32
        } else {
            0.0
        };
        add(&mut sim, &mut rng, p(x, y, z), aff, sensor, w);
    }
    // Pairs exactly at max range, straddling cell edges, high above the hills.
    for k in -3..3 {
        let (x, y) = (f64::from(k) * R, f64::from(k) * R);
        let s = add(&mut sim, &mut rng, p(x, y, 5000.0), FRIENDLY, true, 0.0);
        let e = add(&mut sim, &mut rng, p(x + R, y, 5000.0), HOSTILE, false, 1e9);
        let f = add(&mut sim, &mut rng, p(x, y - R, 5000.0), HOSTILE, false, 1e9);
        edge_pairs.extend([(s, e), (s, f)]);
    }

    // Far outliers (one sensor per side, and emitters near them) must not coarsen the grid or
    // break the equality.
    for (x, y) in [(1e7, 1e7), (-2e7, 5e6)] {
        add(&mut sim, &mut rng, p(x, y, 5000.0), FRIENDLY, true, 0.0);
        add(
            &mut sim,
            &mut rng,
            p(x, y + 50.0, 5000.0),
            HOSTILE,
            true,
            0.0,
        );
        add(
            &mut sim,
            &mut rng,
            p(x + 1000.0, y, 5000.0),
            HOSTILE,
            false,
            1e3,
        );
        add(
            &mut sim,
            &mut rng,
            p(x - 1000.0, y, 5000.0),
            FRIENDLY,
            false,
            1e3,
        );
        add(
            &mut sim,
            &mut rng,
            p(x + R + 1.0, y, 5000.0),
            HOSTILE,
            false,
            1e9,
        );
    }

    sim.step(1);
    let (mut seen, mut unseen) = (0, 0);
    for (id, e) in &emitters {
        let brute = sensors
            .iter()
            .any(|s| sensor_observes(s, e, &rules, &terrain));
        let grid = sim.exposure(*id).unwrap().observed;
        assert_eq!(grid, brute, "unit {id:?} at {:?}", e.position);
        if e.power_w > 0.0 {
            if brute { seen += 1 } else { unseen += 1 }
        }
    }
    for (_, e) in edge_pairs {
        assert!(
            sim.exposure(e).unwrap().observed,
            "{e:?} at exactly max range"
        );
    }
    println!("grid == brute force: {seen} emitters observed, {unseen} not");
    assert!(seen > 50 && unseen > 50, "{seen} observed, {unseen} not");
}

#[test]
fn extreme_coordinates_do_not_panic() {
    let hostile_at = |x: f64| unit(HOSTILE, p(x, 0.0, 0.0), v(0.0, 0.0, 0.0), 10.0);
    // Sensors spread to the edges of f64, and a tiny max range over a wide span.
    for (range, xs) in [
        (5000.0, [-1e300, 1e300]),
        (5000.0, [-1e23, 1e23]),
        (5000.0, [-f64::MAX, f64::MAX]),
        (2.0, [-1e19, 1e19]),
        (1e-12, [-6e6, 6e6]),
    ] {
        let mut sim = Sim::new(13);
        sim.set_rules(rules(range, 0.0));
        for x in xs {
            let s = sim.spawn_unit(unit(FRIENDLY, p(x, 0.0, 0.0), v(0.0, 0.0, 0.0), 0.0));
            sim.add_sensor(s, Sensor::default());
        }
        let e = sim.spawn_unit(hostile_at(0.0));
        sim.step(2);
        assert!(!sim.exposure(e).unwrap().observed, "range {range}, {xs:?}");
        sim.frame();
    }
    // A co-located sensor and emitter far from the origin are still observed.
    for x in [1e23, -1e15, 1e300] {
        let mut sim = Sim::new(14);
        let rules = rules(5000.0, 0.0);
        sim.set_rules(rules.clone());
        let s = sim.spawn_unit(unit(FRIENDLY, p(x, 0.0, 0.0), v(0.0, 0.0, 0.0), 0.0));
        sim.add_sensor(s, Sensor::default());
        let e = sim.spawn_unit(hostile_at(x));
        sim.step(1);
        let brute = sensor_observes(
            &SensorView {
                position: [x, 0.0, 0.0],
                affiliation: FRIENDLY,
                params: rules.sensor,
            },
            &EmitterView {
                position: [x, 0.0, 0.0],
                affiliation: HOSTILE,
                power_w: 10.0,
            },
            &rules,
            &Terrain::flat(),
        );
        assert!(brute);
        assert_eq!(sim.exposure(e).unwrap().observed, brute, "at x = {x}");
    }
    // A heatmap with a tiny cutoff over widely spread emitters.
    let mut sim = Sim::new(15);
    for x in [-3e6, 3e6, -1e300, 1e300] {
        sim.spawn_unit(hostile_at(x));
    }
    let spec = HeatmapSpec {
        cell_size_m: 2000.0,
        cells_x: 4096,
        cells_y: 1,
        d_min_m: 1.0,
        cutoff_m: 1e-12,
        theta: HeatmapSpec::EXACT,
    };
    assert_eq!(sim.build_heatmap(spec, HOSTILE).values().len(), 4096);
}

/// Every field of a view as raw bits.
fn bits(frame: &[ScUnitView]) -> Vec<[u64; 5]> {
    frame
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
        .collect()
}

/// Dummy units plus sensors, pursuers near their targets and hilly terrain.
fn scenario(seed: u64) -> Sim {
    let mut sim = Sim::new_dummy(seed, 3000);
    let mut rng = Pcg32::new(seed, 99);
    let heights = (0..32 * 32)
        .map(|_| rng.range_f64(0.0, 200.0) as f32)
        .collect();
    sim.set_terrain(
        Terrain::heightfield(-40_000.0, -40_000.0, 2500.0, 32, 32, heights, 8).unwrap(),
    );
    sim.set_rules(Rules {
        sensor: SensorParams {
            base_range_m: 3000.0,
            reference_power_w: 100.0,
        },
        exposure_threshold_ticks: 15,
        exposure_cap_ticks: 60,
        exposure_decay_per_tick: 2,
        ..rules(8000.0, 0.5)
    });
    for id in (2..3002).step_by(10) {
        sim.add_sensor(StableId(id), Sensor::default());
    }
    for _ in 0..60 {
        let target = StableId(2 + u64::from(rng.below(3000)));
        let t = sim.position(target).unwrap();
        let a = rng.range_f64(0.0, core::f64::consts::TAU);
        sim.spawn_pursuer(
            unit(
                FRIENDLY,
                p(t.x + 1500.0 * a.cos(), t.y + 1500.0 * a.sin(), t.z + 200.0),
                v(0.0, 0.0, 0.0),
                5.0,
            ),
            pursuer(target, 120.0, 0.8, 0.4, 10.0),
        );
    }
    sim
}

#[test]
fn determinism_with_rules() {
    let (mut a, mut b) = (scenario(5), scenario(5));
    let mut events = 0;
    for _ in 0..600 {
        a.step(1);
        b.step(1);
        assert_eq!(a.engagement_events(), b.engagement_events());
        events += a.engagement_events().len();
    }
    let fa = bits(a.frame());
    assert!(fa == bits(b.frame()), "bitwise-identical after 600 ticks");
    // Hidden state too (exposure, RNG, pursuit), not only what the frame shows.
    let saved = a.save().to_vec();
    assert!(saved == b.save(), "identical saves after 600 ticks");
    assert_eq!(a.unit_count(), b.unit_count());
    assert_eq!(fa.len(), a.unit_count() as usize);
    let detected = a
        .frame()
        .iter()
        .filter(|u| u.flags & flags::DETECTED != 0)
        .count();
    println!(
        "600 ticks: {events} engagements, {} units left, {detected} detected",
        a.unit_count()
    );
    assert!(
        events > 10 && detected > 0,
        "the scenario exercises the rules"
    );

    // Chunked stepping is the same simulation, removals included.
    let mut c = scenario(5);
    for _ in 0..40 {
        c.step(15);
    }
    assert!(bits(c.frame()) == fa);
    assert!(c.save() == &saved[..]);
    let mut d = scenario(6);
    d.step(600);
    assert!(bits(d.frame()) != fa);
}

#[test]
fn floating_origin_property() {
    let mut rng = Pcg32::new(77, 1);
    let mut worst: f64 = 0.0;
    for _ in 0..200 {
        let mut sim = Sim::new(1);
        let o = p(
            rng.range_f64(-1e7, 1e7),
            rng.range_f64(-1e7, 1e7),
            rng.range_f64(-1e4, 1e4),
        );
        sim.set_command_node_position(o);
        let mut offsets = Vec::new();
        for _ in 0..50 {
            let d = (
                rng.range_f64(-1e5, 1e5),
                rng.range_f64(-1e5, 1e5),
                rng.range_f64(-1e4, 1e4),
            );
            let id = sim.spawn_unit(unit(
                NEUTRAL,
                p(o.x + d.0, o.y + d.1, o.z + d.2),
                v(0.0, 0.0, 0.0),
                0.0,
            ));
            offsets.push((id, d));
        }
        let frame = sim.frame();
        for (id, d) in offsets {
            let u = frame.iter().find(|u| u.id == id.0).unwrap();
            for (got, want) in [(u.x, d.0), (u.y, d.1), (u.z, d.2)] {
                worst = worst.max((f64::from(got) - want).abs());
            }
            // The engine uses the rules crate's conversion exactly.
            assert_eq!(u.x, to_local_f32(o.x + d.0, o.x));
        }
    }
    println!("worst floating-origin error: {worst:.6} m");
    assert!(worst < 0.01);
}

#[test]
fn heatmap_inverse_square_and_empty() {
    let mut sim = Sim::new(12);
    sim.set_command_node_position(p(10_000.0, -5_000.0, 0.0));
    let spec = HeatmapSpec {
        cell_size_m: 100.0,
        cells_x: 64,
        cells_y: 64,
        d_min_m: 1.0,
        cutoff_m: 100_000.0,
        theta: HeatmapSpec::EXACT,
    };
    // Nothing emits yet (a silent hostile and an emitting friendly).
    sim.spawn_unit(unit(
        HOSTILE,
        p(10_050.0, -4_950.0, 0.0),
        v(0.0, 0.0, 0.0),
        0.0,
    ));
    sim.spawn_unit(unit(
        FRIENDLY,
        p(10_050.0, -4_950.0, 0.0),
        v(0.0, 0.0, 0.0),
        50.0,
    ));
    let map = sim.build_heatmap(spec, HOSTILE);
    assert_eq!(map.values().len(), 64 * 64);
    assert!(map.values().iter().all(|&s| s == 0.0));

    // Cell (32, 32) is centred at (+50, +50) from the command node; put an emitter there.
    sim.spawn_unit(unit(
        HOSTILE,
        p(10_050.0, -4_950.0, 0.0),
        v(0.0, 0.0, 0.0),
        100.0,
    ));
    let map = sim.build_heatmap(spec, HOSTILE);
    assert_eq!(map.cell_center(32, 32), Some((50.0, 50.0)));
    let far = map.get(52, 32).unwrap(); // 2,000 m east
    let near = map.get(42, 32).unwrap(); // 1,000 m east
    assert!((far - 100.0 / 2000.0f32.powi(2)).abs() < 1e-9);
    assert!((near / far - 4.0).abs() < 1e-5, "ratio {}", near / far);
    assert_eq!(map.get(32, 32), Some(100.0), "clamped at d_min");
    let cap = map.capacity();
    for _ in 0..5 {
        sim.step(1);
        assert_eq!(sim.build_heatmap(spec, HOSTILE).capacity(), cap);
    }

    // Beyond the cutoff contributes nothing; an invalid spec is an empty map.
    let short = HeatmapSpec {
        cutoff_m: 500.0,
        ..spec
    };
    assert_eq!(sim.build_heatmap(short, HOSTILE).get(52, 32), Some(0.0));
    let bad = HeatmapSpec {
        cell_size_m: 0.0,
        ..spec
    };
    assert!(sim.build_heatmap(bad, HOSTILE).values().is_empty());
}
