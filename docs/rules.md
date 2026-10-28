# Gameplay rules (Phase 2)

Simple, deterministic, game-level rules. Every number that tunes them is data supplied by the
caller: spawn specs (`UnitSpec`, `Pursuer`, `Sensor`), the `Rules` resource (`Sim::set_rules`)
and the `Terrain` resource (`Sim::set_terrain`). The repository ships no tuning tables; values in
tests and the benchmark are dummies.

The maths is in `crates/rules` (`simcore-rules`, no dependencies): pure `f64` functions with no
state, randomness or allocation. `crates/engine` wires them into the ECS. Units are SI throughout:
metres (m), seconds (s), radians (rad), watts (W). Coordinates are world east-north-up, x east,
y north, z up (absolute altitude).

## Tick order

Each tick of `DT = 1/15 s` runs, single-threaded and always in this order:

1. **Emitter duty cycles** (`EmitterCycle`): set `EmsProfile::emitting` for the tick.
2. **Observation**: mark units observed by an opposing sensor.
3. **Exposure** update.
4. **Detected** flag.
5. **Pursuit steering**: set pursuers' velocities.
6. **Movement**: explicit Euler, `position += velocity * DT`.
7. **Engagement resolution**: swept checks, outcome rolls, removals.

## Detection range

`detection_range(base_range_m, power_w, reference_power_w, max_range_m)`

    range = base_range * sqrt(power / reference_power), clamped to [0, max_range]

A sensor hears an emitter of the reference power out to its base range; four times the power
doubles the range. A non-positive or NaN base range, power or reference power gives 0, as does a
non-positive or non-finite max range. An infinite power or base range reaches the max range; an
undefined ratio (infinite power over an infinite reference, or an infinite base with an infinite
reference) gives 0. `in_detection_range(sensor, emitter, range)` is an
inclusive 3D distance test (`|emitter - sensor|^2 <= range^2`).

## Observation

A sensor (`Sensor` component) is a passive listener. It observes a unit when all hold:

- they are on **opposing** sides: friendly against hostile (`opposing(a, b)`). Neutral and
  unknown units neither observe nor are observed;
- the unit is **emitting** with power above 0 (`EmsProfile::current_power_w`);
- the unit is within the sensor's **detection range** for that power. The sensor's own
  `SensorParams { base_range_m, reference_power_w }` apply, or `Rules::sensor` when it has none.
  `Rules::detection_max_range_m` caps every range;
- **line of sight** is clear (below).

`sensor_observes(sensor, emitter, rules, terrain)` is this test for one pair. A unit is observed
in a tick (`Exposure::observed`) if at least one sensor observes it.

Neutral and unknown emitters skip the query entirely.

**Scaling.** Each side's sensors are binned every tick into a uniform 2D (x, y) grid with cells of
half the max range, aligned to multiples of the cell size from the world origin. The cell size is
fixed. When the sensors' bounding box fits in `max(4096, 2n)` cells, a dense array indexes the
cells; otherwise (a few far outliers, or a huge spread) the sensors are sorted by cell and each
queried row is found by binary search. Both layouts give the same candidates in the same order,
so one far-away sensor costs a binary search per row, not a coarser grid. Cell coordinates are
clamped to `±2^30` cells, so any finite position (up to `±f64::MAX`) is safe; non-finite
positions are binned at 0, and the exact per-pair test rejects them.

Each emitter queries only the opposing side's cells within its reach,
`min(max_range, k_max * sqrt(power))` with `k_max = max(base_range / sqrt(reference_power))` over
that side's sensors, and stops at the first sensor that observes it. Buffers are reused across
ticks, iteration order is fixed by insertion (no hashing), and the result equals a brute-force
check over all pairs (tested on 2,000 units, including units on cell edges, at exactly max range,
and far outliers).

**Cost bound.** Per tick, roughly `sum over emitters of (opposing sensors within reach until the
first that observes it)`. Spread-out scenarios stop early and stay near linear. The worst case is
`emitters x sensors` exact tests: units packed within one or two cells (a few km) with every
pair unobserved, typically because terrain blocks line of sight. Each failed pair costs a range
test plus up to `los_samples` terrain samples. There is no work cap. In the benchmark, 2,000
sensors and 20,000 emitters in a 2 km disc with all lines of sight blocked take ~200 ms per tick
(the same layout on flat terrain, where the first sensor observes, takes ~0.2 ms). Keep dense
clusters of mutually hidden sensors and emitters to a few thousand pairs' worth.

