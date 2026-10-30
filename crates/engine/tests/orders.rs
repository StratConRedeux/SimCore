//! Commands, link delay and the status summary. All numbers are test fixtures.

use simcore_abi::affiliation::{FRIENDLY, HOSTILE};
use simcore_abi::flags;
use simcore_engine::{
    CommandError, EmitterCycle, EmsProfile, LinkRules, Order, OutcomeTable, Position, Pursuer,
    Rules, Sensor, SensorParams, Sim, StableId, UnitSpec, Velocity, kind,
};

fn p(x: f64, y: f64, z: f64) -> Position {
    Position { x, y, z }
}

fn unit(affiliation: u8, k: u16, position: Position, power_w: f32) -> UnitSpec {
    UnitSpec {
        kind: k,
        affiliation,
        position,
        velocity: Velocity::default(),
        ems: EmsProfile {
            emitting: power_w > 0.0,
            erp_w: power_w,
        },
        cycle: None,
    }
}

fn mv(x: f64, y: f64, speed_mps: f64) -> Order {
    Order::Move {
        destination: p(x, y, 0.0),
        speed_mps,
    }
}

fn emitting(sim: &mut Sim, id: StableId) -> bool {
    let f = sim.frame().iter().find(|u| u.id == id.0).unwrap();
    f.flags & flags::EMITTING != 0
}

#[test]
fn move_arrives_and_stops() {
    let mut sim = Sim::new(1);
    let id = sim.spawn_unit(unit(FRIENDLY, kind::GROUND_VEHICLE, p(0.0, 0.0, 0.0), 0.0));
    assert_eq!(sim.command(id, mv(100.0, 0.0, 15.0)), Ok(1));
    assert_eq!(sim.pending_commands(), 1);
    // 100 m at 15 m/s is 6.67 s: 100 ticks, the last one partial.
    sim.step(99);
    let x = sim.position(id).unwrap().x;
    assert!((x - 99.0).abs() < 1e-9, "x = {x}");
    sim.step(1);
    assert!((sim.position(id).unwrap().x - 100.0).abs() < 1e-9);
    sim.step(10);
    let end = sim.position(id).unwrap();
    assert!(
        (end.x - 100.0).abs() < 1e-9 && end.y == 0.0,
        "stopped there: {end:?}"
    );
    assert_eq!(sim.pending_commands(), 0);
}

#[test]
fn halt_and_replacement_moves() {
    let mut sim = Sim::new(1);
    let id = sim.spawn_unit(unit(FRIENDLY, kind::GROUND_VEHICLE, p(0.0, 0.0, 0.0), 0.0));
    sim.command(id, mv(1000.0, 0.0, 15.0)).unwrap();
    sim.step(15);
    // A later move replaces the first.
    sim.command(id, mv(0.0, 1000.0, 15.0)).unwrap();
    sim.step(15);
    let q = sim.position(id).unwrap();
    assert!(
        q.x < 15.0 && q.y > 1.0,
        "turned toward the second destination: {q:?}"
    );
    sim.command(id, Order::Halt).unwrap();
    sim.step(1);
    let stopped = sim.position(id).unwrap();
    sim.step(30);
    assert_eq!(sim.position(id).unwrap(), stopped);
}

#[test]
fn base_delay_is_exact() {
    let mut sim = Sim::new(1);
    sim.set_link_rules(LinkRules {
        base_delay_ticks: 5,
        ..LinkRules::default()
    });
    let id = sim.spawn_unit(unit(FRIENDLY, kind::GROUND_VEHICLE, p(0.0, 0.0, 0.0), 0.0));
    assert_eq!(sim.link_delay(id), Some(5));
    sim.command(id, mv(100.0, 0.0, 15.0)).unwrap();
    sim.step(5);
    assert_eq!(sim.position(id).unwrap().x, 0.0, "not delivered yet");
    assert_eq!(sim.pending_commands(), 1);
    sim.step(1);
    assert!((sim.position(id).unwrap().x - 1.0).abs() < 1e-9);
    assert_eq!(sim.pending_commands(), 0);
}

