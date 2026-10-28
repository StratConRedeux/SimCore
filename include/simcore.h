/* SimCore C API, header revision 1.1 (ABI version 1). Must match crates/abi/src/lib.rs byte for byte. */
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

/* Read-only view of the latest frame. `units` is engine-owned and valid until the
   next call that mutates the world. 24 bytes. */
typedef struct ScFrame {
    const ScUnitView *SC_NULLABLE units; /* NULL when count is 0 or the call failed. */
    uint32_t count;
    uint32_t _reserved;
    uint64_t tick;
} ScFrame;

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

#if defined(__clang__)
#pragma clang assume_nonnull end
#pragma clang diagnostic pop
#endif

#ifdef __cplusplus
}
#endif

#endif /* SIMCORE_H */
