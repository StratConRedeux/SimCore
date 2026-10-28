//! Phase 2 gameplay rules: observation, exposure, detection, pursuit, engagement and the signal
//! heatmap. The maths lives in `simcore_rules`; this module wires it into the ECS.
//!
//! Every tuning number comes from the caller: spawn specs, the [`Rules`] resource and the
//! [`Terrain`] resource.

use bevy_ecs::prelude::*;
use simcore_abi::affiliation;
use simcore_rules::{
    Kinematic, PursuitParams, Vec3, closest_approach, detection_range, in_detection_range,
    pursuit_velocity, signal_strength_sq,
};

use crate::grid::SpatialGrid;
use crate::{
    Affiliation, CommandNode, DT, Detected, EmsProfile, Exposure, Gone, Pcg32, Position, Pursuer,
    Sensor, SensorParams, StableId, Terrain, UnitKind, Velocity,
};

/// Success chance per (pursuer outcome class, target kind), with a default for missing pairs.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OutcomeTable {
    /// Chance in `[0, 1]` for pairs not in the table.
    pub default_chance: f64,
    /// Sorted by `(class, kind)`, unique.
    entries: Vec<(u16, u16, f64)>,
}

impl OutcomeTable {
    /// An empty table where every pair has `default_chance`.
    pub fn new(default_chance: f64) -> OutcomeTable {
        OutcomeTable {
            default_chance,
            entries: Vec::new(),
        }
    }

    /// Sets the chance for `(class, kind)`, replacing any previous value.
    pub fn set(&mut self, class: u16, kind: u16, chance: f64) {
        match self
            .entries
            .binary_search_by_key(&(class, kind), |e| (e.0, e.1))
        {
            Ok(i) => self.entries[i].2 = chance,
            Err(i) => self.entries.insert(i, (class, kind, chance)),
        }
    }

    /// Builder form of [`OutcomeTable::set`].
    pub fn with(mut self, class: u16, kind: u16, chance: f64) -> OutcomeTable {
        self.set(class, kind, chance);
        self
    }

    /// The success chance for `(class, kind)`, clamped to `[0, 1]`; NaN counts as 0.
    pub fn chance(&self, class: u16, kind: u16) -> f64 {
        let c = match self
            .entries
            .binary_search_by_key(&(class, kind), |e| (e.0, e.1))
        {
            Ok(i) => self.entries[i].2,
            Err(_) => self.default_chance,
        };
        if c.is_nan() { 0.0 } else { c.clamp(0.0, 1.0) }
    }
}

/// Caller-supplied gameplay rules. The default is inert: nothing is ever observed and every
/// engagement fails.
#[derive(Resource, Clone, Debug, PartialEq)]
pub struct Rules {
    /// Hard cap on any detection range, in m. Also sizes the observation grid's cells. Zero,
    /// negative or non-finite disables observation.
    pub detection_max_range_m: f64,
    /// Parameters for sensors that do not carry their own.
    pub sensor: SensorParams,
    /// Exposure ticks at which a unit becomes detected. 0 is treated as 1.
    pub exposure_threshold_ticks: u32,
    /// Exposure never exceeds this (nor drops below the threshold as a cap).
    pub exposure_cap_ticks: u32,
    /// Exposure lost per tick while not observed.
    pub exposure_decay_per_tick: u32,
    pub outcomes: OutcomeTable,
}

impl Default for Rules {
    fn default() -> Rules {
        Rules {
            detection_max_range_m: 0.0,
            sensor: SensorParams::default(),
            exposure_threshold_ticks: 1,
            exposure_cap_ticks: 1,
            exposure_decay_per_tick: 1,
            outcomes: OutcomeTable::new(0.0),
        }
    }
}

impl Rules {
    #[inline]
    fn threshold(&self) -> u32 {
        self.exposure_threshold_ticks.max(1)
    }

    #[inline]
    fn cap(&self) -> u32 {
        self.exposure_cap_ticks.max(self.threshold())
    }

    #[inline]
    fn max_range(&self) -> f64 {
        let r = self.detection_max_range_m;
        if r > 0.0 && r.is_finite() { r } else { 0.0 }
    }
}

