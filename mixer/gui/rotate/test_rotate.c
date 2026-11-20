/* Tests and benchmark for rotate.c.
 *   test_rotate            bit-exact checks against the scalar reference,
 *                          for whole rectangles and for the 16-row strips
 *                          gui_partial.c rotates them in
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

/* gui_partial.c rotates a region in strips of 16 source rows. Each strip is
 * first copied into a staging buffer whose row 0 is the strip's first row;
 * rotate_rect then gets rows 0..strip-1 of that buffer, with the source
 * height reduced by the strip's row (CW) or the destination advanced by it
 * (CCW). This repeats that calling convention and compares the whole
 * destination with one scalar rotation of the region from the source. */
static uint32_t stage[16 * SW];
static int check_strips(const uint32_t *src, size_t sstride, uint32_t *a, uint32_t *b, struct rot_rect r,
                        enum rot_dir dir) {
    const size_t dstride = SH * 4, words = (size_t)SH * SW;
    for (size_t i = 0; i < words; i++) a[i] = b[i] = 0xdeadbeef ^ (uint32_t)i;
    for (int y = r.y; y < r.y + r.h; y += 16) {
        int strip = 16 < r.y + r.h - y ? 16 : r.y + r.h - y;
        for (int row = 0; row < strip; row++)
            memcpy(stage + (size_t)row * SW + r.x,
                   (const uint8_t *)src + (size_t)(y + row) * sstride + (size_t)r.x * 4, (size_t)r.w * 4);
        if (dir == ROT_CW)
            rotate_rect(stage, SW * 4, SW, SH - y, a, dstride, (struct rot_rect){r.x, 0, r.w, strip}, ROT_CW);
        else
            rotate_rect(stage, SW * 4, SW, SH, a + y, dstride, (struct rot_rect){r.x, 0, r.w, strip}, ROT_CCW);
    }
    rotate_rect_scalar(src, sstride, SW, SH, b, dstride, r, dir);
    if (memcmp(a, b, words * 4) != 0) {
        fprintf(stderr, "strip mismatch dir %d rect %d,%d %dx%d\n", dir, r.x, r.y, r.w, r.h);
        return 1;
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
    /* Strips: the meter and notification rectangles captured from the device,
     * the source's edges and corners, shapes thinner than the NEON block (4
     * columns, 16 rows), and random small rectangles, in both directions. */
    struct rot_rect strips[] = {
        {167, 341, 28, 27}, {411, 444, 28, 28}, {1010, 45, 16, 30}, {131, 0, 274, 120}, {411, 444, 28, 42},
        {968, 45, 21, 30}, {193, 370, 28, 9}, {193, 384, 10, 8}, {985, 45, 3, 30}, {1003, 67, 7, 8},
        {0, 0, 19, 23}, {SW - 1, SH - 1, 1, 1}, {0, 701, 3, 19}, {1263, 0, 17, 1}, {0, 0, 1, 1},
        {0, SH - 1, 1, 1}, {SW - 1, 0, 1, 1}, {0, 0, SW, 1}, {0, SH - 1, SW, 1}, {0, 0, 1, SH},
        {SW - 1, 0, 1, SH}, {0, 704, 16, 16}, {1264, 0, 16, 16}, {100, 100, 64, 64}, {500, 711, 640, 9},
        {0, 0, SW, SH}, {4, 16, 4, 16}, {5, 17, 4, 16}, {4, 15, 5, 18}, {1276, 703, 4, 17},
    };
    for (size_t i = 0; i < sizeof strips / sizeof strips[0]; i++)
        for (int dir = 0; dir < 2; dir++, cases++)
            failures += check_strips(src, sstride, a, b, strips[i], (enum rot_dir)dir);
    for (int i = 0; i < 400; i++) {
        struct rot_rect r;
        r.x = next() % SW, r.y = next() % SH;
        r.w = 1 + next() % 64, r.h = 1 + next() % 64;
        if (i % 4 == 0) r.x = i % 8 ? SW - 1 - (int)(next() % 40) : (int)(next() % 4); /* at the left or right edge */
        if (i % 4 == 1) r.y = i % 8 == 1 ? SH - 1 - (int)(next() % 40) : (int)(next() % 4); /* top or bottom */
        if (r.w > SW - r.x) r.w = SW - r.x;
        if (r.h > SH - r.y) r.h = SH - r.y;
        failures += check_strips(src, sstride, a, b, r, (enum rot_dir)(i & 1));
        failures += check_strips(src, sstride, a, b, r, (enum rot_dir)(~i & 1));
        cases += 2;
    }
    printf("%d cases, %d failures\n", cases, failures);
    return failures != 0;
}
