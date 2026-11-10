/* GUI presentation pacing and message-thread wait monitor (LD_AUDIT module).
 *
 * For the RØDECaster Pro II GUI 1.7.7 only (rc_gui_juce, SHA-256 aa92a92d…).
 * No instruction is patched and nothing is drawn. Through la_symbind64 it
 * wraps two of the GUI executable's OWN imported calls (libraries' calls are
 * not bound), and acts only at two pinned call sites; every other call passes
 * straight through.
 *
 *  - The frame pump (juce::RodecasterDRMThread::run, 0x7d08a0) locks the
 *    repaint mutex through pthread_mutex_lock at 0x7d09d0 (returns to
 *    0x7d09d4) before each GE2D conversion and DRM commit. At that point the
 *    pump holds no lock: its native event wait has returned and the repaint
 *    mutex is not yet taken. With RC2FX_GUI_PACING=30, 15, 10, 5 or 2, the
 *    module waits there until 1/30, 1/15, 1/10, 1/5 or 1/2 s after the
 *    previous pass, so presentation (GE2D blit + commit) is capped while painting,
 *    input and IPC continue;
 *    frame signals arriving meanwhile coalesce and the next pass presents
 *    the latest painted content. The first pass after an idle period never
 *    waits.
 *  - The message thread waits for the pump inside juce::WaitableEvent::wait
 *    (0x930100), whose imported std::condition_variable::wait is at 0x930158
 *    (returns to 0x93015c), before each paint (callers 0x7cf79c and
 *    0x7dd094, returning to 0x7cf7a0 / 0x7dd098). Those waits are counted
 *    and timed: if pacing is truly non-blocking for the message thread, the
 *    count stays at zero.
 *
 * Per frame (any mode, when logging): the pump's request (before the lock),
 * lock acquired, conversion done (its pthread_mutex_unlock at 0x7d09e0) and
 * the blocking drmModeAtomicCommit at 0x7d0ae4 (start, end), one "frame" line
 * after the commit returns, so frames can be joined with audio events on
 * CLOCK_MONOTONIC. At most 20000 frame lines.
 *
 * RC2FX_GUI_PACING: "observe" (count and time, never wait), "30", "15", "10",
 * "5", "2", or "off"/unset (bind nothing). A wait never exceeds one interval,
 * but the message thread's paint waits for the pump's pass, so at 2 its waits
 * can approach 500 ms (repaint_wait_max_ns). RC2FX_GUI_PACING_LOG: a new
 * file under /run/ (created O_EXCL) for one stats line at most every 5 s (written by the pump
 * thread outside any lock), at the first pass through the pump seam
 * ("first-pass": the launcher's readiness signal) and at exit; capped at 1 MiB.
 *
 * Built with -DRC2FX_PACING_DIRTY (a separate, observe-only module), it also
 * logs every paint's dirty region and each conversion's destination, and
 * accepts only "observe". The repaint callbacks (0x7cf5d0 path A, 0x7dcf20
 * path B) copy their dirty RectangleList into a malloc'd array of {x, y, w, h}
 * int32 entries, then lock the repaint mutex (returning to 0x7cf7b0 /
 * 0x7dd0a8); at that point the array and its count are in the caller's frame
 * at x29 + 0x60 / + 0x6c (A) or + 0x50 / + 0x5c (B). One "paint" line per
 * paint gives the count, bounding box, summed area and up to 16 rectangles.
 * Each "frame" line adds the destination's DMABUF fd and FB ID: global
 * 0x12ec610 points to the current destination descriptor (fd at +0x24,
 * FB ID at +0x20), which the pump reads into x22 before its lock and swaps
 * only after the commit. Nothing is written to the GUI's memory.
 *
 * RC2FX_GUI_PACING_QUIET=1 (production): the log gets only the "start" and
 * "first-pass" lines, every fallback / disable with its reason, and one
 * "counters" line at most every 60 s (written by the pump at its next pass);
 * no frame, paint, convert, verify or stats lines, and no exit line. Unset or
 * any other value: the full diagnostic log above.
 *
 * The wait is a raw clock_nanosleep syscall (not a cancellation point; EINTR
 * retried), so the pump gains no cancellation point; the GUI's errno is saved
 * and restored around the module's own work. Host tests build this with
 * -DRC2FX_PACING_TEST, which matches the sites by symbol name instead of by
 * the pinned offsets and skips the executable hash; everything else is the
 * production code.
 */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <link.h>
#include <stdatomic.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>

#define EXPORT __attribute__((visibility("default")))
#define GUI_SIZE 19761224
#ifndef GUI_PATH
#define GUI_PATH "/usr/bin/rc_gui_juce"
#endif
#define PUMP_RETURN 0x7d09d4u
#define CONVERTED_RETURN 0x7d09e4u /* pthread_mutex_unlock after the GE2D conversion */
#define COMMIT_RETURN 0x7d0ae8u    /* drmModeAtomicCommit */
#define UNLOCK_WORD 0x97e4cbd8u    /* bl pthread_mutex_unlock@plt at 0x7d09e0 */
#define COMMIT_WORD 0x97e4cdd3u    /* bl drmModeAtomicCommit@plt at 0x7d0ae4 */
#define WAIT_RETURN 0x93015cu
#define REPAINT_RETURN_A 0x7cf7a0u
#define REPAINT_RETURN_B 0x7dd098u
#if defined(RC2FX_PACING_PARTIAL) && !defined(RC2FX_PACING_DIRTY)
#error "RC2FX_PACING_PARTIAL builds on RC2FX_PACING_DIRTY"
#endif
#ifdef RC2FX_PACING_DIRTY
#define LOG_CAP (4u << 20)
#define PAINT_A_RETURN 0x7cf7b0u /* pthread_mutex_lock in repaint path A */
#define PAINT_B_RETURN 0x7dd0a8u /* the same in path B */
#define DESTINATION_GLOBAL 0x12ec610u
#define PAINT_LINES 20000
#define PAINT_RECTS_LOGGED 16
#define PAINT_RECTS_MAX 4096
#else
#define LOG_CAP (1u << 20)
#endif
#define LOG_EVERY_NS 5000000000ull
#ifdef RC2FX_PACING_TEST
static uint64_t test_verbose_every_ns = LOG_EVERY_NS;
#endif
#define COUNTERS_EVERY_NS 60000000000ull

