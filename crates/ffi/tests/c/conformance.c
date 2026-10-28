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

/* Every function, through a pointer of exactly the header's type. */
static uint32_t (*const p_abi_version)(void) = sc_abi_version;
static ScWorld *(*const p_create)(uint64_t) = sc_world_create;
static ScWorld *(*const p_create_dummy)(uint64_t, uint32_t) = sc_world_create_dummy;
static void (*const p_destroy)(ScWorld *) = sc_world_destroy;
static void (*const p_step)(ScWorld *, uint32_t) = sc_world_step;
static uint64_t (*const p_tick)(const ScWorld *) = sc_world_tick;
static ScFrame (*const p_frame)(ScWorld *) = sc_world_frame;

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

    if (failures) {
        fprintf(stderr, "conformance: %d failure(s)\n", failures);
        return 1;
    }
    printf("conformance: ok (checksum %.3f)\n", sum);
    return 0;
}
