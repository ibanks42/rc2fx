/* Take an immutable snapshot of a live RC2FX jackd error ring, on the device.
 *
 *     rc2fx_jackerr_snapshot RING_OR_DIRECTORY OUT
 *
 * RING_OR_DIRECTORY is a ring file, or a directory whose newest
 * ring-<pid>-<created>.ring is taken. Each slot is read with its sequence
 * lock (acquire, atomic data loads, fence, recheck); a slot not holding a
 * complete record for its index is counted (unpublished or dropped,
 * overwritten by a later lap, or changed while read), never copied. OUT is
 * written whole (OUT.partial, then rename). Read-only on the ring; never
 * writes to it. Decode OUT with `cargo xtask mixer xruns` (or tests/jackerr_ring.py).
 *
 * OUT: "RC2FXJS1", u32 version 1, u32 record size 96, the ring header's
 * fields as read (pid, created, claimed, dropped, unknown, calls), the
 * snapshot time (0 when the clock could not be read), first index, end index, and counts (stable, unpublished,
 * overwritten, torn), then each stable record as 96 bytes (its seq field
 * holding its index).
 */
#define _GNU_SOURCE
#include "rc2fx_jackerr.h"
#include <dirent.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

struct snapshot_header {
    char magic[8];
    uint32_t version, record_size;
    uint64_t pid, created_ns, claimed, dropped, unknown, calls;
    uint64_t snapshot_ns, first_index, end_index;
    uint64_t stable, unpublished, overwritten, torn;
    uint64_t reserved;
};
_Static_assert(sizeof(struct snapshot_header) == 128, "snapshot header layout");
/* Monotonic now, or 0 (unknown) if the clock cannot be read. */
static uint64_t snapshot_time(void) {
    struct timespec now = {0, 0};
#ifdef RC2FX_JACKERR_TESTING
    if (getenv("RC2FX_SNAPSHOT_TEST_FAIL_CLOCK")) return 0;
#endif
    if (clock_gettime(CLOCK_MONOTONIC, &now) != 0) return 0;
    return (uint64_t)now.tv_sec * 1000000000u + (uint64_t)now.tv_nsec;
}
struct plain_slot {
    uint64_t index, t_ns, kind_flags;
    int64_t value1, value2;
    uint64_t string1[RC2FX_JACKERR_STRING1_WORDS], string2[RC2FX_JACKERR_STRING2_WORDS];
};
_Static_assert(sizeof(struct plain_slot) == 96, "record layout");

/* The newest ring in a directory, by its created time. */
static int newest(const char *directory, char *path, size_t size) {
    DIR *dir = opendir(directory);
    if (!dir) return 0;
    unsigned long long best = 0;
    int found = 0;
    for (struct dirent *entry; (entry = readdir(dir));) {
        int pid;
        unsigned long long created;
        char tail[8];
        if (sscanf(entry->d_name, "ring-%d-%llu.%7s", &pid, &created, tail) == 3 && strcmp(tail, "ring") == 0
            && created >= best) {
            best = created;
            found = snprintf(path, size, "%s/%s", directory, entry->d_name) < (int)size;
        }
    }
    closedir(dir);
    return found;
}