typedef int (*mutex_lock_fn)(pthread_mutex_t *);
typedef int (*commit_fn)(int, void *, uint32_t, void *);
typedef void (*condition_wait_fn)(void *, void *);
typedef int *(*errno_fn)(void);

static uintptr_t main_base;
static bool main_seen, enabled;
static pid_t gui_pid; /* forked children inherit the module but never act */
static uint64_t interval_ns; /* 0: observe only */
static mutex_lock_fn vendor_mutex_lock, vendor_mutex_unlock;
static commit_fn vendor_commit;
/* The frame being presented (pump thread only): request, lock acquired,
 * conversion done, commit start/end; one line per frame once committed. */
static uint64_t frame_request, frame_locked, frame_converted, frame_paced;
static _Atomic uint64_t frames_logged;
static condition_wait_fn vendor_condition_wait;
static errno_fn vendor_errno;
static int log_fd = -1;
static _Atomic uint64_t log_bytes, last_log_ns;
/* Rotation (see rotate_log): where to reopen, one rotator at a time, and
 * what it has done. */
static char log_path[512];
static uint64_t log_cap = LOG_CAP;
static _Atomic int log_rotating;
static _Atomic uint64_t log_rotations, log_dropped, log_reopen_failed;
static bool quiet; /* RC2FX_GUI_PACING_QUIET=1 */
static uint64_t counters_every_ns = COUNTERS_EVERY_NS;
#ifdef RC2FX_PACING_PARTIAL
static long long merge_px; /* defined in gui_partial.c; on the start line */
#endif

/* Pump: passes, waits applied, ACTUAL intervals between passes (after any
 * wait), and clock failures (a pass with no valid time is never paced). */
static _Atomic uint64_t last_pass_ns, passes, paced, paced_ns, paced_max_ns, clock_errors;
static _Atomic uint64_t interval_hist[8]; /* <25 ms, <34, <50, <67, <100, <200, <1000, >= 1 s */
/* Waits at the WaitableEvent::wait site: entered and completed, by caller
 * class. A wait that blocks for good shows as entered > completed, so zero
 * completed repaint waits alone is not taken as proof of zero waits. */
static _Atomic uint64_t repaint_entered, repaint_completed, repaint_wait_ns, repaint_wait_max_ns;
static _Atomic uint64_t other_entered, unclassified_entered;
#ifdef RC2FX_PACING_DIRTY
/* Paints seen, logged, rejected (caller frame or list out of bounds), and
 * with a rectangle outside 1280×720. The last is counted only when paint
 * lines can be written (a log is open and quiet mode is off), which is also
 * the only time it is printed. The partial build's counters line carries
 * paints and paint_rejects, so quiet mode has them too. */
static _Atomic uint64_t paints, paints_logged, paint_rejects, paint_outside;
#endif

#ifdef RC2FX_PACING_TEST
static bool test_fail_clock;
static bool test_counters_at_exit; /* RC2FX_TEST_COUNTERS_AT_EXIT: the totals after the last pass */
#endif
static uint64_t now_ns(void) {
#ifdef RC2FX_PACING_TEST
    if (test_fail_clock) return 0;
#endif
    struct timespec t = {0, 0};
    if (clock_gettime(CLOCK_MONOTONIC, &t) != 0) return 0;
    return (uint64_t)t.tv_sec * 1000000000ull + (uint64_t)t.tv_nsec;
}
static void atomic_max(_Atomic uint64_t *value, uint64_t candidate) {
    uint64_t old = atomic_load_explicit(value, memory_order_relaxed);
    while (candidate > old &&
           !atomic_compare_exchange_weak_explicit(value, &old, candidate, memory_order_relaxed,
                                                  memory_order_relaxed)) {
    }
}
/* Strip pointer authentication / tag bits from a saved return address. */
static uintptr_t plain(uintptr_t address) { return address & ((1ull << 48) - 1); }

#ifdef RC2FX_PACING_TEST
/* Host tests: sites by enclosing symbol (the test program is built -rdynamic). */
static bool converted_site(uintptr_t r);
static bool commit_site(uintptr_t r);
static bool in_function(uintptr_t address, const char *name) {
    Dl_info info;
    return dladdr((void *)(address - 1), &info) && info.dli_sname && strcmp(info.dli_sname, name) == 0;
}
static bool pump_site(uintptr_t r) { return in_function(r, "test_pump_lock_site"); }
static bool converted_site(uintptr_t r) { return in_function(r, "test_pump_unlock_site"); }
static bool commit_site(uintptr_t r) { return in_function(r, "test_commit_site"); }
static bool wait_site(uintptr_t r) { return in_function(r, "test_event_wait"); }
static bool repaint_parent(uintptr_t r) {
    return in_function(r, "test_repaint_a") || in_function(r, "test_repaint_b");
}
#ifdef RC2FX_PACING_DIRTY
static int paint_site(uintptr_t r) {
    return in_function(r, "test_paint_a") ? 1 : in_function(r, "test_paint_b") ? 2 : 0;
}
/* The test program's stand-in for global 0x12ec610 (looked up in the
 * program's namespace: RTLD_DEFAULT would search the audit namespace). */