## Line of sight

`Terrain` is a coarse heightfield: `nx * ny` heights `spacing_m` apart, row-major from
`(origin_x, origin_y)`, sampled bilinearly and clamped to the edge values outside the grid. The
segment from sensor to emitter is tested at `los_samples` interior points,
`t = k / (los_samples + 1)` for `k = 1..=los_samples`, and is blocked if the terrain at any of
them is higher than the segment. End points are not tested, so units on the ground can see each
other. The cost per check is fixed at `los_samples` bilinear samples. Thin ridges between samples
can be missed; that is the trade for a fixed cost.

`Terrain::flat()`, the default, means always visible. `Terrain::heightfield` returns `None`
unless the grid is non-empty, the heights match `nx * ny` and are finite, the origin and
positive spacing are finite, and `los_samples` is at most `Terrain::MAX_LOS_SAMPLES` (1024).

## Exposure and detection

`Exposure { ticks, observed }` and `Detected(bool)` are on every unit.

- Observed this tick: `ticks = min(ticks + 1, cap)`.
- Not observed: `ticks = ticks - exposure_decay_per_tick`, floored at 0.
- Detected: `ticks >= threshold`. The frame's `DETECTED` flag (contract 1.1, bit 2) mirrors it.

`threshold = max(exposure_threshold_ticks, 1)` and `cap = max(exposure_cap_ticks, threshold)`.
A unit continuously observed from tick 1 is detected on exactly the threshold-th tick. The
default `Rules` are inert: max range 0, so nothing is observed.

## Pursuit steering

`pursuit_velocity(pursuer, target, PursuitParams { speed, max_turn_rate, lead }, dt)`

- **Aim point:** `target + target_velocity * (lead * distance / speed)`. `lead` is clamped to
  `[0, 1]`; 0 is pure pursuit, 1 aims where the target will be after the pursuer's straight-line
  time to reach it.
- **Turn:** the heading rotates toward the aim point, in the plane of the two, by at most
  `max_turn_rate * dt` rad. Speed is constant.
- **Degenerate inputs** always give finite results. Zero or invalid speed gives zero velocity. A
  pursuer with no heading points straight at the aim point (or east if it is on top of it). An
  aim point on top of the pursuer keeps the heading. A target directly behind turns left in the
  horizontal plane (or about east when the heading is vertical).

In the engine, `Pursuer { target, speed, max_turn_rate, lead, engagement_radius_m,
outcome_class }` steers once per tick. All pursuers read the same pre-steering state. A pursuer
whose target no longer exists keeps its velocity and flies straight.

## Swept closest approach

`closest_approach(a0, va, b0, vb, dt) -> Approach { time, distance }`: two points moving linearly
over `[0, dt]`. `time = clamp(-(r . w) / |w|^2, 0, dt)` with `r = b0 - a0` and `w = vb - va`;
equal velocities (or `dt <= 0`) give time 0. This is continuous collision detection, so a pursuer
fast enough to pass its target between ticks still registers.

## Engagement

After movement, each pursuer with a live target sweeps the tick just simulated (from
`position - velocity * DT` to `position`, for both). If the closest approach is within
`engagement_radius_m` it **rolls once**: `success = rng.next_f64() < chance`, where `chance` comes
from the outcome table for `(outcome_class, target kind)`.

