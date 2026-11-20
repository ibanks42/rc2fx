/* Host-test stand-in for libdrm commit and the GE2D V4L2 device. Built as a
 * shared library so ioctl() is an imported call that LD_AUDIT can intercept.
 * Non-video fds pass through to the real ioctl syscall.
 *
 * Models a full-frame 90 deg rotation masked by the COMPOSE rectangle (pixels
 * outside COMPOSE stay untouched). Fault/behavior knobs via env:
 *   FAKE_GE2D_DIR=cw|ccw, FAKE_GE2D_DELAY_US, FAKE_GE2D_FAIL_QBUF=n,
 *   FAKE_GE2D_FAIL_DQBUF=n, FAKE_GE2D_FAIL_SELECTION=n,
 *   FAKE_GE2D_FAIL_QUERYBUF=1, FAKE_GE2D_BAD_OFFSET=1.
 * vb2's VM_WRITE mmap rule for OUTPUT buffers cannot be enforced here because
 * the audit module's mmap bypasses this library; gui_partial.c enforces it in
 * test builds instead.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <linux/videodev2.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>

#define SW 1280
#define SH 720

int drmModeAtomicCommit(int fd, void *r, uint32_t f, void *u) {
    (void)fd, (void)r, (void)f, (void)u;
    struct timespec t = {0, 5000000};
    nanosleep(&t, 0);
    return 0;
}

static int video = -1;
static uint32_t *source;
static struct {
    int fd;
    uint32_t *p;
    size_t pitch; /* bytes */
} dests[4];
static int ndests;
static struct v4l2_rect crop = {0, 0, SW, SH}, compose = {0, 0, SH, SW};
static int capture_queued = -1, output_queued, job_done;
static unsigned output_qbufs, capture_dqbufs, selections;
static int token_was_clamped;
/* Counters the test program prints. */
unsigned long long fake_jobs, fake_pixels;

/* Reset fake V4L2 queues while leaving pixel buffers intact (for testing
 * module recovery after caller queue recovery). */
void fake_v4l2_recover_queues(void) {
    capture_queued = -1;
    output_queued = job_done = 0;
}

int fake_v4l2_open(uint32_t **src) {
    video = memfd_create("fake-ge2d", MFD_CLOEXEC);
    if (video < 0 || ftruncate(video, SW * SH * 4) != 0) return -1;
    source = mmap(NULL, SW * SH * 4, PROT_READ | PROT_WRITE, MAP_SHARED, video, 0);
    if (source == MAP_FAILED) return -1;
    *src = source;
    return video;
}
void fake_v4l2_add_dest(int fd, uint32_t *p, size_t pitch) {
    dests[ndests].fd = fd, dests[ndests].p = p, dests[ndests].pitch = pitch;
    ndests++;
}

static int env_int(const char *name) {
    const char *v = getenv(name);
    return v ? atoi(v) : 0;
}

static void run_job(void) {
    uint32_t *dst = NULL;
    size_t pitch = 0;
    for (int i = 0; i < ndests; i++)
        if (dests[i].fd == capture_queued) dst = dests[i].p, pitch = dests[i].pitch / 4;
    if (!dst) return;
    int ccw = getenv("FAKE_GE2D_DIR") && strcmp(getenv("FAKE_GE2D_DIR"), "ccw") == 0;
    for (uint32_t row = compose.top; row < compose.top + compose.height; row++)
        for (uint32_t col = compose.left; col < compose.left + compose.width; col++) {
            /* CW: dst(row x, col SH-1-y) = src(x, y); CCW: dst(row SW-1-x, col y). */
            uint32_t x = ccw ? SW - 1 - row : row, y = ccw ? col : SH - 1 - col;
            dst[row * pitch + col] = source[(size_t)y * SW + x];
        }
    fake_jobs++;
    fake_pixels += (unsigned long long)compose.width * compose.height;
    int delay = env_int("FAKE_GE2D_DELAY_US");
    if (delay) {
        struct timespec t = {delay / 1000000, (long)(delay % 1000000) * 1000};
        nanosleep(&t, 0);
    }
}