static const uint8_t *const *destination_global(void) {
    void *program = dlmopen(LM_ID_BASE, NULL, RTLD_LAZY | RTLD_NOLOAD);
    return program ? (const uint8_t *const *)dlsym(program, "test_current_destination") : NULL;
}
#endif
#else
static bool pump_site(uintptr_t r) { return r - main_base == PUMP_RETURN; }
static bool converted_site(uintptr_t r) { return r - main_base == CONVERTED_RETURN; }
static bool commit_site(uintptr_t r) { return r - main_base == COMMIT_RETURN; }
static bool wait_site(uintptr_t r) { return r - main_base == WAIT_RETURN; }
static bool repaint_parent(uintptr_t r) {
    return r - main_base == REPAINT_RETURN_A || r - main_base == REPAINT_RETURN_B;
}
#ifdef RC2FX_PACING_DIRTY
static int paint_site(uintptr_t r) {
    return r - main_base == PAINT_A_RETURN ? 1 : r - main_base == PAINT_B_RETURN ? 2 : 0;
}
static const uint8_t *const *destination_global(void) {
    return (const uint8_t *const *)(main_base + DESTINATION_GLOBAL);
}
#endif
#endif

/* Opens the log and remembers where to reopen it (an empty log_path means
 * no path to rotate to). Raw open: no cancellation point, as log_line. */
static int open_log(const char *path) {
    size_t length = strlen(path);
    if (length == 0 || length + 3 > sizeof log_path) return -1;
    int fd = open(path, O_WRONLY | O_CREAT | O_EXCL | O_APPEND | O_NOFOLLOW | O_CLOEXEC, 0600);
    if (fd >= 0) memcpy(log_path, path, length + 1);
    return fd;
}

/* A verbose log would reach the cap within hours; instead of stopping, the
 * full log becomes "<log>.1" (replacing an older one) and a fresh log is
 * opened, so /run holds at most 2x the cap. Raw syscalls only: no libc
 * stdio and no cancellation point (renameat/openat/dup3/close, like write
 * below).
 *
 * log_fd keeps its number for the whole run: the new file is opened into a
 * temporary descriptor and dup3 replaces the live one. Closing it instead
 * would free the number for the GUI to open something else, and a thread
 * already past the log_rotating check below would write a log line into that
 * unrelated file. With dup3 a late line lands in the old file or the new
 * one, both this log. Should the rename or the reopen fail, writes continue
 * in the renamed file and the failure is counted.
 *
 * One thread rotates; another thread's line is dropped meanwhile, counted.
 * A writer that was mid-count when the byte count reset only makes the next
 * rotation a little early. */
static void rotate_log(void) {
    int expected = 0;
    if (!atomic_compare_exchange_strong(&log_rotating, &expected, 1)) {
        atomic_fetch_add(&log_dropped, 1);
        return;
    }
    char previous[sizeof log_path + 2];
    size_t length = strlen(log_path);
    if (length > 0 && length + 3 <= sizeof previous) {
        memcpy(previous, log_path, length);
        memcpy(previous + length, ".1", 3);
        if (syscall(SYS_renameat, AT_FDCWD, log_path, AT_FDCWD, previous) < 0) {
            atomic_fetch_add(&log_reopen_failed, 1);
        } else {
            int fd = (int)syscall(SYS_openat, AT_FDCWD, log_path,
                                  O_WRONLY | O_CREAT | O_TRUNC | O_APPEND | O_NOFOLLOW | O_CLOEXEC, 0600);
            if (fd < 0) {
                atomic_fetch_add(&log_reopen_failed, 1);
            } else {
                if (syscall(SYS_dup3, fd, log_fd, O_CLOEXEC) < 0)
                    atomic_fetch_add(&log_reopen_failed, 1);
                (void)syscall(SYS_close, fd);
            }
        }
    }
    atomic_store(&log_bytes, 0);
    atomic_fetch_add(&log_rotations, 1);
    atomic_store(&log_rotating, 0);
}

/* Raw write: not a cancellation point (libc write() is). Ends a log at the
 * cap by rotating it (LOG_CAP, or RC2FX_GUI_PACING_TEST_LOG_CAP when built
 * with RC2FX_PACING_TEST). */
static void log_line(const char *line, size_t n) {
    if (log_fd < 0) return;
    if (atomic_load(&log_rotating)) {
        atomic_fetch_add(&log_dropped, 1);
        return;
    }
    if (atomic_fetch_add(&log_bytes, (uint64_t)n) + (uint64_t)n > log_cap) {
        rotate_log();
        return;
    }
    (void)syscall(SYS_write, log_fd, line, n);
}
static void write_stats(const char *why) {
    if (log_fd < 0) return;
    char extra[64] = "";
#ifdef RC2FX_PACING_PARTIAL
    if (strcmp(why, "start") == 0) snprintf(extra, sizeof extra, " merge_px=%lld quiet=%d", merge_px, (int)quiet);
#endif
    char line[640];
    int n = snprintf(line, sizeof line,
                     "%s pid=%d t=%llu interval_ns=%llu passes=%llu paced=%llu paced_ns=%llu paced_max_ns=%llu "
                     "clock_errors=%llu pass_intervals=%llu,%llu,%llu,%llu,%llu,%llu,%llu,%llu "
                     "repaint_entered=%llu repaint_completed=%llu repaint_wait_ns=%llu repaint_wait_max_ns=%llu "
                     "other_entered=%llu unclassified_entered=%llu%s\n",
                     why, (int)getpid(), (unsigned long long)now_ns(), (unsigned long long)interval_ns,
                     (unsigned long long)passes, (unsigned long long)paced, (unsigned long long)paced_ns,
                     (unsigned long long)paced_max_ns, (unsigned long long)clock_errors,
                     (unsigned long long)interval_hist[0],
                     (unsigned long long)interval_hist[1], (unsigned long long)interval_hist[2],
                     (unsigned long long)interval_hist[3], (unsigned long long)interval_hist[4],
                     (unsigned long long)interval_hist[5], (unsigned long long)interval_hist[6],
                     (unsigned long long)interval_hist[7], (unsigned long long)repaint_entered,
                     (unsigned long long)repaint_completed, (unsigned long long)repaint_wait_ns,
                     (unsigned long long)repaint_wait_max_ns, (unsigned long long)other_entered,
                     (unsigned long long)unclassified_entered, extra);
    if (n <= 0 || (size_t)n >= sizeof line) return;
    log_line(line, (size_t)n);
#ifdef RC2FX_PACING_DIRTY
    if (quiet) return;
    n = snprintf(line, sizeof line, "dirty-%s pid=%d paints=%llu paints_logged=%llu paint_rejects=%llu "
                 "paint_outside=%llu\n", why, (int)getpid(), (unsigned long long)paints,
                 (unsigned long long)paints_logged, (unsigned long long)paint_rejects,
                 (unsigned long long)paint_outside);
    if (n > 0 && (size_t)n < sizeof line) log_line(line, (size_t)n);
#endif
}