/// Whether two affiliations are opposing sides: friendly against hostile. Neutral and unknown
/// units neither observe nor are observed.
#[inline]
pub fn opposing(a: u8, b: u8) -> bool {
    matches!(
        (a, b),
        (affiliation::FRIENDLY, affiliation::HOSTILE)
            | (affiliation::HOSTILE, affiliation::FRIENDLY)
    )
}

/// A sensor as the observation rule sees it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SensorView {
    /// World position in m.
    pub position: [f64; 3],
    pub affiliation: u8,
    pub params: SensorParams,
}

/// An emitter as the observation rule sees it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EmitterView {
    /// World position in m.
    pub position: [f64; 3],
    pub affiliation: u8,
    /// Currently radiated power in W.
    pub power_w: f64,
}

/// The observation rule for one sensor-emitter pair: opposing sides, the emitter radiating,
/// within the sensor's detection range for that power (capped by the rules), and in line of
/// sight. The engine's grid applies exactly this test to its candidates.
pub fn sensor_observes(s: &SensorView, e: &EmitterView, rules: &Rules, terrain: &Terrain) -> bool {
    if e.power_w.is_nan() || e.power_w <= 0.0 || !opposing(s.affiliation, e.affiliation) {
        return false;
    }
    let range = detection_range(
        s.params.base_range_m,
        e.power_w,
        s.params.reference_power_w,
        rules.max_range(),
    );
    let [sx, sy, sz] = s.position;
    let [ex, ey, ez] = e.position;
    range > 0.0
        && in_detection_range(Vec3::new(sx, sy, sz), Vec3::new(ex, ey, ez), range)
        && terrain.line_of_sight(s.position, e.position)
}

/// Maps stable ids to live entities. Ids are allocated sequentially from 1 and never reused.
#[derive(Resource, Debug, Default)]
pub(crate) struct IdIndex {
    entities: Vec<Option<Entity>>,
    live: u32,
}

impl IdIndex {
    pub fn insert(&mut self, id: StableId, e: Entity) {
        let i = (id.0 - 1) as usize;
        if self.entities.len() <= i {
            self.entities.resize(i + 1, None);
        }
        self.entities[i] = Some(e);
        self.live += 1;
    }

    pub fn get(&self, id: StableId) -> Option<Entity> {
        let i = usize::try_from(id.0.checked_sub(1)?).ok()?;
        self.entities.get(i).copied().flatten()
    }

    pub fn remove(&mut self, id: StableId) {
        if let Some(slot) =
            id.0.checked_sub(1)
                .and_then(|i| self.entities.get_mut(i as usize))
            && slot.take().is_some()
        {
            self.live -= 1;
        }
    }

    pub fn live(&self) -> u32 {
        self.live
    }
}

/// Sensors of one side, in a grid.
#[derive(Debug, Default)]
struct SideSensors {
    views: Vec<SensorView>,
    grid: SpatialGrid,
    /// Largest `base_range / sqrt(reference_power)` among them: an emitter of power `P` cannot
    /// be heard beyond `k_max * sqrt(P)`, which bounds the grid query.
    k_max: f64,
}

impl SideSensors {
    fn clear(&mut self) {
        self.views.clear();
        self.k_max = 0.0;
    }

    fn push(&mut self, view: SensorView) {
        let k = view.params.base_range_m / view.params.reference_power_w.sqrt();
        if k > self.k_max {
            self.k_max = k;
        }
        self.views.push(view);
    }

    /// Whether any sensor here observes `e`, stopping at the first that does.
    fn observes(&self, e: &EmitterView, rules: &Rules, terrain: &Terrain) -> bool {
        if self.views.is_empty() {
            return false;
        }
        // Slightly widened, so rounding in the per-pair range can never exceed the query.
        let reach = (self.k_max * e.power_w.sqrt() * (1.0 + 1e-9)).min(rules.max_range());
        if reach.is_nan() || reach <= 0.0 {
            return false;
        }
        let mut seen = false;
        self.grid
            .for_each_near(e.position[0], e.position[1], reach, |i| {
                seen = sensor_observes(&self.views[i as usize], e, rules, terrain);
                !seen
            });
        seen
    }
}

