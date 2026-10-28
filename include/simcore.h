/* SimCore C API, header revision 1.3 (ABI version 1). Must match crates/abi/src/lib.rs byte for byte. */
#ifndef SIMCORE_H
#define SIMCORE_H

#include <stdbool.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Nullability for Swift and clang. Other compilers see a plain C header. */
#if defined(__clang__)
#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wnullability-extension"
#pragma clang assume_nonnull begin
#define SC_NULLABLE _Nullable
#else
#define SC_NULLABLE
#endif

/* Typed constants, so Swift imports them with the field types they are compared against. */
static const uint32_t SC_ABI_VERSION = 1;
static const uint32_t SC_TICKS_PER_SECOND = 15;

static const uint8_t SC_AFFILIATION_UNKNOWN = 0;
static const uint8_t SC_AFFILIATION_FRIENDLY = 1;
static const uint8_t SC_AFFILIATION_HOSTILE = 2;
static const uint8_t SC_AFFILIATION_NEUTRAL = 3;

/* The unit is radiating (radar, datalink, jammer, weapon guidance). */
static const uint8_t SC_FLAG_EMITTING = 1u << 0;
/* The unit is the player's command node, which is the frame's origin. */
static const uint8_t SC_FLAG_COMMAND_NODE = 1u << 1;
/* The unit is currently detected by the opposing side's sensors. */
static const uint8_t SC_FLAG_DETECTED = 1u << 2;

/* One unit. Position is f32 metres relative to the player's command node; velocity is f32
   m/s in the world frame (not relative to the node's motion). Local east-north-up (x east,
   y north, z up). 40 bytes. */
typedef struct ScUnitView {
    uint64_t id;
    float x, y, z;
    float vx, vy, vz;
    uint16_t kind;
    uint8_t affiliation;
    uint8_t flags;
    float ems_power_w;
} ScUnitView;

/* Read-only view of the latest frame. `units` is engine-owned and valid until the next
   sc_world_frame or sc_world_destroy on the same world. Unit order is unspecified (it changes
   after a restore): identify units by id. 24 bytes. */
typedef struct ScFrame {
    const ScUnitView *SC_NULLABLE units; /* NULL when count is 0 or the call failed. */
    uint32_t count;
    uint32_t _reserved;
    uint64_t tick;
} ScFrame;

/* Command types (ScCommand.type). */
/* Travel straight to (x, y, z) at speed_mps, then stop. Replaces any earlier move. Not for
   pursuers. */
static const uint16_t SC_COMMAND_MOVE = 1;
/* Cancel any move and stop. Not for pursuers. */
static const uint16_t SC_COMMAND_HALT = 2;
/* A pursuer pursues target_id instead. Pursuers only. */
static const uint16_t SC_COMMAND_RETARGET = 3;
/* radiate 1: follow the duty cycle (or emit); 0: stay silent. */
static const uint16_t SC_COMMAND_EMISSIONS = 4;

/* One command to a friendly unit, applied after the link delay. Positions are f32 metres
   relative to the command node when the command is issued (the frame's coordinates). Fields
   a type does not use are ignored; reserved fields must be zero. 40 bytes. */
typedef struct ScCommand {
    uint64_t unit_id;
    uint64_t target_id; /* RETARGET */
    float x, y, z;      /* MOVE: destination */
    float speed_mps;    /* MOVE: positive */
    uint16_t type;      /* SC_COMMAND_* */
    uint8_t radiate;    /* EMISSIONS: 0 or 1 */
    uint8_t _reserved0;
    uint32_t _reserved1;
} ScCommand;

/* A summary for the HUD. Counts are of live units. 48 bytes. */
typedef struct ScStatus {
    uint64_t tick;
    uint32_t unit_count;         /* including the command node */
    uint32_t link_delay_ticks;   /* a command to the command node issued now: base + jamming there */
    uint32_t friendly_emitting;  /* friendly units radiating now */
    uint32_t friendly_detected;  /* friendly units the hostile side has detected */
    uint32_t hostile_detected;   /* hostile units the friendly side has detected */
    uint32_t pending_commands;   /* issued, not yet delivered */
    uint32_t dropped_commands;   /* dropped at delivery: the unit was gone (saturating) */
    uint32_t command_node_hits;  /* successful engagements against the command node (saturating) */
    uint32_t awaiting_cleanup;   /* removed units whose storage sc_world_despawn_removed releases */
    uint32_t _reserved;
} ScStatus;