#ifdef RC2FX_PACING_DIRTY
/* The destination of the frame being presented (pump thread only). */
static int frame_dest_fd = -1, frame_dest_fb = -1;

#ifdef RC2FX_PACING_PARTIAL
#include "gui_partial.c"
#else
static void partial_paint(const int32_t *list, int32_t count) { (void)list, (void)count; }
static void partial_paint_rejected(void) {}
#endif

/* One paint's dirty list, read from its caller's frame (`fp`: the repaint
 * function's x29, which equals its sp). Reads only; any bound fails → the
 * paint is counted as rejected and nothing else is read. */
static void log_paint(int path, const uint8_t *fp) {
    atomic_fetch_add_explicit(&paints, 1, memory_order_relaxed);
    const size_t list_at = path == 1 ? 0x60 : 0x50, count_at = path == 1 ? 0x6c : 0x5c;
    const int32_t *list;
    int32_t count;
    memcpy(&list, fp + list_at, sizeof list);
    memcpy(&count, fp + count_at, sizeof count);
    if (!list || ((uintptr_t)list & 7) || count < 1 || count > PAINT_RECTS_MAX) {
        atomic_fetch_add_explicit(&paint_rejects, 1, memory_order_relaxed);
        partial_paint_rejected();
        return;
    }
    partial_paint(list, count);
    /* Everything below only builds the "paint" line and the paint_outside
     * count, which the "dirty-" stats line prints. Quiet mode and a run
     * without a log write neither, so the paint stops here, before any
     * formatting. This runs on the message thread under the repaint mutex. */
    if (log_fd < 0 || quiet) return;
    int64_t x0 = INT32_MAX, y0 = INT32_MAX, x1 = INT32_MIN, y1 = INT32_MIN;
    uint64_t area = 0;
    int outside = 0;
    char rects[PAINT_RECTS_LOGGED * 24 + 1];
    size_t used = 0;
    rects[0] = 0;
    for (int32_t i = 0; i < count; i++) {
        int64_t x = list[4 * i], y = list[4 * i + 1], w = list[4 * i + 2], h = list[4 * i + 3];
        if (w <= 0 || h <= 0) continue;
        if (x < 0 || y < 0 || x + w > 1280 || y + h > 720) outside++;
        if (x < x0) x0 = x;
        if (y < y0) y0 = y;
        if (x + w > x1) x1 = x + w;
        if (y + h > y1) y1 = y + h;
        area += (uint64_t)(w * h);
        if (i < PAINT_RECTS_LOGGED) {
            int n = snprintf(rects + used, sizeof rects - used, "%s%lld:%lld:%lld:%lld", i ? ";" : "",
                             (long long)x, (long long)y, (long long)w, (long long)h);
            if (n > 0 && (size_t)n < sizeof rects - used) used += (size_t)n;
        }
    }
    if (outside) atomic_fetch_add_explicit(&paint_outside, 1, memory_order_relaxed);
    if (log_fd < 0 || quiet || atomic_fetch_add_explicit(&paints_logged, 1, memory_order_relaxed) >= PAINT_LINES) return;
    if (x1 < x0) x0 = y0 = x1 = y1 = 0;
    char line[PAINT_RECTS_LOGGED * 24 + 192];
    int n = snprintf(line, sizeof line, "paint pid=%d t=%llu path=%c n=%d bbox=%lld,%lld,%lld,%lld area=%llu "
                     "outside=%d rects=%s\n", (int)gui_pid, (unsigned long long)now_ns(), path == 1 ? 'A' : 'B',
                     (int)count, (long long)x0, (long long)y0, (long long)(x1 - x0), (long long)(y1 - y0),
                     (unsigned long long)area, outside, rects[0] ? rects : "-");
    if (n > 0 && (size_t)n < sizeof line) log_line(line, (size_t)n);
}

/* The current destination descriptor's DMABUF fd and FB ID. */
static void read_destination(void) {
    const uint8_t *const *global = destination_global();
    const uint8_t *descriptor = global ? *global : NULL;
    frame_dest_fd = frame_dest_fb = -1;
    if (!descriptor || ((uintptr_t)descriptor & 7)) return;
    int32_t fd, fb;
    memcpy(&fd, descriptor + 0x24, sizeof fd);
    memcpy(&fb, descriptor + 0x20, sizeof fb);
    frame_dest_fd = fd, frame_dest_fb = fb;
}

/* The repaint function's frame record, as in pacing_condition_wait. */
static const uint8_t *caller_frame(uintptr_t *ours) {
    uintptr_t *theirs = (uintptr_t *)ours[0];
    if ((uintptr_t)theirs > (uintptr_t)ours && (uintptr_t)theirs - (uintptr_t)ours < (1u << 20) &&
        ((uintptr_t)theirs & 15) == 0)
        return (const uint8_t *)theirs;
    return NULL;
}
#endif

#ifndef RC2FX_PACING_PARTIAL
/* Periodic cumulative line (gui_partial.c has the conversion counters). */
static void write_counters(void) {
    if (log_fd < 0) return;
    char line[128];
    int n = snprintf(line, sizeof line, "counters pid=%d t=%llu passes=%llu log_rotations=%llu log_dropped=%llu "
                     "log_reopen_failed=%llu\n",
                     (int)gui_pid, (unsigned long long)now_ns(), (unsigned long long)passes,
                     (unsigned long long)atomic_load(&log_rotations), (unsigned long long)atomic_load(&log_dropped),
                     (unsigned long long)atomic_load(&log_reopen_failed));
    if (n > 0 && (size_t)n < sizeof line) log_line(line, (size_t)n);
}
#endif