- Success removes the target, except the **command node, which is never removed**: a pursuer
  may target it (a hostile drone aimed at the player's node is an ordinary scenario), and a
  successful roll is reported as a **command-node hit**, but the node stays. The pursuer is
  removed either way.
- Hits are gathered and resolved in ECS query order, which is deterministic. A hit is skipped if
  an earlier hit this tick already removed its pursuer or target.
- Rolls use the Sim's PCG32, seeded from `Sim::new(seed)` on its own stream, so the same seed and
  inputs give the same outcomes.
- Each resolution is recorded as an
  `EngagementEvent { tick, pursuer, target, success, target_removed, command_node_hit }`.
  `success` is the roll; `command_node_hit` is `success` against the command node, and
  `target_removed` is `success` against anything else, so at most one of them is set.
  `Sim::engagement_events()` returns the events from the most recent `step` call, in resolution
  order, from a buffer that is reused.
- `Sim::command_node_hits()` is the running total of command-node hits since `Sim::new`
  (saturating at `u32::MAX`), a field read. The engine attaches no consequence to it: what a hit
  means (damage, a lost game) is for the caller's rules.

**Outcome table** (`Rules::outcomes`, `OutcomeTable`): a sorted list of
`(class, kind) -> chance` with a `default_chance` for missing pairs. Chances are clamped to
`[0, 1]` and NaN counts as 0: 1.0 always succeeds and 0.0 never does.

**Removal** takes the unit out of play at once: its stable id is freed (`Sim::contains` is false),
`Sim::unit_count` drops, and the frame, heatmap and every system skip it. The ECS entity itself
is despawned later, outside `Sim::step`: on the next spawn, or when the caller calls
`Sim::despawn_removed`. That keeps `step` free of allocations (bevy_ecs allocates removal queues
and entity free-list space on despawn). Removed entities hold their storage until then. Frame
order after removals is ECS storage order: deterministic for the same seed and inputs, but not
sorted by id.

The engagement buffers are sized for the live pursuers whenever a pursuer is spawned; their
capacity follows the high-water mark of live pursuers.

## Signal falloff and heatmap

`signal_strength(power_w, distance_m, d_min_m) = power / max(distance, d_min)^2`, a plain
inverse-square fade in W/m^2 (unnormalised, no 4π). `d_min` keeps it bounded near the emitter.
A non-positive clamped distance or a non-finite result gives 0. `signal_strength_sq` is the same
from a squared distance.

`Sim::build_heatmap(HeatmapSpec, affiliation) -> &Heatmap` fills a `cells_x * cells_y` grid of
`f32` centred on the command node. Cell `(i, j)` (`i` east, `j` north) is centred at
`((i + 0.5 - cells_x / 2) * cell_size, (j + 0.5 - cells_y / 2) * cell_size)` relative to the
command node. Each cell sums `signal_strength` over currently emitting units of `affiliation`
whose **horizontal** distance from the cell centre is within `cutoff_m`. The heatmap is Rust-only
for now; it is not in the FFI contract.

`HeatmapSpec::theta` picks between the exact sum and a far-field approximation. `theta =
f64::INFINITY` (`HeatmapSpec::EXACT`) is exact; any finite value (suggested:
`HeatmapSpec::DEFAULT_THETA = 2.0`; below 1 counts as 1) approximates. NaN is also summed
exactly, but it is not a supported value: when `HeatmapSpec` goes into the FFI, `INFINITY` stays
the one encoding of exact (or an explicit mode replaces it), and NaN may become invalid.

**Exact** (unchanged from before `theta` existed, bit for bit):

- Emitters go into the same kind of uniform grid (cells of half the cutoff). Each 8 x 8 block of
  cells gathers its candidates once, then sums them with four fixed partial sums per cell, so the
  result is deterministic.
- **Cost bound:** about `cells x emitters within (cutoff + 5.7 cell widths)` pair evaluations
  (the extra is the half-diagonal of a block), with no cap other than `MAX_CELLS` and the
  caller's cutoff. Spread-out emitters are cheap; clustered ones are not. In the benchmark,
  20,000 emitters packed in a 2 km disc with an 8 km cutoff take ~194 ms for 128 x 128 cells and
  ~1.06 s for 512 x 512.

**Far field** (finite `theta`):

- The emitters go into a quadtree of square grid cells over their bounding box, sorted by Morton
  code (no hashing; ties broken by index). Each grid cell keeps an aggregate: summed power, the
  power-weighted centroid `(x, y)`, the power-weighted second moments about the centroid
  (`Qxx`, `Qxy`, `Qyy`) and a radius `r` that no emitter of the cell is farther than from the
  centroid. A cell with at most 4 emitters, or at the finest of 24 levels, is a leaf; a cell
  whose emitters all fall in one child is not stored, so there are fewer than `2n` cells.
- Each 8 x 8 block of map cells walks the tree once, depth first. A grid cell is **far** when
  the distance from the block's box of cell centres to its emitter disc (the centroid's distance
  minus `r`) is positive, at least `d_min` and at least `theta * 2r`, and the disc is wholly
  within the cutoff of every centre in the box (the farthest distance to the centroid plus `r`
  is at most the cutoff). It then counts as one source and its children are skipped. A grid cell
  whose nearest point is beyond the cutoff is skipped. Otherwise its children are visited, and a
  leaf that is not far adds its emitters exactly. The test uses the whole block, so every map
  cell meets it, and some near cells that a per-cell test would approximate stay exact.
