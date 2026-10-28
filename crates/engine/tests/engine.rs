use simcore_abi::{ScUnitView, flags};
use simcore_engine::{EmsProfile, Position, Sim, TICKS_PER_SECOND, UnitSpec, Velocity, kind};

/// Every field of a view as raw bits, so float comparisons are bitwise.
fn bits(frame: &[ScUnitView]) -> Vec<[u64; 8]> {
    frame
        .iter()
        .map(|u| {
            [
                u.id,
                u64::from(u.x.to_bits()),
                u64::from(u.y.to_bits()),
                u64::from(u.z.to_bits()),
                (u64::from(u.vx.to_bits()) << 32) | u64::from(u.vy.to_bits()),
                u64::from(u.vz.to_bits()),
                (u64::from(u.kind) << 16) | (u64::from(u.affiliation) << 8) | u64::from(u.flags),
                u64::from(u.ems_power_w.to_bits()),
            ]
        })
        .collect()
}

fn unit(position: Position, velocity: Velocity) -> UnitSpec {
    UnitSpec {
        kind: kind::QUADCOPTER,
        affiliation: simcore_abi::affiliation::HOSTILE,
        position,
        velocity,
        ems: EmsProfile::default(),
        cycle: None,
    }
}

#[test]
fn determinism() {
    let mut a = Sim::new_dummy(42, 5000);
    let mut b = Sim::new_dummy(42, 5000);
    a.step(300);
    b.step(300);
    assert_eq!(a.tick(), 300);
    let fa = bits(a.frame());
    let fb = bits(b.frame());
    assert_eq!(fa.len(), 5001);
    assert!(fa == fb, "same seed must give bitwise-identical frames");

    // Stepping in different chunk sizes is the same simulation.
    let mut c = Sim::new_dummy(42, 5000);
    for _ in 0..100 {
        c.step(3);
    }
    assert!(bits(c.frame()) == fa);

    let mut d = Sim::new_dummy(43, 5000);
    d.step(300);
    assert!(
        bits(d.frame()) != fa,
        "different seed must give a different frame"
    );

    // The duty cycle actually toggles emitters over 300 ticks.
    let emitting_now = a
        .frame()
        .iter()
        .filter(|u| u.flags & flags::EMITTING != 0)
        .count();
    let mut e = Sim::new_dummy(42, 5000);
    let emitting_at_0 = e
        .frame()
        .iter()
        .filter(|u| u.flags & flags::EMITTING != 0)
        .count();
    assert!(emitting_now > 0 && emitting_at_0 > 0);
    assert!(bits(e.frame()) != fa);
}

#[test]
fn floating_origin_precision() {
    let origin = Position {
        x: 1_000_000.0,
        y: 2_000_000.0,
        z: 0.0,
    };
    // About 30 km away, with a fractional part f32 cannot hold at 10^6 magnitudes.
    let offset = (21_213.203_456_2_f64, 21_213.203_456_2_f64, 0.0);
    let mut sim = Sim::new(7);
    sim.set_command_node_position(origin);
    let id = sim.spawn_unit(unit(
        Position {
            x: origin.x + offset.0,
            y: origin.y + offset.1,
            z: origin.z + offset.2,
        },
        Velocity::default(),
    ));
    let world = Position {
        x: origin.x + offset.0,
        y: origin.y + offset.1,
        z: 0.0,
    };
    let exact = (world.x - origin.x, world.y - origin.y);

    let u = *sim.frame().iter().find(|u| u.id == id.0).unwrap();
    let err = (f64::from(u.x) - exact.0)
        .abs()
        .max((f64::from(u.y) - exact.1).abs());
    assert!(err < 0.01, "floating-origin error {err} m exceeds 1 cm");

    let naive = (
        f64::from(world.x as f32 - origin.x as f32),
        f64::from(world.y as f32 - origin.y as f32),
    );
    let naive_err = (naive.0 - exact.0).abs().max((naive.1 - exact.1).abs());
    println!("floating-origin error: {err:.6} m; naive f32 absolute cast error: {naive_err:.6} m");
    assert!(naive_err > 0.01 && naive_err > 10.0 * err);
}

#[test]
fn movement() {
    let v = Velocity {
        x: 10.0,
        y: -4.0,
        z: 2.5,
    };
    let mut sim = Sim::new(1);
    let start = Position {
        x: 100.0,
        y: 200.0,
        z: 50.0,
    };
    let id = sim.spawn_unit(unit(start, v));
    let ticks = 45;
    sim.step(ticks);
    let secs = f64::from(ticks) / f64::from(TICKS_PER_SECOND);
    let u = *sim.frame().iter().find(|u| u.id == id.0).unwrap();
    for (got, want) in [
        (u.x, start.x + v.x * secs),
        (u.y, start.y + v.y * secs),
        (u.z, start.z + v.z * secs),
    ] {
        assert!(
            (f64::from(got) - want).abs() < 1e-4,
            "got {got}, want {want}"
        );
    }
    assert_eq!((u.vx, u.vy, u.vz), (10.0, -4.0, 2.5));
}

#[test]
fn frame_buffer_is_reused() {
    let mut sim = Sim::new_dummy(9, 2000);
    let first = sim.frame();
    let (ptr, len) = (first.as_ptr(), first.len());
    for _ in 0..20 {
        sim.step(1);
        let f = sim.frame();
        assert_eq!(f.as_ptr(), ptr);
        assert_eq!(f.len(), len);
    }
    let cap = sim.frame_capacity();
    for _ in 0..20 {
        sim.frame();
        assert_eq!(sim.frame_capacity(), cap);
    }
}

#[test]
fn exactly_one_command_node_at_origin() {
    for mut sim in [Sim::new(3), Sim::new_dummy(3, 1000)] {
        sim.set_command_node_position(Position {
            x: 5_000.0,
            y: -3_000.0,
            z: 10.0,
        });
        sim.step(30);
        let f = sim.frame();
        let cmd: Vec<_> = f
            .iter()
            .filter(|u| u.flags & flags::COMMAND_NODE != 0)
            .collect();
        assert_eq!(cmd.len(), 1);
        assert_eq!((cmd[0].x, cmd[0].y, cmd[0].z), (0.0, 0.0, 0.0));
        assert_eq!(cmd[0].kind, kind::COMMAND_NODE);
    }
}