/* ---- the frame pump's lock of the repaint mutex ------------------------- */
static void pace(void) {
    uint64_t start = now_ns();
    frame_request = start;
    frame_paced = 0;
    uint64_t last = atomic_load_explicit(&last_pass_ns, memory_order_relaxed);
    if (!start) {
        /* No valid time: never wait, and do not disturb the interval state. */
        atomic_fetch_add_explicit(&clock_errors, 1, memory_order_relaxed);
        atomic_fetch_add_explicit(&passes, 1, memory_order_relaxed);
        return;
    }
    if (interval_ns && last && start > last && start < last + interval_ns) {
        uint64_t until = last + interval_ns;
        struct timespec at = {(time_t)(until / 1000000000ull), (long)(until % 1000000000ull)};
        /* Raw syscall: not a cancellation point; resume after signals. */
        while (syscall(SYS_clock_nanosleep, CLOCK_MONOTONIC, TIMER_ABSTIME, &at, NULL) != 0 &&
               errno == EINTR) {
        }
        uint64_t woke = now_ns();
        uint64_t slept = woke > start ? woke - start : 0;
        frame_paced = slept;
        if (!woke) atomic_fetch_add_explicit(&clock_errors, 1, memory_order_relaxed);
        atomic_fetch_add_explicit(&paced, 1, memory_order_relaxed);
        atomic_fetch_add_explicit(&paced_ns, slept, memory_order_relaxed);
        atomic_max(&paced_max_ns, slept);
    }
    uint64_t pass = now_ns();
    if (!pass) {
        atomic_fetch_add_explicit(&clock_errors, 1, memory_order_relaxed);
        atomic_fetch_add_explicit(&passes, 1, memory_order_relaxed);
        return;
    }
    if (last && pass > last) {
        /* The actual pass-to-pass interval, after any wait. */
        uint64_t gap = pass - last;
        static const uint64_t edges[7] = {25000000, 34000000, 50000000, 67000000,
                                          100000000, 200000000, 1000000000};
        unsigned bucket = 0;
        while (bucket < 7 && gap >= edges[bucket]) bucket++;
        atomic_fetch_add_explicit(&interval_hist[bucket], 1, memory_order_relaxed);
    }
    atomic_store_explicit(&last_pass_ns, pass, memory_order_relaxed);
    /* The first verified pass: the launcher's readiness signal. */
    if (atomic_fetch_add_explicit(&passes, 1, memory_order_relaxed) == 0) write_stats("first-pass");
    uint64_t logged = atomic_load_explicit(&last_log_ns, memory_order_relaxed);
    if (pass > logged && pass - logged >= (quiet ? counters_every_ns :
#ifdef RC2FX_PACING_TEST
                                          test_verbose_every_ns
#else
                                          LOG_EVERY_NS
#endif
    )) {
        atomic_store_explicit(&last_log_ns, pass, memory_order_relaxed);
        if (!quiet) write_stats("stats");
        write_counters();
    }
}
static int pacing_mutex_lock(pthread_mutex_t *mutex) {
    uintptr_t from = plain((uintptr_t)__builtin_return_address(0));
    if (enabled && pump_site(from) && getpid() == gui_pid) {
        int *gui_errno = vendor_errno();
        int saved = *gui_errno;
        pace();
        *gui_errno = saved;
        int result = vendor_mutex_lock(mutex);
        frame_locked = now_ns();
#ifdef RC2FX_PACING_DIRTY
        read_destination();
#endif
        *gui_errno = saved;
        return result;
    }
#ifdef RC2FX_PACING_DIRTY
    int path = enabled ? paint_site(from) : 0;
    if (path && getpid() == gui_pid) {
        const uint8_t *frame = caller_frame(__builtin_frame_address(0));
        int result = vendor_mutex_lock(mutex);
        int *gui_errno = vendor_errno();
        int saved = *gui_errno;
        if (frame) log_paint(path, frame);
        else atomic_fetch_add_explicit(&paint_rejects, 1, memory_order_relaxed), partial_paint_rejected();
        *gui_errno = saved;
        return result;
    }
#endif
    return vendor_mutex_lock(mutex);
}
/* The pump's unlock right after the GE2D conversion. */
static int pacing_mutex_unlock(pthread_mutex_t *mutex) {
    uintptr_t from = plain((uintptr_t)__builtin_return_address(0));
    int result = vendor_mutex_unlock(mutex);
    if (enabled && converted_site(from) && getpid() == gui_pid) frame_converted = now_ns();
    return result;
}
/* The pump's atomic commit (blocking; page-flip event). One line per frame. */
static int pacing_commit(int fd, void *request, uint32_t flags, void *user) {
    uintptr_t from = plain((uintptr_t)__builtin_return_address(0));
    if (!enabled || !commit_site(from) || getpid() != gui_pid) return vendor_commit(fd, request, flags, user);
    int *gui_errno = vendor_errno();
    uint64_t begin = now_ns();
    int result = vendor_commit(fd, request, flags, user);
    int saved = *gui_errno;
    uint64_t end = now_ns();
    if (log_fd >= 0 && !quiet && atomic_fetch_add_explicit(&frames_logged, 1, memory_order_relaxed) < 20000) {
#ifdef RC2FX_PACING_DIRTY
        char line[240];
        int n = snprintf(line, sizeof line, "frame pid=%d request=%llu paced_ns=%llu locked=%llu converted=%llu "
                         "commit=%llu committed=%llu result=%d dest_fd=%d dest_fb=%d\n", (int)gui_pid,
                         (unsigned long long)frame_request, (unsigned long long)frame_paced,
                         (unsigned long long)frame_locked, (unsigned long long)frame_converted,
                         (unsigned long long)begin, (unsigned long long)end, result, frame_dest_fd, frame_dest_fb);
#else
        char line[200];
        int n = snprintf(line, sizeof line, "frame pid=%d request=%llu paced_ns=%llu locked=%llu converted=%llu "
                         "commit=%llu committed=%llu result=%d\n", (int)gui_pid,
                         (unsigned long long)frame_request, (unsigned long long)frame_paced,
                         (unsigned long long)frame_locked, (unsigned long long)frame_converted,
                         (unsigned long long)begin, (unsigned long long)end, result);
#endif
        if (n > 0 && (size_t)n < sizeof line) log_line(line, (size_t)n);
    }
    frame_request = frame_locked = frame_converted = 0;
    *gui_errno = saved;
    return result;
}

