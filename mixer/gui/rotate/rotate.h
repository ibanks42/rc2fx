/* 90° rotation of 32-bit pixel rectangles; see rotate.c. */
#ifndef RC2FX_ROTATE_H
#define RC2FX_ROTATE_H
#include <stddef.h>

enum rot_dir { ROT_CW, ROT_CCW };

/* A source rectangle in pixels. */
struct rot_rect {
    int x, y, w, h;
};

/* Rotates rectangle r of the sw × sh source into the sh × sw destination. */
void rotate_rect(const void *src, size_t sstride, int sw, int sh, void *dst, size_t dstride,
                 struct rot_rect r, enum rot_dir dir);
/* The reference: one pixel at a time. */
void rotate_rect_scalar(const void *src, size_t sstride, int sw, int sh, void *dst, size_t dstride,
                        struct rot_rect r, enum rot_dir dir);
/* Where r lands in the destination (for a GE2D COMPOSE rectangle). */
struct rot_rect rot_dst_rect(struct rot_rect r, int sw, int sh, enum rot_dir dir);
#endif
