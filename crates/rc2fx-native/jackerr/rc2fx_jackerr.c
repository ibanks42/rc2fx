/* RC2FX server-side JACK error recorder, for LD_PRELOAD into jackd only.
 *
 * The mixer's libjackserver and jack_alsa.so report failures through
 * jack_error(), whose vendor body is a no-op. This library defines
 * jack_error() earlier in jackd's lookup scope and records the calls it
 * recognises (the server's late-client checks and the ALSA driver's xrun and
 * poll messages) into a fixed ring in a shared RAM file. Nothing is printed
 * or forwarded (the original does nothing), and errno is left as it was.
 *
 * In the call: one CLOCK_MONOTONIC read, an atomic index claim, a CAS that
 * takes the slot only from an older published generation, relaxed atomic
 * stores and bounded string copies. A slot still being written by a stalled
 * writer, or already holding a newer record, makes this record DROPPED
 * (counted), never waited for. No formatting, allocation, lock or I/O.
 * Unknown formats are counted and keep a bounded prefix of the literal.
 *
 * The constructor (not the call) creates the ring, and only when this process
 * is /usr/bin/jackd and 64-bit atomics are lock-free: in a private 0700
 * directory, a new 0600 file per generation (ring-<pid>-<created ns>.ring,
 * O_EXCL: never a truncated or reused mapping), with every page faulted
 * in before use. Anywhere else, jack_error()
 * stays a no-op. Read it with rc2fx_jackerr_snapshot on the device.
 *
 * Build: zig cc -target aarch64-linux-gnu.2.35 -O2 -shared -fPIC -Wall -Wextra
 *        -Werror -fvisibility=hidden rc2fx_jackerr.c -o librc2fx_jackerr.so
 * (-DRC2FX_JACKERR_TESTING adds host-test hooks; never for the device.)
 */
#define _GNU_SOURCE
#include "rc2fx_jackerr.h"
#include <errno.h>
#include <fcntl.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

#define EXPORT __attribute__((visibility("default")))
#define SLOTS RC2FX_JACKERR_SLOTS
#define STRING1_WORDS RC2FX_JACKERR_STRING1_WORDS
#define STRING2_WORDS RC2FX_JACKERR_STRING2_WORDS

enum kind {
    NOT_FINISHED = 1, FINISHED_LATE = 2, TIMED_XRUN = 3, ALSA_XRUN_MSECS = 4, ALSA_WAIT_STATUS = 5,
    INTERNAL_XRUN = 6, POLL_FAILED = 7, POLL_EXITING = 8, POLL_RETRYING = 9, POLL_RECOVERY_FAILED = 10,
    POLL_INTERRUPT = 11, UNKNOWN = 255,
};
enum flags {
    STRING1_TRUNCATED = 1, STRING2_TRUNCATED = 2, STRING1_NULL = 4, STRING2_NULL = 8, CLOCK_FAILED = 16,
};

static struct rc2fx_jackerr_header *ring_header;
static struct rc2fx_jackerr_slot *ring_slots;
#ifdef RC2FX_JACKERR_TESTING
static _Atomic uint64_t fault_calls;
static _Atomic uint64_t fault_published;
EXPORT uint64_t rc2fx_jackerr_test_fault_calls(void) { return atomic_load(&fault_calls); }
EXPORT uint64_t rc2fx_jackerr_test_fault_published(void) { return atomic_load(&fault_published); }
#endif

/* Optional kernel flight-recorder trigger, reached only AFTER the fault
 * record is published. Normally a no-op; unlike callback uprobes this cannot
 * perturb the audio work that caused the first fault. Keep its ELF symbol. */