/// Reused per-tick buffers and bookkeeping for the systems below.
#[derive(Resource, Debug, Default)]
pub(crate) struct Scratch {
    friendly: SideSensors,
    hostile: SideSensors,
    steer: Vec<(Entity, Velocity)>,
    /// Units observed this tick, and units with non-zero exposure after the last update. While
    /// both are zero (and the rules are unchanged) exposure and detection have nothing to do.
    observed: u32,
    exposed: u32,
}

/// Marks every unit observed this tick by at least one opposing sensor.
///
/// Each side's sensors go into a uniform grid with cells of half the max detection range, and
/// each emitter only tests the opposing side's sensors in cells within its reach, stopping at
/// the first sensor that observes it. Neutral and unknown emitters are never observed, so they
/// skip the query.
pub(crate) fn observe(
    rules: Res<Rules>,
    terrain: Res<Terrain>,
    mut scratch: ResMut<Scratch>,
    sensors: Query<(&Position, &Affiliation, &Sensor, &Gone)>,
    mut units: Query<(&Position, &Affiliation, &EmsProfile, &Gone, &mut Exposure)>,
) {
    let scratch = &mut *scratch;
    let max_range = rules.max_range();
    scratch.friendly.clear();
    scratch.hostile.clear();
    if max_range > 0.0 {
        for (p, a, s, gone) in &sensors {
            if gone.0 {
                continue;
            }
            let side = match a.0 {
                affiliation::FRIENDLY => &mut scratch.friendly,
                affiliation::HOSTILE => &mut scratch.hostile,
                _ => continue,
            };
            side.push(SensorView {
                position: [p.x, p.y, p.z],
                affiliation: a.0,
                params: s.params.unwrap_or(rules.sensor),
            });
        }
    }
    let no_sensors = scratch.friendly.views.is_empty() && scratch.hostile.views.is_empty();
    if no_sensors && scratch.observed == 0 {
        // Nothing was observed last tick and nothing can be now: every flag is already false.
        return;
    }
    for side in [&mut scratch.friendly, &mut scratch.hostile] {
        let views = &side.views;
        side.grid.build(views.len(), max_range / 2.0, |i| {
            (views[i].position[0], views[i].position[1])
        });
    }
    let mut observed = 0;
    for (p, a, ems, gone, mut exposure) in &mut units {
        if gone.0 {
            continue;
        }
        let power_w = f64::from(ems.current_power_w());
        let opposing = match a.0 {
            affiliation::FRIENDLY => Some(&scratch.hostile),
            affiliation::HOSTILE => Some(&scratch.friendly),
            _ => None,
        };
        let seen = power_w > 0.0
            && opposing.is_some_and(|opposing| {
                opposing.observes(
                    &EmitterView {
                        position: [p.x, p.y, p.z],
                        affiliation: a.0,
                        power_w,
                    },
                    &rules,
                    &terrain,
                )
            });
        observed += u32::from(seen);
        if exposure.observed != seen {
            exposure.observed = seen;
        }
    }
    scratch.observed = observed;
}

/// Exposure +1 (capped) while observed; otherwise it decays by the rules value.
pub(crate) fn update_exposure(
    rules: Res<Rules>,
    mut scratch: ResMut<Scratch>,
    mut q: Query<&mut Exposure>,
) {
    if scratch.observed == 0 && scratch.exposed == 0 {
        return;
    }
    let (cap, decay) = (rules.cap(), rules.exposure_decay_per_tick);
    let mut exposed = 0;
    for mut e in &mut q {
        let ticks = if e.observed {
            e.ticks.saturating_add(1).min(cap)
        } else {
            e.ticks.saturating_sub(decay)
        };
        exposed += u32::from(ticks > 0);
        if e.ticks != ticks {
            e.ticks = ticks;
        }
    }
    scratch.exposed = exposed;
}

