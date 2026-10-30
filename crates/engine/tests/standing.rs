//! Standing orders: zone engagement from limited stock, carried out by the engine every tick.
//! All numbers are test fixtures.

use simcore_abi::affiliation::{FRIENDLY, HOSTILE, NEUTRAL};
use simcore_engine::{
    EmsProfile, LaunchEvent, MAX_STANDING_ORDERS, OutcomeTable, Position, Pursuer, Rules, Sensor,
    SensorParams, Sim, StableId, StandingOrder, StandingOrderError, StandingOrderStatus, UnitSpec,
    Velocity, kind,
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

/// The command node hears every emitter within 50 km at once; engagements always succeed.
fn world(chance: f64) -> Sim {
    let mut sim = Sim::new(1);
    sim.set_rules(Rules {
        detection_max_range_m: 50_000.0,
        sensor: SensorParams {
            base_range_m: 50_000.0,
            reference_power_w: 1.0,
        },
        exposure_threshold_ticks: 1,
        exposure_cap_ticks: 1,
        exposure_decay_per_tick: 1,
        outcomes: OutcomeTable::new(chance),
    });
    sim.add_sensor(StableId(1), Sensor::default());
    sim
}

fn hostile(sim: &mut Sim, k: u16, x: f64, y: f64, power_w: f32) -> StableId {
    sim.spawn(unit(HOSTILE, k, p(x, y, 0.0), power_w), None)
        .unwrap()
}

/// Engages anything within 5 km of (10 km, 0), launching from 10 m above the node.
fn order(stock: u32, cooldown_ticks: u32) -> StandingOrder {
    StandingOrder {
        center_x: 10_000.0,
        center_y: 0.0,
        radius_m: 5_000.0,
        kinds: 0,
        launch: unit(FRIENDLY, kind::FIXED_WING_DRONE, p(0.0, 0.0, 10.0), 0.0),
        pursuer: Pursuer {
            target: StableId(0),
            speed: 200.0,
            max_turn_rate: 2.0,
            lead: 0.5,
            engagement_radius_m: 20.0,
            outcome_class: 1,
        },
        stock,
        cooldown_ticks,
    }
}

/// Steps one tick at a time, collecting the launches.
fn launches(sim: &mut Sim, ticks: u32) -> Vec<LaunchEvent> {
    let mut all = Vec::new();
    for _ in 0..ticks {
        sim.step(1);
        all.extend_from_slice(sim.standing_order_launches());
    }
    all
}

#[test]
fn launches_at_the_nearest_detected_hostile_in_the_zone() {
    let mut sim = world(0.0);
    let far = hostile(&mut sim, kind::GROUND_RADAR, 13_000.0, 0.0, 10.0);
    let near = hostile(&mut sim, kind::GROUND_RADAR, 9_000.0, 500.0, 10.0);
    // Undetected (silent), outside the zone, and not hostile: none of these is engaged.
    hostile(&mut sim, kind::GROUND_RADAR, 10_000.0, 0.0, 0.0);
    hostile(&mut sim, kind::GROUND_RADAR, 16_000.0, 0.0, 10.0);
    sim.spawn(
        unit(NEUTRAL, kind::GROUND_RADAR, p(10_000.0, 10.0, 0.0), 10.0),
        None,
    )
    .unwrap();
    sim.set_command_node_position(p(0.0, -100.0, 0.0));

    let id = sim.add_standing_order(order(5, 30)).unwrap();
    assert_eq!(id, 1);
    sim.step(1);
    let first = sim.standing_order_launches().to_vec();
    assert_eq!(first.len(), 1, "one launch per order per tick");
    let l = first[0];
    assert_eq!((l.tick, l.order, l.target), (1, id, near));
    // It leaves from the node (plus the offset), heading straight at the target.
    let at = sim.position(l.pursuer).unwrap();
    assert_eq!(at, p(0.0, -100.0, 10.0));
    let frame = sim.frame().to_vec();
    let u = frame.iter().find(|u| u.id == l.pursuer.0).unwrap();
    let (dx, dy, dz) = (9_000.0, 600.0, -10.0);
    let d = f64::sqrt(dx * dx + dy * dy + dz * dz);
    assert!((f64::from(u.vx) - 200.0 * dx / d).abs() < 1e-3);
    assert!((f64::from(u.vy) - 200.0 * dy / d).abs() < 1e-3);
    assert_eq!(
        sim.standing_order(id),
        Some(StandingOrderStatus {
            remaining: 4,
            ready_tick: 31
        })
    );

    // The near radar is taken, so after the cooldown the far one is next; then nothing is left.
    let rest = launches(&mut sim, 60);
    assert_eq!(rest.len(), 1);
    assert_eq!((rest[0].tick, rest[0].target), (31, far));
    assert_eq!(sim.standing_order(id).unwrap().remaining, 3);
}

#[test]
fn launched_pursuers_engage_and_report_as_the_pursuer() {
    let mut sim = world(1.0);
    let target = hostile(&mut sim, kind::JAMMER, 6_000.0, 0.0, 10.0);
    sim.add_standing_order(order(1, 0)).unwrap();
    sim.step(1);
    let l = sim.standing_order_launches()[0];
    let mut hit = None;
    for _ in 0..15 * 60 {
        sim.step(1);
        if let Some(e) = sim.engagement_events().first() {
            hit = Some(*e);
            break;
        }
    }
    let e = hit.expect("the launch reaches its target");
    assert_eq!(
        (e.pursuer, e.target, e.target_removed),
        (l.pursuer, target, true)
    );
    assert!(!sim.contains(target));
}

#[test]
fn one_pursuer_per_target_across_orders_and_strikes() {
    let mut sim = world(0.0);
    let struck = hostile(&mut sim, kind::GROUND_RADAR, 8_000.0, 0.0, 10.0);
    let open = hostile(&mut sim, kind::GROUND_RADAR, 12_000.0, 0.0, 10.0);
    // The player's own strike is already after the nearer radar.
    let mut strike = order(1, 0).pursuer;
    strike.target = struck;
    sim.spawn(order(1, 0).launch, Some(strike)).unwrap();

    let a = sim.add_standing_order(order(5, 0)).unwrap();
    let b = sim.add_standing_order(order(5, 0)).unwrap();
    sim.step(1);
    let got = sim.standing_order_launches().to_vec();
    // The first order takes the free radar; the second has nothing left to take.
    assert_eq!(got.len(), 1);
    assert_eq!((got[0].order, got[0].target), (a, open));
    assert_eq!(sim.standing_order(b).unwrap().remaining, 5);
    assert!(launches(&mut sim, 30).is_empty());
}

#[test]
fn the_kind_mask_picks_what_is_engaged() {
    let mut sim = world(0.0);
    hostile(&mut sim, kind::GROUND_VEHICLE, 9_500.0, 0.0, 10.0);
    let jammer = hostile(&mut sim, kind::JAMMER, 12_000.0, 0.0, 10.0);
    let mut o = order(5, 0);
    o.kinds = (1 << kind::JAMMER) | (1 << kind::GROUND_RADAR);
    sim.add_standing_order(o).unwrap();
    let got = launches(&mut sim, 5);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].target, jammer);
}

