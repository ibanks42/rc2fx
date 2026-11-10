/* Tests of gui_partial.c as the mixer builds it: no RC2FX_PACING_TEST, so the
 * CPU rules in cpu_eligible are always on, and the real cpu_rotate_regions
 * strip loop runs with whatever rotate_rect the target has (NEON on aarch64).
 * The LD_AUDIT harness in gui_pacing_test.cpp cannot do that: its paint
 * stand-ins are x86-64, and its build gates the CPU path behind a test knob.
 *
 * This file includes gui_partial.c and supplies the few things gui_pacing.c
 * gives it, a stand-in GE2D behind vendor_ioctl, and the destination
 * descriptor. The stand-in GE2D rotates with its own loop (not rotate.c), and
 * after every conversion the whole destination is compared with a third,
 * plain rotation of the whole source.
 *
 *   partial_plan_test cw|ccw SEED FRAMES [fail-qbuf N | fail-selection N | stage-clock 1]
 *
 * Paints are random: meter-like groups of small rectangles, single pixels,
 * rectangles on the source's edges and corners, rectangles that hang outside
 * the source or have no area, large redraws, more rectangles than the pending
 * list holds, rejected paints, and frames with no paint. It prints one line
 * per conversion (its kind, reason and a hash of the destination) and a
 * summary; the exit status is 1 if any destination differed from the rotated
 * source. Two builds of gui_partial.c given the same arguments must print the
 * same hashes, which is how an edit to the planner or the CPU path is
 * compared with the code before it:
 *
 *   cc -O2 -DRC2FX_GUI_RECT_PLAN3 -DPARTIAL_SOURCE='"old/gui_partial.c"' ...
 *
 * Host: cc -O2 -Wall -Wextra -Wno-unused-function. aarch64: zig cc -target
 * aarch64-linux-musl -mcpu=cortex_a53 -static, run under qemu-aarch64.
 *
 * "fail-qbuf N" fails the N-th output QBUF and "fail-selection N" the N-th
 * S_SELECTION. Then the checks are: a conversion on the CPU path whose token
 * QBUF failed wrote no destination pixel, and every destination is right
 * again at its next conversion. A failed QBUF of a GE2D plan's last job is
 * the vendor's own failed submission; the pending damage is already cleared
 * there, so that case is reported and not checked.
 *
 * "stage-clock 1" replaces the clock with one that only this program moves,
 * so the times on the counters line are known exactly. The CPU rotation reads
 * the clock three times per 16-row strip: after the copy into the stage,
 * after the rotation and after the yield. Under this clock those readings are
 * 1 ms, 10 ms and 100 ms after the one before, so a rotation of S strips
 * shows S ms of staging, 10 S ms of rotation and 100 S ms inside the yield,
 * and lasts 111 S + 1 ms.
 *
 * After the summary the program prints the module's counters line, as the
 * mixer's log would hold it after the last conversion.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdarg.h>
#include <stdatomic.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>

/* What gui_pacing.c provides to gui_partial.c. */
static uintptr_t main_base;
static bool enabled = true;
static pid_t gui_pid;
static int log_fd = -1;
static bool quiet;
static _Atomic uint64_t passes, log_rotations, log_dropped, log_reopen_failed, paints, paint_rejects;
static long long merge_px;
typedef int *(*errno_fn)(void);
static int *own_errno(void) { return &errno; }
static errno_fn vendor_errno = own_errno;
/* "stage-clock": each reading is later than the last by a step chosen by its
 * place since the module last looked at the destination descriptor, which it
 * does just before a CPU rotation's first reading. The readings after that
 * one are the strip loop's: after the copy, the rotation and the yield. */
