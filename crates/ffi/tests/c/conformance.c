/* Header conformance test: compiled against include/simcore.h, linked with libsimcore_ffi.a.
   Calls every function in the header and checks results. Exit code 0 means pass. */
#include "simcore.h"

#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

/* Layout as the header defines it must match the Rust #[repr(C)] asserts in crates/abi. */
_Static_assert(sizeof(ScUnitView) == 40, "ScUnitView size");
_Static_assert(_Alignof(ScUnitView) == 8, "ScUnitView align");
_Static_assert(offsetof(ScUnitView, x) == 8, "x");
_Static_assert(offsetof(ScUnitView, vx) == 20, "vx");
_Static_assert(offsetof(ScUnitView, kind) == 32, "kind");
_Static_assert(offsetof(ScUnitView, affiliation) == 34, "affiliation");
_Static_assert(offsetof(ScUnitView, flags) == 35, "flags");
_Static_assert(offsetof(ScUnitView, ems_power_w) == 36, "ems_power_w");
_Static_assert(sizeof(ScFrame) == 24, "ScFrame size");
_Static_assert(sizeof(ScCommand) == 40, "ScCommand size");
_Static_assert(_Alignof(ScCommand) == 8, "ScCommand align");
_Static_assert(offsetof(ScCommand, target_id) == 8, "target_id");
_Static_assert(offsetof(ScCommand, x) == 16, "command x");
_Static_assert(offsetof(ScCommand, speed_mps) == 28, "speed_mps");
_Static_assert(offsetof(ScCommand, type) == 32, "type");
_Static_assert(offsetof(ScCommand, radiate) == 34, "radiate");
_Static_assert(offsetof(ScCommand, _reserved1) == 36, "_reserved1");
_Static_assert(sizeof(ScStatus) == 48, "ScStatus size");
_Static_assert(offsetof(ScStatus, unit_count) == 8, "unit_count");
_Static_assert(offsetof(ScStatus, awaiting_cleanup) == 40, "awaiting_cleanup");
_Static_assert(sizeof(ScBytes) == 16, "ScBytes size");

/* Every function, through a pointer of exactly the header's type. */
static uint32_t (*const p_abi_version)(void) = sc_abi_version;
static ScWorld *(*const p_create)(uint64_t) = sc_world_create;
static ScWorld *(*const p_create_dummy)(uint64_t, uint32_t) = sc_world_create_dummy;
static void (*const p_destroy)(ScWorld *) = sc_world_destroy;
static void (*const p_step)(ScWorld *, uint32_t) = sc_world_step;
static uint64_t (*const p_tick)(const ScWorld *) = sc_world_tick;
static ScFrame (*const p_frame)(ScWorld *) = sc_world_frame;
static uint64_t (*const p_command)(ScWorld *, const ScCommand *) = sc_world_command;
static uint32_t (*const p_link_delay)(ScWorld *, uint64_t) = sc_world_link_delay;
static ScStatus (*const p_status)(ScWorld *) = sc_world_status;
static void (*const p_despawn_removed)(ScWorld *) = sc_world_despawn_removed;
static ScBytes (*const p_save)(ScWorld *) = sc_world_save;
static ScWorld *(*const p_restore)(const uint8_t *, uint64_t) = sc_world_restore;