/* ---- the message thread's wait for the pump ----------------------------- */
static void pacing_condition_wait(void *condition, void *lock) {
    uintptr_t from = plain((uintptr_t)__builtin_return_address(0));
    if (!enabled || !wait_site(from) || getpid() != gui_pid) {
        vendor_condition_wait(condition, lock);
        return;
    }
    /* Who called WaitableEvent::wait: the vendor function keeps a frame
     * record (x29, x30) at its x29, which is our saved frame pointer. */
    uintptr_t *ours = __builtin_frame_address(0);
    uintptr_t *theirs = (uintptr_t *)ours[0];
    enum { REPAINT, OTHER, UNCLASSIFIED } kind = UNCLASSIFIED;
    if ((uintptr_t)theirs > (uintptr_t)ours && (uintptr_t)theirs - (uintptr_t)ours < (1u << 20) &&
        ((uintptr_t)theirs & 7) == 0)
        kind = repaint_parent(plain(theirs[1])) ? REPAINT : OTHER;
    if (kind != REPAINT) {
        atomic_fetch_add_explicit(kind == OTHER ? &other_entered : &unclassified_entered, 1,
                                  memory_order_relaxed);
        vendor_condition_wait(condition, lock);
        return;
    }
    atomic_fetch_add_explicit(&repaint_entered, 1, memory_order_relaxed);
    uint64_t start = now_ns();
    vendor_condition_wait(condition, lock);
    int *gui_errno = vendor_errno();
    int saved = *gui_errno;
    uint64_t end = now_ns();
    if (!start || !end) atomic_fetch_add_explicit(&clock_errors, 1, memory_order_relaxed);
    uint64_t waited = start && end > start ? end - start : 0;
    atomic_fetch_add_explicit(&repaint_completed, 1, memory_order_relaxed);
    atomic_fetch_add_explicit(&repaint_wait_ns, waited, memory_order_relaxed);
    atomic_max(&repaint_wait_max_ns, waited);
    *gui_errno = saved;
}

/* ---- admission ---------------------------------------------------------- */
struct sha256 {
    uint32_t h[8];
    uint64_t bytes;
    uint8_t block[64];
    size_t used;
};
static const uint32_t K[64] = {
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2};
static uint32_t rotr(uint32_t x, unsigned n) { return x >> n | x << (32 - n); }
static void sha_block(struct sha256 *s, const uint8_t *p) {
    uint32_t w[64], a = s->h[0], b = s->h[1], c = s->h[2], d = s->h[3], e = s->h[4], f = s->h[5],
                    g = s->h[6], h = s->h[7];
    for (unsigned i = 0; i < 16; ++i)
        w[i] = (uint32_t)p[4 * i] << 24 | (uint32_t)p[4 * i + 1] << 16 | (uint32_t)p[4 * i + 2] << 8 | p[4 * i + 3];
    for (unsigned i = 16; i < 64; ++i)
        w[i] = w[i - 16] + (rotr(w[i - 15], 7) ^ rotr(w[i - 15], 18) ^ w[i - 15] >> 3) + w[i - 7] +
               (rotr(w[i - 2], 17) ^ rotr(w[i - 2], 19) ^ w[i - 2] >> 10);
    for (unsigned i = 0; i < 64; ++i) {
        uint32_t t1 = h + (rotr(e, 6) ^ rotr(e, 11) ^ rotr(e, 25)) + ((e & f) ^ (~e & g)) + K[i] + w[i];
        uint32_t t2 = (rotr(a, 2) ^ rotr(a, 13) ^ rotr(a, 22)) + ((a & b) ^ (a & c) ^ (b & c));
        h = g, g = f, f = e, e = d + t1, d = c, c = b, b = a, a = t1 + t2;
    }
    s->h[0] += a, s->h[1] += b, s->h[2] += c, s->h[3] += d, s->h[4] += e, s->h[5] += f, s->h[6] += g, s->h[7] += h;
}
static void sha_update(struct sha256 *s, const uint8_t *p, size_t n) {
    s->bytes += n;
    while (n--) {
        s->block[s->used++] = *p++;
        if (s->used == 64) sha_block(s, s->block), s->used = 0;
    }
}
static const struct sha256 SHA_INIT = {{0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f,
                                        0x9b05688c, 0x1f83d9ab, 0x5be0cd19}, 0, {0}, 0};
static void sha_final(struct sha256 *s, uint8_t out[32]) {
    uint64_t bits = s->bytes * 8;
    uint8_t one = 0x80, zero = 0, tail[8];
    sha_update(s, &one, 1);
    while (s->used != 56) sha_update(s, &zero, 1);
    for (unsigned i = 0; i < 8; ++i) tail[7 - i] = (uint8_t)(bits >> (i * 8));
    sha_update(s, tail, 8);
    for (unsigned i = 0; i < 32; ++i) out[i] = (uint8_t)(s->h[i / 4] >> (24 - (i % 4) * 8));
}
#ifndef RC2FX_PACING_TEST
/* The whole executable, then the exact instruction words at both sites (in
 * the mapped image): bl pthread_mutex_lock@plt and bl condition_variable::wait@plt. */