/* Engine-owned bytes. 16 bytes. */
typedef struct ScBytes {
    const uint8_t *SC_NULLABLE data; /* NULL when len is 0 or the call failed. */
    uint64_t len;
} ScBytes;

/* Revision 1.3: game setup. Every double below must be finite; distances, powers, rates and
   chances must also be non-negative, and chances at most 1. Reserved fields must be zero. */

/* Gameplay rules. The outcome table's entries are passed separately. 48 bytes. */
typedef struct ScRules {
    double detection_max_range_m;    /* cap on any detection range; 0 disables observation */
    double sensor_base_range_m;      /* shared sensor parameters (see ScSensor) */
    double sensor_reference_power_w;
    double default_chance;           /* for (class, kind) pairs not in the table */
    uint32_t exposure_threshold_ticks; /* detected at this exposure; 0 is treated as 1 */
    uint32_t exposure_cap_ticks;
    uint32_t exposure_decay_per_tick;
    uint32_t _reserved;
} ScRules;

/* A pursuer of outcome_class engaging a target of target_kind succeeds with chance. 16 bytes. */
typedef struct ScOutcome {
    uint16_t outcome_class;
    uint16_t target_kind;
    uint32_t _reserved;
    double chance;
} ScOutcome;

/* Link delay = base + ceil(per_km * km from the command node) + jam delay if an emitting
   hostile jammer is within jam_radius_m, capped at max_delay_ticks. 32 bytes. */
typedef struct ScLinkRules {
    double delay_ticks_per_km;
    double jam_radius_m;
    uint32_t base_delay_ticks;
    uint32_t jam_delay_ticks;
    uint32_t max_delay_ticks;
    uint32_t _reserved;
} ScLinkRules;

/* A heightfield of nx * ny samples spacing_m apart, row-major: sample (i, j) is at
   (origin_x + i * spacing_m, origin_y + j * spacing_m), metres relative to the command node
   when the call is made. 40 bytes. */
typedef struct ScTerrain {
    double origin_x, origin_y;
    double spacing_m;      /* positive */
    uint32_t nx, ny;       /* at least 1 each */
    uint32_t los_samples;  /* per line-of-sight check, at most 1024 */
    uint32_t _reserved;
} ScTerrain;

/* Spawn options (ScUnitSpec.options). */
static const uint8_t SC_SPAWN_EMITTING = 1u << 0; /* radiates continuously; not with CYCLE */
static const uint8_t SC_SPAWN_CYCLE = 1u << 1;    /* radiates on the cycle_* duty cycle */
static const uint8_t SC_SPAWN_PURSUER = 1u << 2;  /* target_id, outcome_class, pursuit fields */
static const uint8_t SC_SPAWN_SENSOR = 1u << 3;   /* passive sensor, shared rules parameters */

/* One unit to spawn. Position is metres relative to the command node when the call is made;
   velocity is m/s in the world frame, as in ScUnitView. A pursuer's velocity is only its initial heading. Fields an option does
   not use are ignored. 112 bytes. */
typedef struct ScUnitSpec {
    double x, y, z;
    double vx, vy, vz;
    float erp_w;          /* radiated power while emitting, W */
    uint16_t kind;        /* any type code except the command node's */
    uint8_t affiliation;  /* SC_AFFILIATION_* */
    uint8_t options;      /* SC_SPAWN_* */
    uint32_t cycle_on_ticks, cycle_off_ticks, cycle_phase; /* CYCLE */
    uint16_t outcome_class; /* PURSUER: outcome table row */
    uint16_t _reserved;
    uint64_t target_id;     /* PURSUER: an existing unit */
    double speed_mps;       /* PURSUER: positive */
    double max_turn_rate_rad_s; /* PURSUER */
    double lead;            /* PURSUER: 0 (pure pursuit) to 1 */
    double engagement_radius_m; /* PURSUER */
} ScUnitSpec;

/* Passive sensor: an emitter of reference_power_w is heard out to base_range_m, scaling with
   the square root of power. 16 bytes. */
