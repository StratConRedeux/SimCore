//! Save and restore: a versioned, checksummed, little-endian binary snapshot of the whole world.
//!
//! The app treats a snapshot as opaque bytes. A restored world continues exactly as the saved
//! one would have: same frames, events and rolls. Layout (all integers little-endian):
//!
//! ```text
//! header:  b"SCSN"  format_version u32  payload_len u64  fnv1a64(payload) u64
//! payload: clock, ids, counters, PRNG; Rules; LinkRules; Terrain; units; moves; pending commands
//! ```
//!
//! Units are written in stable id order, so the bytes are canonical: saving a restored world
//! gives the same bytes. The ECS iterates a restored world in a different order (bevy matches
//! archetypes through a hash map, so its order depends on the world's history); the simulation
//! does not depend on that order. Removed units are despawned before saving and never written.

use bevy_ecs::prelude::*;

use crate::gameplay::{IdIndex, Scratch};
use crate::orders::{Move, Moves, Pending};
use crate::{
    Affiliation, CommandNode, Detected, Emcon, EmitterCycle, EmsProfile, Exposure, Gone, LinkRules,
    Order, OutcomeTable, Position, Pursuer, Rules, Sensor, SensorParams, Sim, SimClock, StableId,
    Terrain, UnitKind, Velocity,
};

/// Bumped whenever the payload layout changes. Older formats are rejected, not migrated, until
/// a release needs to read them.
pub const SNAPSHOT_FORMAT_VERSION: u32 = 1;

const MAGIC: &[u8; 4] = b"SCSN";
const HEADER_LEN: usize = 4 + 4 + 8 + 8;
/// Stable ids are below this. The id index is 8 bytes per id up to the largest, so this caps
/// what a snapshot can make the engine allocate at 128 MiB (16.7 million units ever spawned).
const MAX_ID: u64 = crate::MAX_UNIT_ID + 1;
/// Ticks and command ids are below this, so no later arithmetic on them can overflow.
const MAX_COUNTER: u64 = 1 << 62;

/// Why [`Sim::restore`] rejected a snapshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnapshotError {
    /// Too short, wrong magic or wrong length: not a snapshot.
    NotASnapshot,
    /// Written by a format this build does not read.
    UnsupportedVersion(u32),
    /// The checksum does not match: the bytes were damaged.
    Corrupt,
    /// Well-formed bytes describing an impossible world (named field).
    Invalid(&'static str),
}

type SaveQuery = (
    &'static StableId,
    &'static UnitKind,
    &'static Affiliation,
    &'static Position,
    &'static Velocity,
    &'static EmsProfile,
    &'static Exposure,
    &'static Detected,
    &'static Emcon,
    Has<CommandNode>,
    Option<&'static EmitterCycle>,
    Option<&'static Pursuer>,
    Option<&'static Sensor>,
);

// Per-unit flag bits.
const U_COMMAND_NODE: u8 = 1 << 0;
const U_CYCLE: u8 = 1 << 1;
const U_PURSUER: u8 = 1 << 2;
const U_SENSOR: u8 = 1 << 3;
const U_SENSOR_PARAMS: u8 = 1 << 4;
const U_EMITTING: u8 = 1 << 5;
const U_DETECTED: u8 = 1 << 6;
const U_OBSERVED: u8 = 1 << 7;

// Order tags.
const O_MOVE: u8 = 1;
const O_HALT: u8 = 2;
const O_RETARGET: u8 = 3;
const O_EMISSIONS: u8 = 4;

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

struct W<'a>(&'a mut Vec<u8>);

impl W<'_> {
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn f32(&mut self, v: f32) {
        self.u32(v.to_bits());
    }
    fn f64(&mut self, v: f64) {
        self.u64(v.to_bits());
    }
    fn pos(&mut self, x: f64, y: f64, z: f64) {
        self.f64(x);
        self.f64(y);
        self.f64(z);
    }
    fn len(&mut self, n: usize) {
        // Every list here is bounded by u32 (unit counts, terrain samples, queue lengths).
        self.u32(u32::try_from(n).expect("snapshot list longer than u32::MAX"));
    }
}

struct R<'a>(&'a [u8]);

type Parsed<T> = Result<T, SnapshotError>;

