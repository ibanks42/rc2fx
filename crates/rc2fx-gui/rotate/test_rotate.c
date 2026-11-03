/* Tests and benchmark for rotate.c.
 *   test_rotate            bit-exact checks against the scalar reference
 *   test_rotate bench      full 1280×720 frame and a few dirty rectangles
 * Host: cc -O2; aarch64: zig cc -target aarch64-linux-musl, run under
 * qemu-aarch64 (timing there is functional only).
 */
#include "rotate.h"

#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#define SW 1280
#define SH 720

static uint64_t rng = 0x243f6a8885a308d3ull;
static uint32_t next(void) {
    rng ^= rng << 13, rng ^= rng >> 7, rng ^= rng << 17;
    return (uint32_t)(rng >> 16);
}
static double now(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return t.tv_sec + t.tv_nsec * 1e-9;
}

/* Rotates r both ways and compares with the reference, including that no
 * destination pixel outside the rotated rectangle changed. */
static int check(const uint32_t *src, size_t sstride, uint32_t *a, uint32_t *b, size_t dstride,
                 struct rot_rect r, enum rot_dir dir) {
    size_t words = dstride / 4 * SW;
    for (size_t i = 0; i < words; i++) a[i] = b[i] = 0xdeadbeef ^ (uint32_t)i;
    rotate_rect(src, sstride, SW, SH, a, dstride, r, dir);
    rotate_rect_scalar(src, sstride, SW, SH, b, dstride, r, dir);
    if (memcmp(a, b, words * 4) != 0) {
        fprintf(stderr, "mismatch dir %d rect %d,%d %dx%d\n", dir, r.x, r.y, r.w, r.h);
        return 1;
    }
    struct rot_rect d = rot_dst_rect(r, SW, SH, dir);
    for (int row = 0; row < SW; row++)
        for (int col = 0; col < SH; col++) {
            size_t i = (size_t)row * (dstride / 4) + col;
            int inside = row >= d.y && row < d.y + d.h && col >= d.x && col < d.x + d.w;
            if (!inside && a[i] != (0xdeadbeef ^ (uint32_t)i)) {
                fprintf(stderr, "write outside dir %d rect %d,%d %dx%d at %d,%d\n", dir, r.x, r.y, r.w,
                        r.h, row, col);
                return 1;
            }
        }
    return 0;
}

int main(int argc, char **argv) {
    /* Strides with padding, as drivers may return. */
    size_t sstride = SW * 4 + 64, dstride = SH * 4 + 128;
    uint32_t *src = aligned_alloc(64, sstride * SH);
    uint32_t *a = aligned_alloc(64, dstride * SW), *b = aligned_alloc(64, dstride * SW);
    if (!src || !a || !b) return 2;
    for (size_t i = 0; i < sstride / 4 * SH; i++) src[i] = next();

    if (argc > 1 && strcmp(argv[1], "bench") == 0) {
        static const struct {
            const char *name;
            struct rot_rect r;
        } cases[] = {
            {"full 1280x720", {0, 0, SW, SH}},
            {"meter strip 640x64", {320, 600, 640, 64}},
            {"8 meters 16x300 (one)", {100, 300, 16, 300}},
            {"bbox 1000x320", {140, 380, 1000, 320}},
        };
        for (size_t c = 0; c < sizeof cases / sizeof cases[0]; c++) {
            struct rot_rect r = cases[c].r;
            for (int dir = 0; dir < 2; dir++) {
                double best = 1e9, best_ref = 1e9;
                for (int rep = 0; rep < 7; rep++) {
                    double t0 = now();
                    rotate_rect(src, sstride, SW, SH, a, dstride, r, (enum rot_dir)dir);
                    double t1 = now();
                    rotate_rect_scalar(src, sstride, SW, SH, b, dstride, r, (enum rot_dir)dir);
                    double t2 = now();
                    if (t1 - t0 < best) best = t1 - t0;
                    if (t2 - t1 < best_ref) best_ref = t2 - t1;
                }
                double px = (double)r.w * r.h;
                printf("%-24s %s  tiled %8.3f ms (%5.2f ns/px)  scalar %8.3f ms (%5.2f ns/px)\n",
                       cases[c].name, dir ? "ccw" : "cw ", best * 1e3, best * 1e9 / px, best_ref * 1e3,
                       best_ref * 1e9 / px);
            }
        }
        return 0;
    }

    int failures = 0, cases = 0;
    struct rot_rect fixed[] = {
        {0, 0, SW, SH}, {0, 0, 1, 1}, {SW - 1, SH - 1, 1, 1}, {0, 0, 16, 16}, {4, 16, 4, 16},
        {3, 5, 17, 33}, {1, 2, 15, 15}, {SW - 17, SH - 31, 17, 31}, {640, 0, 4, SH}, {0, 360, SW, 16},
    };
    for (size_t i = 0; i < sizeof fixed / sizeof fixed[0]; i++)
        for (int dir = 0; dir < 2; dir++, cases++) failures += check(src, sstride, a, b, dstride, fixed[i], dir);
    for (int i = 0; i < 300; i++) {
        struct rot_rect r;
        r.x = next() % SW, r.y = next() % SH;
        r.w = 1 + next() % (SW - r.x), r.h = 1 + next() % (SH - r.y);
        if (i % 3 == 0) { /* narrow, like a meter */
            int w = 1 + (int)(next() % 40);
            r.w = w < SW - r.x ? w : SW - r.x;
        }
        failures += check(src, sstride, a, b, dstride, r, (enum rot_dir)(i & 1));
        cases++;
    }
    printf("%d cases, %d failures\n", cases, failures);
    return failures != 0;
}