EXPORT __attribute__((noinline)) void rc2fx_jackerr_fault(uint64_t kind, const char *client, uint64_t mono_ns) {
#ifdef RC2FX_JACKERR_TESTING
    uint64_t count = atomic_load(&ring_header->claimed);
    atomic_fetch_add(&fault_calls, 1);
    if (count && atomic_load(&ring_slots[(count - 1) % SLOTS].seq) == 2 * count)
        atomic_fetch_add(&fault_published, 1);
#endif
    __asm__ volatile("" : : "r"(kind), "r"(client), "r"(mono_ns) : "memory");
}

#ifdef RC2FX_JACKERR_TESTING
static _Atomic int test_fail_clock;
EXPORT void rc2fx_jackerr_test_fail_clock(int fail) { atomic_store(&test_fail_clock, fail); }
EXPORT struct rc2fx_jackerr_slot *rc2fx_jackerr_test_slot(uint64_t index) {
    return ring_slots ? &ring_slots[index % SLOTS] : NULL;
}
EXPORT struct rc2fx_jackerr_header *rc2fx_jackerr_test_header(void) { return ring_header; }
#endif

static int now_ns(uint64_t *t) {
#ifdef RC2FX_JACKERR_TESTING
    if (atomic_load(&test_fail_clock)) return 0;
#endif
    struct timespec now;
    if (clock_gettime(CLOCK_MONOTONIC, &now) != 0) return 0;
    *t = (uint64_t)now.tv_sec * 1000000000u + (uint64_t)now.tv_nsec;
    return 1;
}

static void copy_string(_Atomic uint64_t *words, int count, const char *text, uint8_t *length,
                        uint16_t *flags, uint16_t truncated, uint16_t null) {
    unsigned char bytes[STRING1_WORDS * 8] = {0};
    size_t room = (size_t)count * 8, n = 0;
    if (!text) {
        *flags |= null;
    } else {
        n = strnlen(text, room + 1);
        if (n > room) { n = room; *flags |= truncated; }
        memcpy(bytes, text, n);
    }
    *length = (uint8_t)n;
    for (int i = 0; i < count; i++) {
        uint64_t word;
        memcpy(&word, bytes + i * 8, 8);
        atomic_store_explicit(&words[i], word, memory_order_relaxed);
    }
}

static void record(enum kind kind, int64_t value1, int64_t value2, const char *string1, const char *string2) {
    uint16_t flags = 0;
    uint64_t t = 0;
    if (!now_ns(&t)) flags |= CLOCK_FAILED;
    uint64_t index = atomic_fetch_add_explicit(&ring_header->claimed, 1, memory_order_relaxed);
    struct rc2fx_jackerr_slot *slot = &ring_slots[index % SLOTS];
    /* Take the slot only from an older, published (even) generation. Odd:
     * another writer is (or stalled) inside it. >= ours: a later lap already
     * published there. Either way this record is dropped, not waited for. */
    uint64_t current = atomic_load_explicit(&slot->seq, memory_order_acquire);
    for (;;) {
        if ((current & 1) || current >= 2 * index + 2) {
            atomic_fetch_add_explicit(&ring_header->dropped, 1, memory_order_relaxed);
            return;
        }
        if (atomic_compare_exchange_weak_explicit(&slot->seq, &current, 2 * index + 1,
                                                  memory_order_acq_rel, memory_order_acquire))
            break;
    }
    /* Orders the odd sequence before the data stores. */
    atomic_thread_fence(memory_order_release);
    uint8_t len1 = 0, len2 = 0;
    copy_string(slot->string1, STRING1_WORDS, string1, &len1, &flags, STRING1_TRUNCATED, STRING1_NULL);
    copy_string(slot->string2, STRING2_WORDS, string2, &len2, &flags, STRING2_TRUNCATED, STRING2_NULL);
    atomic_store_explicit(&slot->t_ns, t, memory_order_relaxed);
    atomic_store_explicit(&slot->kind_flags,
                          (uint64_t)kind | (uint64_t)flags << 16 | (uint64_t)len1 << 32 | (uint64_t)len2 << 40,
                          memory_order_relaxed);
    atomic_store_explicit(&slot->value1, value1, memory_order_relaxed);
    atomic_store_explicit(&slot->value2, value2, memory_order_relaxed);
    /* Only this writer holds the odd value, so only it publishes the slot. */
    atomic_store_explicit(&slot->seq, 2 * index + 2, memory_order_release);
    if (kind >= NOT_FINISHED && kind <= POLL_RECOVERY_FAILED) {
        uint64_t mono_ns = 0;
        now_ns(&mono_ns);
        rc2fx_jackerr_fault((uint64_t)kind, string1, mono_ns);
    }
}

