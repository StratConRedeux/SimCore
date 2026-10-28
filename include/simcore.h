/* SimCore C API, header revision 1.2 (ABI version 1). Must match crates/abi/src/lib.rs byte for byte. */
#ifndef SIMCORE_H
#define SIMCORE_H

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

/* One unit. Position and velocity are f32 metres relative to the player's
   command node, local east-north-up (x east, y north, z up). 40 bytes. */
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

#if defined(__clang__)
#pragma clang assume_nonnull end
#pragma clang diagnostic pop
#endif

#ifdef __cplusplus
}
#endif

#endif /* SIMCORE_H */