static int failures = 0;
#define CHECK(cond)                                                                \
    do {                                                                           \
        if (!(cond)) {                                                             \
            fprintf(stderr, "FAIL %s:%d: %s\n", __FILE__, __LINE__, #cond);        \
            failures++;                                                            \
        }                                                                          \
    } while (0)

static unsigned command_nodes(ScFrame f, const ScUnitView **out) {
    unsigned n = 0;
    for (uint32_t i = 0; i < f.count; i++) {
        if (f.units[i].flags & SC_FLAG_COMMAND_NODE) {
            n++;
            *out = &f.units[i];
        }
    }
    return n;
}

int main(void) {
    CHECK(p_abi_version() == SC_ABI_VERSION);

    /* NULL handles return safe defaults. */
    p_destroy(NULL);
    p_step(NULL, 5);
    CHECK(p_tick(NULL) == 0);
    ScFrame nf = p_frame(NULL);
    CHECK(nf.units == NULL && nf.count == 0 && nf.tick == 0);

    /* Unrepresentable frame size is refused, not UB. */
    CHECK(p_create_dummy(1, UINT32_MAX) == NULL);

    /* Empty world: only the command node, at the origin. */
    ScWorld *w = p_create(42);
    CHECK(w != NULL);
    CHECK(p_tick(w) == 0);
    ScFrame f = p_frame(w);
    CHECK(f.units != NULL && f.count == 1 && f.tick == 0);
    const ScUnitView *cmd = NULL;
    CHECK(command_nodes(f, &cmd) == 1);
    CHECK(cmd && cmd->x == 0.0f && cmd->y == 0.0f && cmd->z == 0.0f);
    CHECK(cmd && cmd->affiliation == SC_AFFILIATION_FRIENDLY);
    p_step(w, SC_TICKS_PER_SECOND);
    CHECK(p_tick(w) == SC_TICKS_PER_SECOND);
    CHECK(p_frame(w).tick == SC_TICKS_PER_SECOND);
    p_step(w, 0);
    CHECK(p_tick(w) == SC_TICKS_PER_SECOND);
    p_destroy(w);

    /* Dummy worlds: count, flags, affiliations, determinism. */
    const uint32_t n = 10000;
    ScWorld *a = p_create_dummy(7, n);
    ScWorld *b = p_create_dummy(7, n);
    CHECK(a != NULL && b != NULL);
    p_step(a, 300);
    p_step(b, 300);
    CHECK(p_tick(a) == 300 && p_tick(b) == 300);

    ScFrame fa = p_frame(a);
    ScFrame fb = p_frame(b);
    CHECK(fa.count == n + 1 && fb.count == n + 1);
    CHECK(fa.tick == 300);
    CHECK(fa._reserved == 0);
    CHECK(command_nodes(fa, &cmd) == 1);
    CHECK(cmd && cmd->x == 0.0f && cmd->y == 0.0f && cmd->z == 0.0f);
    CHECK(memcmp(fa.units, fb.units, (size_t)fa.count * sizeof(ScUnitView)) == 0);

    unsigned emitting = 0, bad_aff = 0, bad_power = 0;
    double sum = 0.0;
    for (uint32_t i = 0; i < fa.count; i++) {
        const ScUnitView *u = &fa.units[i];
        if (u->affiliation > SC_AFFILIATION_NEUTRAL) bad_aff++;
        if (u->flags & SC_FLAG_EMITTING) {
            emitting++;
            if (!(u->ems_power_w > 0.0f)) bad_power++;
        } else if (u->ems_power_w != 0.0f) {
            bad_power++;
        }
        sum += (double)u->x + u->y + u->z;
    }
    CHECK(bad_aff == 0);
    CHECK(bad_power == 0);
    CHECK(emitting > 0 && emitting < n);

    /* Copy out while valid; the copy stays usable after the world moves on. */
    ScUnitView *copy = malloc((size_t)fa.count * sizeof(ScUnitView));
    CHECK(copy != NULL);
    memcpy(copy, fa.units, (size_t)fa.count * sizeof(ScUnitView));

    /* Different seed gives a different world. */
    ScWorld *c = p_create_dummy(8, n);
    p_step(c, 300);
    ScFrame fc = p_frame(c);
    CHECK(fc.count == n + 1);
    CHECK(memcmp(copy, fc.units, (size_t)fc.count * sizeof(ScUnitView)) != 0);

    /* Frames advance and the buffer is reused (same address once the count is stable). */
    p_step(a, 1);
    ScFrame fa2 = p_frame(a);
    CHECK(fa2.tick == 301 && fa2.count == n + 1);
    CHECK(fa2.units == fa.units);
    CHECK(memcmp(copy, fa2.units, (size_t)fa2.count * sizeof(ScUnitView)) != 0);

    free(copy);
    p_destroy(a);
    p_destroy(b);
    p_destroy(c);

    /* Revision 1.2: NULL inputs. */
    ScCommand none = {0};
    CHECK(p_command(NULL, &none) == 0);
    CHECK(p_link_delay(NULL, 1) == UINT32_MAX);
    ScStatus ns = p_status(NULL);
    CHECK(ns.tick == 0 && ns.unit_count == 0);
    p_despawn_removed(NULL);
    ScBytes nb = p_save(NULL);
    CHECK(nb.data == NULL && nb.len == 0);
    CHECK(p_restore(NULL, 0) == NULL);

    /* A command to the command node, a status, a save and a restore. */
    ScWorld *d = p_create_dummy(5, 1000);
    CHECK(p_link_delay(d, 1) == 0);
    CHECK(p_link_delay(d, 1u << 20) == UINT32_MAX);
    ScCommand mv = {.unit_id = 1, .x = 15.0f, .speed_mps = 15.0f, .type = SC_COMMAND_MOVE};
    CHECK(p_command(d, NULL) == 0);
    CHECK(p_command(d, &mv) == 1);
    ScCommand bad = mv;
    bad._reserved0 = 1;
    CHECK(p_command(d, &bad) == 0);
    ScStatus st = p_status(d);
    CHECK(st.unit_count == 1001 && st.pending_commands == 1 && st._reserved == 0);

    ScBytes saved = p_save(d);
    CHECK(saved.data != NULL && saved.len > 0);
    uint8_t *bytes = malloc((size_t)saved.len);
    CHECK(bytes != NULL);
    memcpy(bytes, saved.data, (size_t)saved.len);
    ScWorld *e = p_restore(bytes, saved.len);
    CHECK(e != NULL);
    bytes[saved.len / 2] ^= 1;
    CHECK(p_restore(bytes, saved.len) == NULL);
    CHECK(p_restore(bytes, 3) == NULL);
    free(bytes);

    /* The command node moves 15 m in 15 ticks; the frame stays centred on it. */
    p_step(d, 20);
    p_step(e, 20);
    ScStatus sd = p_status(d), se = p_status(e);
    CHECK(sd.tick == 20 && sd.pending_commands == 0);
    CHECK(memcmp(&sd, &se, sizeof sd) == 0);
    CHECK(p_frame(d).count == p_frame(e).count);
    p_despawn_removed(d);
    p_destroy(d);
    p_destroy(e);

    if (failures) {
        fprintf(stderr, "conformance: %d failure(s)\n", failures);
        return 1;
    }
    printf("conformance: ok (checksum %.3f)\n", sum);
    return 0;
}