#[test]
fn delay_grows_with_distance_and_jamming() {
    let mut sim = Sim::new(1);
    sim.set_link_rules(LinkRules {
        base_delay_ticks: 2,
        delay_ticks_per_km: 1.5,
        jam_radius_m: 2000.0,
        jam_delay_ticks: 30,
        max_delay_ticks: 1000,
    });
    let near = sim.spawn_unit(unit(FRIENDLY, kind::GROUND_VEHICLE, p(0.0, 0.0, 0.0), 0.0));
    let far = sim.spawn_unit(unit(
        FRIENDLY,
        kind::GROUND_VEHICLE,
        p(10_000.0, 0.0, 0.0),
        0.0,
    ));
    assert_eq!(sim.link_delay(near), Some(2));
    // 10 km * 1.5 = 15.
    assert_eq!(sim.link_delay(far), Some(17));
    assert_eq!(sim.status().link_delay_ticks, 2);

    // A hostile jammer near `far`, emitting.
    let jammer = sim.spawn_unit(UnitSpec {
        cycle: Some(EmitterCycle {
            on_ticks: 1,
            off_ticks: 1,
            phase: 0,
        }),
        ..unit(HOSTILE, kind::JAMMER, p(11_000.0, 0.0, 0.0), 500.0)
    });
    assert_eq!(sim.link_delay(far), Some(47));
    assert_eq!(sim.link_delay(near), Some(2), "out of the jammer's radius");
    // A tick applies the cycle at its starting tick: tick 0 is on, tick 1 is off.
    sim.step(1);
    assert!(emitting(&mut sim, jammer));
    sim.step(1);
    assert!(!emitting(&mut sim, jammer));
    assert_eq!(sim.link_delay(far), Some(17));

    // A friendly jammer, or a hostile non-jammer, does not jam.
    let mut sim2 = Sim::new(1);
    sim2.set_link_rules(*sim.link_rules());
    let u = sim2.spawn_unit(unit(FRIENDLY, kind::GROUND_VEHICLE, p(0.0, 0.0, 0.0), 0.0));
    sim2.spawn_unit(unit(FRIENDLY, kind::JAMMER, p(10.0, 0.0, 0.0), 500.0));
    sim2.spawn_unit(unit(HOSTILE, kind::GROUND_RADAR, p(10.0, 0.0, 0.0), 500.0));
    assert_eq!(sim2.link_delay(u), Some(2));
    sim2.spawn_unit(unit(HOSTILE, kind::JAMMER, p(10.0, 0.0, 0.0), 500.0));
    assert_eq!(sim2.link_delay(u), Some(32));
    assert_eq!(
        sim2.status().link_delay_ticks,
        32,
        "the node itself is jammed"
    );

    // The cap, and degenerate rules.
    sim2.set_link_rules(LinkRules {
        delay_ticks_per_km: f64::INFINITY,
        max_delay_ticks: 600,
        ..LinkRules::default()
    });
    let distant = sim2.spawn_unit(unit(FRIENDLY, kind::GROUND_VEHICLE, p(5.0, 0.0, 0.0), 0.0));
    assert_eq!(sim2.link_delay(distant), Some(600));
    // Zero distance adds nothing even at an infinite rate.
    assert_eq!(sim2.link_delay(StableId(1)), Some(0));
    // No real delay equals UINT32_MAX, which the C API uses for "no such unit".
    sim2.set_link_rules(LinkRules {
        base_delay_ticks: u32::MAX,
        ..LinkRules::default()
    });
    assert_eq!(sim2.link_delay(distant), Some(u32::MAX - 1));
    sim2.set_link_rules(LinkRules {
        delay_ticks_per_km: f64::NAN,
        jam_radius_m: f64::NAN,
        jam_delay_ticks: 9,
        ..LinkRules::default()
    });
    assert_eq!(sim2.link_delay(distant), Some(0));
    assert_eq!(sim2.link_delay(StableId(999)), None);
}

fn pursuer(target: StableId) -> Pursuer {
    Pursuer {
        target,
        speed: 50.0,
        max_turn_rate: 3.0,
        lead: 0.0,
        engagement_radius_m: 5.0,
        outcome_class: 1,
    }
}