- Emitters are summed per map cell exactly as in the exact mode, cutoff test included. A far
  grid cell adds `P / d^2 - (Qxx + Qyy) / d^4 + 4 (dx^2 Qxx + 2 dx dy Qxy + dy^2 Qyy) / d^6`,
  with `d = (dx, dy)` from the map cell centre to its centroid: the expansion of the exact sum to
  second order (the first-order term is zero about the centroid). Since no far cell straddles
  the cutoff circle, or comes within `d_min` of a centre, neither the cutoff nor `d_min` changes
  its terms.
- **Error.** The only error is the truncated expansion. Each far cell is off by at most a fixed
  fraction of its own exact contribution, set by `theta` alone (the centroid is at least
  `(2 theta + 1) r` away), so a map cell is off by at most that fraction of its exact value. A
  random search over layouts of 2 to 4 emitters puts that fraction at about 7% for `theta = 1`,
  1.3% for `theta = 2` and 0.2% for `theta = 4`. In the tested layouts (clustered, spread,
  mixed, a pair of emitters on a grid cell's diagonal, and a block of emitters straddling the
  cutoff), the worst cell at `theta = 2` is 0.67% off its exact value (the pair; 0.08% for the
  others), 2.0% at `theta = 1` and 0.03% at `theta = 4`. The tests assert, at `theta >= 2`,
  that every cell is within 5% of its exact value (and so within the larger of 5% of the cell
  and 1% of the map's maximum).
- **Cost bound:** per 8 x 8 block, the grid cells visited (a few dozen per tree level, growing
  with `theta^2`) plus the emitters of the leaves that are not far, then `64 x` that many pair
  evaluations, plus an `O(n log n)` sort per build. Those leaves include every leaf in a band
  around the block's cutoff circle about 10 cell widths (the diagonal of the block's centres)
  plus a leaf wide, since a grid cell is only aggregated when it is wholly inside the cutoff of
  every centre. So the cost does not grow with `cells x
  emitters` while the emitters are well inside the cutoff (the 128 x 128 clustered benchmark
  takes ~6.3 ms), but it does grow with `cells x emitters near the cutoff circle`: at 512 x 512
  the circle of a third of the blocks crosses the cluster, and the build takes ~140 ms (exact:
  ~1.06 s). When the cutoff is only a few block widths, little gets aggregated and the far field
  can be about 10% slower than exact (the 500 m benchmark); use `EXACT` there.
- Deterministic: a fixed sort, traversal and summation order, and the same inputs give the same
  bits. Exact and far-field results differ, so callers should not mix them in one comparison.

Both modes:

- An invalid spec gives an empty map: a non-positive or non-finite cell size or cutoff, no cells,
  or more than `HeatmapSpec::MAX_CELLS` (4096 x 4096).
- All buffers are reused. Rebuilding does not allocate once the emitter count and spec are
  stable (tested with a counting allocator in both modes).
- Size the cutoff and the grid for the expected emitter density, and build on demand rather
  than every tick.

## Floating origin

`to_local_f32(world, origin) = (world - origin) as f32`. The subtraction is in `f64`, so frames are
precise relative to the command node rather than the world's zero. `Sim::frame` uses it for
positions. Tested with origins up to 10,000 km and offsets up to 100 km: the error stays under
1 cm (the worst seen is about 3.9 mm, half an `f32` ulp at 100 km).