#define FORMAT_NOT_FINISHED "JackEngine::XRun: client = %s was not finished, state = %s"
#define FORMAT_FINISHED_LATE "JackEngine::XRun: client %s finished after current callback"
#define FORMAT_TIMED_XRUN "JackTimedDriver::Process XRun = %ld usec"
#define FORMAT_ALSA_XRUN "**** alsa_pcm: xrun of at least %.3f msecs"
#define FORMAT_WAIT_STATUS "ALSA XRun wait_status = %d"
#define FORMAT_INTERNAL_XRUN "internal xrun detected: frame_offset = %ld"
#define FORMAT_POLL_FAILED "ALSA: poll call failed (%s)"
#define FORMAT_POLL_EXITING "ALSA: poll time out, polled for %lu usecs, Reached max retry cnt = %d, Exiting"
#define FORMAT_POLL_RETRYING "ALSA: poll time out, polled for %lu usecs, Retrying with a recovery, retry cnt = %d"
#define FORMAT_POLL_RECOVERY "ALSA: poll time out, recovery failed with status = %d"
#define FORMAT_POLL_INTERRUPT "ALSA: poll interrupt"

/* The vendor's jack_error() is a no-op; this one records instead, reading
 * arguments only for a recognised format and only with exactly its types. */
EXPORT void jack_error(const char *format, ...) {
    if (!ring_header) return;
    int saved = errno;
    atomic_fetch_add_explicit(&ring_header->calls, 1, memory_order_relaxed);
    if (!format) {
        atomic_fetch_add_explicit(&ring_header->unknown, 1, memory_order_relaxed);
        errno = saved;
        return;
    }
    va_list args;
    va_start(args, format);
    if (strcmp(format, FORMAT_NOT_FINISHED) == 0) {
        const char *client = va_arg(args, const char *);
        const char *state = va_arg(args, const char *);
        record(NOT_FINISHED, 0, 0, client, state);
    } else if (strcmp(format, FORMAT_FINISHED_LATE) == 0) {
        record(FINISHED_LATE, 0, 0, va_arg(args, const char *), NULL);
    } else if (strcmp(format, FORMAT_TIMED_XRUN) == 0) {
        record(TIMED_XRUN, va_arg(args, long), 0, NULL, NULL);
    } else if (strcmp(format, FORMAT_ALSA_XRUN) == 0) {
        double msecs = va_arg(args, double);
        int64_t usecs = msecs != msecs ? -1 : msecs > 9e15 ? INT64_MAX : msecs < -9e15 ? INT64_MIN : (int64_t)(msecs * 1000.0);
        record(ALSA_XRUN_MSECS, usecs, 0, NULL, NULL);
    } else if (strcmp(format, FORMAT_WAIT_STATUS) == 0) {
        record(ALSA_WAIT_STATUS, va_arg(args, int), 0, NULL, NULL);
    } else if (strcmp(format, FORMAT_INTERNAL_XRUN) == 0) {
        record(INTERNAL_XRUN, va_arg(args, long), 0, NULL, NULL);
    } else if (strcmp(format, FORMAT_POLL_FAILED) == 0) {
        record(POLL_FAILED, 0, 0, va_arg(args, const char *), NULL);
    } else if (strcmp(format, FORMAT_POLL_EXITING) == 0) {
        unsigned long usecs = va_arg(args, unsigned long);
        int retries = va_arg(args, int);
        record(POLL_EXITING, (int64_t)usecs, retries, NULL, NULL);
    } else if (strcmp(format, FORMAT_POLL_RETRYING) == 0) {
        unsigned long usecs = va_arg(args, unsigned long);
        int retries = va_arg(args, int);
        record(POLL_RETRYING, (int64_t)usecs, retries, NULL, NULL);
    } else if (strcmp(format, FORMAT_POLL_RECOVERY) == 0) {
        record(POLL_RECOVERY_FAILED, va_arg(args, int), 0, NULL, NULL);
    } else if (strcmp(format, FORMAT_POLL_INTERRUPT) == 0) {
        record(POLL_INTERRUPT, 0, 0, NULL, NULL);
    } else {
        atomic_fetch_add_explicit(&ring_header->unknown, 1, memory_order_relaxed);
        /* The literal itself, bounded: never its arguments. */
        size_t n = strnlen(format, STRING1_WORDS * 8);
        record(UNKNOWN, 0, 0, format, n == STRING1_WORDS * 8 ? format + n : NULL);
    }
    va_end(args);
    errno = saved;
}

