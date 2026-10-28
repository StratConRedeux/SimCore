//! Drives `Sim::step` in real time with a fixed-timestep accumulator, the way a host app would.
//!
//! `cargo run --release -p simcore-engine --example headless -- [seconds] [units]`

use std::time::{Duration, Instant};

use simcore_engine::{Sim, TICKS_PER_SECOND};

fn main() {
    let mut args = std::env::args().skip(1);
    let seconds: f64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(3.0);
    let units: u32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(10_000);

    let mut sim = Sim::new_dummy(42, units);
    let dt = Duration::from_secs_f64(1.0 / f64::from(TICKS_PER_SECOND));
    // Never run more than this many ticks per wake-up, so a long stall cannot spiral.
    const MAX_CATCH_UP: u32 = 30;

    let start = Instant::now();
    let mut last = start;
    let mut accumulator = Duration::ZERO;
    let mut sim_time = Duration::ZERO;

    while start.elapsed().as_secs_f64() < seconds {
        let now = Instant::now();
        accumulator += now - last;
        last = now;

        let mut due = 0;
        while accumulator >= dt && due < MAX_CATCH_UP {
            accumulator -= dt;
            due += 1;
        }
        if due > 0 {
            let t = Instant::now();
            sim.step(due);
            let frame_len = sim.frame().len();
            sim_time += t.elapsed();
            println!(
                "tick {:>4}  +{due} step(s)  {frame_len} units  sim+frame {:>7.3} ms",
                sim.tick(),
                t.elapsed().as_secs_f64() * 1e3
            );
        }
        // Sleep until the next tick is due rather than spinning: the CPU stays idle in between.
        std::thread::sleep(dt.saturating_sub(accumulator));
    }

    let wall = start.elapsed().as_secs_f64();
    println!(
        "{} ticks in {wall:.3} s wall ({:.2} ticks/s, target {TICKS_PER_SECOND}); \
         busy {:.2} ms total, {:.3} ms per tick",
        sim.tick(),
        sim.tick() as f64 / wall,
        sim_time.as_secs_f64() * 1e3,
        sim_time.as_secs_f64() * 1e3 / sim.tick().max(1) as f64
    );
}