/// Detected while exposure is at or above the threshold.
pub(crate) fn update_detected(
    rules: Res<Rules>,
    scratch: Res<Scratch>,
    mut q: Query<(&Exposure, &mut Detected)>,
    mut was_exposed: Local<bool>,
) {
    // With no exposure now or last tick, every flag is already false (the threshold is >= 1).
    let exposed = scratch.exposed > 0;
    if !exposed && !*was_exposed && !rules.is_changed() {
        return;
    }
    *was_exposed = exposed;
    let threshold = rules.threshold();
    for (e, mut d) in &mut q {
        let detected = e.ticks >= threshold;
        if d.0 != detected {
            d.0 = detected;
        }
    }
}

/// Turns each pursuer toward its target. All pursuers read the same pre-tick state, so the
/// order of pursuers does not matter. A pursuer whose target is gone keeps its velocity.
#[allow(clippy::type_complexity)]
pub(crate) fn steer_pursuers(
    index: Res<IdIndex>,
    mut scratch: ResMut<Scratch>,
    pursuers: Query<(Entity, &Pursuer, &Gone)>,
    // Every unit's kinematics (read), then pursuers' velocities (write).
    mut kin: ParamSet<(
        Query<(&Position, &Velocity)>,
        Query<&mut Velocity, With<Pursuer>>,
    )>,
) {
    let out = &mut scratch.steer;
    out.clear();
    {
        let kin = kin.p0();
        for (e, p, gone) in &pursuers {
            if gone.0 {
                continue;
            }
            let Some(target) = index.get(p.target).filter(|&t| t != e) else {
                continue;
            };
            let (Ok((pp, pv)), Ok((tp, tv))) = (kin.get(e), kin.get(target)) else {
                continue;
            };
            let v = pursuit_velocity(
                kinematic(pp, pv),
                kinematic(tp, tv),
                PursuitParams {
                    speed: p.speed,
                    max_turn_rate: p.max_turn_rate,
                    lead: p.lead,
                },
                DT,
            );
            out.push((
                e,
                Velocity {
                    x: v.x,
                    y: v.y,
                    z: v.z,
                },
            ));
        }
    }
    let mut vel = kin.p1();
    for &(e, v) in out.iter() {
        if let Ok(mut current) = vel.get_mut(e)
            && *current != v
        {
            *current = v;
        }
    }
}

#[inline]
fn kinematic(p: &Position, v: &Velocity) -> Kinematic {
    Kinematic {
        position: Vec3::new(p.x, p.y, p.z),
        velocity: Vec3::new(v.x, v.y, v.z),
    }
}

/// One resolved engagement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EngagementEvent {
    /// [`crate::Sim::tick`] once the engagement's tick completed.
    pub tick: u64,
    pub pursuer: StableId,
    pub target: StableId,
    /// The roll succeeded. The pursuer is removed either way.
    pub success: bool,
    /// The target was removed: a success against anything but the command node, which is
    /// never removed.
    pub target_removed: bool,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Hit {
    pursuer_entity: Entity,
    pursuer: StableId,
    target_entity: Entity,
    target: StableId,
    chance: f64,
}