#[test]
fn stock_runs_out_and_cancelling_stops_launches_but_not_flights() {
    let mut sim = world(0.0);
    for i in 0..6 {
        hostile(
            &mut sim,
            kind::GROUND_RADAR,
            8_000.0 + 500.0 * f64::from(i),
            0.0,
            10.0,
        );
    }
    let id = sim.add_standing_order(order(3, 0)).unwrap();
    // Cooldown 0 still means one launch per tick.
    let got = launches(&mut sim, 10);
    assert_eq!(got.iter().map(|l| l.tick).collect::<Vec<_>>(), [1, 2, 3]);
    assert_eq!(sim.standing_order(id).unwrap().remaining, 0);
    assert!(
        sim.standing_orders().any(|(i, _)| i == id),
        "stays until cancelled"
    );

    let other = sim.add_standing_order(order(3, 0)).unwrap();
    sim.step(1);
    let flying = sim.standing_order_launches()[0].pursuer;
    assert!(sim.cancel_standing_order(other));
    assert!(!sim.cancel_standing_order(other));
    assert_eq!(sim.standing_order(other), None);
    assert!(launches(&mut sim, 10).is_empty());
    assert!(sim.contains(flying), "a launched unit carries on");
}

#[test]
fn many_ticks_at_once_launch_exactly_as_one_at_a_time() {
    let build = || {
        let mut sim = world(0.5);
        for i in 0..40 {
            let a = f64::from(i) * 0.7;
            hostile(
                &mut sim,
                kind::GROUND_RADAR,
                10_000.0 + 4_000.0 * a.cos(),
                4_000.0 * a.sin(),
                10.0,
            );
        }
        sim.add_standing_order(order(25, 7)).unwrap();
        let mut o = order(10, 3);
        o.center_x = 12_000.0;
        o.kinds = 1 << kind::GROUND_RADAR;
        sim.add_standing_order(o).unwrap();
        sim
    };
    let (mut a, mut b) = (build(), build());
    let one_by_one = launches(&mut a, 600);
    b.step(600);
    assert!(one_by_one.len() > 20);
    assert_eq!(b.standing_order_launches(), &one_by_one[..]);
    assert_eq!(a.status(), b.status());
}