int main(int argc, char **argv) {
    if (argc != 3) { fprintf(stderr, "usage: %s RING_OR_DIRECTORY OUT\n", argv[0]); return 2; }
    char path[512];
    struct stat st;
    if (stat(argv[1], &st) != 0) { perror(argv[1]); return 2; }
    if (S_ISDIR(st.st_mode)) {
        if (!newest(argv[1], path, sizeof path)) { fprintf(stderr, "no ring in %s\n", argv[1]); return 2; }
    } else if (snprintf(path, sizeof path, "%s", argv[1]) >= (int)sizeof path) {
        return 2;
    }
    int fd = open(path, O_RDONLY | O_NOFOLLOW | O_CLOEXEC);
    if (fd < 0) { perror(path); return 2; }
    size_t size = sizeof(struct rc2fx_jackerr_header) + (size_t)RC2FX_JACKERR_SLOTS * sizeof(struct rc2fx_jackerr_slot);
    if (fstat(fd, &st) != 0 || (size_t)st.st_size != size) { fprintf(stderr, "%s: wrong size\n", path); return 2; }
    void *map = mmap(NULL, size, PROT_READ, MAP_SHARED, fd, 0);
    close(fd);
    if (map == MAP_FAILED) { perror("mmap"); return 2; }
    const struct rc2fx_jackerr_header *ring = map;
    const struct rc2fx_jackerr_slot *slots = (const struct rc2fx_jackerr_slot *)(ring + 1);
    char magic[8];
    memcpy(magic, ring->magic, 8);
    atomic_thread_fence(memory_order_acquire);
    if (memcmp(magic, "RC2FXJE2", 8) != 0 || ring->version != 2 || ring->slot_size != sizeof(struct rc2fx_jackerr_slot)
        || ring->slots != RC2FX_JACKERR_SLOTS) {
        fprintf(stderr, "%s: not a version 2 ring\n", path);
        return 2;
    }
    struct snapshot_header out = {0};
    memcpy(out.magic, "RC2FXJS1", 8);
    out.version = 1;
    out.record_size = sizeof(struct plain_slot);
    out.pid = ring->pid;
    out.created_ns = ring->created_ns;
    out.end_index = atomic_load_explicit((_Atomic uint64_t *)&ring->claimed, memory_order_acquire);
    out.claimed = out.end_index;
    out.dropped = atomic_load_explicit((_Atomic uint64_t *)&ring->dropped, memory_order_relaxed);
    out.unknown = atomic_load_explicit((_Atomic uint64_t *)&ring->unknown, memory_order_relaxed);
    out.calls = atomic_load_explicit((_Atomic uint64_t *)&ring->calls, memory_order_relaxed);
    out.snapshot_ns = snapshot_time();
    out.first_index = out.end_index > RC2FX_JACKERR_SLOTS ? out.end_index - RC2FX_JACKERR_SLOTS : 0;
    struct plain_slot *records = calloc(RC2FX_JACKERR_SLOTS, sizeof *records);
    if (!records) return 2;
    for (uint64_t index = out.first_index; index < out.end_index; index++) {
        _Atomic uint64_t *seq = (_Atomic uint64_t *)&slots[index % RC2FX_JACKERR_SLOTS].seq;
        const struct rc2fx_jackerr_slot *slot = &slots[index % RC2FX_JACKERR_SLOTS];
        uint64_t before = atomic_load_explicit(seq, memory_order_acquire);
        if (before < 2 * index + 2) { out.unpublished++; continue; }
        if (before > 2 * index + 2) { out.overwritten++; continue; }
        struct plain_slot copy = {.index = index};
#define LOAD(field) atomic_load_explicit((_Atomic __typeof__(copy.field) *)&slot->field, memory_order_relaxed)
        copy.t_ns = LOAD(t_ns);
        copy.kind_flags = LOAD(kind_flags);
        copy.value1 = LOAD(value1);
        copy.value2 = LOAD(value2);
        for (int i = 0; i < RC2FX_JACKERR_STRING1_WORDS; i++)
            copy.string1[i] = atomic_load_explicit((_Atomic uint64_t *)&slot->string1[i], memory_order_relaxed);
        for (int i = 0; i < RC2FX_JACKERR_STRING2_WORDS; i++)
            copy.string2[i] = atomic_load_explicit((_Atomic uint64_t *)&slot->string2[i], memory_order_relaxed);
        atomic_thread_fence(memory_order_acquire);
        if (atomic_load_explicit(seq, memory_order_relaxed) != before) { out.torn++; continue; }
        records[out.stable++] = copy;
    }
    char partial[600];
    if (snprintf(partial, sizeof partial, "%s.partial", argv[2]) >= (int)sizeof partial) return 2;
    FILE *file = fopen(partial, "wbx");
    if (!file) { perror(partial); return 2; }
    int ok = fwrite(&out, sizeof out, 1, file) == 1
        && fwrite(records, sizeof *records, out.stable, file) == out.stable;
    ok = fclose(file) == 0 && ok;
    if (!ok || rename(partial, argv[2]) != 0) { unlink(partial); perror(argv[2]); return 2; }
    printf("%s: %llu stable, %llu unpublished, %llu overwritten, %llu torn of %llu claimed (%llu dropped)\n",
           path, (unsigned long long)out.stable, (unsigned long long)out.unpublished,
           (unsigned long long)out.overwritten, (unsigned long long)out.torn,
           (unsigned long long)out.claimed, (unsigned long long)out.dropped);
    return 0;
}