type PursuerQuery = (
    Entity,
    &'static StableId,
    &'static Pursuer,
    &'static Position,
    &'static Velocity,
    &'static Gone,
);
type TargetQuery = (&'static Position, &'static Velocity, &'static UnitKind);

/// Cached queries, buffers and the seeded PRNG for engagement resolution.
pub(crate) struct Engagements {
    pursuers: QueryState<PursuerQuery>,
    targets: QueryState<TargetQuery>,
    hits: Vec<Hit>,
    pub events: Vec<EngagementEvent>,
    /// Units removed since the last [`Engagements::despawn_removed`], still in the world.
    removed: Vec<Entity>,
    rng: Pcg32,
    /// Live pursuers: an upper bound on hits and events in one `step` call, and half of one on
    /// removals between two spawns.
    live_pursuers: usize,
}

impl Engagements {
    pub fn new(world: &mut World, seed: u64) -> Engagements {
        Engagements {
            pursuers: world.query(),
            targets: world.query(),
            hits: Vec::new(),
            events: Vec::new(),
            removed: Vec::new(),
            rng: Pcg32::new(seed, 0x45_4e_47_41_47_45), // "ENGAGE"
            live_pursuers: 0,
        }
    }

    /// Despawns the units removed since the last call, then makes room for one more pursuer.
    ///
    /// Called before every pursuer spawn. Each live pursuer engages at most once, removing at
    /// most two units, so no buffer grows during `step` until the next spawn.
    pub fn add_pursuer(&mut self, world: &mut World) {
        self.despawn_removed(world);
        self.live_pursuers += 1;
        let n = self.live_pursuers;
        self.hits.reserve(n.saturating_sub(self.hits.len()));
        self.events.reserve(n.saturating_sub(self.events.len()));
        self.removed.reserve(2 * n);
    }

    /// Despawns units that engagements removed. Their stable ids are already gone.
    pub fn despawn_removed(&mut self, world: &mut World) {
        for e in self.removed.drain(..) {
            world.despawn(e);
        }
    }

    /// Last step of a tick, after movement: every pursuer whose swept approach to its target
    /// over the tick came within its engagement radius rolls once against the outcome table.
    ///
    /// Hits are gathered in query order, then resolved in that order. A hit is skipped if an
    /// earlier one this tick already removed its pursuer or its target. The command node is
    /// never removed.
    ///
    /// Removal takes the unit out of the id index and marks it [`Gone`]; the entity itself is
    /// despawned later, outside `step`, so resolution does not allocate.
    pub fn resolve(&mut self, world: &mut World, tick: u64) {
        self.hits.clear();
        {
            let world: &World = world;
            let index = world.resource::<IdIndex>();
            let rules = world.resource::<Rules>();
            for (e, id, p, pos, vel, gone) in self.pursuers.iter(world) {
                if gone.0 {
                    continue;
                }
                let Some(te) = index.get(p.target).filter(|&t| t != e) else {
                    continue;
                };
                let Ok((tp, tv, kind)) = self.targets.get(world, te) else {
                    continue;
                };
                // Positions are post-movement: sweep back over the tick just simulated.
                let (a, b) = (kinematic(pos, vel), kinematic(tp, tv));
                let approach = closest_approach(
                    a.position - a.velocity * DT,
                    a.velocity,
                    b.position - b.velocity * DT,
                    b.velocity,
                    DT,
                );
                if approach.distance <= p.engagement_radius_m {
                    self.hits.push(Hit {
                        pursuer_entity: e,
                        pursuer: *id,
                        target_entity: te,
                        target: p.target,
                        chance: rules.outcomes.chance(p.outcome_class, kind.0),
                    });
                }
            }
        }
        for k in 0..self.hits.len() {
            let h = self.hits[k];
            let index = world.resource::<IdIndex>();
            if index.get(h.pursuer) != Some(h.pursuer_entity)
                || index.get(h.target) != Some(h.target_entity)
            {
                continue;
            }
            let success = self.rng.next_f64() < h.chance;
            let target_removed =
                success && !world.entity(h.target_entity).contains::<CommandNode>();
            if target_removed {
                self.remove(world, h.target, h.target_entity);
            }
            self.remove(world, h.pursuer, h.pursuer_entity);
            self.events.push(EngagementEvent {
                tick,
                pursuer: h.pursuer,
                target: h.target,
                success,
                target_removed,
            });
        }
    }

    /// Takes a unit out of play: out of the id index, marked [`Gone`] (which observation,
    /// steering, engagement, the frame and the heatmap skip), stopped so movement skips it,
    /// with its exposure cleared, and queued for despawning.
    fn remove(&mut self, world: &mut World, id: StableId, e: Entity) {
        world.resource_mut::<IdIndex>().remove(id);
        let mut u = world.entity_mut(e);
        if u.contains::<Pursuer>() {
            self.live_pursuers -= 1;
        }
        // In-place writes only: inserting would look up archetype edges, which can allocate.
        if let Some(mut g) = u.get_mut::<Gone>() {
            g.0 = true;
        }
        if let Some(mut v) = u.get_mut::<Velocity>() {
            *v = Velocity::default();
        }
        if let Some(mut x) = u.get_mut::<Exposure>() {
            *x = Exposure::default();
        }
        if let Some(mut d) = u.get_mut::<Detected>() {
            d.0 = false;
        }
        if let Some(mut ems) = u.get_mut::<EmsProfile>() {
            ems.emitting = false;
        }
        self.removed.push(e);
    }
}

/// Size and falloff of a signal heatmap. All values are data.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HeatmapSpec {
    /// Cell edge in m.
    pub cell_size_m: f64,
    /// Cells east-west and north-south, centred on the command node.
    pub cells_x: u32,
    pub cells_y: u32,
    /// Minimum distance in the falloff, in m (see [`simcore_rules::signal_strength`]).
    pub d_min_m: f64,
    /// Emitters farther than this from a cell centre (horizontally, in m) are ignored.
    pub cutoff_m: f64,
}