impl<'a> R<'a> {
    fn take(&mut self, n: usize) -> Parsed<&'a [u8]> {
        if self.0.len() < n {
            return Err(SnapshotError::Invalid("truncated payload"));
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Ok(a)
    }
    fn arr<const N: usize>(&mut self) -> Parsed<[u8; N]> {
        Ok(self.take(N)?.try_into().expect("length checked"))
    }
    fn u8(&mut self) -> Parsed<u8> {
        Ok(self.arr::<1>()?[0])
    }
    fn bool(&mut self, field: &'static str) -> Parsed<bool> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(SnapshotError::Invalid(field)),
        }
    }
    fn u16(&mut self) -> Parsed<u16> {
        Ok(u16::from_le_bytes(self.arr()?))
    }
    fn u32(&mut self) -> Parsed<u32> {
        Ok(u32::from_le_bytes(self.arr()?))
    }
    fn u64(&mut self) -> Parsed<u64> {
        Ok(u64::from_le_bytes(self.arr()?))
    }
    fn f32(&mut self) -> Parsed<f32> {
        Ok(f32::from_bits(self.u32()?))
    }
    fn f64(&mut self) -> Parsed<f64> {
        Ok(f64::from_bits(self.u64()?))
    }
    fn pos(&mut self) -> Parsed<Position> {
        Ok(Position {
            x: self.f64()?,
            y: self.f64()?,
            z: self.f64()?,
        })
    }
    /// A list length, checked against the bytes left so a damaged count cannot make us
    /// allocate more than the snapshot could hold.
    fn len(&mut self, min_item_bytes: usize, field: &'static str) -> Parsed<usize> {
        let n = self.u32()? as usize;
        if n.saturating_mul(min_item_bytes) > self.0.len() {
            return Err(SnapshotError::Invalid(field));
        }
        Ok(n)
    }
}

const UNIT_MIN_BYTES: usize = 8 + 2 + 1 + 1 + 48 + 4 + 4 + 1;

impl Sim {
    /// Writes a snapshot of the whole world and returns it. The buffer is reused across calls.
    ///
    /// Despawns removed units first ([`Sim::despawn_removed`]); otherwise it does not change
    /// the simulation.
    pub fn save(&mut self) -> &[u8] {
        self.despawn_removed();
        let mut buf = std::mem::take(&mut self.snapshot);
        buf.clear();
        buf.resize(HEADER_LEN, 0);
        self.write_payload(&mut W(&mut buf));
        let payload_len = (buf.len() - HEADER_LEN) as u64;
        let sum = fnv1a64(&buf[HEADER_LEN..]);
        buf[0..4].copy_from_slice(MAGIC);
        buf[4..8].copy_from_slice(&SNAPSHOT_FORMAT_VERSION.to_le_bytes());
        buf[8..16].copy_from_slice(&payload_len.to_le_bytes());
        buf[16..24].copy_from_slice(&sum.to_le_bytes());
        self.snapshot = buf;
        &self.snapshot
    }

