# Benchmarks

Release build (`lto = "fat"`, `codegen-units = 1`), Apple M6 Mac (macOS 27.0), rustc 1.99.1,
bevy_ecs 0.20.0, single-threaded executor.

```sh
cargo run --release -p simcore-engine --example bench -- 50000
```

## Dummy world

50,000 dummy units plus the command node, with no sensors or pursuers. The Phase 2 systems still
run, but they skip their per-unit passes while nothing is observed or exposed.

| Measurement | Result |
|---|---|
| `step(1)` (all systems; only duty cycle and movement have work) | ~0.055 ms median (~1.1 ns/unit) |
| `frame()` (rebuild 50,001 views) | ~0.117 ms median (~2.3 ns/unit) |
| Frame buffer | 2,000,040 B (1.91 MiB; 40 B x 50,001, exact capacity) |
| ECS world heap after `new_dummy` | 14,174,447 B (13.52 MiB, ~283 B/unit) |
| Heap allocations in 300 x (`step(1)` + `frame()`) | 0 |

The world heap grew from ~227 B/unit with Phase 2's `Exposure`, `Detected` and removal-marker
components and the id index. `frame()` went from ~0.098 ms to ~0.117 ms because it now reads the
removal marker to skip removed units (see Allocations below).

## Rules scenario

50,000 units in a 40 km disc over a 64 x 64 heightfield (1.3 km spacing, 16 line-of-sight
samples). 2,500 sensors, split between the two sides. 30% of units are emitting (14,814 at the
end of the run), with powers spread over 1 W to ~30 kW. 200 pursuers start 3 km from random
targets. Max detection range is 8 km. All values are dummies.

| Measurement | Result |
|---|---|
| `step(1)` median | ~1.65 ms |
| `step(1)` p99 | ~1.85 ms (max ~1.9 ms) |
| Engagements during the 300 measured ticks | 104 (in 29 of the ticks) |
| Heap allocations in 300 x (`step(1)` + `frame()`) | 0 (asserted by the bench) |
| Heatmap build, 128 x 128 (500 m cells, 8 km cutoff, ~4,900 emitters), exact | ~3.50 ms median, ~3.61 ms p99 |
| Same, far field (`theta = 2`) | ~3.89 ms median, ~4.12 ms p99 (worst cell 0.08% off) |
| Heap allocations in 50 exact + 50 far-field heatmap builds | 0 (asserted) |

In this spread-out scenario the cutoff (8 km) is only twice a block's width, so most grid cells
within it are either not far enough to aggregate or straddle a block's cutoff circle (and are
summed emitter by emitter, so the cutoff stays exact). Little gets aggregated, and the far field
is ~11% slower than exact: use `HeatmapSpec::EXACT` when the cutoff is only a few block widths.

**Allocations.** The engine's own buffers are reused. The grids reserve their worst-case size up
front, and the engagement buffers are sized for the live pursuers when a pursuer is spawned.
`World::despawn` would allocate inside bevy_ecs (per-component `RemovedComponents` queues and
entity free-list growth), so an engagement does not despawn: it frees the stable id, marks the
entity removed (skipped by every system, the frame and the heatmap) and queues it. The queued
entities are despawned on the next spawn or `Sim::despawn_removed`, outside `step`, where those
allocations happen instead.

## Worst cases

Layouts outside the main scenario, chosen to hit the cost bounds in `docs/rules.md`. Medians of
`step(1)` over 20 ticks (heatmap: 5 builds). Dummy values.

| Layout | Result |
|---|---|
| 2 km cluster, 500 sensors x 5,000 emitters, all lines of sight blocked | ~12.4 ms (flat terrain: ~0.06 ms) |
| 2 km cluster, 1,000 x 10,000, blocked | ~49 ms (flat: ~0.12 ms) |
| 2 km cluster, 2,000 x 20,000, blocked | ~197 ms (flat: ~0.23 ms) |
| Rules scenario plus one sensor per side at (1e7, 1e7) m | ~2.0 ms (without: ~1.67 ms) |
| Heatmap 128 x 128, 50 m cells, 8 km cutoff, 20,000 emitters in a 2 km disc, exact | ~194 ms |
| Same, far field (`theta = 2`) | ~6.3 ms (worst cell 0.05% off; 0.021% of the max) |
| Heatmap 512 x 512, same emitters, exact | ~1,064 ms |
| Same, far field (`theta = 2`) | ~140 ms (worst cell 0.05% off; the cutoff circle of a third of the blocks crosses the cluster) |

**Cost.** Observation dominates the step: grid queries over the opposing side's sensors, an exact
range test per candidate, and up to 16 bilinear terrain samples per in-range pair. Each emitter
stops at the first sensor that observes it, so the cost is near linear while sensors do observe.
When nothing observes and units are packed into a few cells, every emitter tests every sensor:
the blocked clusters above grow 4x when both counts double (`emitters x sensors`). There is no
work cap; avoid large, dense groups of mutually hidden sensors and emitters.

The grid's cell size is fixed at half the max range. A far outlier switches it to a sorted layout
(binary search per row) instead of coarsening the cells, which costs ~20% here rather than
putting every sensor in one cell.

The exact heatmap costs about cells x emitters within the cutoff (plus a block's half-diagonal),
with no cap: clustered emitters near the map make every cell expensive. The far field
(`HeatmapSpec::theta`, see `docs/rules.md`) replaces each distant grid cell of emitters with its
aggregate (with a quadrupole term), so a block of cells costs about the tree cells it visits
plus the emitters of the leaves that are near it or straddle its cutoff circle: 31x faster on the
128 x 128 cluster. On the 512 x 512 the cutoff circle of many blocks crosses the cluster, and
those emitters are summed one by one to keep the cutoff exact, so it is only 7.6x faster. The
error stays within ~1.3% of each cell at `theta = 2`, including at the cutoff. Exact mode is unchanged (bit for bit, tested). Either way the heatmap is
built on demand, not every tick.

Medians over 300 iterations after one warm-up step and frame, with hot caches. Memory comes from
a counting global allocator in the example, so it is exact heap bytes, not RSS. The ECS figure
includes table growth slack and per-component change ticks.

In the real-time loop (`examples/headless.rs`, 50,000 dummy units) a tick plus frame cost ~1 ms,
because the thread sleeps between ticks and wakes with cold caches and a lower clock. That is the
better guide to per-tick cost in an app; the tables above are the steady-state ceiling.