#[test]
fn rejected_commands() {
    let mut sim = Sim::new(1);
    let friendly = sim.spawn_unit(unit(FRIENDLY, kind::GROUND_VEHICLE, p(0.0, 0.0, 0.0), 0.0));
    let hostile = sim.spawn_unit(unit(HOSTILE, kind::GROUND_VEHICLE, p(500.0, 0.0, 0.0), 0.0));
    let missile = sim.spawn_pursuer(
        unit(FRIENDLY, kind::FIXED_WING_DRONE, p(0.0, 500.0, 100.0), 0.0),
        pursuer(hostile),
    );
    use CommandError::*;
    assert_eq!(sim.command(StableId(99), Order::Halt), Err(UnknownUnit));
    assert_eq!(sim.command(hostile, Order::Halt), Err(NotFriendly));
    assert_eq!(sim.command(missile, mv(1.0, 1.0, 1.0)), Err(WrongUnitType));
    assert_eq!(sim.command(missile, Order::Halt), Err(WrongUnitType));
    let retarget = |target| Order::Retarget { target };
    assert_eq!(sim.command(friendly, retarget(hostile)), Err(WrongUnitType));
    assert_eq!(sim.command(missile, retarget(missile)), Err(InvalidOrder));
    assert_eq!(
        sim.command(missile, retarget(StableId(99))),
        Err(InvalidOrder)
    );
    for bad in [
        mv(f64::NAN, 0.0, 1.0),
        mv(f64::INFINITY, 0.0, 1.0),
        mv(1.0, 0.0, 0.0),
        mv(1.0, 0.0, -3.0),
        mv(1.0, 0.0, f64::NAN),
        mv(1.0, 0.0, f64::INFINITY),
    ] {
        assert_eq!(sim.command(friendly, bad), Err(InvalidOrder), "{bad:?}");
    }
    assert_eq!(sim.pending_commands(), 0, "nothing rejected was queued");
    // Valid ones get increasing ids, the command node included.
    assert_eq!(sim.command(missile, retarget(friendly)), Ok(1));
    assert_eq!(sim.command(StableId(1), mv(5.0, 5.0, 2.0)), Ok(2));
    assert_eq!(
        sim.command(missile, Order::Emissions { radiate: false }),
        Ok(3)
    );
}

#[test]
fn retarget_and_drop_at_delivery() {
    let mut sim = Sim::new(1);
    sim.set_rules(Rules {
        outcomes: OutcomeTable::new(1.0),
        ..Rules::default()
    });
    sim.set_link_rules(LinkRules {
        base_delay_ticks: 3,
        ..LinkRules::default()
    });
    let a = sim.spawn_unit(unit(HOSTILE, kind::GROUND_VEHICLE, p(200.0, 0.0, 0.0), 0.0));
    let b = sim.spawn_unit(unit(
        HOSTILE,
        kind::GROUND_VEHICLE,
        p(-200.0, 0.0, 0.0),
        0.0,
    ));
    let m = sim.spawn_pursuer(
        unit(FRIENDLY, kind::FIXED_WING_DRONE, p(0.0, 0.0, 0.0), 0.0),
        pursuer(a),
    );
    sim.command(m, Order::Retarget { target: b }).unwrap();
    // Delivered at the start of tick 3, then it flies to b.
    let mut hit = None;
    for _ in 0..200 {
        sim.step(1);
        if let Some(e) = sim.engagement_events().first() {
            hit = Some(*e);
            break;
        }
    }
    let e = hit.expect("engaged");
    assert_eq!(e.target, b);
    assert!(sim.contains(a) && !sim.contains(b) && !sim.contains(m));

    // A command to a unit removed before delivery is dropped and counted.
    let victim = sim.spawn_unit(unit(FRIENDLY, kind::GROUND_VEHICLE, p(0.0, 0.0, 0.0), 0.0));
    sim.spawn_pursuer(
        unit(HOSTILE, kind::FIXED_WING_DRONE, p(0.0, 1.0, 0.0), 0.0),
        pursuer(victim),
    );
    sim.set_link_rules(LinkRules {
        base_delay_ticks: 50,
        ..LinkRules::default()
    });
    sim.command(victim, mv(10.0, 0.0, 1.0)).unwrap();
    sim.step(1);
    assert!(!sim.contains(victim));
    let s = sim.status();
    assert_eq!((s.pending_commands, s.dropped_commands), (1, 0));
    assert_eq!(s.awaiting_cleanup, 2, "victim and pursuer");
    sim.step(60);
    let s = sim.status();
    assert_eq!((s.pending_commands, s.dropped_commands), (0, 1));
    sim.despawn_removed();
    assert_eq!(sim.status().awaiting_cleanup, 0);
}

#[test]
fn emission_control() {
    let mut sim = Sim::new(1);
    let cycled = sim.spawn_unit(UnitSpec {
        cycle: Some(EmitterCycle {
            on_ticks: 10,
            off_ticks: 10,
            phase: 0,
        }),
        ..unit(FRIENDLY, kind::GROUND_RADAR, p(0.0, 0.0, 0.0), 100.0)
    });
    let steady = sim.spawn_unit(unit(FRIENDLY, kind::JAMMER, p(1.0, 0.0, 0.0), 100.0));
    assert!(emitting(&mut sim, cycled) && emitting(&mut sim, steady));
    assert_eq!(sim.status().friendly_emitting, 2);

    sim.command(cycled, Order::Emissions { radiate: false })
        .unwrap();
    sim.command(steady, Order::Emissions { radiate: false })
        .unwrap();
    for _ in 0..40 {
        sim.step(1);
        assert!(!emitting(&mut sim, cycled) && !emitting(&mut sim, steady));
    }
    assert_eq!(sim.status().friendly_emitting, 0);

    sim.command(cycled, Order::Emissions { radiate: true })
        .unwrap();
    sim.command(steady, Order::Emissions { radiate: true })
        .unwrap();
    // Tick 40 is "on" for the cycle (40 % 20 < 10).
    sim.step(1);
    assert!(emitting(&mut sim, cycled) && emitting(&mut sim, steady));
    sim.step(10);
    assert!(!emitting(&mut sim, cycled), "back on its duty cycle");
}