static bool stage_clock;
static uint64_t stage_now;
static unsigned stage_place;
static uint64_t now_ns(void) {
    if (stage_clock) {
        static const uint64_t step[3] = {100000000, 1000000, 10000000}; /* yield, copy, rotation */
        stage_now += step[stage_place++ % 3];
        return stage_now;
    }
    struct timespec t = {0, 0};
    clock_gettime(CLOCK_MONOTONIC, &t);
    return (uint64_t)t.tv_sec * 1000000000ull + (uint64_t)t.tv_nsec;
}
static uintptr_t plain(uintptr_t address) { return address; }
/* The log: nothing is kept but the counters line asked for at the end. */
static char captured[8192];
static bool capture;
static void log_line(const char *line, size_t n) {
    if (capture && n < sizeof captured) memcpy(captured, line, n), captured[n] = 0;
}
static const uint8_t *current_descriptor;
static const uint8_t *const *destination_global(void) {
    stage_place = 0;
    return &current_descriptor;
}

#ifndef PARTIAL_SOURCE
#define PARTIAL_SOURCE "gui_partial.c"
#endif
#include PARTIAL_SOURCE

/* The stand-in GE2D. The source is a memfd, so the module's own mmap of the
 * OUTPUT buffer and its ppoll both work on the descriptor. */
static uint32_t *source, *dests[2];
static int dest_fds[2] = {41, 42}, current;
static struct v4l2_rect crop = {0, 0, SRC_W, SRC_H}, compose = {0, 0, SRC_H, SRC_W};
static unsigned long long jobs_run, job_pixels, selections, output_qbufs, calls;
static unsigned long long fail_qbuf, fail_selection;
static bool bad_compose;

static void ge2d_job(void) {
    uint32_t *dst = dests[current];
    int w = (int)crop.width, h = (int)crop.height;
    /* The compose rectangle must be the crop's place in the rotated frame. */
    int left = partial_dir == ROT_CW ? SRC_H - crop.top - h : crop.top;
    int top = partial_dir == ROT_CW ? crop.left : SRC_W - crop.left - w;
    if (compose.left != left || compose.top != top || (int)compose.width != h || (int)compose.height != w)
        bad_compose = true;
    for (int j = 0; j < h; j++)
        for (int i = 0; i < w; i++) {
            uint32_t pixel = source[(size_t)(crop.top + j) * SRC_W + crop.left + i];
            if (partial_dir == ROT_CW)
                dst[(size_t)(compose.top + i) * SRC_H + compose.left + (h - 1 - j)] = pixel;
            else
                dst[(size_t)(compose.top + (w - 1 - i)) * SRC_H + compose.left + j] = pixel;
        }
    jobs_run++, job_pixels += (unsigned long long)w * h;
}

static int fake_ioctl(int fd, unsigned long request, ...) {
    va_list ap;
    va_start(ap, request);
    void *arg = va_arg(ap, void *);
    va_end(ap);
    (void)fd;
    calls++;
    if (request == VIDIOC_S_SELECTION) {
        struct v4l2_selection *s = arg;
        if (++selections == fail_selection) return errno = EIO, -1;
        if (s->type == V4L2_BUF_TYPE_VIDEO_OUTPUT) crop = s->r;
        else compose = s->r;
        return 0;
    }
    if (request == VIDIOC_QBUF) {
        struct v4l2_buffer *b = arg;
        if (b->type != V4L2_BUF_TYPE_VIDEO_OUTPUT) return 0;
        if (++output_qbufs == fail_qbuf) return errno = EIO, -1;
        ge2d_job();
        return 0;
    }
    if (request == VIDIOC_QUERYBUF) {
        struct v4l2_buffer *b = arg;
        b->length = SRC_W * SRC_H * 4, b->m.offset = 0;
        return 0;
    }
    return 0; /* DQBUF */
}

static uint32_t rng;
static uint32_t next(void) {
    rng ^= rng << 13, rng ^= rng >> 17, rng ^= rng << 5;
    return rng;
}
static int below(int n) { return (int)(next() % (uint32_t)n); }

/* One paint: the renderer draws inside the part of each rectangle that is in
 * the source, and the module gets the list as the GUI holds it. */