impl HeatmapSpec {
    /// Most cells a heatmap may have (4096 x 4096).
    pub const MAX_CELLS: u64 = 1 << 24;

    /// Centre of cell `(i, j)` relative to the command node, in m.
    fn cell_center(&self, i: u32, j: u32) -> (f64, f64) {
        (
            (f64::from(i) + 0.5 - f64::from(self.cells_x) / 2.0) * self.cell_size_m,
            (f64::from(j) + 0.5 - f64::from(self.cells_y) / 2.0) * self.cell_size_m,
        )
    }

    fn is_valid(&self) -> bool {
        let cells = u64::from(self.cells_x) * u64::from(self.cells_y);
        self.cell_size_m > 0.0
            && self.cell_size_m.is_finite()
            && self.cutoff_m > 0.0
            && self.cutoff_m.is_finite()
            && cells > 0
            && cells <= Self::MAX_CELLS
    }
}

/// Signal strength per cell around the command node, from units of one affiliation.
///
/// Cell `(i, j)` is `values()[j * cells_x + i]`; `i` grows east, `j` north. Each cell holds the
/// sum of `power / max(d, d_min)^2` (W/m^2, unnormalised) over currently emitting units within
/// the cutoff, where `d` is the horizontal distance from the cell centre.
#[derive(Debug, Default)]
pub struct Heatmap {
    spec: Option<HeatmapSpec>,
    origin: Position,
    values: Vec<f32>,
    emitters: Vec<(f64, f64, f64)>,
    grid: SpatialGrid,
    /// Candidates for the current block of cells, relative to the block centre.
    near: Candidates,
}

/// Emitters near one block of cells, as columns padded to a multiple of [`LANES`].
#[derive(Debug, Default)]
struct Candidates {
    x: Vec<f64>,
    y: Vec<f64>,
    power: Vec<f64>,
}

/// Heatmap cells are evaluated in blocks of `BLOCK x BLOCK` sharing one candidate list.
const BLOCK: u32 = 8;
/// Independent partial sums per cell, so the inner loop can use SIMD. A fixed count and order
/// keep the result deterministic.
const LANES: usize = 4;

impl Heatmap {
    /// The spec of the last build, or `None` if it was invalid (the map is then empty).
    pub fn spec(&self) -> Option<HeatmapSpec> {
        self.spec
    }

    /// Row-major cell strengths; empty when the spec was invalid.
    pub fn values(&self) -> &[f32] {
        &self.values
    }

    /// Strength of cell `(i, j)`.
    pub fn get(&self, i: u32, j: u32) -> Option<f32> {
        let s = self.spec?;
        (i < s.cells_x && j < s.cells_y).then(|| self.values[(j * s.cells_x + i) as usize])
    }

    /// Centre of cell `(i, j)` in m relative to the command node (x east, y north).
    pub fn cell_center(&self, i: u32, j: u32) -> Option<(f64, f64)> {
        self.spec.map(|s| s.cell_center(i, j))
    }

    /// World position of the command node when the map was built.
    pub fn origin(&self) -> Position {
        self.origin
    }

    /// Reused buffers' capacity, for allocation checks.
    pub fn capacity(&self) -> usize {
        self.values.capacity()
    }

