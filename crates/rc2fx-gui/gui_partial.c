/* Partial GE2D conversion (G3), included by gui_pacing.c when built with
 * -DRC2FX_PACING_DIRTY -DRC2FX_PACING_PARTIAL (`cargo xtask mixer-build`
 * adds -DRC2FX_GUI_RECT_PLAN3, the three-job planner).
 *
 * Modes: "partial-cw" / "partial-ccw" (the rotation GE2D applies) and
 * "partial-verify-cw" / "partial-verify-ccw": the same, plus a whole-frame
 * pixel check after the first PARTIAL_VERIFY_FIRST partial conversions into
 * each destination and every PARTIAL_VERIFY_EVERY-th after that. Any
 * mismatch ends partial conversion for good (reason=verify-fail); each check
 * also reports which rotation the destination matches at sample points
 * where the two predict different pixels, so one run settles the
 * orientation.
 *
 * Each paint's rectangles (read at the paint lock seam, holding the repaint
 * mutex) join a pending list per destination; both destinations get them,
 * since the two alternate. At the vendor's conversion into destination D
 * (holding the same mutex), the pending list is merged greedily (the pair
 * whose bounding box adds the fewest pixels, while that is under
 * PARTIAL_MERGE_PX, then down to PARTIAL_MAX_JOBS) and converted as that many
 * GE2D jobs: all but the last are run here, at the vendor's output QBUF,
 * each as S_SELECTION (OUTPUT CROP, CAPTURE COMPOSE) → QBUF output → poll →
 * DQBUF output and capture → re-QBUF of the vendor's capture buffer; the
 * last one is the vendor's own job, with its selection set. A full frame is
 * converted instead on the first conversion into each destination, after a
 * rejected paint, when merged area exceeds PARTIAL_FULL_PERCENT, and after
 * errors. List overflow collapses the pending set into one bounding box.
 * The baseline budget merges remaining jobs and falls back to full when that
 * box exceeds the area cap. With RC2FX_GUI_RECT_PLAN3, the cap is three, the default
 * optional merge threshold is 20000 pixels, and the extra-job budget is soft:
 * all planned jobs still run; a counter records an overrun. This is a job
 * count bound, not a time bound (each job can poll for up to two seconds).
 * There is no periodic refresh: the whole-frame verify
 * (below) is what establishes that pending rectangles cover every source
 * change. A file named "partial-off" next to
 * the log disables partial conversion for good at the next conversion.
 * One "convert" line per conversion: destination, kind, reason, jobs,
 * pixels and each job's time.
 *
 * RC2FX_GUI_PARTIAL_MERGE_PX overrides PARTIAL_MERGE_PX: decimal digits only,
 * 0..1000000; anything else keeps the default and logs "merge-px-invalid"
 * once. The start line carries the effective value. In quiet mode the
 * convert and verify lines give way to "fallback" lines (a full frame for a
 * reason other than "forced" or "area" while partial conversion is still on;
 * at most PARTIAL_FALLBACK_LINES), one "disable" line when partial conversion
 * ends for good, and the periodic "counters" line.
 *
 * The counters line ends with plan-shape and size fields (README.md lists
 * them): each conversion counted in one path (path_*, which sum to
 * conversions), GE2D plans by job count and by total pixels, pixels through
 * GE2D and through the CPU, and the CPU rotations' time by kind and by stage.
 * They are cumulative integers added on the pump thread; the stage times read
 * CLOCK_MONOTONIC three times per 16-row strip.
 */
#include <linux/videodev2.h>
#include <poll.h>
#include <sys/mman.h>

#include "rotate/rotate.c"


/* G1 fixes these: per-job overhead / per-pixel time, and the caps. */
/* Measured on the device 2026-10-31: a GE2D job costs about 4.3 ms fixed
 * plus about 45 ns per pixel, so a merge that adds N pixels pays while
 * N * 45 ns < 4.3 ms, i.e. N < ~96k; 80k keeps a margin. */
#ifndef PARTIAL_MERGE_PX
#ifdef RC2FX_GUI_RECT_PLAN3
#define PARTIAL_MERGE_PX 20000
#else
#define PARTIAL_MERGE_PX 80000
#endif
#endif
#define PARTIAL_MERGE_PX_MAX 1000000
#ifndef PARTIAL_MAX_JOBS
#define PARTIAL_MAX_JOBS 4
#endif
#ifdef RC2FX_GUI_RECT_PLAN3
#if PARTIAL_MAX_JOBS < 3
#error "RC2FX_GUI_RECT_PLAN3 requires at least three job slots"
#endif
#define PARTIAL_PLAN_JOBS 3
#define PARTIAL_POLICY "plan3"
#else
#define PARTIAL_PLAN_JOBS PARTIAL_MAX_JOBS
#define PARTIAL_POLICY "baseline"
#endif
#ifndef PARTIAL_BUDGET_NS
#define PARTIAL_BUDGET_NS 8000000ull
#endif
#ifndef PARTIAL_FULL_PERCENT
#define PARTIAL_FULL_PERCENT 50
#endif
#ifndef PARTIAL_VERIFY_EVERY
#define PARTIAL_VERIFY_EVERY 50
#endif
#ifndef PARTIAL_VERIFY_FIRST
#define PARTIAL_VERIFY_FIRST 4
#endif
#define PARTIAL_LIST 32
#if defined(RC2FX_PACING_TEST) && defined(RC2FX_TEST_PARTIAL_LINES)
#define PARTIAL_LINES RC2FX_TEST_PARTIAL_LINES
#else
#define PARTIAL_LINES 20000
#endif
#define PARTIAL_FALLBACK_LINES 1000
/* The counters line counts the GE2D plans of two or more jobs up to this many
 * pixels (plan_multi_le8192). A rule that rotated those plans on the CPU was
 * tried and dropped; the count records how many plans it would have covered. */
#define PLAN_SMALL_PX 8192
/* The counters line's buffer. A line that does not fit is not written, so
 * this is about twice the line's length with every counter at its largest. */
#define COUNTERS_LINE_MAX 4096
#define SRC_W 1280
#define SRC_H 720
#define DST_PITCH (SRC_H * 4)

#ifdef RC2FX_PACING_TEST
#define CAPTURE_QBUF_SITE(r) in_function(r, "test_capture_qbuf_site")
#define OUTPUT_QBUF_SITE(r) in_function(r, "test_output_qbuf_site")
#define CAPTURE_DQBUF_SITE(r) in_function(r, "test_capture_dqbuf_site")
#define OUTPUT_DQBUF_SITE(r) in_function(r, "test_output_dqbuf_site")
#else
#define CAPTURE_QBUF_SITE(r) ((r) - main_base == 0x7d05c0u)
#define OUTPUT_QBUF_SITE(r) ((r) - main_base == 0x7d0638u)
#define CAPTURE_DQBUF_SITE(r) ((r) - main_base == 0x7d080cu)
#define OUTPUT_DQBUF_SITE(r) ((r) - main_base == 0x7d0820u)
#endif

