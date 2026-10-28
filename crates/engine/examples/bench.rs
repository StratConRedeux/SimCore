//! Times `step(1)` and `frame()` and measures heap use with a counting allocator.
//!
//! `cargo run --release -p simcore-engine --example bench -- [units]`

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::time::Instant;

use simcore_abi::ScUnitView;
use simcore_engine::Sim;

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
}