    fn write_payload(&mut self, w: &mut W) {
        let (rng, command_node_hits) = self.engagements.saved_state();
        w.u64(self.tick());
        w.u64(self.next_id);
        w.u64(self.commands.last_seq);
        w.u32(self.commands.dropped);
        w.u32(command_node_hits);
        w.u64(rng.0);
        w.u64(rng.1);

        let r = self.world.resource::<Rules>();
        w.f64(r.detection_max_range_m);
        w.f64(r.sensor.base_range_m);
        w.f64(r.sensor.reference_power_w);
        w.u32(r.exposure_threshold_ticks);
        w.u32(r.exposure_cap_ticks);
        w.u32(r.exposure_decay_per_tick);
        w.f64(r.outcomes.default_chance);
        w.len(r.outcomes.entries().len());
        for &(class, kind, chance) in r.outcomes.entries() {
            w.u16(class);
            w.u16(kind);
            w.f64(chance);
        }

        let l = *self.world.resource::<LinkRules>();
        w.u32(l.base_delay_ticks);
        w.f64(l.delay_ticks_per_km);
        w.f64(l.jam_radius_m);
        w.u32(l.jam_delay_ticks);
        w.u32(l.max_delay_ticks);

        match self.world.resource::<Terrain>().parts() {
            None => w.u8(0),
            Some((ox, oy, spacing, nx, ny, heights, los)) => {
                w.u8(1);
                w.f64(ox);
                w.f64(oy);
                w.f64(spacing);
                w.u32(nx);
                w.u32(ny);
                w.u32(los);
                w.len(heights.len());
                for &h in heights {
                    w.f32(h);
                }
            }
        }

        let mut q = self.world.query::<SaveQuery>();
        let mut units: Vec<_> = q.iter(&self.world).collect();
        units.sort_unstable_by_key(|u| *u.0);
        w.len(units.len());
        for (id, kind, aff, p, v, ems, exp, det, emcon, cmd, cycle, pursuer, sensor) in units {
            let params = sensor.and_then(|s| s.params);
            let mut bits = 0;
            for (on, bit) in [
                (cmd, U_COMMAND_NODE),
                (cycle.is_some(), U_CYCLE),
                (pursuer.is_some(), U_PURSUER),
                (sensor.is_some(), U_SENSOR),
                (params.is_some(), U_SENSOR_PARAMS),
                (ems.emitting, U_EMITTING),
                (det.0, U_DETECTED),
                (exp.observed, U_OBSERVED),
            ] {
                if on {
                    bits |= bit;
                }
            }
            w.u64(id.0);
            w.u16(kind.0);
            w.u8(aff.0);
            w.u8(bits);
            w.pos(p.x, p.y, p.z);
            w.pos(v.x, v.y, v.z);
            w.f32(ems.erp_w);
            w.u32(exp.ticks);
            w.u8(u8::from(emcon.silent));
            if let Some(c) = cycle {
                w.u32(c.on_ticks);
                w.u32(c.off_ticks);
                w.u32(c.phase);
            }
            if let Some(p) = pursuer {
                w.u64(p.target.0);
                w.f64(p.speed);
                w.f64(p.max_turn_rate);
                w.f64(p.lead);
                w.f64(p.engagement_radius_m);
                w.u16(p.outcome_class);
            }
            if let Some(s) = params {
                w.f64(s.base_range_m);
                w.f64(s.reference_power_w);
            }
        }

        let moves = &self.world.resource::<Moves>().0;
        w.len(moves.len());
        for m in moves {
            w.u64(m.unit.0);
            w.pos(m.destination.x, m.destination.y, m.destination.z);
            w.f64(m.speed_mps);
            w.u8(u8::from(m.arriving));
        }

        w.len(self.commands.pending.len());
        for p in &self.commands.pending {
            w.u64(p.deliver_tick);
            w.u64(p.seq);
            w.u64(p.unit.0);
            match p.order {
                Order::Move {
                    destination: d,
                    speed_mps,
                } => {
                    w.u8(O_MOVE);
                    w.pos(d.x, d.y, d.z);
                    w.f64(speed_mps);
                }
                Order::Halt => w.u8(O_HALT),
                Order::Retarget { target } => {
                    w.u8(O_RETARGET);
                    w.u64(target.0);
                }
                Order::Emissions { radiate } => {
                    w.u8(O_EMISSIONS);
                    w.u8(u8::from(radiate));
                }
            }
        }
    }

    /// Rebuilds a world from [`Sim::save`] output. It continues exactly as the saved world
    /// would have. Rejects anything this build did not write, damaged bytes, and impossible
    /// worlds; it never panics on bad input.
    pub fn restore(bytes: &[u8]) -> Result<Sim, SnapshotError> {
        if bytes.len() < HEADER_LEN || &bytes[0..4] != MAGIC {
            return Err(SnapshotError::NotASnapshot);
        }
        let mut h = R(&bytes[4..HEADER_LEN]);
        let version = h.u32()?;
        let (payload_len, sum) = (h.u64()?, h.u64()?);
        if version != SNAPSHOT_FORMAT_VERSION {
            return Err(SnapshotError::UnsupportedVersion(version));
        }
        let payload = &bytes[HEADER_LEN..];
        if payload.len() as u64 != payload_len {
            return Err(SnapshotError::NotASnapshot);
        }
        if fnv1a64(payload) != sum {
            return Err(SnapshotError::Corrupt);
        }
        let mut r = R(payload);
        let sim = read_payload(&mut r)?;
        if !r.0.is_empty() {
            return Err(SnapshotError::Invalid("trailing bytes"));
        }
        Ok(sim)
    }
}

/// No command is delivered more than this many ticks after the saved tick (link delays are
/// `u32`).
const MAX_DELAY_TICKS: u64 = u32::MAX as u64;

/// Whether a saved move or queued command could have been accepted: the checks of
/// `orders::validate` that still hold after issue. Its unit may be gone (the order is then
/// dropped on the next tick, as in the saved world), and so may a retarget's target.
fn consistent(sim: &Sim, unit: StableId, order: &Order) -> bool {
    let values_ok = match *order {
        Order::Move {
            destination: d,
            speed_mps,
        } => {
            d.x.is_finite()
                && d.y.is_finite()
                && d.z.is_finite()
                && speed_mps > 0.0
                && speed_mps.is_finite()
        }
        Order::Retarget { target } => target != unit,
        Order::Halt | Order::Emissions { .. } => true,
    };
    let Some(e) = sim.entity(unit) else {
        return values_ok;
    };
    let u = sim.world.entity(e);
    let friendly = u.get::<Affiliation>().map(|a| a.0) == Some(simcore_abi::affiliation::FRIENDLY);
    let pursuer = u.contains::<Pursuer>();
    let type_ok = match order {
        Order::Move { .. } | Order::Halt => !pursuer,
        Order::Retarget { .. } => pursuer,
        Order::Emissions { .. } => true,
    };
    values_ok && friendly && type_ok
}

