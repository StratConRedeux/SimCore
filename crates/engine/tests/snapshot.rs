//! Save and restore: a restored world continues bit for bit like the original, and damaged or
//! foreign bytes are rejected without panicking. All numbers are test fixtures.

mod common;

use common::{Exercised, rich_world, script};
use simcore_abi::ScUnitView;
use simcore_abi::affiliation::{FRIENDLY, HOSTILE};
use simcore_engine::{
    EmitterCycle, EmsProfile, LinkRules, Order, OutcomeTable, Pcg32, Position, Pursuer, Rules,
    SNAPSHOT_FORMAT_VERSION, Sensor, SensorParams, Sim, SnapshotError, StableId, StandingOrder,
    Terrain, UnitSpec, Velocity, kind,
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
    // Standing orders over the dummy area, one with an emitting, duty-cycled launch unit.
    for (i, (x, y)) in [(-20_000.0, 0.0), (15_000.0, 10_000.0), (0.0, -25_000.0)]
        .into_iter()
        .enumerate()
    {
        let mut launch = unit(FRIENDLY, kind::QUADCOPTER, p(0.0, 0.0, 30.0), 2.0);
        if i == 1 {
            launch.cycle = Some(EmitterCycle {
                on_ticks: 4,
                off_ticks: 9,
                phase: 2,
            });
        }
        sim.add_standing_order(StandingOrder {
            center_x: x,
            center_y: y,
            radius_m: 12_000.0,
            kinds: if i == 2 { 1 << kind::GROUND_RADAR } else { 0 },
            launch,
            pursuer: Pursuer {
                target: StableId(0),
                speed: 140.0,
                max_turn_rate: 1.2,
                lead: 0.4,
                engagement_radius_m: 15.0,
                outcome_class: 1,
            },
            stock: 40,
            cooldown_ticks: 11 + 5 * i as u32,
        })
        .unwrap();
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
    let (mut engagements, mut launched) = (0, 0);
    for _ in 0..8 {
        orders(&mut a, &mut rng);
        a.step(25);
        engagements += a.engagement_events().len();
        launched += a.standing_order_launches().len();
    }
    orders(&mut a, &mut rng);
    a.step(1);
    let before = a.status();
    assert!(
        engagements > 0,
        "the scenario removes units before the save"
    );
    assert!(launched > 0, "standing orders launch before the save");
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
    // The listing a host rediscovers orders from is the same, order by order.
    assert!(
        a.standing_orders_with_status()
            .eq(b.standing_orders_with_status())
    );
    assert!(a.standing_orders().count() > 0);
    assert!(bits(a.frame()) == bits(b.frame()));

    // Both continue under the same new commands, tick by tick.
    let (mut ra, mut rb) = (Pcg32::new(5, 5), Pcg32::new(5, 5));
    let (mut events, mut launches) = (0, 0);
    for chunk in 0..30 {
        if chunk % 3 == 0 {
            orders(&mut a, &mut ra);
            orders(&mut b, &mut rb);
        }
        for _ in 0..10 {
            a.step(1);
            b.step(1);
            assert_eq!(a.engagement_events(), b.engagement_events());
            assert_eq!(a.standing_order_launches(), b.standing_order_launches());
            events += a.engagement_events().len();
            launches += a.standing_order_launches().len();
        }
        assert_eq!(a.status(), b.status(), "chunk {chunk}");
        assert!(bits(a.frame()) == bits(b.frame()), "chunk {chunk}");
        // The hidden state too (exposure, moves, RNG, cooldowns), not only what is shown.
        assert!(a.save() == b.save(), "chunk {chunk}");
    }
    println!(
        "after restore: {events} engagements, {launches} launches, {} units, {:?}",
        a.unit_count(),
        a.status()
    );
    assert!(events > 0);
    assert!(launches > 0);
}

/// A rich world run three ways under the same script: never saved, saved every chunk, and
/// saved and restored every few ticks at varying phases (mid-chunk, with commands in flight,
/// pursuers closing, exposures building and cooldowns running). All three stay byte-identical.
#[test]
fn save_restore_mid_run_is_invisible() {
    let (mut plain, mut saved, mut restored) = (rich_world(21), rich_world(21), rich_world(21));
    let mut rngs = [Pcg32::new(21, 1), Pcg32::new(21, 1), Pcg32::new(21, 1)];
    let mut exercised = Exercised::default();
    let (mut restores, mut in_flight) = (0, 0);
    for chunk in 0..40u32 {
        script(&mut plain, &mut rngs[0]);
        script(&mut saved, &mut rngs[1]);
        script(&mut restored, &mut rngs[2]);
        for t in 0..10u32 {
            plain.step(1);
            saved.step(1);
            restored.step(1);
            assert_eq!(plain.engagement_events(), restored.engagement_events());
            assert_eq!(
                plain.standing_order_launches(),
                restored.standing_order_launches()
            );
            exercised.record(&plain);
            if (chunk * 10 + t) % 7 == 3 {
                in_flight += usize::from(restored.pending_commands() > 0);
                restored = Sim::restore(restored.save()).expect("restores");
                restores += 1;
            }
        }
        assert!(saved.save() == restored.save(), "chunk {chunk}");
    }
    assert!(
        plain.save() == saved.save(),
        "saving does not change the run"
    );
    println!("{restores} restores ({in_flight} with commands in flight), {exercised:?}");
    exercised.assert_rich();
    assert!(in_flight > 0);
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
    sim.add_standing_order(small_order()).unwrap();
    let mut o = small_order();
    o.launch.cycle = Some(EmitterCycle {
        on_ticks: 2,
        off_ticks: 3,
        phase: 1,
    });
    let cancelled = sim.add_standing_order(o).unwrap();
    sim.add_standing_order(o).unwrap();
    sim.cancel_standing_order(cancelled);
    sim.save().to_vec()
}

fn small_order() -> StandingOrder {
    StandingOrder {
        center_x: 5.0,
        center_y: 5.0,
        radius_m: 50.0,
        kinds: 1 << kind::GROUND_RADAR,
        launch: unit(FRIENDLY, kind::QUADCOPTER, p(0.0, 0.0, 5.0), 1.0),
        pursuer: Pursuer {
            target: StableId(0),
            speed: 3.0,
            max_turn_rate: 1.0,
            lead: 0.0,
            engagement_radius_m: 1.0,
            outcome_class: 0,
        },
        stock: 4,
        cooldown_ticks: 9,
    }
}

#[test]
fn format_1_snapshots_restore_with_no_standing_orders() {
    // Format 1 is format 2 without the trailing standing-order section: an empty one is the
    // last id (8 bytes) and a count (4 bytes).
    let mut sim = Sim::new_dummy(4, 50);
    sim.step(20);
    let v2 = sim.save().to_vec();
    let mut v1 = v2[..v2.len() - 12].to_vec();
    v1[4..8].copy_from_slice(&1u32.to_le_bytes());
    let payload_len = (v1.len() - 24) as u64;
    v1[8..16].copy_from_slice(&payload_len.to_le_bytes());
    reseal(&mut v1);
    let mut restored = Sim::restore(&v1).expect("format 1 restores");
    assert_eq!(restored.save(), &v2[..], "and saves as format 2");
    assert_eq!(restored.standing_orders().count(), 0);

    // A format-1 header on format-2 bytes leaves the section over.
    let mut v = v2.clone();
    v[4..8].copy_from_slice(&1u32.to_le_bytes());
    reseal(&mut v);
    assert_eq!(
        Sim::restore(&v).err(),
        Some(SnapshotError::Invalid("trailing bytes"))
    );
}

#[test]
fn impossible_standing_orders_are_rejected() {
    // The standing-order section is last; the last order's fields end the payload:
    // ..., speed, turn, lead, radius (f64 each), outcome class (u16).
    let good = small_world();
    let n = good.len();
    let restored = Sim::restore(&good).unwrap();
    assert_eq!(
        restored
            .standing_orders()
            .map(|(id, _)| id)
            .collect::<Vec<_>>(),
        [1, 3]
    );
    let speed_at = n - 2 - 4 * 8;
    for (at, bytes, what) in [
        (speed_at, 0f64.to_le_bytes().to_vec(), "launch speed"),
        (speed_at + 16, 2f64.to_le_bytes().to_vec(), "launch lead"),
    ] {
        let mut v = good.clone();
        v[at..at + bytes.len()].copy_from_slice(&bytes);
        reseal(&mut v);
        assert!(
            matches!(Sim::restore(&v), Err(SnapshotError::Invalid(_))),
            "{what}"
        );
    }
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

/// A snapshot of the rich world mid-run: units of every kind with cycles, sensors and
/// exposure, pursuers, moves and commands in flight, standing orders part-way through stock.
fn rich_snapshot() -> Vec<u8> {
    let mut sim = rich_world(8);
    let mut rng = Pcg32::new(8, 8);
    for _ in 0..6 {
        script(&mut sim, &mut rng);
        sim.step(9);
    }
    script(&mut sim, &mut rng);
    assert!(sim.pending_commands() > 0);
    sim.save().to_vec()
}

/// What an accepted tampered snapshot must satisfy. It is canonical: saving the restored world
/// gives back exactly the input, so no field was read leniently or dropped (with one known
/// exception: `Pcg32::from_state` forces an even RNG increment odd instead of rejecting it).
/// And it runs, and (with `round_trip`) what it saves after running restores to the same bytes.
/// A restore costs far more than a step or a save, so the exhaustive test leaves that out.
fn check_accepted(mut sim: Sim, input: &[u8], round_trip: bool) {
    // Payload: tick, next id, last command id (u64 each), two u32 counters, RNG state, then
    // the RNG increment, whose low byte is here.
    const RNG_INCREMENT: usize = 24 + 8 * 3 + 4 * 2 + 8;
    let mut expected = input.to_vec();
    expected[RNG_INCREMENT] |= 1;
    reseal(&mut expected);
    assert!(
        sim.save() == &expected[..],
        "an accepted snapshot is canonical"
    );
    sim.step(3);
    let _ = sim.frame();
    let _ = sim.status();
    let resaved = sim.save().to_vec();
    if round_trip {
        let mut again = Sim::restore(&resaved).expect("a resaved world restores");
        assert!(again.save() == &resaved[..], "and saves to the same bytes");
    }
}

#[test]
fn tampered_payloads_never_panic() {
    // With the checksum recomputed, damage reaches the parser: it must reject or accept, never
    // panic, and anything accepted must run.
    for (name, good, rounds) in [
        ("small", small_world(), 20_000),
        ("rich", rich_snapshot(), 4_000),
    ] {
        tamper_randomly(name, &good, rounds);
    }
}

fn tamper_randomly(name: &str, good: &[u8], rounds: u32) {
    let mut rng = Pcg32::new(1, 1);
    let (mut accepted, mut rejected) = (0, 0);
    for round in 0..rounds {
        let mut v = good.to_vec();
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
            Ok(sim) => {
                accepted += 1;
                check_accepted(sim, &v, true);
            }
            Err(e) => {
                rejected += 1;
                assert!(matches!(e, SnapshotError::Invalid(_)), "{e:?}");
            }
        }
    }
    println!("{name}, tampered: {accepted} accepted, {rejected} rejected");
    assert!(rejected > 0);
}

#[test]
fn every_payload_byte_tampered() {
    // Each payload byte of a rich snapshot, in turn, set to 0, to 0xff or with its top bit
    // flipped (cycling, so every field sees each), the checksum recomputed: every result is
    // rejected as invalid or is a sound world.
    let good = rich_snapshot();
    let (mut accepted, mut rejected) = (0, 0);
    for i in 24..good.len() {
        let mut v = good.clone();
        v[i] = match i % 3 {
            0 => 0,
            1 => 0xff,
            _ => v[i] ^ 0x80,
        };
        if v[i] == good[i] {
            v[i] ^= 0x80;
        }
        reseal(&mut v);
        match Sim::restore(&v) {
            Ok(sim) => {
                accepted += 1;
                check_accepted(sim, &v, false);
            }
            Err(e) => {
                rejected += 1;
                assert!(matches!(e, SnapshotError::Invalid(_)), "byte {i}: {e:?}");
            }
        }
    }
    println!(
        "{} bytes: {accepted} accepted, {rejected} rejected",
        good.len()
    );
    assert!(rejected > 0 && accepted > 0);
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

/// A world of `n` friendly units, all moving, saved mid-move.
fn moving_world(n: u32) -> Sim {
    let mut sim = Sim::new(1);
    for i in 0..n {
        let id = sim.spawn_unit(unit(
            FRIENDLY,
            kind::GROUND_VEHICLE,
            p(f64::from(i), 0.0, 0.0),
            0.0,
        ));
        let to = Order::Move {
            destination: p(0.0, 1e4, 0.0),
            speed_mps: 5.0,
        };
        sim.command(id, to).unwrap();
    }
    sim.step(1);
    sim
}

/// Fastest of three restores, in seconds.
fn restore_secs(bytes: &[u8]) -> f64 {
    (0..3)
        .map(|_| {
            let t = std::time::Instant::now();
            std::hint::black_box(Sim::restore(bytes).unwrap());
            t.elapsed().as_secs_f64()
        })
        .fold(f64::MAX, f64::min)
}

#[test]
fn many_moves_restore_in_linear_time() {
    // Scaling, not wall-clock time, so it holds on slow CI runners and in debug builds:
    // four times the moves must cost well under the sixteen times a quadratic check would.
    let small = moving_world(10_000).save().to_vec();
    let mut big_sim = moving_world(40_000);
    let big = big_sim.save().to_vec();
    let (ts, tb) = (restore_secs(&small), restore_secs(&big));
    println!(
        "restore: 10,000 moves {:.1} ms, 40,000 moves {:.1} ms ({:.1}x)",
        ts * 1e3,
        tb * 1e3,
        tb / ts
    );
    assert!(tb / ts < 8.0, "{:.1}x for 4x the moves", tb / ts);

    let mut b = Sim::restore(&big).unwrap();
    big_sim.step(5);
    b.step(5);
    assert!(bits(big_sim.frame()) == bits(b.frame()));
}
