//! Save and restore: a restored world continues bit for bit like the original, and damaged or
//! foreign bytes are rejected without panicking. All numbers are test fixtures.

use simcore_abi::ScUnitView;
use simcore_abi::affiliation::{FRIENDLY, HOSTILE};
use simcore_engine::{
    EmsProfile, LinkRules, Order, OutcomeTable, Pcg32, Position, Pursuer, Rules,
    SNAPSHOT_FORMAT_VERSION, Sensor, SensorParams, Sim, SnapshotError, StableId, Terrain, UnitSpec,
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

/// Every field of every unit as raw bits, in id order. Frame order is unspecified, and a
/// restored world iterates its units in a different order.
fn bits(frame: &[ScUnitView]) -> Vec<[u64; 5]> {
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

/// A busy world: dummy units, terrain, sensors (some with their own parameters), pursuers on
/// both sides, link delay with jamming, and a history of moves, retargets and silences.
fn scenario(seed: u64) -> Sim {
    let mut sim = Sim::new_dummy(seed, 2000);
    let mut rng = Pcg32::new(seed, 7);
    let heights = (0..24 * 24)
        .map(|_| rng.range_f64(0.0, 150.0) as f32)
        .collect();
    sim.set_terrain(
        Terrain::heightfield(-40_000.0, -40_000.0, 3500.0, 24, 24, heights, 6).unwrap(),
    );
    sim.set_rules(Rules {
        detection_max_range_m: 9000.0,
        sensor: SensorParams {
            base_range_m: 3000.0,
            reference_power_w: 100.0,
        },
        exposure_threshold_ticks: 10,
        exposure_cap_ticks: 40,
        exposure_decay_per_tick: 2,
        outcomes: OutcomeTable::new(0.5).with(1, kind::GROUND_RADAR, 0.9),
    });
    sim.set_link_rules(LinkRules {
        base_delay_ticks: 3,
        delay_ticks_per_km: 0.5,
        jam_radius_m: 4000.0,
        jam_delay_ticks: 20,
        max_delay_ticks: 120,
    });
    for id in (2..2002).step_by(7) {
        let own = (id % 3 == 0).then_some(SensorParams {
            base_range_m: 5000.0,
            reference_power_w: 50.0,
        });
        sim.add_sensor(StableId(id), Sensor { params: own });
    }
    sim.add_sensor(StableId(1), Sensor::default());
    for i in 0..80 {
        let target = StableId(2 + u64::from(rng.below(2000)));
        let t = sim.position(target).unwrap();
        let a = rng.range_f64(0.0, core::f64::consts::TAU);
        let side = if i % 2 == 0 { FRIENDLY } else { HOSTILE };
        sim.spawn_pursuer(
            unit(
                side,
                kind::FIXED_WING_DRONE,
                p(t.x + 1200.0 * a.cos(), t.y + 1200.0 * a.sin(), t.z + 150.0),
                if i % 3 == 0 { 5.0 } else { 0.0 },
            ),
            Pursuer {
                target,
                speed: 110.0,
                max_turn_rate: 0.9,
                lead: 0.3,
                engagement_radius_m: 12.0,
                outcome_class: 1,
            },
        );
    }
    sim
}

/// Issues a deterministic batch of commands to friendly units (rejections are fine).
fn orders(sim: &mut Sim, rng: &mut Pcg32) {
    let mut ids: Vec<u64> = sim.frame().iter().map(|u| u.id).collect();
    ids.sort_unstable();
    for _ in 0..40 {
        let id = StableId(ids[rng.below(ids.len() as u32) as usize]);
        let order = match rng.below(4) {
            0 => Order::Move {
                destination: p(
                    rng.range_f64(-30_000.0, 30_000.0),
                    rng.range_f64(-30_000.0, 30_000.0),
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
        let _ = sim.command(id, order);
    }
}

#[test]
fn restored_world_continues_identically() {
    let mut a = scenario(11);
    let mut rng = Pcg32::new(11, 3);
    let mut engagements = 0;
    for _ in 0..8 {
        orders(&mut a, &mut rng);
        a.step(25);
        engagements += a.engagement_events().len();
    }
    orders(&mut a, &mut rng);
    a.step(1);
    let before = a.status();
    assert!(
        engagements > 0,
        "the scenario removes units before the save"
    );
    assert!(
        before.pending_commands > 0,
        "commands are in flight at the save"
    );

    let saved = a.save().to_vec();
    assert_eq!(
        a.status().awaiting_cleanup,
        0,
        "save despawns removed units"
    );
    let mut b = Sim::restore(&saved).expect("restores");
    assert_eq!(
        b.save(),
        &saved[..],
        "a restored world saves to the same bytes"
    );
    assert_eq!(a.save(), &saved[..], "saving twice gives the same bytes");

    let (s_a, s_b) = (a.status(), b.status());
    assert_eq!(s_a, s_b);
    assert!(bits(a.frame()) == bits(b.frame()));

    // Both continue under the same new commands, tick by tick.
    let (mut ra, mut rb) = (Pcg32::new(5, 5), Pcg32::new(5, 5));
    let mut events = 0;
    for chunk in 0..30 {
        if chunk % 3 == 0 {
            orders(&mut a, &mut ra);
            orders(&mut b, &mut rb);
        }
        for _ in 0..10 {
            a.step(1);
            b.step(1);
            assert_eq!(a.engagement_events(), b.engagement_events());
            events += a.engagement_events().len();
        }
        assert_eq!(a.status(), b.status(), "chunk {chunk}");
        assert!(bits(a.frame()) == bits(b.frame()), "chunk {chunk}");
    }
    println!(
        "after restore: {events} engagements, {} units, {:?}",
        a.unit_count(),
        a.status()
    );
    assert!(events > 0);
}

#[test]
fn restore_of_a_fresh_world() {
    let mut a = Sim::new(3);
    let saved = a.save().to_vec();
    let mut b = Sim::restore(&saved).unwrap();
    assert_eq!(b.unit_count(), 1);
    a.step(10);
    b.step(10);
    assert!(bits(a.frame()) == bits(b.frame()));
}

fn reseal(bytes: &mut [u8]) {
    // Recompute the FNV-1a checksum over the payload, so tampering reaches the parser.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in &bytes[24..] {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    bytes[16..24].copy_from_slice(&h.to_le_bytes());
}

fn small_world() -> Vec<u8> {
    let mut sim = Sim::new_dummy(9, 30);
    sim.set_link_rules(LinkRules {
        base_delay_ticks: 40,
        ..LinkRules::default()
    });
    sim.add_sensor(StableId(4), Sensor::default());
    sim.set_terrain(Terrain::heightfield(0.0, 0.0, 100.0, 3, 2, vec![1.0; 6], 2).unwrap());
    let t = sim.spawn_unit(unit(HOSTILE, kind::GROUND_RADAR, p(9.0, 9.0, 0.0), 0.0));
    let m = sim.spawn_pursuer(
        unit(FRIENDLY, kind::FIXED_WING_DRONE, p(0.0, 0.0, 0.0), 0.0),
        Pursuer {
            target: t,
            speed: 1.0,
            max_turn_rate: 1.0,
            lead: 0.0,
            engagement_radius_m: 0.0,
            outcome_class: 0,
        },
    );
    sim.command(m, Order::Emissions { radiate: false }).unwrap();
    sim.command(StableId(1), Order::Halt).unwrap();
    sim.save().to_vec()
}

#[test]
fn rejects_foreign_and_damaged_bytes() {
    let good = small_world();
    assert!(Sim::restore(&good).is_ok());
    assert_eq!(Sim::restore(&[]).err(), Some(SnapshotError::NotASnapshot));
    assert_eq!(
        Sim::restore(b"not a snapshot at all, no").err(),
        Some(SnapshotError::NotASnapshot)
    );

    let mut v = good.clone();
    v[4..8].copy_from_slice(&(SNAPSHOT_FORMAT_VERSION + 1).to_le_bytes());
    assert_eq!(
        Sim::restore(&v).err(),
        Some(SnapshotError::UnsupportedVersion(
            SNAPSHOT_FORMAT_VERSION + 1
        ))
    );

    // Every truncation is rejected.
    for n in 0..good.len() {
        assert!(Sim::restore(&good[..n]).is_err(), "truncated to {n}");
    }
    // Every single-bit flip in the payload is caught by the checksum.
    for i in 24..good.len() {
        for bit in 0..8 {
            let mut v = good.clone();
            v[i] ^= 1 << bit;
            assert_eq!(
                Sim::restore(&v).err(),
                Some(SnapshotError::Corrupt),
                "byte {i}"
            );
        }
    }
}

#[test]
fn tampered_payloads_never_panic() {
    // With the checksum recomputed, damage reaches the parser: it must reject or accept, never
    // panic, and anything accepted must run.
    let good = small_world();
    let mut rng = Pcg32::new(1, 1);
    let (mut accepted, mut rejected) = (0, 0);
    for round in 0..20_000 {
        let mut v = good.clone();
        let flips = 1 + rng.below(4);
        for _ in 0..flips {
            let i = 24 + rng.below((v.len() - 24) as u32) as usize;
            v[i] = if round % 2 == 0 {
                rng.next_u32() as u8
            } else {
                v[i] ^ (1 << rng.below(8))
            };
        }
        reseal(&mut v);
        match Sim::restore(&v) {
            Ok(mut sim) => {
                accepted += 1;
                sim.step(3);
                let _ = sim.frame();
                let _ = sim.status();
                let _ = sim.save();
            }
            Err(e) => {
                rejected += 1;
                assert!(matches!(e, SnapshotError::Invalid(_)), "{e:?}");
            }
        }
    }
    println!("tampered: {accepted} accepted, {rejected} rejected");
    assert!(rejected > 0);
}

#[test]
fn out_of_range_counters_are_rejected() {
    // Payload offsets: tick at 0, next id at 8, last command id at 16.
    let good = small_world();
    for (offset, value, what) in [
        (0, u64::MAX, "tick"),
        (0, (1u64 << 62) + 1, "tick"),
        (8, 1u64 << 27, "next id"),
        (8, 0, "next id"),
        (16, u64::MAX, "last command id"),
    ] {
        let mut v = good.clone();
        v[24 + offset..32 + offset].copy_from_slice(&value.to_le_bytes());
        reseal(&mut v);
        assert!(
            matches!(Sim::restore(&v), Err(SnapshotError::Invalid(_))),
            "{what} = {value}"
        );
    }
}

#[test]
fn many_moves_restore_in_linear_time() {
    let mut sim = Sim::new(1);
    let ids: Vec<StableId> = (0..40_000)
        .map(|i| {
            sim.spawn_unit(unit(
                FRIENDLY,
                kind::GROUND_VEHICLE,
                p(f64::from(i), 0.0, 0.0),
                0.0,
            ))
        })
        .collect();
    for &id in &ids {
        sim.command(
            id,
            Order::Move {
                destination: p(0.0, 1e4, 0.0),
                speed_mps: 5.0,
            },
        )
        .unwrap();
    }
    sim.step(1);
    let bytes = sim.save().to_vec();
    let t = std::time::Instant::now();
    let mut b = Sim::restore(&bytes).unwrap();
    let ms = t.elapsed().as_secs_f64() * 1e3;
    println!("restore with 40,000 moves: {ms:.1} ms");
    // Quadratic duplicate checks took about 250 ms in release here; linear is a few ms.
    assert!(ms < 100.0, "{ms} ms");
    sim.step(5);
    b.step(5);
    assert!(bits(sim.frame()) == bits(b.frame()));
}