#[test]
fn status_counts() {
    let mut sim = Sim::new(1);
    sim.set_rules(Rules {
        detection_max_range_m: 5000.0,
        sensor: SensorParams {
            base_range_m: 5000.0,
            reference_power_w: 1.0,
        },
        ..Rules::default()
    });
    let f_sensor = sim.spawn_unit(unit(FRIENDLY, kind::GROUND_VEHICLE, p(0.0, 0.0, 0.0), 0.0));
    sim.add_sensor(f_sensor, Sensor::default());
    let h_sensor = sim.spawn_unit(unit(HOSTILE, kind::GROUND_VEHICLE, p(100.0, 0.0, 0.0), 0.0));
    sim.add_sensor(h_sensor, Sensor::default());
    // Two friendly emitters, one hostile emitter, one silent hostile.
    sim.spawn_unit(unit(FRIENDLY, kind::JAMMER, p(10.0, 0.0, 0.0), 10.0));
    sim.spawn_unit(unit(FRIENDLY, kind::JAMMER, p(20.0, 0.0, 0.0), 10.0));
    sim.spawn_unit(unit(HOSTILE, kind::GROUND_RADAR, p(30.0, 0.0, 0.0), 10.0));
    sim.spawn_unit(unit(HOSTILE, kind::GROUND_VEHICLE, p(40.0, 0.0, 0.0), 0.0));
    sim.step(1);
    let s = sim.status();
    assert_eq!(s.tick, 1);
    assert_eq!(s.unit_count, 7);
    assert_eq!(s.friendly_emitting, 2);
    assert_eq!(s.friendly_detected, 2);
    assert_eq!(s.hostile_detected, 1);
    assert_eq!(s.link_delay_ticks, 0);
}

#[test]
fn unrepresentably_far_moves_end_instead_of_stalling_or_going_nan() {
    let mut sim = Sim::new(1);
    // dx * dx overflows at this range; the scaled distance does not.
    let a = sim.spawn_unit(unit(FRIENDLY, kind::GROUND_VEHICLE, p(0.0, 0.0, 0.0), 0.0));
    sim.command(a, mv(1e200, 0.0, 10.0)).unwrap();
    // The difference itself is not finite here.
    let b = sim.spawn_unit(unit(
        FRIENDLY,
        kind::GROUND_VEHICLE,
        p(-1e308, 0.0, 0.0),
        0.0,
    ));
    sim.command(b, mv(1e308, 0.0, 10.0)).unwrap();
    sim.step(3);
    assert!(
        (sim.position(a).unwrap().x - 2.0).abs() < 1e-9,
        "moving at 10 m/s"
    );
    let pb = sim.position(b).unwrap();
    assert!(pb.x == -1e308 && pb.y == 0.0, "stopped, not NaN: {pb:?}");
}