int ioctl(int fd, unsigned long request, ...) {
    va_list ap;
    va_start(ap, request);
    void *arg = va_arg(ap, void *);
    va_end(ap);
    if (fd != video || video < 0) return (int)syscall(SYS_ioctl, fd, request, arg);
    switch (request) {
    case VIDIOC_G_FMT: {
        struct v4l2_format *f = arg;
        if (f->type != V4L2_BUF_TYPE_VIDEO_OUTPUT && f->type != V4L2_BUF_TYPE_VIDEO_CAPTURE)
            return errno = EINVAL, -1;
        f->fmt.pix.width = f->type == V4L2_BUF_TYPE_VIDEO_OUTPUT ? SW : SH;
        f->fmt.pix.height = f->type == V4L2_BUF_TYPE_VIDEO_OUTPUT ? SH : SW;
        f->fmt.pix.bytesperline = f->fmt.pix.width * 4;
        f->fmt.pix.pixelformat = env_int("FAKE_GE2D_BAD_FMT") == 1 ? 0 : V4L2_PIX_FMT_ARGB32;
        if (env_int("FAKE_GE2D_BAD_FMT") == 2) f->fmt.pix.bytesperline += 4;
        if (env_int("FAKE_GE2D_BAD_FMT") == 3) f->fmt.pix.width++;
        f->fmt.pix.sizeimage = SW * SH * 4;
        return 0;
    }
    case VIDIOC_S_SELECTION: {
        struct v4l2_selection *s = arg;
        int nth_selection = env_int("FAKE_GE2D_FAIL_SELECTION");
        if (nth_selection && ++selections == (unsigned)nth_selection) return errno = EIO, -1;
        if (s->r.left < 0 || s->r.top < 0) return errno = EINVAL, -1;
        if (token_was_clamped && env_int("FAKE_GE2D_FAIL_RESTORE")) return errno = EIO, -1;
        if (s->type == V4L2_BUF_TYPE_VIDEO_OUTPUT && s->target == V4L2_SEL_TGT_CROP) {
            if (env_int("FAKE_GE2D_FAIL_FULL_RESTORE") && fake_jobs >= 2 && s->r.width == SW && s->r.height == SH)
                return errno = EIO, -1;
            if ((uint32_t)s->r.left + s->r.width > SW || (uint32_t)s->r.top + s->r.height > SH)
                return errno = EINVAL, -1;
            if (env_int("FAKE_GE2D_CLAMP_TOKEN") && fake_jobs >= 2 && s->r.width == 16 && s->r.height == 16) {
                s->r.width = 32;
                token_was_clamped = 1;
            }
            crop = s->r;
            return 0;
        }
        if (s->type == V4L2_BUF_TYPE_VIDEO_CAPTURE && s->target == V4L2_SEL_TGT_COMPOSE) {
            if ((uint32_t)s->r.left + s->r.width > SH || (uint32_t)s->r.top + s->r.height > SW)
                return errno = EINVAL, -1;
            compose = s->r;
            return 0;
        }
        return errno = EINVAL, -1;
    }
    case VIDIOC_QUERYBUF: {
        struct v4l2_buffer *b = arg;
        if ((b->type != V4L2_BUF_TYPE_VIDEO_OUTPUT && b->type != V4L2_BUF_TYPE_VIDEO_CAPTURE) || b->index != 0)
            return errno = EINVAL, -1;
        if (env_int("FAKE_GE2D_FAIL_QUERYBUF")) return errno = EIO, -1;
        /* Unaligned offset forces mmap to fail with EINVAL. */
        b->m.offset = env_int("FAKE_GE2D_BAD_OFFSET") ? 1u : 0;
        b->length = SW * SH * 4;
        if (b->type == V4L2_BUF_TYPE_VIDEO_CAPTURE && env_int("FAKE_GE2D_SHORT_CAPTURE")) b->length--;
        return 0;
    }
    case VIDIOC_QBUF: {
        struct v4l2_buffer *b = arg;
        if (b->type == V4L2_BUF_TYPE_VIDEO_CAPTURE) {
            if (capture_queued >= 0 || b->memory != V4L2_MEMORY_DMABUF) return errno = EINVAL, -1;
            capture_queued = b->m.fd;
            return 0;
        }
        if (b->type == V4L2_BUF_TYPE_VIDEO_OUTPUT) {
            int nth = env_int("FAKE_GE2D_FAIL_QBUF");
            if (nth && ++output_qbufs == (unsigned)nth) return errno = EIO, -1;
            if (output_queued || capture_queued < 0) return errno = EINVAL, -1;
            output_queued = 1;
            run_job();
            job_done = 1;
            return 0;
        }
        return errno = EINVAL, -1;
    }
    case VIDIOC_DQBUF: {
        struct v4l2_buffer *b = arg;
        if (b->type == V4L2_BUF_TYPE_VIDEO_CAPTURE) {
            int nth = env_int("FAKE_GE2D_FAIL_DQBUF");
            if (nth && ++capture_dqbufs == (unsigned)nth) return errno = EIO, -1;
            if (!job_done || capture_queued < 0) return errno = EAGAIN, -1;
            b->m.fd = capture_queued;
            if (env_int("FAKE_GE2D_BAD_DQBUF_ID")) b->index = 1;
            capture_queued = -1;
            return 0;
        }
        if (b->type == V4L2_BUF_TYPE_VIDEO_OUTPUT) {
            if (!output_queued) return errno = EAGAIN, -1;
            output_queued = 0, job_done = 0;
            return 0;
        }
        return errno = EINVAL, -1;
    }
    default:
        return errno = ENOTTY, -1;
    }
}
