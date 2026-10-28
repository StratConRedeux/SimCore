//! Times `step(1)` and `frame()` and measures heap use with a counting allocator, first on the
//! plain dummy world, then on a Phase 2 rules scenario (sensors, emitters, pursuers, terrain)
//! with the signal heatmap.
//!
//! `cargo run --release -p simcore-engine --example bench -- [units]`

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::time::Instant;

use simcore_abi::ScUnitView;
use simcore_abi::affiliation::{FRIENDLY, HOSTILE, NEUTRAL};
use simcore_engine::{
    EmsProfile, HeatmapSpec, OutcomeTable, Pcg32, Position, Pursuer, Rules, Sensor, SensorParams,
    Sim, StableId, Terrain, UnitSpec, Velocity, kind,
};

struct Counting;
static LIVE_BYTES: AtomicUsize = AtomicUsize::new(0);
static ALLOCS: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        LIVE_BYTES.fetch_add(l.size(), Relaxed);
        ALLOCS.fetch_add(1, Relaxed);
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        LIVE_BYTES.fetch_sub(l.size(), Relaxed);
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        LIVE_BYTES.fetch_add(new, Relaxed);
        LIVE_BYTES.fetch_sub(l.size(), Relaxed);
        ALLOCS.fetch_add(1, Relaxed);
        unsafe { System.realloc(p, l, new) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

fn p99(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    v[(v.len() * 99 / 100).min(v.len() - 1)]
}

const SENSORS: u32 = 2_500;
const PURSUERS: u32 = 200;

/// `units` units (plus the command node) in a 40 km disc over 64 x 64 hilly terrain: 2,500
/// sensors, 30% of units emitting, 200 pursuers starting 3 km from their targets. Dummy values.
fn rules_scenario(units: u32) -> Sim {
    let mut sim = Sim::new(42);
    let mut rng = Pcg32::new(42, 0x52_55_4c_45_53); // "RULES"
    let heights = (0..64 * 64)
        .map(|_| rng.range_f64(0.0, 300.0) as f32)
        .collect();
    let terrain = Terrain::heightfield(-41_600.0, -41_600.0, 1300.0, 64, 64, heights, 16)
        .expect("valid terrain");
    sim.set_rules(Rules {
        detection_max_range_m: 8_000.0,
        sensor: SensorParams {
            base_range_m: 4_000.0,
            reference_power_w: 100.0,
        },
        exposure_threshold_ticks: 15,
        exposure_cap_ticks: 60,
        exposure_decay_per_tick: 1,
        outcomes: OutcomeTable::new(0.5),
    });
    const KINDS: [u16; 5] = [
        kind::QUADCOPTER,
        kind::FIXED_WING_DRONE,
        kind::GROUND_RADAR,
        kind::GROUND_VEHICLE,
        kind::JAMMER,
    ];
    let plain = units.saturating_sub(PURSUERS);
    for i in 0..plain {
        let k = KINDS[rng.below(KINDS.len() as u32) as usize];
        let r = 40_000.0 * rng.next_f64().sqrt();
        let theta = rng.range_f64(0.0, core::f64::consts::TAU);
        let (x, y) = (r * theta.cos(), r * theta.sin());
        let ground = terrain.height_at(x, y) + 2.0;
        let (z, speed) = match k {
            kind::QUADCOPTER => (
                ground + rng.range_f64(20.0, 300.0),
                rng.range_f64(0.0, 20.0),
            ),
            kind::FIXED_WING_DRONE => (ground + rng.range_f64(500.0, 3000.0), 50.0),
            _ => (ground, rng.range_f64(0.0, 5.0)),
        };
        let heading = rng.range_f64(0.0, core::f64::consts::TAU);
        let emitting = rng.below(100) < 30;
        // Sensors split evenly between the two sides; other units also include neutrals.
        let sensor = i < SENSORS;
        let affiliation = if sensor {
            [FRIENDLY, HOSTILE][rng.below(2) as usize]
        } else {
            [FRIENDLY, HOSTILE, NEUTRAL][rng.below(3) as usize]
        };
        let id = sim.spawn_unit(UnitSpec {
            kind: k,
            affiliation,
            position: Position { x, y, z },
            velocity: Velocity {
                x: speed * heading.cos(),
                y: speed * heading.sin(),
                z: 0.0,
            },
            ems: EmsProfile {
                emitting,
                erp_w: 10f64.powf(rng.range_f64(0.0, 4.5)) as f32,
            },
            cycle: None,
        });
        if sensor {
            sim.add_sensor(id, Sensor::default());
        }
    }
    sim.set_terrain(terrain);
    for _ in 0..PURSUERS.min(units) {
        let target = StableId(2 + u64::from(rng.below(plain.max(1))));
        let t = sim.position(target).unwrap_or_default();
        let a = rng.range_f64(0.0, core::f64::consts::TAU);
        sim.spawn_pursuer(
            UnitSpec {
                kind: kind::FIXED_WING_DRONE,
                affiliation: FRIENDLY,
                position: Position {
                    x: t.x + 3000.0 * a.cos(),
                    y: t.y + 3000.0 * a.sin(),
                    z: t.z + 300.0,
                },
                velocity: Velocity::default(),
                ems: EmsProfile::default(),
                cycle: None,
            },
            Pursuer {
                target,
                speed: 150.0,
                max_turn_rate: 0.6,
                lead: 0.3,
                engagement_radius_m: 10.0,
                outcome_class: 1,
            },
        );
    }
    sim
}

/// Adds one sensor per side far outside the scenario, at (1e7, 1e7) m.
fn add_outliers(sim: &mut Sim) {
    for affiliation in [FRIENDLY, HOSTILE] {
        let id = sim.spawn_unit(UnitSpec {
            kind: kind::GROUND_RADAR,
            affiliation,
            position: Position {
                x: 1e7,
                y: 1e7,
                z: 0.0,
            },
            velocity: Velocity::default(),
            ems: EmsProfile::default(),
            cycle: None,
        });
        sim.add_sensor(id, Sensor::default());
    }
}

fn time_steps(sim: &mut Sim, iters: usize) -> Vec<f64> {
    (0..iters)
        .map(|_| {
            let t = Instant::now();
            sim.step(1);
            t.elapsed().as_secs_f64() * 1e3
        })
        .collect()
}

/// `sensors` friendly sensors and `emitters` hostile emitters, all in a 2 km disc, with every
/// line of sight blocked by a 1,000 m plateau (or flat terrain): nothing is observed, so each
/// emitter tests every sensor within reach. Dummy values.
fn cluster(sensors: u32, emitters: u32, blocked: bool) -> Sim {
    let mut sim = Sim::new(7);
    let mut rng = Pcg32::new(7, 0x43_4c_55_53); // "CLUS"
    sim.set_rules(Rules {
        detection_max_range_m: 8_000.0,
        sensor: SensorParams {
            base_range_m: 4_000.0,
            reference_power_w: 100.0,
        },
        exposure_threshold_ticks: 15,
        exposure_cap_ticks: 60,
        exposure_decay_per_tick: 1,
        outcomes: OutcomeTable::new(0.5),
    });
    if blocked {
        sim.set_terrain(
            Terrain::heightfield(-5_000.0, -5_000.0, 160.0, 64, 64, vec![1000.0; 64 * 64], 16)
                .expect("valid terrain"),
        );
    }
    for i in 0..sensors + emitters {
        let r = 2_000.0 * rng.next_f64().sqrt();
        let theta = rng.range_f64(0.0, core::f64::consts::TAU);
        let sensor = i < sensors;
        let id = sim.spawn_unit(UnitSpec {
            kind: kind::GROUND_VEHICLE,
            affiliation: if sensor { FRIENDLY } else { HOSTILE },
            position: Position {
                x: r * theta.cos(),
                y: r * theta.sin(),
                z: 0.0,
            },
            velocity: Velocity::default(),
            ems: EmsProfile {
                emitting: !sensor,
                erp_w: 100.0,
            },
            cycle: None,
        });
        if sensor {
            sim.add_sensor(id, Sensor::default());
        }
    }
    sim
}

/// Layouts outside the main scenario that hit the documented worst cases.
fn bench_worst_cases(units: u32) {
    println!();
    println!("worst cases (median step(1) over 20 ticks):");
    for (s, e) in [(500, 5_000), (1_000, 10_000), (2_000, 20_000)] {
        let (mut blocked, mut flat) = (cluster(s, e, true), cluster(s, e, false));
        let (b, f) = (time_steps(&mut blocked, 20), time_steps(&mut flat, 20));
        println!(
            "  2 km cluster, {s} sensors x {e} emitters: LOS blocked {:.3} ms, flat {:.3} ms",
            median(b),
            median(f)
        );
    }

    let mut base = rules_scenario(units);
    let mut outliers = rules_scenario(units);
    add_outliers(&mut outliers);
    base.step(1);
    outliers.step(1);
    println!(
        "  rules scenario {:.3} ms; with one far sensor per side at (1e7, 1e7) m {:.3} ms",
        median(time_steps(&mut base, 50)),
        median(time_steps(&mut outliers, 50))
    );

    // 20,000 hostile emitters in a 2 km disc, mapped from the middle of the cluster.
    let mut sim = cluster(0, 20_000, false);
    for (cell, n) in [(50.0, 128), (50.0, 512)] {
        let spec = HeatmapSpec {
            cell_size_m: cell,
            cells_x: n,
            cells_y: n,
            d_min_m: 10.0,
            cutoff_m: 8_000.0,
            theta: HeatmapSpec::EXACT,
        };
        let far = HeatmapSpec {
            theta: HeatmapSpec::DEFAULT_THETA,
            ..spec
        };
        let ((exact, _), (approx, _)) = (
            time_heatmap(&mut sim, spec, 5),
            time_heatmap(&mut sim, far, 5),
        );
        let (rel, of_max) = heatmap_error(&mut sim, far);
        println!(
            "  heatmap {n}x{n} ({cell} m cells, 8 km cutoff) over 20,000 clustered emitters: \
             exact {:.3} ms, theta {} {:.3} ms (worst error {:.2}% of a cell, {:.3}% of the max)",
            median(exact),
            far.theta,
            median(approx),
            rel * 100.0,
            of_max * 100.0
        );
    }
}

/// Times `iters` heatmap builds after one warm-up build. Returns the times in ms and the heap
/// allocations made by the builds.
fn time_heatmap(sim: &mut Sim, spec: HeatmapSpec, iters: usize) -> (Vec<f64>, usize) {
    sim.build_heatmap(spec, HOSTILE);
    let mut ms = Vec::with_capacity(iters);
    let before = ALLOCS.load(Relaxed);
    for _ in 0..iters {
        let t = Instant::now();
        std::hint::black_box(sim.build_heatmap(spec, HOSTILE));
        ms.push(t.elapsed().as_secs_f64() * 1e3);
    }
    (ms, ALLOCS.load(Relaxed) - before)
}

/// Worst error of `spec` against the exact map: relative to the cell's exact value (over cells
/// above 1e-6 of the max), and relative to the map's max.
fn heatmap_error(sim: &mut Sim, spec: HeatmapSpec) -> (f64, f64) {
    let exact = HeatmapSpec {
        theta: HeatmapSpec::EXACT,
        ..spec
    };
    let exact: Vec<f32> = sim.build_heatmap(exact, HOSTILE).values().to_vec();
    let approx = sim.build_heatmap(spec, HOSTILE).values();
    let max = f64::from(exact.iter().copied().fold(0.0, f32::max));
    let (mut rel, mut of_max) = (0.0f64, 0.0f64);
    for (&e, &a) in exact.iter().zip(approx) {
        let err = f64::from((a - e).abs());
        if f64::from(e) > 1e-6 * max {
            rel = rel.max(err / f64::from(e));
        }
        of_max = of_max.max(err / max);
    }
    (rel, of_max)
}

fn bench_rules(units: u32) {
    const ITERS: usize = 300;
    let mut sim = rules_scenario(units);
    sim.step(1);
    sim.frame();
    let spec = HeatmapSpec {
        cell_size_m: 500.0,
        cells_x: 128,
        cells_y: 128,
        d_min_m: 10.0,
        cutoff_m: 8_000.0,
        theta: HeatmapSpec::EXACT,
    };
    let far = HeatmapSpec {
        theta: HeatmapSpec::DEFAULT_THETA,
        ..spec
    };
    sim.build_heatmap(spec, HOSTILE);
    sim.build_heatmap(far, HOSTILE);

    let mut step_ms = Vec::with_capacity(ITERS);
    let mut engagements = 0;
    let allocs_before = ALLOCS.load(Relaxed);
    let (mut quiet_ticks, mut quiet_allocs) = (0, 0);
    for _ in 0..ITERS {
        let a0 = ALLOCS.load(Relaxed);
        let t = Instant::now();
        sim.step(1);
        step_ms.push(t.elapsed().as_secs_f64() * 1e3);
        engagements += sim.engagement_events().len();
        if sim.engagement_events().is_empty() {
            quiet_ticks += 1;
            quiet_allocs += ALLOCS.load(Relaxed) - a0;
        }
        std::hint::black_box(sim.frame());
    }
    let steady_allocs = ALLOCS.load(Relaxed) - allocs_before;

    let (heat_ms, exact_allocs) = time_heatmap(&mut sim, spec, 50);
    let (far_ms, far_allocs) = time_heatmap(&mut sim, far, 50);
    let heat_allocs = exact_allocs + far_allocs;
    let (rel, of_max) = heatmap_error(&mut sim, far);

    let frame = sim.frame();
    let emitting = frame
        .iter()
        .filter(|u| u.flags & simcore_abi::flags::EMITTING != 0)
        .count();
    let detected = frame
        .iter()
        .filter(|u| u.flags & simcore_abi::flags::DETECTED != 0)
        .count();
    println!();
    println!(
        "rules scenario: {} units, {SENSORS} sensors, {emitting} emitting, {PURSUERS} pursuers, \
         64x64 terrain",
        sim.unit_count()
    );
    println!(
        "step(1):  median {:.3} ms  p99 {:.3} ms  max {:.3} ms",
        median(step_ms.clone()),
        p99(step_ms.clone()),
        step_ms.iter().copied().fold(0.0, f64::max)
    );
    println!("engagements during {ITERS} ticks: {engagements}; detected at end: {detected}");
    println!(
        "heap allocations during {ITERS} x (step(1) + frame()): {steady_allocs} \
         ({quiet_allocs} in the {quiet_ticks} ticks without engagements)"
    );
    println!(
        "heatmap 128x128 (500 m cells, 8 km cutoff): exact median {:.3} ms  p99 {:.3} ms; \
         theta {} median {:.3} ms  p99 {:.3} ms (worst error {:.2}% of a cell, {:.3}% of the \
         max); allocations in 2 x 50 builds: {heat_allocs}",
        median(heat_ms.clone()),
        p99(heat_ms),
        far.theta,
        median(far_ms.clone()),
        p99(far_ms),
        rel * 100.0,
        of_max * 100.0
    );
    assert_eq!(steady_allocs, 0, "steady-state ticks allocate");
    assert_eq!(heat_allocs, 0, "heatmap rebuilds allocate");
}

fn main() {
    let units: u32 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(50_000);
    const ITERS: usize = 300;

    let before = LIVE_BYTES.load(Relaxed);
    let mut sim = Sim::new_dummy(42, units);
    let world_bytes = LIVE_BYTES.load(Relaxed) - before;

    // Warm up: first step builds the schedule; first frame sizes the buffer.
    sim.step(1);
    sim.frame();
    let frame_bytes = sim.frame_capacity() * size_of::<ScUnitView>();

    let mut step_ms = Vec::with_capacity(ITERS);
    let mut frame_ms = Vec::with_capacity(ITERS);
    let allocs_before = ALLOCS.load(Relaxed);
    for _ in 0..ITERS {
        let t = Instant::now();
        sim.step(1);
        step_ms.push(t.elapsed().as_secs_f64() * 1e3);
        let t = Instant::now();
        std::hint::black_box(sim.frame());
        frame_ms.push(t.elapsed().as_secs_f64() * 1e3);
    }
    let steady_allocs = ALLOCS.load(Relaxed) - allocs_before;

    let n = f64::from(sim.unit_count());
    let (s, f) = (median(step_ms.clone()), median(frame_ms.clone()));
    println!("units (incl. command node): {}", sim.unit_count());
    println!(
        "step(1):  median {s:.3} ms  min {:.3} ms  ({:.1} ns/unit)",
        step_ms.iter().copied().fold(f64::MAX, f64::min),
        s * 1e6 / n
    );
    println!(
        "frame():  median {f:.3} ms  min {:.3} ms  ({:.1} ns/unit)",
        frame_ms.iter().copied().fold(f64::MAX, f64::min),
        f * 1e6 / n
    );
    println!(
        "frame buffer: {frame_bytes} bytes ({:.2} MiB)",
        frame_bytes as f64 / 1048576.0
    );
    println!(
        "ECS world heap (after new_dummy, before frame): {world_bytes} bytes ({:.2} MiB, {:.0} B/unit)",
        world_bytes as f64 / 1048576.0,
        world_bytes as f64 / n
    );
    println!("heap allocations during {ITERS} x (step(1) + frame()): {steady_allocs}");

    // Save and restore of the rules scenario (sensors, pursuers, terrain): the app saves
    // regularly during play, so this has to stay well under a frame budget's worth of time.
    let mut sim = rules_scenario(units);
    sim.step(30);
    let (mut save_ms, mut restore_ms) = (Vec::new(), Vec::new());
    let mut bytes = Vec::new();
    for _ in 0..20 {
        let t = Instant::now();
        bytes = sim.save().to_vec();
        save_ms.push(t.elapsed().as_secs_f64() * 1e3);
        let t = Instant::now();
        std::hint::black_box(Sim::restore(&bytes).unwrap());
        restore_ms.push(t.elapsed().as_secs_f64() * 1e3);
    }
    println!(
        "\nsnapshot of the rules scenario ({} units): {} bytes ({:.2} MiB); save median {:.2} ms, restore median {:.2} ms",
        sim.unit_count(),
        bytes.len(),
        bytes.len() as f64 / 1048576.0,
        median(save_ms),
        median(restore_ms)
    );

    bench_rules(units);
    bench_worst_cases(units);
}