typedef struct ScSensor {
    double base_range_m;
    double reference_power_w;
} ScSensor;

/* Opaque world handle. All calls on one handle must come from one thread at a time. */
typedef struct ScWorld ScWorld;

uint32_t sc_abi_version(void);

/* Returns NULL on failure. Same seed + same inputs => same simulation. */
ScWorld *SC_NULLABLE sc_world_create(uint64_t seed);

/* Creates a world populated with `unit_count` dummy units (benchmarks and UI work).
   Returns NULL on failure, including unit_count == UINT32_MAX (the units plus the
   command node would not fit in ScFrame.count). */
ScWorld *SC_NULLABLE sc_world_create_dummy(uint64_t seed, uint32_t unit_count);

/* Safe to call with NULL. */
void sc_world_destroy(ScWorld *SC_NULLABLE world);

/* Advances `ticks` fixed steps of 1/SC_TICKS_PER_SECOND seconds. */
void sc_world_step(ScWorld *world, uint32_t ticks);

uint64_t sc_world_tick(const ScWorld *world);

/* Builds the frame relative to the command node and returns a view into it. */
ScFrame sc_world_frame(ScWorld *world);

/* Queues a command for delivery after the link delay. Returns its id (from 1), or 0 if it is
   rejected: unknown or non-friendly unit, wrong unit type, invalid values or reserved fields.
   A command issued at tick t with delay d is applied at the start of the tick that begins at
   t + d. If its unit is gone by then it is dropped (ScStatus.dropped_commands). */
uint64_t sc_world_command(ScWorld *world, const ScCommand *command);

/* Ticks a command to `unit_id` issued now would take to arrive, at most UINT32_MAX - 1.
   UINT32_MAX if the unit does not exist. */
uint32_t sc_world_link_delay(ScWorld *world, uint64_t unit_id);

ScStatus sc_world_status(ScWorld *world);

/* Releases the storage of units removed by engagements. They already left the frame; call
   this at natural pauses (sc_world_save does it too). */
void sc_world_despawn_removed(ScWorld *world);

/* Saves the whole world as opaque, versioned, checksummed bytes. `data` is engine-owned and
   valid until the next sc_world_save or sc_world_destroy on the same world. Also releases
   removed units' storage. */
ScBytes sc_world_save(ScWorld *world);

/* A world restored from sc_world_save bytes. It continues exactly as the saved world would
   have. NULL if the bytes are not a snapshot this build reads, or are damaged. */
ScWorld *SC_NULLABLE sc_world_restore(const uint8_t *SC_NULLABLE data, uint64_t len);

/* Game setup (revision 1.3). Each call checks all its input and changes nothing if it returns
   false or 0. They may be called at any time between steps, and are saved with the world. */

/* Replaces the rules. `outcomes` may be NULL only when outcome_count is 0; each (class, kind)
   pair may appear once. */
bool sc_world_set_rules(ScWorld *world, const ScRules *rules,
                        const ScOutcome *SC_NULLABLE outcomes, uint32_t outcome_count);

/* Replaces the link-delay rules for commands issued from now on. */
bool sc_world_set_link_rules(ScWorld *world, const ScLinkRules *rules);

/* Replaces the terrain. NULL `terrain` means flat (everything in line of sight); otherwise
   `heights` holds height_count == nx * ny finite heights in metres, copied. */
bool sc_world_set_terrain(ScWorld *world, const ScTerrain *SC_NULLABLE terrain,
                          const float *SC_NULLABLE heights, uint64_t height_count);

/* Spawns a unit and returns its id (ids are never reused), or 0 if the spec is rejected or
   ids up to 2^24 - 1 are used up. */
uint64_t sc_world_spawn(ScWorld *world, const ScUnitSpec *spec);

/* Makes a live unit (the command node included) a passive sensor, replacing any previous one.
   NULL `sensor` uses the shared rules' parameters. False for an unknown unit or bad values. */
bool sc_world_add_sensor(ScWorld *world, uint64_t unit_id, const ScSensor *SC_NULLABLE sensor);

#if defined(__clang__)
#pragma clang assume_nonnull end
#pragma clang diagnostic pop
#endif

#ifdef __cplusplus
}
#endif

#endif /* SIMCORE_H */