    pub(crate) fn build<'a>(
        &mut self,
        spec: HeatmapSpec,
        origin: Position,
        units: impl Iterator<Item = (&'a Position, &'a Affiliation, &'a EmsProfile, &'a Gone)>,
        side: u8,
    ) {
        self.values.clear();
        self.emitters.clear();
        self.origin = origin;
        if !spec.is_valid() {
            self.spec = None;
            return;
        }
        self.spec = Some(spec);
        // Only emitters within the cutoff of the map's area can contribute.
        let half_x = f64::from(spec.cells_x) * spec.cell_size_m / 2.0 + spec.cutoff_m;
        let half_y = f64::from(spec.cells_y) * spec.cell_size_m / 2.0 + spec.cutoff_m;
        for (p, a, ems, gone) in units {
            let power = f64::from(ems.current_power_w());
            if !gone.0
                && a.0 == side
                && power > 0.0
                && (p.x - origin.x).abs() <= half_x
                && (p.y - origin.y).abs() <= half_y
            {
                self.emitters.push((p.x, p.y, power));
            }
        }
        let em = &self.emitters;
        self.grid
            .build(em.len(), spec.cutoff_m / 2.0, |i| (em[i].0, em[i].1));
        self.values
            .resize((spec.cells_x * spec.cells_y) as usize, 0.0);
        if em.is_empty() {
            return;
        }
        let cutoff2 = spec.cutoff_m * spec.cutoff_m;
        let block_half_diag =
            f64::from(BLOCK) * spec.cell_size_m * core::f64::consts::FRAC_1_SQRT_2;
        let reach = spec.cutoff_m + block_half_diag;
        for bj in (0..spec.cells_y).step_by(BLOCK as usize) {
            for bi in (0..spec.cells_x).step_by(BLOCK as usize) {
                // Block centre, in world metres.
                let (c0x, c0y) = spec.cell_center(bi, bj);
                let off = (f64::from(BLOCK) - 1.0) / 2.0 * spec.cell_size_m;
                let (bx, by) = (origin.x + c0x + off, origin.y + c0y + off);
                // Every emitter within the cutoff of any cell in the block, relative to it.
                let near = &mut self.near;
                near.x.clear();
                near.y.clear();
                near.power.clear();
                self.grid.for_each_near(bx, by, reach, |k| {
                    let (ex, ey, power) = em[k as usize];
                    let (dx, dy) = (ex - bx, ey - by);
                    if dx * dx + dy * dy <= reach * reach {
                        near.x.push(dx);
                        near.y.push(dy);
                        near.power.push(power);
                    }
                    true
                });
                if near.x.is_empty() {
                    continue;
                }
                // Pad with zero-power entries beyond the cutoff.
                while !near.x.len().is_multiple_of(LANES) {
                    near.x.push(2.0 * reach);
                    near.y.push(0.0);
                    near.power.push(0.0);
                }
                for j in bj..(bj + BLOCK).min(spec.cells_y) {
                    for i in bi..(bi + BLOCK).min(spec.cells_x) {
                        let (cx, cy) = spec.cell_center(i, j);
                        let (px, py) = (origin.x + cx - bx, origin.y + cy - by);
                        let mut acc = [0.0f64; LANES];
                        let lanes = near
                            .x
                            .as_chunks::<LANES>()
                            .0
                            .iter()
                            .zip(near.y.as_chunks::<LANES>().0.iter())
                            .zip(near.power.as_chunks::<LANES>().0.iter());
                        for ((xs, ys), ps) in lanes {
                            for l in 0..LANES {
                                let (dx, dy) = (xs[l] - px, ys[l] - py);
                                let d2 = dx * dx + dy * dy;
                                let s = signal_strength_sq(ps[l], d2, spec.d_min_m);
                                acc[l] += if d2 <= cutoff2 { s } else { 0.0 };
                            }
                        }
                        let sum = (acc[0] + acc[1]) + (acc[2] + acc[3]);
                        self.values[(j * spec.cells_x + i) as usize] = sum as f32;
                    }
                }
            }
        }
    }
}