typedef int (*ioctl_fn)(int, unsigned long, ...);
static ioctl_fn vendor_ioctl;
static bool partial_on, partial_verify;
static enum rot_dir partial_dir;
static char kill_path[256];
static uint64_t budget_ns = PARTIAL_BUDGET_NS;
static unsigned verify_every = PARTIAL_VERIFY_EVERY;
static long long merge_px = PARTIAL_MERGE_PX;

struct pending {
    int fd; /* destination DMABUF fd; -1 unassigned */
    int n;
    bool force;
    unsigned partials; /* partial conversions into this destination */
    struct rot_rect r[PARTIAL_LIST];
};
/* Paint thread and pump thread, both holding the repaint mutex. */
static struct pending slots[2] = {{.fd = -1, .force = true}, {.fd = -1, .force = true}};
/* Large redraws always use full CPU rotation. Small damage stays on GE2D. */
#ifdef RC2FX_PACING_TEST
/* Host-only fixture controls, absent from the production module. */
static bool test_cpu_rotation;
static char force_path[192];
static uint64_t last_forced_ns;
static bool force_full;
#define FORCE_FULL_EVERY_NS 500000000ull
#endif
#define CPU_STRIP 16
static uint64_t cpu_frames, cpu_ns, cpu_max_ns, cpu_fail;
static bool disabled; /* permanently full frame */
static const char *disabled_why = "";
/* Pump thread only. */
static int video_fd = -1, cap_dest = -1;
static struct v4l2_buffer saved_capture;
static bool capture_saved;
static struct rot_rect selected = {0, 0, SRC_W, SRC_H};
static bool selection_known = true;
static uint64_t conversions;
static bool verify_due; /* the conversion being completed is to be checked */
static _Atomic uint64_t convert_lines;
static uint32_t *verify_source;
static size_t verify_source_len;
/* Periodic counters (pump thread only): conversions above, partial and
 * full ones, full ones by reason, extra jobs run, and GE2D time (extra jobs
 * plus the vendor's job, QBUF to its capture DQBUF). */
static const char *const full_reasons[] = {"forced", "area", "clock", "budget", "selection", "selection-or-qbuf",
                                           "job-failed", "kill", "verify-fail", "third-destination",
                                           "full-selection", "forced-full",
                                           "other"};
#define FULL_REASONS (sizeof full_reasons / sizeof full_reasons[0])
static uint64_t partial_count, full_count, full_by[FULL_REASONS], extra_jobs, ge2d_ns;
/* Partial conversions whose remaining jobs were merged into one bounding box
 * because the extra jobs ran past PARTIAL_BUDGET_NS. */
static uint64_t budget_merged, soft_budget_exceeded;
static uint64_t fallback_lines;
/* Plan shapes and sizes for the counters line (pump thread only, cumulative).
 * Every conversion is counted in exactly one path, where its convert line is
 * opened. */
enum { PATH_GE2D_PLAN, PATH_CPU_MEDIUM, PATH_CPU_FULL, PATH_GE2D_FULL, PATHS };
static uint64_t path_count[PATHS];
/* GE2D plans: by job count, by total pixels (up to each edge, then above the
 * last), those of two or more jobs up to PLAN_SMALL_PX, and those that are
 * the token job of a conversion with nothing pending. */
#define PLAN_PX_BUCKETS 7
static const long long plan_px_edges[PLAN_PX_BUCKETS - 1] = {1024, 2048, 4096, 8192, 16384, 32768};
static uint64_t plan_jobs[PARTIAL_MAX_JOBS], plan_px_hist[PLAN_PX_BUCKETS], plan_multi_small, plan_empty;
/* Pixels in the GE2D jobs the driver accepted, pixels the CPU rotated, and
 * pixels the planner's merges added to converted plans. */
static uint64_t ge2d_px_total, cpu_px_total, merge_added_px;
static long long merge_added; /* by the latest merge(); counted when its plan is converted */
/* Completed CPU rotations by kind, and all of them by stage. */
enum { CPU_FULL, CPU_MEDIUM, CPU_KINDS };
static uint64_t cpu_kind_frames[CPU_KINDS], cpu_kind_ns[CPU_KINDS];
static uint64_t cpu_last_ns, cpu_stage_ns, cpu_rotate_ns, cpu_yield_ns;

static bool parse_partial_mode(const char *mode) {
    static const struct {
        const char *name;
        enum rot_dir dir;
        bool verify;
    } modes[] = {{"partial-cw", ROT_CW, false}, {"partial-ccw", ROT_CCW, false},
                 {"partial-verify-cw", ROT_CW, true}, {"partial-verify-ccw", ROT_CCW, true}};
    for (size_t i = 0; i < sizeof modes / sizeof modes[0]; i++)
        if (strcmp(mode, modes[i].name) == 0) {
            partial_on = true, partial_dir = modes[i].dir, partial_verify = modes[i].verify;
            return true;
        }
    return false;
}

/* The kill switch: <directory of the log>/partial-off. */
static void partial_setup(const char *log) {
    if (log) {
        const char *slash = strrchr(log, '/');
        size_t dir = slash ? (size_t)(slash - log) : 0;
        if (dir && dir + sizeof "/partial-off" < sizeof kill_path) {
            memcpy(kill_path, log, dir);
            memcpy(kill_path + dir, "/partial-off", sizeof "/partial-off");
        }
    }
}

#ifdef RC2FX_PACING_TEST
/* Host test of the counters line's length: every counter starts within 2^32
 * of the largest 64-bit value. */
static void test_huge_counters(void) {
    const uint64_t huge = UINT64_MAX - (1ull << 32);
    conversions = partial_count = full_count = extra_jobs = ge2d_ns = budget_merged = soft_budget_exceeded = huge;
    cpu_frames = cpu_ns = cpu_max_ns = cpu_fail = huge;
    plan_multi_small = plan_empty = ge2d_px_total = cpu_px_total = merge_added_px = huge;
    cpu_stage_ns = cpu_rotate_ns = cpu_yield_ns = huge;
    for (size_t i = 0; i < FULL_REASONS; i++) full_by[i] = huge;
    for (int i = 0; i < PATHS; i++) path_count[i] = huge;
    for (int i = 0; i < PARTIAL_MAX_JOBS; i++) plan_jobs[i] = huge;
    for (int i = 0; i < PLAN_PX_BUCKETS; i++) plan_px_hist[i] = huge;
    for (int i = 0; i < CPU_KINDS; i++) cpu_kind_frames[i] = cpu_kind_ns[i] = huge;
    atomic_store(&passes, huge), atomic_store(&paints, huge), atomic_store(&paint_rejects, huge);
    atomic_store(&log_rotations, huge), atomic_store(&log_dropped, huge), atomic_store(&log_reopen_failed, huge);
}
#endif