/* A private directory: create it 0700, or accept an existing one only if it
 * is a real directory owned by us with no group/other access. */
static int private_directory(const char *path) {
    if (mkdir(path, 0700) != 0 && errno != EEXIST) return 0;
    struct stat st;
    if (lstat(path, &st) != 0) return 0;
    return S_ISDIR(st.st_mode) && st.st_uid == geteuid() && (st.st_mode & 077) == 0;
}

/* Only in /usr/bin/jackd (RC2FX_JACKERR_EXE names another executable for
 * host tests); anywhere else jack_error() stays a no-op. */
__attribute__((constructor)) static void start(void) {
    int saved = errno;
    static _Atomic uint64_t probe;
    if (!atomic_is_lock_free(&probe)) goto out;
    const char *expected = getenv("RC2FX_JACKERR_EXE");
    if (!expected) expected = "/usr/bin/jackd";
    char exe[256];
    ssize_t n = readlink("/proc/self/exe", exe, sizeof exe - 1);
    if (n <= 0) goto out;
    exe[n] = 0;
    if (strcmp(exe, expected) != 0) goto out;
    const char *directory = getenv("RC2FX_JACKERR_DIR");
    if (!directory) directory = RC2FX_JACKERR_DIR;
    if (!private_directory(directory)) goto out;
    uint64_t created = 0;
    if (!now_ns(&created)) goto out;
    char path[512];
    if (snprintf(path, sizeof path, "%s/ring-%d-%llu.ring", directory, (int)getpid(),
                 (unsigned long long)created) >= (int)sizeof path) goto out;
    size_t size = sizeof(struct rc2fx_jackerr_header) + (size_t)SLOTS * sizeof(struct rc2fx_jackerr_slot);
    int fd = open(path, O_RDWR | O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC, 0600);
    if (fd < 0) goto out;
    if (ftruncate(fd, (off_t)size) != 0) { close(fd); unlink(path); goto out; }
    void *map = mmap(NULL, size, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    close(fd);
    if (map == MAP_FAILED) { unlink(path); goto out; }
    /* Prefault each page now (the new file contains zeros), avoiding first-use
     * faults. This is not mlock: later residency is not guaranteed here. */
    memset(map, 0, size);
    struct rc2fx_jackerr_header *header = map;
    header->version = 2;
    header->slot_size = sizeof(struct rc2fx_jackerr_slot);
    header->slots = SLOTS;
    header->pid = (uint32_t)getpid();
    header->created_ns = created;
    ring_slots = (struct rc2fx_jackerr_slot *)(header + 1);
    /* The magic last: a reader that sees it sees a complete header. */
    atomic_thread_fence(memory_order_release);
    memcpy(header->magic, "RC2FXJE2", 8);
    ring_header = header;
out:
    errno = saved;
}