static void paint(int32_t (*list)[4], int count, uint32_t colour) {
    for (int r = 0; r < count; r++) {
        long long x0 = list[r][0], y0 = list[r][1], x1 = x0 + list[r][2], y1 = y0 + list[r][3];
        if (x0 < 0) x0 = 0;
        if (y0 < 0) y0 = 0;
        if (x1 > SRC_W) x1 = SRC_W;
        if (y1 > SRC_H) y1 = SRC_H;
        for (long long y = y0; y < y1; y++)
            for (long long x = x0; x < x1; x++)
                source[y * SRC_W + x] = colour ^ (uint32_t)(y * SRC_W + x) * 0x01000193u;
    }
    atomic_fetch_add(&paints, 1);
    partial_paint(&list[0][0], count);
}

static int32_t list[64][4];
static int meters(void) {
    static const int anchors[10][2] = {{0, 0}, {1250, 0}, {0, 690}, {1250, 690}, {193, 370},
                                       {985, 45},  {411, 444}, {640, 360},  {4, 16},   {1272, 700}};
    int n = 0;
    for (int groups = 1 + below(3); groups > 0; groups--) {
        const int *anchor = anchors[below(10)];
        for (int count = 1 + below(3); count > 0; count--, n++) {
            list[n][0] = anchor[0] + below(30), list[n][1] = anchor[1] + below(30);
            list[n][2] = 1 + below(30), list[n][3] = 1 + below(30);
        }
    }
    return n; /* some hang over the right and bottom edges */
}
static int paint_some(int frame) {
    uint32_t colour = 0x9e3779b9u * (uint32_t)(frame + 1);
    int kind = below(24), n = 0;
    switch (kind) {
    case 0: /* nothing painted */
        return kind;
    case 1: /* single pixels in the four corners and one elsewhere */
        list[0][0] = 0, list[0][1] = 0, list[1][0] = SRC_W - 1, list[1][1] = 0;
        list[2][0] = 0, list[2][1] = SRC_H - 1, list[3][0] = SRC_W - 1, list[3][1] = SRC_H - 1;
        list[4][0] = below(SRC_W), list[4][1] = below(SRC_H);
        for (n = 0; n < 5; n++) list[n][2] = list[n][3] = 1;
        n = 2 + below(4);
        break;
    case 2: /* thin shapes: below the NEON block in one dimension or both */
        for (n = 0; n < 3; n++) {
            list[n][0] = below(SRC_W - 40), list[n][1] = below(SRC_H - 40);
            list[n][2] = below(2) ? 1 + below(3) : 4 + below(36), list[n][3] = below(2) ? 1 + below(15) : 16 + below(24);
        }
        break;
    case 3: /* rectangles hanging outside the source, and ones with no area */
        list[0][0] = -7, list[0][1] = -3, list[0][2] = 20, list[0][3] = 19;
        list[1][0] = SRC_W - 5, list[1][1] = SRC_H - 9, list[1][2] = 4000, list[1][3] = 4000;
        list[2][0] = 300, list[2][1] = 300, list[2][2] = 0, list[2][3] = 12;
        list[3][0] = 600, list[3][1] = 200, list[3][2] = -4, list[3][3] = 5;
        list[4][0] = INT32_MAX - 3, list[4][1] = 10, list[4][2] = 9, list[4][3] = 9;
        list[5][0] = -2000000000, list[5][1] = 650, list[5][2] = 2000000017, list[5][3] = 33;
        n = 6;
        break;
    case 4: /* a large redraw, as the notification panel */
        list[0][0] = below(SRC_W - 274), list[0][1] = below(SRC_H - 120), list[0][2] = 274, list[0][3] = 120;
        n = 1;
        break;
    case 5: /* a large redraw with meters */
        n = meters();
        list[n][0] = below(SRC_W - 274), list[n][1] = below(SRC_H - 120), list[n][2] = 274, list[n][3] = 120;
        n++;
        break;
    case 6: /* more than half the frame */
        list[0][0] = below(100), list[0][1] = below(60), list[0][2] = 1100, list[0][3] = 640;
        n = 1;
        break;
    case 7: /* more rectangles than the pending list holds, in two paints */
        for (n = 0; n < 40; n++) {
            list[n][0] = 500 + below(60), list[n][1] = 300 + below(40);
            list[n][2] = 1 + below(6), list[n][3] = 1 + below(6);
        }
        paint(list, 25, colour);
        paint(list + 25, 15, colour ^ 0x5a5a5a5au);
        return kind;
    case 8: /* a rejected paint: the renderer may have drawn anywhere */
        for (int i = 0; i < 50; i++) {
            int at = below(SRC_W * SRC_H);
            source[at] = next();
        }
        atomic_fetch_add(&paints, 1), atomic_fetch_add(&paint_rejects, 1);
        partial_paint_rejected();
        return kind;
    case 9: /* two jobs of 4,096 pixels or a little more, far apart */
        list[0][0] = 100 + below(8), list[0][1] = 100, list[0][2] = 64, list[0][3] = 64;
        list[1][0] = 900, list[1][1] = 500 + below(8), list[1][2] = 64, list[1][3] = 64 + below(2);
        n = 2;
        break;
    case 10: /* full-width and full-height slivers */
        list[0][0] = 0, list[0][1] = below(SRC_H), list[0][2] = SRC_W, list[0][3] = 1;
        list[1][0] = below(SRC_W), list[1][1] = 0, list[1][2] = 1, list[1][3] = SRC_H;
        n = 1 + below(2);
        break;
    default: /* meter updates, the common case; sometimes two paints */
        n = meters();
        if (below(4) == 0) {
            paint(list, n, colour ^ 0x33333333u);
            n = meters();
        }
        break;
    }
    paint(list, n, colour);
    return kind;
}