static bool admitted(void) {
    static const uint8_t expected[32] = {0xaa, 0x92, 0xa9, 0x2d, 0x38, 0x4b, 0xdb, 0x24, 0xbc, 0x1c, 0xdb,
                                         0x01, 0x61, 0x5a, 0xc2, 0x80, 0xd4, 0xd4, 0x86, 0x7e, 0x66, 0x11,
                                         0x58, 0xdf, 0x5c, 0x7f, 0x86, 0xff, 0x98, 0xb6, 0x05, 0xca};
    int fd = open("/proc/self/exe", O_RDONLY | O_CLOEXEC);
    if (fd < 0) return false;
    struct stat st;
    if (fstat(fd, &st) != 0 || st.st_size != GUI_SIZE) {
        close(fd);
        return false;
    }
    struct sha256 s = SHA_INIT;
    uint8_t buffer[16384], digest[32];
    ssize_t n;
    while ((n = read(fd, buffer, sizeof buffer)) > 0) sha_update(&s, buffer, (size_t)n);
    close(fd);
    if (n < 0) return false;
    sha_final(&s, digest);
    if (memcmp(expected, digest, 32) != 0) return false;
    uint32_t pump_call, wait_call, unlock_call, commit_call;
    memcpy(&pump_call, (const void *)(main_base + PUMP_RETURN - 4), 4);
    memcpy(&wait_call, (const void *)(main_base + WAIT_RETURN - 4), 4);
    memcpy(&unlock_call, (const void *)(main_base + CONVERTED_RETURN - 4), 4);
    memcpy(&commit_call, (const void *)(main_base + COMMIT_RETURN - 4), 4);
#ifdef RC2FX_PACING_DIRTY
    /* The two paint lock calls, the loads after them that fix the list and
     * count offsets, both functions' frame setup (x29 = sp), and the pump's
     * destination chain (x19 = 0x12eb7d8, x22 = [x19 + 0xe38], fd at +0x24). */
    static const struct {
        uint32_t at, word;
    } words[] = {
        {0x7cf7ac, 0x97e4d10d}, {0x7cf7b0, 0xf94033e0}, {0x7cf7b4, 0xb9806ff5}, {0x7cf5d4, 0xa9b97bfd},
        {0x7cf5d8, 0x910003fd}, {0x7dd0a4, 0x97e49acf}, {0x7dd0a8, 0xf9402be0}, {0x7dd0ac, 0xb9805ff5},
        {0x7dcf24, 0xa9ba7bfd}, {0x7dcf28, 0x910003fd}, {0x7d08b0, 0xf00058d3}, {0x7d08b4, 0x911f6273},
        {0x7d090c, 0xf9471e76}, {0x7d09d4, 0xb94026c0},
#ifdef RC2FX_PACING_PARTIAL
        /* bl ioctl at the capture QBUF, output QBUF and capture DQBUF, and
         * the request constants built before them. */
        {0x7d05bc, 0x97e4ced1}, {0x7d0634, 0x97e4ceb3}, {0x7d0808, 0x97e4ce3e}, {0x7d05b0, 0xb900a3e4},
        {0x7d0578, 0xd28ac1e1}, {0x7d057c, 0xf2b80b01}, {0x7d0600, 0xd28ac1e1}, {0x7d0604, 0xf2b80b01},
        {0x7d0800, 0xd28ac221}, {0x7d0804, 0xf2b80b01},
#endif
    };
    for (size_t i = 0; i < sizeof words / sizeof words[0]; i++) {
        uint32_t word;
        memcpy(&word, (const void *)(main_base + words[i].at), 4);
        if (word != words[i].word) return false;
    }
#endif
    return pump_call == 0x97e4cc84u && wait_call == 0x97df4f92u && unlock_call == UNLOCK_WORD &&
           commit_call == COMMIT_WORD;
}
#endif

static void stop(void) {
    if (getpid() == gui_pid && !quiet) write_stats("exit");
#ifdef RC2FX_PACING_TEST
    if (getpid() == gui_pid && test_counters_at_exit) write_counters();
#endif
}