/* Once, after the log is open: the merge override and the test knobs. */
static void partial_config(void) {
    const char *v = getenv("RC2FX_GUI_PARTIAL_MERGE_PX");
    if (v) {
        long long value = 0;
        size_t i = 0;
        while (i < 8 && v[i] >= '0' && v[i] <= '9') value = value * 10 + (v[i++] - '0');
        if (i > 0 && !v[i] && value <= PARTIAL_MERGE_PX_MAX) {
            merge_px = value;
        } else {
            char shown[33];
            size_t n = 0;
            for (; v[n] && n < sizeof shown - 1; n++) {
                char c = v[n];
                shown[n] = (c >= '0' && c <= '9') || (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') ||
                                   c == '-' || c == '+' || c == '.' || c == '_'
                               ? c
                               : '?';
            }
            shown[n] = 0;
            char line[160];
            int len = snprintf(line, sizeof line, "merge-px-invalid pid=%d t=%llu value=%s using=%lld\n",
                               (int)getpid(), (unsigned long long)now_ns(), n ? shown : "-", merge_px);
            if (len > 0 && (size_t)len < sizeof line) log_line(line, (size_t)len);
        }
    }
#ifdef RC2FX_PACING_TEST
    test_cpu_rotation = getenv("RC2FX_TEST_CPU_ROTATE") != NULL;
    if ((v = getenv("RC2FX_TEST_FORCE_FULL_FILE")) && strlen(v) + 1 < sizeof force_path)
        memcpy(force_path, v, strlen(v) + 1);
    if ((v = getenv("RC2FX_TEST_PARTIAL_BUDGET_US"))) budget_ns = strtoull(v, NULL, 10) * 1000ull;
    if ((v = getenv("RC2FX_TEST_PARTIAL_VERIFY_EVERY"))) verify_every = (unsigned)strtoul(v, NULL, 10);
    if (getenv("RC2FX_TEST_COUNTERS_HUGE")) test_huge_counters();
#endif
}

static struct rot_rect bbox2(struct rot_rect a, struct rot_rect b) {
    int x0 = a.x < b.x ? a.x : b.x, y0 = a.y < b.y ? a.y : b.y;
    int x1 = a.x + a.w > b.x + b.w ? a.x + a.w : b.x + b.w, y1 = a.y + a.h > b.y + b.h ? a.y + a.h : b.y + b.h;
    return (struct rot_rect){x0, y0, x1 - x0, y1 - y0};
}
static long long area_of(struct rot_rect r) { return (long long)r.w * r.h; }

static void pending_add(struct pending *p, struct rot_rect r) {
    if (p->force) return; /* a full frame comes next anyway */
    if (p->n == PARTIAL_LIST) {
        struct rot_rect b = r;
        for (int i = 0; i < p->n; i++) b = bbox2(b, p->r[i]);
        p->r[0] = b, p->n = 1;
        return;
    }
    p->r[p->n++] = r;
}

/* Paint seam: the paint's rectangles, clipped to the source. */
static void partial_paint(const int32_t *list, int32_t count) {
    if (!partial_on) return;
    for (int32_t i = 0; i < count; i++) {
        int x0 = list[4 * i], y0 = list[4 * i + 1];
        long long x1 = (long long)x0 + list[4 * i + 2], y1 = (long long)y0 + list[4 * i + 3];
        if (x0 < 0) x0 = 0;
        if (y0 < 0) y0 = 0;
        if (x1 > SRC_W) x1 = SRC_W;
        if (y1 > SRC_H) y1 = SRC_H;
        if (x1 <= x0 || y1 <= y0) continue;
        struct rot_rect r = {x0, y0, (int)(x1 - x0), (int)(y1 - y0)};
        for (int s = 0; s < 2; s++) pending_add(&slots[s], r);
    }
}
static void partial_paint_rejected(void) {
    for (int s = 0; s < 2; s++) slots[s].force = true;
}

/* Greedy minimum-extra-area merge in place; returns at most the policy cap.
 * merge_added is left as the pixels the merges added: each bounding box less
 * its two rectangles, where that is positive. */
static int merge(struct rot_rect *r, int n) {
    merge_added = 0;
    while (n > 1) {
        /* Overlapping pairs add negative pixels: no sentinel value. */
        long long best = 0;
        int bi = 0, bj = 1;
        bool found = false;
        for (int i = 0; i < n; i++)
            for (int j = i + 1; j < n; j++) {
                long long extra = area_of(bbox2(r[i], r[j])) - area_of(r[i]) - area_of(r[j]);
                if (!found || extra < best) best = extra, bi = i, bj = j, found = true;
            }
        if (best >= merge_px && n <= PARTIAL_PLAN_JOBS) break;
        if (best > 0) merge_added += best;
        r[bi] = bbox2(r[bi], r[bj]);
        r[bj] = r[--n];
    }
    return n;
}

/* The conversion being logged (pump thread): written once its last job
 * (the vendor's) is dequeued, or at the next conversion if it never was. */
static struct {
    bool open;
    const char *kind, *reason;
    int dest, jobs;
    long long px, cpu_px, job_px[PARTIAL_MAX_JOBS];
    uint64_t job_ns[PARTIAL_MAX_JOBS], extra_ns, last_start;
} record;

static void convert_line(int64_t last_ns) {
    if (!record.open) return;
    record.open = false;
    if (last_ns >= 0) ge2d_ns += (uint64_t)last_ns;
    if (log_fd < 0 || quiet || atomic_fetch_add_explicit(&convert_lines, 1, memory_order_relaxed) >= PARTIAL_LINES) return;
    char times[PARTIAL_MAX_JOBS * 12 + 8] = "", pixels[PARTIAL_MAX_JOBS * 12 + 8] = "";
    size_t used = 0, pused = 0;
    for (int i = 0; i < record.jobs - 1 && i < PARTIAL_MAX_JOBS; i++) {
        int n = snprintf(times + used, sizeof times - used, "%llu,", (unsigned long long)(record.job_ns[i] / 1000));
        if (n > 0 && (size_t)n < sizeof times - used) used += (size_t)n;
    }
    for (int i = 0; i < record.jobs && i < PARTIAL_MAX_JOBS; i++) {
        int n = snprintf(pixels + pused, sizeof pixels - pused, "%s%lld", i ? "," : "", record.job_px[i]);
        if (n > 0 && (size_t)n < sizeof pixels - pused) pused += (size_t)n;
    }
    /* The last job is the vendor's: -1 if it was never dequeued. */
    snprintf(times + used, sizeof times - used, "%lld", last_ns < 0 ? -1LL : (long long)(last_ns / 1000));
    char line[384];
    int n = snprintf(line, sizeof line, "convert pid=%d t=%llu dest_fd=%d kind=%s reason=%s jobs=%d px=%lld "
                     "job_px=%s job_us=%s extra_us=%llu cpu_px=%lld\n", (int)gui_pid, (unsigned long long)now_ns(),
                     record.dest, record.kind, record.reason, record.jobs, record.px, pixels, times,
                     (unsigned long long)(record.extra_ns / 1000), record.cpu_px);
    if (n > 0 && (size_t)n < sizeof line) log_line(line, (size_t)n);
}
static void convert_open(const char *kind, const char *reason, int jobs, long long px, const uint64_t *job_ns,
                         const long long *job_px, uint64_t extra_ns) {
    convert_line(-1); /* the previous one, if its dequeue never came */
    record.open = true, record.kind = kind, record.reason = reason, record.dest = cap_dest;
    record.jobs = jobs, record.px = px, record.extra_ns = extra_ns, record.cpu_px = 0;
    memcpy(record.job_ns, job_ns, sizeof record.job_ns);
    memcpy(record.job_px, job_px, sizeof record.job_px);
    record.last_start = now_ns();
}

static int select_rect(int fd, struct rot_rect r) {
    if (selection_known && memcmp(&r, &selected, sizeof r) == 0) return 0;
    struct rot_rect d = rot_dst_rect(r, SRC_W, SRC_H, partial_dir);
    struct v4l2_selection crop = {.type = V4L2_BUF_TYPE_VIDEO_OUTPUT, .target = V4L2_SEL_TGT_CROP};
    struct v4l2_selection compose = {.type = V4L2_BUF_TYPE_VIDEO_CAPTURE, .target = V4L2_SEL_TGT_COMPOSE};
    crop.r = (struct v4l2_rect){r.x, r.y, (uint32_t)r.w, (uint32_t)r.h};
    compose.r = (struct v4l2_rect){d.x, d.y, (uint32_t)d.w, (uint32_t)d.h};
    selection_known = false; /* until both succeed */
    if (vendor_ioctl(fd, VIDIOC_S_SELECTION, &crop) != 0 || vendor_ioctl(fd, VIDIOC_S_SELECTION, &compose) != 0)
        return -1;
    selected = r, selection_known = true;
    return 0;
}

/* Quiet mode: one line for a fallback or the disable, with its reason. */
static void event_line(const char *kind, const char *reason) {
    char line[160];
    int n = snprintf(line, sizeof line, "%s pid=%d t=%llu reason=%s conversions=%llu\n", kind, (int)gui_pid,
                     (unsigned long long)now_ns(), reason, (unsigned long long)conversions);
    if (n > 0 && (size_t)n < sizeof line) log_line(line, (size_t)n);
}

static void disable(const char *why) {
    if (disabled) return;
    disabled = true, disabled_why = why;
    if (quiet) event_line("disable", why);
}

/* A full frame: counted by reason; in quiet mode, a fallback (not the
 * routine "forced" / "area", and not after the disable, already logged)
 * gets a line. */
static void count_full(const char *reason) {
    size_t i = 0;
    while (i < FULL_REASONS - 1 && strcmp(full_reasons[i], reason) != 0) i++;
    full_count++, full_by[i]++;
    if (quiet && !disabled && i > 1 && fallback_lines++ < PARTIAL_FALLBACK_LINES) event_line("fallback", reason);
}

/* A GE2D plan, as it is converted: its path, job count and size. */
static void count_plan(int jobs, long long px, bool nothing_pending) {
    path_count[PATH_GE2D_PLAN]++;
    plan_jobs[(jobs < 1 ? 1 : jobs > PARTIAL_MAX_JOBS ? PARTIAL_MAX_JOBS : jobs) - 1]++;
    int bucket = 0;
    while (bucket < PLAN_PX_BUCKETS - 1 && px > plan_px_edges[bucket]) bucket++;
    plan_px_hist[bucket]++;
    if (jobs >= 2 && px <= PLAN_SMALL_PX) plan_multi_small++;
    if (nothing_pending) plan_empty++;
    merge_added_px += (uint64_t)merge_added;
}

/* "a,b,c" for a counters field; `out` holds 21 bytes per value and one more. */
static void counter_list(char *out, size_t size, const uint64_t *values, int count) {
    size_t used = 0;
    out[0] = 0;
    for (int i = 0; i < count; i++) {
        int n = snprintf(out + used, size - used, "%s%llu", i ? "," : "", (unsigned long long)values[i]);
        if (n > 0 && (size_t)n < size - used) used += (size_t)n;
    }
}

/* Quiet mode's periodic line. New fields go at the end, after
 * log_reopen_failed, so a reader of the earlier fields is not disturbed. The
 * line is built in a static buffer, off the pump thread's stack: the pump is
 * the only thread that writes it. */
static void write_counters(void) {
    if (log_fd < 0) return;
    char reasons[FULL_REASONS * 40 + 2];
    size_t used = 0;
    reasons[0] = 0;
    for (size_t i = 0; i < FULL_REASONS; i++) {
        if (!full_by[i]) continue;
        int n = snprintf(reasons + used, sizeof reasons - used, "%s%s:%llu", used ? "," : "", full_reasons[i],
                         (unsigned long long)full_by[i]);
        if (n > 0 && (size_t)n < sizeof reasons - used) used += (size_t)n;
    }
    char by_jobs[PARTIAL_MAX_JOBS * 21 + 1], by_px[PLAN_PX_BUCKETS * 21 + 1];
    counter_list(by_jobs, sizeof by_jobs, plan_jobs, PARTIAL_PLAN_JOBS);
    counter_list(by_px, sizeof by_px, plan_px_hist, PLAN_PX_BUCKETS);
    static char line[COUNTERS_LINE_MAX];
    int n = snprintf(line, sizeof line, "counters pid=%d t=%llu passes=%llu conversions=%llu partial=%llu full=%llu "
                     "full_by_reason=%s budget_merged=%llu planner_policy=%s merge_px=%lld budget_ns=%llu "
                     "soft_budget_exceeded=%llu "
                     "extra_jobs=%llu ge2d_us=%llu disabled=%s "
                     "cpu_frames=%llu cpu_us=%llu cpu_max_us=%llu cpu_fail=%llu "
                     "log_rotations=%llu log_dropped=%llu log_reopen_failed=%llu "
                     "path_ge2d_plan=%llu path_cpu_medium=%llu path_cpu_full=%llu path_ge2d_full=%llu "
                     "plan_jobs=%s plan_px_hist=%s plan_multi_le8192=%llu "
                     "plan_empty=%llu ge2d_px=%llu cpu_px=%llu merge_added_px=%llu "
                     "cpu_full_frames=%llu cpu_full_us=%llu cpu_medium_frames=%llu cpu_medium_us=%llu "
                     "cpu_stage_us=%llu cpu_rotate_us=%llu cpu_yield_us=%llu "
                     "paints=%llu paint_rejects=%llu\n", (int)gui_pid,
                     (unsigned long long)now_ns(), (unsigned long long)passes, (unsigned long long)conversions,
                     (unsigned long long)partial_count, (unsigned long long)full_count, used ? reasons : "-",
                     (unsigned long long)budget_merged, PARTIAL_POLICY, merge_px, (unsigned long long)budget_ns,
                     (unsigned long long)soft_budget_exceeded,
                     (unsigned long long)extra_jobs,
                     (unsigned long long)(ge2d_ns / 1000), disabled ? disabled_why : "-",
                     (unsigned long long)cpu_frames,
                     (unsigned long long)(cpu_ns / 1000),
                     (unsigned long long)(cpu_max_ns / 1000), (unsigned long long)cpu_fail,
                     (unsigned long long)atomic_load(&log_rotations), (unsigned long long)atomic_load(&log_dropped),
                     (unsigned long long)atomic_load(&log_reopen_failed),
                     (unsigned long long)path_count[PATH_GE2D_PLAN], (unsigned long long)path_count[PATH_CPU_MEDIUM],
                     (unsigned long long)path_count[PATH_CPU_FULL], (unsigned long long)path_count[PATH_GE2D_FULL],
                     by_jobs, by_px, (unsigned long long)plan_multi_small, (unsigned long long)plan_empty,
                     (unsigned long long)ge2d_px_total, (unsigned long long)cpu_px_total,
                     (unsigned long long)merge_added_px,
                     (unsigned long long)cpu_kind_frames[CPU_FULL], (unsigned long long)(cpu_kind_ns[CPU_FULL] / 1000),
                     (unsigned long long)cpu_kind_frames[CPU_MEDIUM],
                     (unsigned long long)(cpu_kind_ns[CPU_MEDIUM] / 1000),
                     (unsigned long long)(cpu_stage_ns / 1000), (unsigned long long)(cpu_rotate_ns / 1000),
                     (unsigned long long)(cpu_yield_ns / 1000),
                     (unsigned long long)atomic_load(&paints), (unsigned long long)atomic_load(&paint_rejects));
    if (n > 0 && (size_t)n < sizeof line) log_line(line, (size_t)n);
}

/* One extra job for rectangle r (the vendor's capture buffer is queued). */
static int extra_job(int fd, struct v4l2_buffer *output, struct rot_rect r) {
    if (select_rect(fd, r) != 0) return -1; /* nothing queued: a full frame is still possible */
    struct v4l2_buffer out = *output;
    if (vendor_ioctl(fd, VIDIOC_QBUF, &out) != 0) return -1;
    struct pollfd p = {.fd = fd, .events = POLLIN};
    struct timespec limit = {2, 0}; /* as the vendor's select */
    long ready;
    do ready = syscall(SYS_ppoll, &p, 1, &limit, NULL, 0);
    while (ready < 0 && errno == EINTR);
    struct v4l2_buffer dq_out = {.type = V4L2_BUF_TYPE_VIDEO_OUTPUT, .memory = output->memory};
    struct v4l2_buffer dq_cap = {.type = V4L2_BUF_TYPE_VIDEO_CAPTURE, .memory = saved_capture.memory};
    struct v4l2_buffer requeue = saved_capture;
    bool got_cap = false, got_out = false;
    if (ready > 0) got_cap = vendor_ioctl(fd, VIDIOC_DQBUF, &dq_cap) == 0;
    if (got_cap) got_out = vendor_ioctl(fd, VIDIOC_DQBUF, &dq_out) == 0;
    if (got_cap && got_out && vendor_ioctl(fd, VIDIOC_QBUF, &requeue) == 0) return 0;
    /* Best effort back to the vendor's state (its capture buffer queued,
     * the output not), then full frames for good. */
    disable("job-failed");
    if (!got_cap) got_cap = vendor_ioctl(fd, VIDIOC_DQBUF, &dq_cap) == 0;
    if (!got_out) (void)vendor_ioctl(fd, VIDIOC_DQBUF, &dq_out);
    requeue = saved_capture;
    if (got_cap) (void)vendor_ioctl(fd, VIDIOC_QBUF, &requeue);
    return -2;
}

/* The source mapping the rotation reads: our own read-only use of the OUTPUT
 * buffer. PROT_WRITE is required by vb2_mmap even for reads (see verify). */
static int ensure_source(int fd) {
    if (verify_source) return 0;
    struct v4l2_buffer b = {.type = V4L2_BUF_TYPE_VIDEO_OUTPUT, .memory = V4L2_MEMORY_MMAP, .index = 0};
    if (vendor_ioctl(fd, VIDIOC_QUERYBUF, &b) != 0 || b.length < SRC_W * SRC_H * 4) return -1;
    void *m = mmap(NULL, b.length, PROT_READ | PROT_WRITE, MAP_SHARED, fd, b.m.offset);
    if (m == MAP_FAILED) return -1;
    verify_source = m, verify_source_len = b.length;
    return 0;
}


/* Rotate planned damage in 16-row strips, first copying each strip
 * into cacheable staging with contiguous reads. The copy turns small
 * non-cacheable reads into 64-byte ones. This runs on the pump thread while
 * the vendor's token job completes its normal handshake. */
static _Alignas(64) uint32_t cpu_stage[CPU_STRIP * SRC_W];
/* A later clock reading less an earlier one; 0 if the clock failed. */
static uint64_t ns_between(uint64_t from, uint64_t to) { return to > from ? to - from : 0; }
static int cpu_rotate_regions(int fd, const struct rot_rect *regions, int count) {
    if (count <= 0) return -1;
    for (int i = 0; i < count; i++) {
        struct rot_rect r = regions[i];
        if (r.x < 0 || r.y < 0 || r.x >= SRC_W || r.y >= SRC_H || r.w <= 0 || r.h <= 0 ||
            r.w > SRC_W - r.x || r.h > SRC_H - r.y) return -1;
    }
    const uint8_t *const *global = destination_global();
    const uint8_t *descriptor = global ? *global : NULL;
    if (!descriptor) return -1;
    int32_t dfd;
    uint32_t *dst;
    memcpy(&dfd, descriptor + 0x24, 4);
    memcpy(&dst, descriptor + 0x18, sizeof dst);
    if (!dst || dfd != cap_dest) return -1;
    if (ensure_source(fd) != 0) return -1;
    uint64_t t0 = now_ns(), mark = t0;
    for (int i = 0; i < count; i++) {
        struct rot_rect r = regions[i];
        for (int y = r.y; y < r.y + r.h; y += CPU_STRIP) {
            int strip = CPU_STRIP < r.y + r.h - y ? CPU_STRIP : r.y + r.h - y;
            if (r.x == 0 && r.w == SRC_W) {
                memcpy(cpu_stage, (const void *)(verify_source + (size_t)y * SRC_W), (size_t)strip * SRC_W * 4);
            } else {
                for (int row = 0; row < strip; row++)
                    memcpy(cpu_stage + (size_t)row * SRC_W + r.x,
                           (const void *)(verify_source + (size_t)(y + row) * SRC_W + r.x),
                           (size_t)r.w * 4);
            }
            uint64_t staged = now_ns();
            /* The staged strip is rows 0..strip-1 of the buffer, but it belongs at
             * source row y, and rotate_rect derives the destination column from the
             * rect's row: CW writes column sh-1-y, so shrinking sh by y keeps the
             * column right; CCW writes column y itself, so the destination pointer
             * moves forward by y pixels (still inside the destination). */
            if (partial_dir == ROT_CW)
                rotate_rect(cpu_stage, SRC_W * 4, SRC_W, SRC_H - y, dst, DST_PITCH,
                            (struct rot_rect){r.x, 0, r.w, strip}, ROT_CW);
            else
                rotate_rect(cpu_stage, SRC_W * 4, SRC_W, SRC_H, dst + y, DST_PITCH,
                            (struct rot_rect){r.x, 0, r.w, strip}, ROT_CCW);
            record.cpu_px += (long long)r.w * strip;
            cpu_px_total += (uint64_t)r.w * (uint64_t)strip;
            uint64_t rotated = now_ns();
            /* Raw syscall: no libc call and no cancellation point on the pump. */
            (void)syscall(SYS_sched_yield);
            /* For the counters line: this strip's copy into the stage, its
             * rotation, and the time inside the yield. */
            uint64_t yielded = now_ns();
            cpu_stage_ns += ns_between(mark, staged), cpu_rotate_ns += ns_between(staged, rotated);
            cpu_yield_ns += ns_between(rotated, yielded);
            mark = yielded;
        }
    }
    uint64_t spent = now_ns() - t0;
    cpu_frames++, cpu_ns += spent, cpu_last_ns = spent;
    if (spent > cpu_max_ns) cpu_max_ns = spent;
    return 0;
}

/* Large deliberate redraws use the CPU; errors retain the vendor fallback.
 * The threshold is fixed, with no runtime mode or share selector. */
static bool cpu_eligible(const char *reason, long long pending_px) {
#ifdef RC2FX_PACING_TEST
    if (!test_cpu_rotation) return false;
#endif
    if (disabled || !reason) return false;
    /* A 32,880-pixel redraw job overlapped downstream audio failures.
     * Use the strip-staged CPU path for planned jobs at
     * least 32K pixels; smaller meter updates retain the compact GE2D plan. */
    if (strcmp(reason, "medium") == 0) return pending_px >= 32768;
    bool deliberate = strcmp(reason, "forced") == 0 || strcmp(reason, "forced-full") == 0 ||
                      strcmp(reason, "area") == 0 || strcmp(reason, "budget") == 0 ||
                      strcmp(reason, "third-destination") == 0;
    if (!deliberate) return false;
    long percent = 100;
    if (pending_px > 0) percent = pending_px * 100 / ((long long)SRC_W * SRC_H);
    return percent >= PARTIAL_FULL_PERCENT;
}

static int partial_output_qbuf(int fd, unsigned long request, struct v4l2_buffer *output) {
    const struct rot_rect full = {0, 0, SRC_W, SRC_H};
    uint64_t start = now_ns(), job_ns[PARTIAL_MAX_JOBS] = {0};
    long long job_px[PARTIAL_MAX_JOBS] = {0};
    conversions++;
    verify_due = false;
    bool dose = false;
#ifdef RC2FX_PACING_TEST
    uint64_t now = now_ns();
    force_full = force_path[0] && syscall(SYS_faccessat, AT_FDCWD, force_path, F_OK, 0) == 0;
    dose = force_full && now - last_forced_ns >= FORCE_FULL_EVERY_NS;
    if (dose) last_forced_ns = now;
#endif
    /* Raw syscalls here and below: the pump gains no cancellation point. */
    if (kill_path[0] && !disabled && syscall(SYS_faccessat, AT_FDCWD, kill_path, F_OK, 0) == 0) disable("kill");
    struct pending *p = NULL;
    for (int s = 0; s < 2 && !p; s++)
        if (slots[s].fd == cap_dest) p = &slots[s];
    for (int s = 0; s < 2 && !p; s++)
        if (slots[s].fd < 0) p = &slots[s], p->fd = cap_dest, p->force = true, p->n = 0;
    if (!p) disable("third-destination");
    const char *reason = disabled             ? disabled_why
                         : !start             ? "clock"
                         : dose               ? "forced-full"
                         : p->force           ? "forced"
                                              : NULL;
    struct rot_rect jobs[PARTIAL_LIST];
    int k = 0;
    long long px = 0;
    bool nothing_pending = false;
    if (!reason) {
        for (int i = 0; i < p->n; i++) jobs[i] = p->r[i];
        k = merge(jobs, p->n);
        for (int i = 0; i < k; i++) px += area_of(jobs[i]);
        if (px * 100 > (long long)SRC_W * SRC_H * PARTIAL_FULL_PERCENT) reason = "area";
        if (k == 0) { /* nothing changed: a token job */
            jobs[0] = (struct rot_rect){0, 0, 8, 8}, k = 1, px = 64;
            nothing_pending = true;
        }
#ifdef RC2FX_PACING_TEST
        if (getenv("RC2FX_TEST_PLAN_LOG")) {
            char line[PARTIAL_MAX_JOBS * 80 + 100];
            int used = snprintf(line, sizeof line, "test-plan policy=%s jobs=%d px=%lld rects=", PARTIAL_POLICY, k, px);
            for (int i = 0; i < k && used > 0 && (size_t)used < sizeof line; i++) {
                int n = snprintf(line + used, sizeof line - (size_t)used, "%s%d:%d:%d:%d", i ? ";" : "",
                                 jobs[i].x, jobs[i].y, jobs[i].w, jobs[i].h);
                if (n < 0 || (size_t)n >= sizeof line - (size_t)used) { used = -1; break; }
                used += n;
            }
            if (used > 0 && (size_t)used + 1 < sizeof line) {
                line[used++] = '\n';
                log_line(line, (size_t)used);
            }
        }
#endif
    }
    if (!reason) {
        for (int i = 0; i < k; i++) {
            if (cpu_eligible("medium", area_of(jobs[i]))) {
                reason = "medium";
                break;
            }
        }
    }
    if (!reason) {
        uint64_t spent = 0;
        bool merged = false;
#ifdef RC2FX_GUI_RECT_PLAN3
        bool soft_exceeded = false;
#endif
        for (int i = 0; i < k - 1; i++) {
            uint64_t t0 = now_ns();
            int r = extra_job(fd, output, jobs[i]);
            job_ns[i] = now_ns() - t0, spent += job_ns[i];
            extra_jobs++;
            if (r != -1) ge2d_px_total += (uint64_t)area_of(jobs[i]); /* the driver took the job */
#ifdef RC2FX_GUI_RECT_PLAN3
            if (spent > budget_ns && !soft_exceeded) soft_exceeded = true, soft_budget_exceeded++;
#endif
            if (r == -2) {
                reason = "job-failed";
                break;
            }
            if (r == -1) {
                reason = "selection-or-qbuf";
                break;
            }
#ifndef RC2FX_GUI_RECT_PLAN3
            if (spent > budget_ns && i < k - 2) {
                /* Out of time with jobs i+1..k-1 left: one bounding box over
                 * all of them becomes the final (vendor's) job. It covers
                 * every remaining pending rectangle, so the destination still
                 * ends up equal to the rotated source. A full frame only if
                 * that box is most of the frame anyway. */
                struct rot_rect box = jobs[i + 1];
                for (int j = i + 2; j < k; j++) box = bbox2(box, jobs[j]);
                if (area_of(box) * 100 > (long long)SRC_W * SRC_H * PARTIAL_FULL_PERCENT) {
                    reason = "budget";
                    break;
                }
                jobs[i + 1] = box, k = i + 2, merged = true;
                px = 0;
                for (int j = 0; j < k; j++) px += area_of(jobs[j]);
                break;
            }
#endif
        }
        ge2d_ns += spent;
        if (!reason && select_rect(fd, jobs[k - 1]) != 0) reason = "selection";
        if (!reason) {
            p->n = 0;
            p->partials++;
            partial_count++;
            if (merged) budget_merged++;
            verify_due = partial_verify && (p->partials <= PARTIAL_VERIFY_FIRST ||
                                            (verify_every && p->partials % verify_every == 0));
            for (int i = 0; i < k && i < PARTIAL_MAX_JOBS; i++) job_px[i] = area_of(jobs[i]);
            count_plan(k, px, nothing_pending);
            convert_open("partial", merged ? "budget-merged" : "-", k, px, job_ns, job_px, spent);
            int result = vendor_ioctl(fd, request, output);
            if (result == 0) ge2d_px_total += (uint64_t)area_of(jobs[k - 1]);
            return result;
        }
    }
    /* Either the full fallback or all planned CPU regions cover the pending damage. */
    if (p) p->n = 0, p->force = false;
    bool medium = strcmp(reason, "medium") == 0;
    if (medium) {
        p->partials++;
        partial_count++;
        verify_due = partial_verify && (p->partials <= PARTIAL_VERIFY_FIRST ||
                                        (verify_every && p->partials % verify_every == 0));
    } else {
        count_full(reason);
    }
    /* GE2D gets a 16x16 token job to preserve the vendor handshake. The CPU
     * rotates every planned region (or the full fallback) before present. */
    if (cpu_eligible(reason, px)) {
        const struct rot_rect ge2d_crop = {0, 0, 16, 16};
        if (select_rect(fd, ge2d_crop) != 0) {
            cpu_fail++;
            if (medium) {
                partial_count--;
                p->partials--;
                verify_due = false;
                reason = "selection";
                count_full(reason);
            }
        } else {
            job_px[0] = area_of(ge2d_crop);
            const int kind = medium ? CPU_MEDIUM : CPU_FULL;
            path_count[medium ? PATH_CPU_MEDIUM : PATH_CPU_FULL]++;
            if (medium) merge_added_px += (uint64_t)merge_added;
            convert_open("cpu", reason, 1, job_px[0], job_ns, job_px, 0);
            int result = vendor_ioctl(fd, request, output);
            if (result == 0) ge2d_px_total += (uint64_t)job_px[0];
            if (result != 0 || cpu_rotate_regions(fd, medium ? jobs : &full, medium ? k : 1) != 0) {
                cpu_fail++;
                /* A failed token submission must not consume pending damage.
                 * After caller/driver queue recovery, refresh both destinations.
                 * Do not touch destination pixels when submission failed. */
                for (int s = 0; s < 2; s++) slots[s].force = true;
            } else {
                cpu_kind_frames[kind]++, cpu_kind_ns[kind] += cpu_last_ns;
            }
            return result;
        }
    }
    if (select_rect(fd, full) != 0) {
        disable("full-selection");
    }
    job_px[0] = (long long)SRC_W * SRC_H;
    path_count[PATH_GE2D_FULL]++;
    convert_open("full", reason, 1, job_px[0], job_ns, job_px, 0);
    int result = vendor_ioctl(fd, request, output);
    if (result == 0) ge2d_px_total += (uint64_t)job_px[0];
    return result;
}

/* Debug mode: after the vendor's capture DQBUF, the destination (its CPU
 * mapping, descriptor +0x18) must equal the rotated source everywhere. The
 * source is our own read-only mapping of the OUTPUT buffer. Compared in
 * bands of 16 source columns through a cached copy. */
/* Why a scheduled check did not run; logged so a device log shows it. */
static void verify_skip(const char *reason, long dfd, int dst_known, int error) {
    if (quiet) return;
    char line[192];
    int n = snprintf(line, sizeof line, "verify-skip pid=%d t=%llu reason=%s dfd=%ld cap_dest=%d dst=%d errno=%d\n",
                     (int)gui_pid, (unsigned long long)now_ns(), reason, dfd, cap_dest, dst_known, error);
    if (n > 0 && (size_t)n < sizeof line) log_line(line, (size_t)n);
}

static void partial_verify_now(int fd) {
    const uint8_t *const *global = destination_global();
    const uint8_t *descriptor = global ? *global : NULL;
    if (!descriptor) return verify_skip("descriptor-null", -1, 0, 0);
    int32_t dfd;
    const uint32_t *dst;
    memcpy(&dfd, descriptor + 0x24, 4);
    memcpy(&dst, descriptor + 0x18, sizeof dst);
    if (dfd != cap_dest) return verify_skip("dest-mismatch", dfd, dst != NULL, 0);
    if (!dst) return verify_skip("dst-null", dfd, 0, 0);
    if (!verify_source) {
        struct v4l2_buffer b = {.type = V4L2_BUF_TYPE_VIDEO_OUTPUT, .memory = V4L2_MEMORY_MMAP, .index = 0};
        if (vendor_ioctl(fd, VIDIOC_QUERYBUF, &b) != 0 || b.length < SRC_W * SRC_H * 4)
            return verify_skip("querybuf", dfd, 1, errno);
        /* PROT_WRITE is required here even though only reads follow: vb2_mmap
         * rejects a mapping of an OUTPUT-queue buffer without VM_WRITE
         * ("VM_WRITE needed", -EINVAL) — found on the device, errno 22. The
         * module never writes through it; verify_source below is const. */
        const int prot = PROT_READ | PROT_WRITE;
#ifdef RC2FX_PACING_TEST
        /* The fake GE2D mirrors that rule unconditionally: it cannot
         * intercept the module's own mmap (the audit module has its own
         * namespace), so the test build applies it here. */
        if (!(prot & PROT_WRITE)) {
            errno = EINVAL;
            return verify_skip("mmap", dfd, 1, errno);
        }
#endif
        void *m = mmap(NULL, b.length, prot, MAP_SHARED, fd, b.m.offset);
        if (m == MAP_FAILED) return verify_skip("mmap", dfd, 1, errno);
        verify_source = m, verify_source_len = b.length;
    }
    static uint32_t band[16 * SRC_H], expect[16 * SRC_H];
    uint64_t t0 = now_ns(), mismatches = 0;
    int first_row = -1, first_col = -1;
    for (int x = 0; x < SRC_W; x += 16) {
        for (int y = 0; y < SRC_H; y++) memcpy(band + y * 16, verify_source + (size_t)y * SRC_W + x, 64);
        rotate_rect(band, 64, 16, SRC_H, expect, DST_PITCH, (struct rot_rect){0, 0, 16, SRC_H}, partial_dir);
        int row0 = partial_dir == ROT_CW ? x : SRC_W - 16 - x;
        for (int j = 0; j < 16; j++)
            for (int c = 0; c < SRC_H; c++)
                if (dst[(size_t)(row0 + j) * (DST_PITCH / 4) + c] != expect[j * SRC_H + c]) {
                    if (!mismatches++) first_row = row0 + j, first_col = c;
                }
    }
    /* Which rotation does the destination hold? Vote on a grid of points
     * where the two rotations predict different source pixels. */
    unsigned votes[2] = {0, 0}, deciding = 0;
    for (int row = 3; row < SRC_W; row += 20)
        for (int col = 5; col < SRC_H; col += 20) {
            uint32_t cw = verify_source[(size_t)(SRC_H - 1 - col) * SRC_W + row];
            uint32_t ccw = verify_source[(size_t)col * SRC_W + (SRC_W - 1 - row)];
            if (cw == ccw) continue;
            uint32_t got = dst[(size_t)row * (DST_PITCH / 4) + col];
            deciding++;
            votes[0] += got == cw, votes[1] += got == ccw;
        }
    if (mismatches) {
        disable("verify-fail"); /* full frames for good, from the next conversion */
        for (int s = 0; s < 2; s++) slots[s].force = true;
    }
    char line[256];
    int n = snprintf(line, sizeof line, "verify pid=%d t=%llu dest_fd=%d mismatches=%llu first=%d,%d "
                     "mode=%s matches_cw=%u matches_ccw=%u of=%u us=%llu\n", (int)gui_pid,
                     (unsigned long long)now_ns(), cap_dest, (unsigned long long)mismatches, first_row, first_col,
                     partial_dir == ROT_CW ? "cw" : "ccw", votes[0], votes[1], deciding,
                     (unsigned long long)((now_ns() - t0) / 1000));
    if (!quiet && n > 0 && (size_t)n < sizeof line) log_line(line, (size_t)n);
}

static int pacing_ioctl(int fd, unsigned long request, void *arg) {
    uintptr_t from = plain((uintptr_t)__builtin_return_address(0));
    if (!enabled || !partial_on || getpid() != gui_pid) return vendor_ioctl(fd, request, arg);
    struct v4l2_buffer *b = arg;
    int *gui_errno = vendor_errno();
    int saved = *gui_errno;
    if (request == VIDIOC_QBUF && CAPTURE_QBUF_SITE(from) && b && b->type == V4L2_BUF_TYPE_VIDEO_CAPTURE) {
        int result = vendor_ioctl(fd, request, arg);
        int err = *gui_errno;
        capture_saved = result == 0 && b->memory == V4L2_MEMORY_DMABUF;
        if (capture_saved) saved_capture = *b, video_fd = fd, cap_dest = b->m.fd;
        *gui_errno = err;
        return result;
    }
    if (request == VIDIOC_QBUF && OUTPUT_QBUF_SITE(from) && b && b->type == V4L2_BUF_TYPE_VIDEO_OUTPUT &&
        capture_saved && fd == video_fd) {
        capture_saved = false;
        int result = partial_output_qbuf(fd, request, b);
        if (result == 0) *gui_errno = saved; /* errno as the vendor's call alone would leave it */
        return result;
    }
    if (request == VIDIOC_DQBUF && CAPTURE_DQBUF_SITE(from) && fd == video_fd) {
        int result = vendor_ioctl(fd, request, arg);
        int err = *gui_errno;
        if (result == 0) convert_line((int64_t)(now_ns() - record.last_start));
        if (result == 0 && verify_due) partial_verify_now(fd);
        verify_due = false;
        *gui_errno = result == 0 ? saved : err;
        return result;
    }
    return vendor_ioctl(fd, request, arg);
}
