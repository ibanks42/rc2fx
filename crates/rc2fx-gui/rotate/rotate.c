/* 90° rotation of 32-bit pixels (BA24/ARGB32 → XR24 is a plain 32-bit copy:
 * the GE2D driver maps both to BGRA8888 byte order) for a rectangle of the
 * source, written straight to the rotated destination.
 *
 * Source: sw × sh pixels, `sstride` bytes per row. Destination: sh × sw
 * pixels (rotated), `dstride` bytes per row.
 *   ROT_CW:  dst(row x,          col sh-1-y) = src(x, y)
 *   ROT_CCW: dst(row sw-1-x,     col y)      = src(x, y)
 * Which of the two the GUI's GE2D setup (V4L2_CID_ROTATE 270) produces must
 * be checked against a GE2D frame; the mainline driver's flip choice for
 * 90/270 changed between v5.11 and v5.15.
 *
 * NEON path: 16 source rows × 4 columns per step, i.e. four 4×4 transposes
 * (trn1/trn2 on .4s then .2d). Each step reads 16 B from each of 16 source
 * rows and writes four 64-byte destination runs, the access pattern that
 * write-combining (Normal non-cacheable) destination memory merges best.
 * Edges that are not multiples of 4/16 fall back to scalar.
 */
#include "rotate.h"

#include <stddef.h>
#include <stdint.h>
#if defined(__aarch64__) && !defined(ROTATE_SCALAR_ONLY)
#include <arm_neon.h>
#endif

static inline uint32_t *row32(void *base, size_t stride, int row) {
    return (uint32_t *)((uint8_t *)base + (size_t)row * stride);
}
static inline const uint32_t *crow32(const void *base, size_t stride, int row) {
    return (const uint32_t *)((const uint8_t *)base + (size_t)row * stride);
}

void rotate_rect_scalar(const void *src, size_t sstride, int sw, int sh, void *dst, size_t dstride,
                        struct rot_rect r, enum rot_dir dir) {
    for (int y = r.y; y < r.y + r.h; y++) {
        const uint32_t *s = crow32(src, sstride, y);
        for (int x = r.x; x < r.x + r.w; x++) {
            if (dir == ROT_CW)
                row32(dst, dstride, x)[sh - 1 - y] = s[x];
            else
                row32(dst, dstride, sw - 1 - x)[y] = s[x];
        }
    }
}

#if defined(__aarch64__) && !defined(ROTATE_SCALAR_ONLY)
/* 4×4 transpose of 32-bit lanes: rows a..d in, columns out (in place). */
static inline void transpose4(uint32x4_t *a, uint32x4_t *b, uint32x4_t *c, uint32x4_t *d) {
    uint32x4_t t0 = vtrn1q_u32(*a, *b), t1 = vtrn2q_u32(*a, *b);
    uint32x4_t t2 = vtrn1q_u32(*c, *d), t3 = vtrn2q_u32(*c, *d);
    *a = vreinterpretq_u32_u64(vtrn1q_u64(vreinterpretq_u64_u32(t0), vreinterpretq_u64_u32(t2)));
    *b = vreinterpretq_u32_u64(vtrn1q_u64(vreinterpretq_u64_u32(t1), vreinterpretq_u64_u32(t3)));
    *c = vreinterpretq_u32_u64(vtrn2q_u64(vreinterpretq_u64_u32(t0), vreinterpretq_u64_u32(t2)));
    *d = vreinterpretq_u32_u64(vtrn2q_u64(vreinterpretq_u64_u32(t1), vreinterpretq_u64_u32(t3)));
}

/* 16 source rows starting at y (in destination column order for `dir`),
 * 4 source columns starting at x: four destination rows of 16 pixels. */
static inline void block16x4(const void *src, size_t sstride, int sw, int sh, void *dst, size_t dstride,
                             int x, int y, enum rot_dir dir) {
    uint32x4_t v[16];
    for (int i = 0; i < 16; i++) {
        /* CW: destination columns increase as source rows decrease. */
        int row = dir == ROT_CW ? y + 15 - i : y + i;
        v[i] = vld1q_u32(crow32(src, sstride, row) + x);
    }
    for (int i = 0; i < 16; i += 4) transpose4(&v[i], &v[i + 1], &v[i + 2], &v[i + 3]);
    /* v[4q + k] now holds source column x+k for rows block q. */
    for (int k = 0; k < 4; k++) {
        uint32_t *out = dir == ROT_CW ? row32(dst, dstride, x + k) + (sh - 1 - (y + 15))
                                      : row32(dst, dstride, sw - 1 - (x + k)) + y;
        uint32x4x4_t q = {{v[k], v[4 + k], v[8 + k], v[12 + k]}};
        vst1q_u32_x4(out, q);
    }
}
#endif

void rotate_rect(const void *src, size_t sstride, int sw, int sh, void *dst, size_t dstride,
                 struct rot_rect r, enum rot_dir dir) {
#if defined(__aarch64__) && !defined(ROTATE_SCALAR_ONLY)
    int y16 = r.h / 16 * 16, x4 = r.w / 4 * 4;
    for (int by = 0; by < y16; by += 16)
        for (int bx = 0; bx < x4; bx += 4)
            block16x4(src, sstride, sw, sh, dst, dstride, r.x + bx, r.y + by, dir);
    /* Scalar edges: the right columns, then the bottom rows. */
    if (x4 < r.w)
        rotate_rect_scalar(src, sstride, sw, sh, dst, dstride,
                           (struct rot_rect){r.x + x4, r.y, r.w - x4, y16}, dir);
    if (y16 < r.h)
        rotate_rect_scalar(src, sstride, sw, sh, dst, dstride,
                           (struct rot_rect){r.x, r.y + y16, r.w, r.h - y16}, dir);
#else
    rotate_rect_scalar(src, sstride, sw, sh, dst, dstride, r, dir);
#endif
}

struct rot_rect rot_dst_rect(struct rot_rect r, int sw, int sh, enum rot_dir dir) {
    if (dir == ROT_CW) return (struct rot_rect){sh - r.y - r.h, r.x, r.h, r.w};
    return (struct rot_rect){r.y, sw - r.x - r.w, r.h, r.w};
}