EXPORT unsigned int la_version(unsigned int version) { return version >= LAV_CURRENT ? LAV_CURRENT : 0; }
EXPORT unsigned int la_objopen(struct link_map *map, Lmid_t lmid, uintptr_t *cookie) {
    if (lmid != LM_ID_BASE) return 0;
    bool main = map->l_name[0] == '\0';
    if (main) main_base = (uintptr_t)map->l_addr, main_seen = true;
    *cookie = main ? 1 : 2;
    return LA_FLG_BINDTO | LA_FLG_BINDFROM;
}
EXPORT void la_preinit(uintptr_t *cookie) {
    (void)cookie;
    const char *mode = getenv("RC2FX_GUI_PACING");
    if (!mode || strcmp(mode, "off") == 0) return;
    if (strcmp(mode, "observe") == 0) interval_ns = 0;
#ifdef RC2FX_PACING_DIRTY
    /* This build never waits: observe, or (partial build) partial conversion. */
#ifdef RC2FX_PACING_PARTIAL
    else if (parse_partial_mode(mode)) interval_ns = 0;
#endif
    else return;
#else
    else if (strcmp(mode, "30") == 0) interval_ns = 33333334;
    else if (strcmp(mode, "15") == 0) interval_ns = 66666667;
    else if (strcmp(mode, "10") == 0) interval_ns = 100000000;
    else if (strcmp(mode, "5") == 0) interval_ns = 200000000;
    else if (strcmp(mode, "2") == 0) interval_ns = 500000000;
    else return;
#endif
#ifndef RC2FX_PACING_TEST
    /* Processes the GUI starts (system()/popen() helpers) inherit LD_AUDIT:
     * anything that is not the GUI executable stays silent and inactive. */
    char exe[256];
    ssize_t length = readlink("/proc/self/exe", exe, sizeof exe - 1);
    if (length <= 0) return;
    exe[length] = 0;
    if (strcmp(exe, GUI_PATH) != 0) return;
    if (!main_seen || !admitted()) {
        char refused[96];
        int n = snprintf(refused, sizeof refused, "RC2FX GUI pacing: pid %d executable not admitted; inactive\n",
                         (int)getpid());
        if (n > 0 && (size_t)n < sizeof refused) (void)write(STDERR_FILENO, refused, (size_t)n);
        return;
    }
#endif
    /* The GUI's own errno (its libc instance): required, else inactive. */
    void *base_libc = dlmopen(LM_ID_BASE, "libc.so.6", RTLD_NOW | RTLD_NOLOAD);
    void *errno_symbol = base_libc ? dlsym(base_libc, "__errno_location") : NULL;
    /* (The two imports may bind lazily, after this; their wrappers exist
     * only once bound, so nothing to check for them here.) */
    if (!errno_symbol) {
        char refused[96];
        int n = snprintf(refused, sizeof refused, "RC2FX GUI pacing: pid %d GUI errno unresolved; inactive\n",
                         (int)getpid());
        if (n > 0 && (size_t)n < sizeof refused) (void)write(STDERR_FILENO, refused, (size_t)n);
        return;
    }
    memcpy(&vendor_errno, &errno_symbol, sizeof vendor_errno);
    const char *quiet_env = getenv("RC2FX_GUI_PACING_QUIET");
    quiet = quiet_env && strcmp(quiet_env, "1") == 0;
#ifdef RC2FX_PACING_TEST
    if ((quiet_env = getenv("RC2FX_TEST_COUNTERS_EVERY_MS")))
        counters_every_ns = strtoull(quiet_env, NULL, 10) * 1000000ull;
    if ((quiet_env = getenv("RC2FX_TEST_VERBOSE_EVERY_MS")))
        test_verbose_every_ns = strtoull(quiet_env, NULL, 10) * 1000000ull;
    /* A small cap, so a test reaches a rotation in a few lines. */
    if ((quiet_env = getenv("RC2FX_GUI_PACING_TEST_LOG_CAP"))) {
        unsigned long long cap = strtoull(quiet_env, NULL, 10);
        if (cap >= 200) log_cap = cap;
    }
#endif
    const char *log = getenv("RC2FX_GUI_PACING_LOG");
    if (log && strncmp(log, "/run/", 5) == 0 && !strstr(log, "..")) log_fd = open_log(log);
#ifdef RC2FX_PACING_TEST
    if (log_fd < 0) {
        const char *test_log = getenv("RC2FX_GUI_PACING_TEST_LOG");
        if (test_log) log_fd = open_log(test_log);
    }
#endif
#ifdef RC2FX_PACING_PARTIAL
    partial_setup(log_fd >= 0 ? getenv("RC2FX_GUI_PACING_LOG") : NULL);
#ifdef RC2FX_PACING_TEST
    if (log_fd >= 0 && !kill_path[0]) partial_setup(getenv("RC2FX_GUI_PACING_TEST_LOG"));
#endif
    partial_config();
#endif
    atomic_store(&last_log_ns, now_ns());
#ifdef RC2FX_PACING_TEST
    test_fail_clock = getenv("RC2FX_GUI_PACING_TEST_FAIL_CLOCK") != NULL;
    test_counters_at_exit = getenv("RC2FX_TEST_COUNTERS_AT_EXIT") != NULL;
    {   /* Known answer for the admission hash: SHA-256("abc"). */
        static const uint8_t abc[32] = {0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40,
                                        0xde, 0x5d, 0xae, 0x22, 0x23, 0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17,
                                        0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61, 0xf2, 0x00, 0x15, 0xad};
        struct sha256 s = SHA_INIT;
        uint8_t digest[32];
        sha_update(&s, (const uint8_t *)"abc", 3);
        sha_final(&s, digest);
        write_stats(memcmp(digest, abc, 32) == 0 ? "sha-ok" : "sha-bad");
    }
#endif
    write_stats("start");
    atexit(stop);
    gui_pid = getpid();
    enabled = true;
}
EXPORT uintptr_t la_symbind64(Elf64_Sym *symbol, unsigned int index, uintptr_t *reference,
                              uintptr_t *definition, unsigned int *flags, const char *name) {
    (void)index, (void)definition;
    uintptr_t original = (uintptr_t)symbol->st_value;
    const char *mode = getenv("RC2FX_GUI_PACING");
    /* Only the executable's own references, and only when a mode is set. */
    if (*reference != 1 || !mode || strcmp(mode, "off") == 0) return original;
    if (strcmp(name, "pthread_mutex_lock") == 0) {
        memcpy(&vendor_mutex_lock, &original, sizeof original);
        *flags |= LA_SYMB_NOPLTENTER | LA_SYMB_NOPLTEXIT;
        return (uintptr_t)&pacing_mutex_lock;
    }
    if (strcmp(name, "pthread_mutex_unlock") == 0) {
        memcpy(&vendor_mutex_unlock, &original, sizeof original);
        *flags |= LA_SYMB_NOPLTENTER | LA_SYMB_NOPLTEXIT;
        return (uintptr_t)&pacing_mutex_unlock;
    }
    if (strcmp(name, "drmModeAtomicCommit") == 0) {
        memcpy(&vendor_commit, &original, sizeof original);
        *flags |= LA_SYMB_NOPLTENTER | LA_SYMB_NOPLTEXIT;
        return (uintptr_t)&pacing_commit;
    }
#ifdef RC2FX_PACING_PARTIAL
    if (strcmp(name, "ioctl") == 0 && strncmp(mode, "partial-", 8) == 0) {
        memcpy(&vendor_ioctl, &original, sizeof original);
        *flags |= LA_SYMB_NOPLTENTER | LA_SYMB_NOPLTEXIT;
        return (uintptr_t)&pacing_ioctl;
    }
#endif
    if (strcmp(name, "_ZNSt18condition_variable4waitERSt11unique_lockISt5mutexE") == 0) {
        memcpy(&vendor_condition_wait, &original, sizeof original);
        *flags |= LA_SYMB_NOPLTENTER | LA_SYMB_NOPLTEXIT;
        return (uintptr_t)&pacing_condition_wait;
    }
    return original;
}