fn read_payload(r: &mut R) -> Parsed<Sim> {
    // The seed is replaced by the saved PRNG state below.
    let mut sim = Sim::without_units(0);
    let tick = r.u64()?;
    let next_id = r.u64()?;
    let last_seq = r.u64()?;
    let dropped = r.u32()?;
    let command_node_hits = r.u32()?;
    let rng = (r.u64()?, r.u64()?);
    if !(2..=MAX_ID).contains(&next_id) {
        return Err(SnapshotError::Invalid("next id"));
    }
    if tick > MAX_COUNTER || last_seq > MAX_COUNTER {
        return Err(SnapshotError::Invalid("tick or command id"));
    }
    sim.world.resource_mut::<SimClock>().tick = tick;
    sim.next_id = next_id;
    sim.commands.last_seq = last_seq;
    sim.commands.dropped = dropped;
    sim.engagements.restore_state(rng, command_node_hits);

    let mut rules = Rules {
        detection_max_range_m: r.f64()?,
        sensor: SensorParams {
            base_range_m: r.f64()?,
            reference_power_w: r.f64()?,
        },
        exposure_threshold_ticks: r.u32()?,
        exposure_cap_ticks: r.u32()?,
        exposure_decay_per_tick: r.u32()?,
        outcomes: OutcomeTable::new(r.f64()?),
    };
    let mut last = None;
    for _ in 0..r.len(12, "outcome entries")? {
        let key = (r.u16()?, r.u16()?);
        if last.is_some_and(|l| l >= key) {
            return Err(SnapshotError::Invalid("outcome entries"));
        }
        last = Some(key);
        rules.outcomes.set(key.0, key.1, r.f64()?);
    }
    sim.set_rules(rules);

    sim.set_link_rules(LinkRules {
        base_delay_ticks: r.u32()?,
        delay_ticks_per_km: r.f64()?,
        jam_radius_m: r.f64()?,
        jam_delay_ticks: r.u32()?,
        max_delay_ticks: r.u32()?,
    });

    match r.u8()? {
        0 => {}
        1 => {
            let (ox, oy, spacing) = (r.f64()?, r.f64()?, r.f64()?);
            let (nx, ny, los) = (r.u32()?, r.u32()?, r.u32()?);
            let n = r.len(4, "terrain heights")?;
            let mut heights = Vec::with_capacity(n);
            for _ in 0..n {
                heights.push(r.f32()?);
            }
            let t = Terrain::heightfield(ox, oy, spacing, nx, ny, heights, los)
                .ok_or(SnapshotError::Invalid("terrain"))?;
            sim.set_terrain(t);
        }
        _ => return Err(SnapshotError::Invalid("terrain kind")),
    }

    let (mut observed, mut exposed) = (0, 0);
    let mut last_id = 1;
    let mut command_node = None;
    for _ in 0..r.len(UNIT_MIN_BYTES, "unit count")? {
        let id = StableId(r.u64()?);
        let kind = UnitKind(r.u16()?);
        let affiliation = Affiliation(r.u8()?);
        let bits = r.u8()?;
        let position = r.pos()?;
        let velocity = Velocity {
            x: r.f64()?,
            y: r.f64()?,
            z: r.f64()?,
        };
        let erp_w = r.f32()?;
        let ticks = r.u32()?;
        let silent = r.bool("emission control")?;
        let cycle = (bits & U_CYCLE != 0)
            .then(|| -> Parsed<_> {
                Ok(EmitterCycle {
                    on_ticks: r.u32()?,
                    off_ticks: r.u32()?,
                    phase: r.u32()?,
                })
            })
            .transpose()?;
        let pursuer = (bits & U_PURSUER != 0)
            .then(|| -> Parsed<_> {
                Ok(Pursuer {
                    target: StableId(r.u64()?),
                    speed: r.f64()?,
                    max_turn_rate: r.f64()?,
                    lead: r.f64()?,
                    engagement_radius_m: r.f64()?,
                    outcome_class: r.u16()?,
                })
            })
            .transpose()?;
        let params = (bits & U_SENSOR_PARAMS != 0)
            .then(|| -> Parsed<_> {
                Ok(SensorParams {
                    base_range_m: r.f64()?,
                    reference_power_w: r.f64()?,
                })
            })
            .transpose()?;
        let is_cmd = bits & U_COMMAND_NODE != 0;
        if bits & U_SENSOR == 0 && params.is_some() {
            return Err(SnapshotError::Invalid("sensor parameters without a sensor"));
        }
        // Strictly increasing ids, as `save` writes them: unique, and in range.
        if !(last_id..next_id).contains(&id.0) {
            return Err(SnapshotError::Invalid("unit id"));
        }
        if is_cmd != (id.0 == 1) || (is_cmd && pursuer.is_some()) {
            return Err(SnapshotError::Invalid("command node"));
        }
        let exposure = Exposure {
            ticks,
            observed: bits & U_OBSERVED != 0,
        };
        observed += u32::from(exposure.observed);
        exposed += u32::from(ticks > 0);

        let base = (
            id,
            kind,
            affiliation,
            position,
            velocity,
            EmsProfile {
                emitting: bits & U_EMITTING != 0,
                erp_w,
            },
            exposure,
            Detected(bits & U_DETECTED != 0),
            Gone::default(),
            Emcon { silent },
        );
        let e = if is_cmd {
            sim.world.spawn((base, CommandNode)).id()
        } else if let Some(p) = pursuer {
            sim.engagements.add_pursuer(&mut sim.world);
            sim.world.spawn((base, p)).id()
        } else {
            sim.world.spawn(base).id()
        };
        if let Some(c) = cycle {
            sim.world.entity_mut(e).insert(c);
        }
        if bits & U_SENSOR != 0 {
            sim.world.entity_mut(e).insert(Sensor { params });
        }
        sim.world.resource_mut::<IdIndex>().insert(id, e);
        last_id = id.0 + 1;
        if is_cmd {
            command_node = Some(e);
        }
    }
    sim.command_node = command_node.ok_or(SnapshotError::Invalid("no command node"))?;
    sim.world
        .resource_mut::<Scratch>()
        .set_counts(observed, exposed);

    let n = r.len(8 + 24 + 8 + 1, "move count")?;
    let mut moves = Vec::with_capacity(n);
    // Marks units with a move, to reject duplicates in linear time. Bounded by `MAX_ID`.
    let mut moving = if n > 0 {
        vec![false; next_id as usize]
    } else {
        Vec::new()
    };
    for _ in 0..n {
        let unit = StableId(r.u64()?);
        let destination = r.pos()?;
        let speed_mps = r.f64()?;
        let arriving = r.bool("move arriving")?;
        // A unit removed on the saved tick still has its move; the next tick drops it, as it
        // would have in the saved world.
        if !(1..next_id).contains(&unit.0) || std::mem::replace(&mut moving[unit.0 as usize], true)
        {
            return Err(SnapshotError::Invalid("move unit"));
        }
        let order = Order::Move {
            destination,
            speed_mps,
        };
        if !consistent(&sim, unit, &order) {
            return Err(SnapshotError::Invalid("move"));
        }
        moves.push(Move {
            unit,
            destination,
            speed_mps,
            arriving,
        });
    }
    sim.world.resource_mut::<Moves>().0 = moves;

    let n = r.len(8 + 8 + 8 + 1, "pending command count")?;
    let mut pending: Vec<Pending> = Vec::with_capacity(n);
    for _ in 0..n {
        let (deliver_tick, seq, unit) = (r.u64()?, r.u64()?, StableId(r.u64()?));
        let order = match r.u8()? {
            O_MOVE => Order::Move {
                destination: r.pos()?,
                speed_mps: r.f64()?,
            },
            O_HALT => Order::Halt,
            O_RETARGET => Order::Retarget {
                target: StableId(r.u64()?),
            },
            O_EMISSIONS => Order::Emissions {
                radiate: r.bool("emissions order")?,
            },
            _ => return Err(SnapshotError::Invalid("order type")),
        };
        // Strictly descending by (deliver_tick, seq), as the queue keeps them.
        let key = (deliver_tick, seq);
        let in_order = pending.last().is_none_or(|q| (q.deliver_tick, q.seq) > key);
        let due = (tick..=tick + MAX_DELAY_TICKS).contains(&deliver_tick);
        if !in_order || !due || seq == 0 || seq > last_seq || !consistent(&sim, unit, &order) {
            return Err(SnapshotError::Invalid("pending commands"));
        }
        pending.push(Pending {
            deliver_tick,
            seq,
            unit,
            order,
        });
    }
    sim.commands.pending = pending;
    Ok(sim)
}
