//! The signal heatmap's exact and far-field modes: equality with the exact sum, error bounds,
//! determinism and allocations. All numbers are test fixtures.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use simcore_abi::affiliation::{FRIENDLY, HOSTILE};
use simcore_engine::{EmsProfile, HeatmapSpec, Pcg32, Position, Sim, UnitSpec, Velocity, kind};

/// Counts allocations made on the current thread while counting is on, so tests running in
/// parallel do not see each other's.
struct Counting;

thread_local! {
    static COUNTING: Cell<bool> = const { Cell::new(false) };
    static ALLOCS: Cell<usize> = const { Cell::new(0) };
}

fn note_alloc() {
    if COUNTING.try_with(Cell::get).unwrap_or(false) {
        let _ = ALLOCS.try_with(|a| a.set(a.get() + 1));
    }
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        note_alloc();
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        note_alloc();
        unsafe { System.realloc(p, l, new) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// Allocations on this thread while running `f`.
fn allocations(f: impl FnOnce()) -> usize {
    ALLOCS.with(|a| a.set(0));
    COUNTING.with(|c| c.set(true));
    f();
    COUNTING.with(|c| c.set(false));
    ALLOCS.with(Cell::get)
}

/// FNV-1a over the bits of every cell.
fn hash(values: &[f32]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for v in values {
        for b in v.to_bits().to_le_bytes() {
            h = (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    h
}

/// The command node at (12 km, -7 km) with `clustered` hostile emitters in a 2 km disc 300 m
/// off it, `spread` more in a 40 km disc, and silent or friendly units mixed in.
fn emitters(seed: u64, clustered: u32, spread: u32) -> Sim {
    let mut sim = Sim::new(seed);
    let (ox, oy) = (12_000.0, -7_000.0);
    sim.set_command_node_position(Position {
        x: ox,
        y: oy,
        z: 0.0,
    });
    let mut rng = Pcg32::new(seed, 0x48_45_41_54); // "HEAT"
    for i in 0..clustered + spread {
        let (radius, cx) = if i < clustered {
            (2_000.0, 300.0)
        } else {
            (40_000.0, 0.0)
        };
        let r = radius * rng.next_f64().sqrt();
        let a = rng.range_f64(0.0, core::f64::consts::TAU);
        let power = 10f64.powf(rng.range_f64(0.0, 4.0)) as f32;
        let roll = rng.below(10);
        sim.spawn_unit(UnitSpec {
            kind: kind::GROUND_VEHICLE,
            affiliation: if roll == 0 { FRIENDLY } else { HOSTILE },
            position: Position {
                x: ox + cx + r * a.cos(),
                y: oy + r * a.sin(),
                z: 0.0,
            },
            velocity: Velocity::default(),
            ems: EmsProfile {
                emitting: roll != 1,
                erp_w: power,
            },
            cycle: None,
        });
    }
    sim
}

fn spec(cell_size_m: f64, cells: (u32, u32), cutoff_m: f64, theta: f64) -> HeatmapSpec {
    HeatmapSpec {
        cell_size_m,
        cells_x: cells.0,
        cells_y: cells.1,
        d_min_m: 10.0,
        cutoff_m,
        theta,
    }
}

/// The layouts below, recorded from the implementation before the far field was added.
const GOLDEN: [(f64, (u32, u32), f64, u64); 2] = [
    (50.0, (100, 84), 8_000.0, 0x3eed_0baf_0cf9_e128),
    (400.0, (64, 64), 30_000.0, 0xed9b_a0e3_523e_3197),
];

#[test]
fn exact_mode_is_unchanged() {
    let mut sim = emitters(3, 3_000, 3_000);
    for (cell, cells, cutoff, golden) in GOLDEN {
        for theta in [HeatmapSpec::EXACT, f64::NAN] {
            let values = sim
                .build_heatmap(spec(cell, cells, cutoff, theta), HOSTILE)
                .values();
            assert!(values.iter().any(|&v| v > 0.0));
            assert_eq!(hash(values), golden, "{cell} m cells, theta {theta}");
        }
    }
}

/// Worst error of `theta` against the exact map: `(largest |error| / exact value over cells
/// above 1e-6 of the max, largest |error| / max, largest |error| / allowed)`, where allowed is
/// the larger of 5% of the cell's exact value and 1% of the map's max.
fn far_field_error(sim: &mut Sim, spec: HeatmapSpec) -> (f64, f64, f64) {
    let exact: Vec<f32> = sim
        .build_heatmap(
            HeatmapSpec {
                theta: HeatmapSpec::EXACT,
                ..spec
            },
            HOSTILE,
        )
        .values()
        .to_vec();
    let approx = sim.build_heatmap(spec, HOSTILE).values();
    let max = exact.iter().copied().fold(0.0f32, f32::max) as f64;
    assert!(max > 0.0);
    let (mut rel, mut of_max, mut of_allowed) = (0.0f64, 0.0f64, 0.0f64);
    for (&e, &a) in exact.iter().zip(approx) {
        let (e, a) = (f64::from(e), f64::from(a));
        let err = (a - e).abs();
        if e > 1e-6 * max {
            rel = rel.max(err / e);
        }
        of_max = of_max.max(err / max);
        of_allowed = of_allowed.max(err / (0.05 * e).max(0.01 * max));
    }
    (rel, of_max, of_allowed)
}

/// A map centred on the origin with one 100 W hostile emitter at each of `points`.
fn layout(points: impl IntoIterator<Item = (f64, f64)>) -> Sim {
    let mut sim = Sim::new(1);
    sim.set_command_node_position(Position::default());
    for (x, y) in points {
        sim.spawn_unit(UnitSpec {
            kind: kind::GROUND_VEHICLE,
            affiliation: HOSTILE,
            position: Position { x, y, z: 0.0 },
            velocity: Velocity::default(),
            ems: EmsProfile {
                emitting: true,
                erp_w: 100.0,
            },
            cycle: None,
        });
    }
    sim
}

/// Checks the error at `theta = 1, 2, 4`: at `theta >= 2`, within 5% of every cell (and so
/// within the larger of 5% of the cell and 1% of the max).
fn check_far_field(name: &str, sim: &mut Sim, cell: f64, cells: (u32, u32), cutoff: f64) {
    for theta in [1.0, HeatmapSpec::DEFAULT_THETA, 4.0] {
        let (rel, of_max, of_allowed) = far_field_error(sim, spec(cell, cells, cutoff, theta));
        println!(
            "{name}, theta {theta}: worst error {:.3}% of the cell, {:.4}% of the max \
             ({:.2} of the allowed)",
            rel * 100.0,
            of_max * 100.0,
            of_allowed
        );
        if theta >= HeatmapSpec::DEFAULT_THETA {
            assert!(of_allowed <= 1.0, "{name}, theta {theta}: {of_allowed}");
            assert!(rel <= 0.05, "{name}, theta {theta}: {rel}");
        }
    }
}

#[test]
fn far_field_error_is_bounded() {
    // (name, clustered, spread, cell size, cells, cutoff)
    let layouts = [
        ("clustered", 5_000, 0, 50.0, (96, 96), 8_000.0),
        ("clustered, coarse", 5_000, 0, 200.0, (60, 44), 8_000.0),
        ("spread", 0, 20_000, 400.0, (100, 100), 20_000.0),
        (
            "spread, wide cutoff",
            0,
            20_000,
            400.0,
            (100, 100),
            100_000.0,
        ),
        ("mixed", 3_000, 3_000, 50.0, (100, 84), 8_000.0),
        (
            "mixed, short cutoff",
            3_000,
            3_000,
            100.0,
            (64, 64),
            1_500.0,
        ),
    ];
    for (name, clustered, spread, cell, cells, cutoff) in layouts {
        let mut sim = emitters(9, clustered, spread);
        check_far_field(name, &mut sim, cell, cells, cutoff);
    }
}

#[test]
fn far_field_error_is_bounded_for_adversarial_layouts() {
    // Two emitters on a grid cell's diagonal, pointing at the map: one source at their
    // centroid alone is 18% off. Moved outwards, the pair is aggregated at ever smaller ratios
    // of its radius to its distance, down to the opening limit.
    for shift in [0.0, 300.0, 500.0, 700.0, 1_000.0, 2_000.0] {
        for (a, b) in [
            ((650.0, 650.0), (1_050.0, 1_050.0)),
            ((-650.0, 650.0), (-1_050.0, 1_050.0)),
            ((900.0, 0.0), (1_466.0, 0.0)),
        ] {
            let out = |(x, y): (f64, f64)| {
                let l = f64::hypot(x, y);
                (x * (l + shift) / l, y * (l + shift) / l)
            };
            let mut sim = layout([out(a), out(b)]);
            check_far_field("pair", &mut sim, 10.0, (16, 16), 8_000.0);
        }
    }
    // A 50 x 50 block of emitters off the map, straddling the 8 km cutoff: every cell's signal
    // comes from emitters at the cutoff.
    let mut sim =
        layout((0..50).flat_map(|i| {
            (0..50).map(move |j| (7_600.0 + 16.0 * i as f64, -400.0 + 16.0 * j as f64))
        }));
    check_far_field("straddling block", &mut sim, 10.0, (16, 16), 8_000.0);
    check_far_field("straddling block", &mut sim, 50.0, (64, 64), 8_000.0);
}

#[test]
fn far_field_is_deterministic() {
    let (mut a, mut b) = (emitters(4, 4_000, 4_000), emitters(4, 4_000, 4_000));
    for theta in [1.0, HeatmapSpec::DEFAULT_THETA, 10.0] {
        let s = spec(60.0, (90, 70), 9_000.0, theta);
        let first = hash(a.build_heatmap(s, HOSTILE).values());
        assert_eq!(first, hash(b.build_heatmap(s, HOSTILE).values()));
        assert_eq!(first, hash(a.build_heatmap(s, HOSTILE).values()));
        // Another map in between leaves no trace.
        a.build_heatmap(spec(500.0, (16, 16), 30_000.0, theta), HOSTILE);
        assert_eq!(first, hash(a.build_heatmap(s, HOSTILE).values()));
    }
    let s = spec(60.0, (90, 70), 9_000.0, HeatmapSpec::DEFAULT_THETA);
    let mut c = emitters(5, 4_000, 4_000);
    assert_ne!(
        hash(a.build_heatmap(s, HOSTILE).values()),
        hash(c.build_heatmap(s, HOSTILE).values())
    );
}

#[test]
fn repeated_builds_do_not_allocate() {
    let mut sim = emitters(6, 3_000, 3_000);
    // The first step builds the schedule.
    sim.step(1);
    for theta in [HeatmapSpec::EXACT, HeatmapSpec::DEFAULT_THETA] {
        let s = spec(50.0, (100, 84), 8_000.0, theta);
        sim.build_heatmap(s, HOSTILE);
        let n = allocations(|| {
            for _ in 0..10 {
                sim.step(1);
                std::hint::black_box(sim.build_heatmap(s, HOSTILE));
            }
        });
        assert_eq!(n, 0, "theta {theta}");
    }
}

#[test]
fn far_field_edge_cases() {
    // Coincident emitters (one finest cell), with d_min from 0 to wider than the map.
    let mut sim = Sim::new(1);
    for _ in 0..50 {
        sim.spawn_unit(UnitSpec {
            kind: kind::GROUND_VEHICLE,
            affiliation: HOSTILE,
            position: Position {
                x: 3_000.0,
                y: 0.0,
                z: 0.0,
            },
            velocity: Velocity::default(),
            ems: EmsProfile {
                emitting: true,
                erp_w: 10.0,
            },
            cycle: None,
        });
    }
    for d_min in [0.0, 10.0, 50_000.0] {
        let s = HeatmapSpec {
            d_min_m: d_min,
            ..spec(100.0, (40, 30), 10_000.0, HeatmapSpec::DEFAULT_THETA)
        };
        let approx = sim.build_heatmap(s, HOSTILE).values().to_vec();
        let exact = sim
            .build_heatmap(
                HeatmapSpec {
                    theta: HeatmapSpec::EXACT,
                    ..s
                },
                HOSTILE,
            )
            .values();
        for (a, e) in approx.iter().zip(exact) {
            assert!((a - e).abs() <= 1e-6 * e, "d_min {d_min}: {a} vs {e}");
        }
    }
    // Theta below 1 counts as 1.
    let mut sim = emitters(2, 2_000, 0);
    let one = hash(
        sim.build_heatmap(spec(50.0, (64, 64), 8_000.0, 1.0), HOSTILE)
            .values(),
    );
    for theta in [0.0, -3.0, 0.5] {
        let values = sim.build_heatmap(spec(50.0, (64, 64), 8_000.0, theta), HOSTILE);
        assert_eq!(hash(values.values()), one);
    }
    // Extreme spreads and cutoffs neither panic nor give non-finite cells.
    let mut sim = Sim::new(3);
    for x in [-1e300, -3e6, 0.0, 5.0, 3e6, 1e300, f64::MAX] {
        sim.spawn_unit(UnitSpec {
            kind: kind::GROUND_VEHICLE,
            affiliation: HOSTILE,
            position: Position { x, y: x, z: 0.0 },
            velocity: Velocity::default(),
            ems: EmsProfile {
                emitting: true,
                erp_w: 10.0,
            },
            cycle: None,
        });
    }
    for cutoff in [1e-12, 1e4, 1e300, f64::MAX] {
        let values = sim.build_heatmap(spec(2_000.0, (64, 64), cutoff, 2.0), HOSTILE);
        assert!(
            values.values().iter().all(|v| v.is_finite()),
            "cutoff {cutoff}"
        );
    }
}