static uint64_t hash_of(const uint32_t *pixels) {
    uint64_t hash = 0xcbf29ce484222325ull; /* FNV-1a */
    for (int i = 0; i < SRC_W * SRC_H; i++) hash = (hash ^ pixels[i]) * 0x100000001b3ull;
    return hash;
}
static long mismatches_in(const uint32_t *dst) {
    long count = 0;
    for (int row = 0; row < SRC_W; row++)
        for (int col = 0; col < SRC_H; col++) {
            int x = partial_dir == ROT_CCW ? SRC_W - 1 - row : row;
            int y = partial_dir == ROT_CCW ? col : SRC_H - 1 - col;
            count += dst[(size_t)row * SRC_H + col] != source[(size_t)y * SRC_W + x];
        }
    return count;
}

int main(int argc, char **argv) {
    if (argc < 4) return 2;
    if (!parse_partial_mode(strcmp(argv[1], "ccw") == 0 ? "partial-verify-ccw" : "partial-verify-cw")) return 2;
    rng = 0x85ebca6bu * (uint32_t)(atoi(argv[2]) + 1);
    if (!rng) rng = 1;
    int frames = atoi(argv[3]);
    if (argc >= 6 && strcmp(argv[4], "fail-qbuf") == 0) fail_qbuf = strtoull(argv[5], NULL, 10);
    if (argc >= 6 && strcmp(argv[4], "fail-selection") == 0) fail_selection = strtoull(argv[5], NULL, 10);
    bool injected = fail_qbuf || fail_selection;
    if (argc >= 6 && strcmp(argv[4], "stage-clock") == 0) stage_clock = true;
    gui_pid = getpid();
    vendor_ioctl = fake_ioctl;
    verify_every = 1;
    partial_config();

    int video = (int)syscall(SYS_memfd_create, "fake-ge2d-source", 0);
    if (video < 0 || ftruncate(video, SRC_W * SRC_H * 4) != 0) return 3;
    source = mmap(NULL, SRC_W * SRC_H * 4, PROT_READ | PROT_WRITE, MAP_SHARED, video, 0);
    if (source == MAP_FAILED) return 3;
    static uint8_t descriptors[2][0x28] __attribute__((aligned(16)));
    for (int d = 0; d < 2; d++) {
        dests[d] = malloc((size_t)SRC_W * SRC_H * 4);
        if (!dests[d]) return 3;
        for (int i = 0; i < SRC_W * SRC_H; i++) dests[d][i] = 0xdead0000u + (uint32_t)i;
        int32_t fd = dest_fds[d], fb = 31 + d;
        memcpy(descriptors[d] + 0x18, &dests[d], sizeof dests[d]);
        memcpy(descriptors[d] + 0x20, &fb, 4);
        memcpy(descriptors[d] + 0x24, &fd, 4);
    }
    for (int i = 0; i < SRC_W * SRC_H; i++) source[i] = 0x01000193u * (uint32_t)i;

    uint32_t *before = malloc((size_t)SRC_W * SRC_H * 4);
    if (!before) return 3;
    int bad = 0, unchecked = 0, cpu_wrote_after_failed_qbuf = 0;
    /* A failed vendor submission of a GE2D job (not a CPU token) has already
     * cleared that destination's pending damage: not checked until it is
     * whole again. */
    bool lost[2] = {false, false};
    unsigned long long kinds[3] = {0}; /* medium, GE2D plan, full frame */
    for (int f = 0; f < frames; f++) {
        int painted = paint_some(f);
        int d = f & 1;
        current = d, cap_dest = dest_fds[d], current_descriptor = descriptors[d];
        saved_capture = (struct v4l2_buffer){.type = V4L2_BUF_TYPE_VIDEO_CAPTURE, .memory = V4L2_MEMORY_DMABUF};
        saved_capture.m.fd = dest_fds[d];
        struct v4l2_buffer output = {.type = V4L2_BUF_TYPE_VIDEO_OUTPUT, .memory = V4L2_MEMORY_MMAP};
        if (injected) memcpy(before, dests[d], (size_t)SRC_W * SRC_H * 4);
        int result = partial_output_qbuf(video, VIDIOC_QBUF, &output);
        const char *kind = record.kind ? record.kind : "?", *reason = record.reason ? record.reason : "?";
        bool on_cpu = strcmp(kind, "cpu") == 0, plan = strcmp(kind, "partial") == 0;
        if (result == 0) {
            /* The vendor's capture DQBUF: the log line and the module's own check. */
            convert_line((int64_t)(now_ns() - record.last_start));
            if (verify_due) partial_verify_now(video);
            verify_due = false;
        }
        kinds[strcmp(reason, "medium") == 0 ? 0 : plan ? 1 : 2]++;
        long mismatches = mismatches_in(dests[d]);
        if (result != 0 && on_cpu) {
            /* The token job was not accepted: no CPU pixel may be written,
             * and both destinations are refreshed at their next conversion. */
            if (memcmp(before, dests[d], (size_t)SRC_W * SRC_H * 4) != 0) cpu_wrote_after_failed_qbuf++;
        } else if (result != 0) {
            lost[d] = true, unchecked++;
        } else if (mismatches && lost[d]) {
            unchecked++;
        } else {
            if (mismatches) bad++;
            lost[d] = false;
        }
        printf("frame %d dest %d paint %d result %d kind %s reason %s mismatches %ld hash %016llx\n", f, d, painted,
               result, kind, reason, mismatches, (unsigned long long)hash_of(dests[d]));
    }
    printf("frames %d bad %d unchecked %d medium %llu ge2d_plan %llu full %llu\n", frames, bad, unchecked, kinds[0],
           kinds[1], kinds[2]);
    printf("ge2d jobs %llu pixels %llu selections %llu ioctls %llu extra_jobs %llu cpu_frames %llu cpu_fail %llu "
           "soft_budget_exceeded %llu disabled %s\n", jobs_run, job_pixels, selections, calls,
           (unsigned long long)extra_jobs, (unsigned long long)cpu_frames, (unsigned long long)cpu_fail,
           (unsigned long long)soft_budget_exceeded, disabled ? disabled_why : "-");
    log_fd = STDOUT_FILENO, capture = true;
    write_counters();
    log_fd = -1, capture = false;
    fputs(captured, stdout);
    if (bad_compose) printf("a compose rectangle was not the rotated crop\n");
    if (cpu_wrote_after_failed_qbuf) printf("CPU pixels written after a failed token QBUF\n");
    /* Without an injected failure nothing may be disabled or fail. */
    bool clean = !bad && !bad_compose && !cpu_wrote_after_failed_qbuf && (injected || (!disabled && !cpu_fail));
    return clean ? 0 : 1;
}