#[test]
fn invalid_orders_are_rejected() {
    let mut sim = world(0.0);
    let bad = |f: fn(&mut StandingOrder)| {
        let mut o = order(1, 0);
        f(&mut o);
        o
    };
    for (o, e) in [
        (bad(|o| o.radius_m = 0.0), StandingOrderError::InvalidZone),
        (
            bad(|o| o.radius_m = f64::INFINITY),
            StandingOrderError::InvalidZone,
        ),
        (
            bad(|o| o.center_x = f64::NAN),
            StandingOrderError::InvalidZone,
        ),
        (
            bad(|o| o.launch.affiliation = HOSTILE),
            StandingOrderError::InvalidUnit,
        ),
        (
            bad(|o| o.launch.kind = kind::COMMAND_NODE),
            StandingOrderError::InvalidUnit,
        ),
        (
            bad(|o| o.launch.ems.erp_w = -1.0),
            StandingOrderError::InvalidUnit,
        ),
        (
            bad(|o| o.launch.position.z = f64::NAN),
            StandingOrderError::InvalidUnit,
        ),
        (
            bad(|o| o.pursuer.speed = 0.0),
            StandingOrderError::InvalidPursuer,
        ),
        (
            bad(|o| o.pursuer.lead = 2.0),
            StandingOrderError::InvalidPursuer,
        ),
        (
            bad(|o| o.stock = u32::MAX),
            StandingOrderError::InvalidStock,
        ),
    ] {
        assert_eq!(sim.add_standing_order(o), Err(e), "{o:?}");
    }
    // A launch velocity is ignored, so even a non-finite one is accepted.
    assert!(
        sim.add_standing_order(bad(|o| o.launch.velocity.x = f64::NAN))
            .is_ok()
    );
    for _ in 1..MAX_STANDING_ORDERS {
        sim.add_standing_order(order(1, 0)).unwrap();
    }
    assert_eq!(
        sim.add_standing_order(order(1, 0)),
        Err(StandingOrderError::TooMany)
    );
}

#[test]
fn an_order_without_stock_never_launches() {
    let mut sim = world(0.0);
    hostile(&mut sim, kind::GROUND_RADAR, 10_000.0, 0.0, 10.0);
    sim.add_standing_order(order(0, 0)).unwrap();
    assert!(launches(&mut sim, 10).is_empty());
}

#[test]
fn an_extreme_launch_speed_still_launches_and_leaves_others_their_targets() {
    // A huge speed over a tiny distance must not overflow the launch heading: the launch
    // happens (so it claims the target) and its velocity is finite.
    let mut sim = world(0.0);
    let t = hostile(&mut sim, kind::GROUND_RADAR, 10_000.0, 0.0, 10.0);
    let mut fast = order(1, 0);
    fast.pursuer.speed = 1e300;
    fast.launch.position = p(10_000.0, 1e-10, 0.0);
    sim.add_standing_order(fast).unwrap();
    sim.step(1);
    let l = sim.standing_order_launches().to_vec();
    assert_eq!(l.len(), 1);
    assert_eq!(l[0].target, t);
    // The engine's f64 velocity: the frame's f32 can't hold 1e300 at all.
    let e = sim.entity(l[0].pursuer).unwrap();
    let v = *sim.world().get::<Velocity>(e).unwrap();
    assert!(v.x.is_finite() && v.y.is_finite() && v.z.is_finite());
    assert!(v.y < 0.0, "pointing down y, at the target");
}

#[test]
fn the_listing_reports_each_order_with_its_progress() {
    let mut sim = world(0.0);
    hostile(&mut sim, kind::GROUND_RADAR, 10_000.0, 0.0, 10.0);
    let a = sim.add_standing_order(order(2, 30)).unwrap();
    let mut far = order(0, 5);
    far.center_y = 40_000.0;
    let b = sim.add_standing_order(far).unwrap();
    sim.step(1);
    let got: Vec<_> = sim
        .standing_orders_with_status()
        .map(|(id, o, s)| (id, o.center_y, s))
        .collect();
    let ready = StandingOrderStatus {
        remaining: 1,
        ready_tick: 31,
    };
    let empty = StandingOrderStatus {
        remaining: 0,
        ready_tick: 0,
    };
    assert_eq!(got, [(a, 0.0, ready), (b, 40_000.0, empty)]);
    assert_eq!(sim.standing_order(a), Some(ready));
    assert!(sim.cancel_standing_order(a));
    assert_eq!(sim.standing_orders_with_status().count(), 1);
}

#[test]
fn to_frame_is_relative_to_the_command_node_now() {
    let mut sim = world(0.0);
    let at = p(10_000.25, -3.0, 2.0);
    assert_eq!(sim.to_frame(at), [10_000.25, -3.0, 2.0]);
    sim.set_command_node_position(p(1_000_000.0, 1.0, 0.0));
    assert_eq!(sim.to_frame(at), [-989_999.75, -4.0, 2.0]);
}