#[test]
fn direct_orders_reach_any_side_at_once() {
    let mut sim = Sim::new(1);
    sim.set_rules(Rules {
        outcomes: OutcomeTable::new(1.0),
        ..Rules::default()
    });
    // A long link delay and a hostile jammer next to everything: direct orders ignore both.
    sim.set_link_rules(LinkRules {
        base_delay_ticks: 500,
        jam_radius_m: 10_000.0,
        jam_delay_ticks: 500,
        max_delay_ticks: 2000,
        ..LinkRules::default()
    });
    sim.spawn_unit(unit(HOSTILE, kind::JAMMER, p(0.0, -100.0, 0.0), 100.0));
    let mover = sim.spawn_unit(unit(HOSTILE, kind::GROUND_VEHICLE, p(0.0, 50.0, 0.0), 0.0));
    let drifter = sim.spawn_unit(UnitSpec {
        velocity: Velocity {
            x: 3.0,
            y: 0.0,
            z: 0.0,
        },
        ..unit(HOSTILE, kind::GROUND_VEHICLE, p(0.0, -50.0, 0.0), 0.0)
    });
    let radar = sim.spawn_unit(UnitSpec {
        cycle: Some(EmitterCycle {
            on_ticks: 10,
            off_ticks: 10,
            phase: 0,
        }),
        ..unit(HOSTILE, kind::GROUND_RADAR, p(500.0, 0.0, 0.0), 100.0)
    });
    let neutral = sim.spawn_unit(unit(0, kind::GROUND_VEHICLE, p(900.0, 0.0, 0.0), 0.0));
    let a = sim.spawn_unit(unit(
        FRIENDLY,
        kind::GROUND_VEHICLE,
        p(2000.0, 0.0, 0.0),
        0.0,
    ));
    let b = sim.spawn_unit(unit(
        FRIENDLY,
        kind::GROUND_VEHICLE,
        p(-300.0, 0.0, 0.0),
        0.0,
    ));
    let seeker = sim.spawn_pursuer(
        unit(HOSTILE, kind::FIXED_WING_DRONE, p(-200.0, 0.0, 0.0), 0.0),
        pursuer(a),
    );

    use CommandError::*;
    // Same type and value checks as commands, for every side.
    assert_eq!(sim.order(StableId(99), Order::Halt), Err(UnknownUnit));
    assert_eq!(sim.order(seeker, mv(1.0, 1.0, 1.0)), Err(WrongUnitType));
    assert_eq!(sim.order(seeker, Order::Halt), Err(WrongUnitType));
    let retarget = |target| Order::Retarget { target };
    assert_eq!(sim.order(mover, retarget(a)), Err(WrongUnitType));
    assert_eq!(sim.order(seeker, retarget(seeker)), Err(InvalidOrder));
    assert_eq!(sim.order(seeker, retarget(StableId(99))), Err(InvalidOrder));
    for bad in [
        mv(f64::NAN, 0.0, 1.0),
        mv(1.0, 0.0, 0.0),
        mv(1.0, 0.0, -1.0),
    ] {
        assert_eq!(sim.order(mover, bad), Err(InvalidOrder), "{bad:?}");
    }
    // Commands to them are still refused.
    assert_eq!(sim.command(mover, Order::Halt), Err(NotFriendly));

    assert_eq!(sim.order(mover, mv(100.0, 50.0, 15.0)), Ok(()));
    assert_eq!(sim.order(drifter, Order::Halt), Ok(()));
    assert_eq!(
        sim.order(radar, Order::Emissions { radiate: false }),
        Ok(())
    );
    assert_eq!(sim.order(neutral, mv(900.0, 30.0, 15.0)), Ok(()));
    assert_eq!(sim.order(seeker, retarget(b)), Ok(()));
    assert_eq!(sim.order(a, Order::Emissions { radiate: false }), Ok(()));
    let s = sim.status();
    assert_eq!(
        (s.pending_commands, s.dropped_commands),
        (0, 0),
        "not queued"
    );

    // Halt stops the drifter at once; the move starts on the next tick.
    assert_eq!(
        sim.world()
            .get::<Velocity>(sim.entity(drifter).unwrap())
            .copied(),
        Some(Velocity::default())
    );
    // Tick 0 is "on" for the radar's cycle, but it is silent from the next tick.
    assert!(emitting(&mut sim, radar));
    sim.step(1);
    assert!(!emitting(&mut sim, radar));
    let mut engaged = None;
    for _ in 0..99 {
        sim.step(1);
        assert!(!emitting(&mut sim, radar), "silent through its cycle");
        engaged = engaged.or(sim.engagement_events().first().copied());
    }
    // 100 m at 15 m/s: 100 ticks, the last one partial.
    assert!((sim.position(mover).unwrap().x - 100.0).abs() < 1e-9);
    assert_eq!(sim.position(drifter).unwrap(), p(0.0, -50.0, 0.0));
    assert!((sim.position(neutral).unwrap().y - 30.0).abs() < 1e-9);
    let e = engaged.expect("the retargeted pursuer engaged");
    assert_eq!((e.pursuer, e.target), (seeker, b));
    assert!(sim.contains(a) && !sim.contains(b));

    // Back on the duty cycle; a removed unit takes no orders.
    assert_eq!(sim.order(radar, Order::Emissions { radiate: true }), Ok(()));
    assert_eq!(sim.order(b, Order::Halt), Err(UnknownUnit));
    let t = sim.tick();
    sim.step(1);
    assert_eq!(emitting(&mut sim, radar), t % 20 < 10);
    // Commands carry on numbering from 1: orders took no command ids.
    assert_eq!(sim.command(a, Order::Halt), Ok(1));
}
