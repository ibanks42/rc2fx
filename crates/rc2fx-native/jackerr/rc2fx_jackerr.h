/* Shared layout of the RC2FX jackd error ring (rc2fx_jackerr.c writes it,
 * rc2fx_jackerr_snapshot.c reads it). Little-endian, CLOCK_MONOTONIC. */
#ifndef RC2FX_JACKERR_H
#define RC2FX_JACKERR_H
#include <stdatomic.h>
#include <stdint.h>

#define RC2FX_JACKERR_SLOTS 4096u
#define RC2FX_JACKERR_STRING1_WORDS 5 /* 40 bytes */
#define RC2FX_JACKERR_STRING2_WORDS 2 /* 16 bytes */
#define RC2FX_JACKERR_DIR "/dev/shm/rc2fx-jackerr"

/* 64 bytes, then RC2FX_JACKERR_SLOTS slots of 96 bytes. */
struct rc2fx_jackerr_header {
    char magic[8];             /* "RC2FXJE2" */
    uint32_t version;          /* 2 */
    uint32_t slot_size;        /* 96 */
    uint32_t slots;
    uint32_t pid;
    uint64_t created_ns;
    _Atomic uint64_t claimed;  /* indices handed out */
    _Atomic uint64_t dropped;  /* claimed, but the slot was busy or newer: not written */
    _Atomic uint64_t unknown;  /* calls with an unrecognised or null format */
    _Atomic uint64_t calls;    /* every jack_error call */
};
/* One record. `seq` is the slot's sequence lock: 2i+1 while record i is
 * written, 2i+2 once published. A writer takes a slot only by CAS from an
 * older, even (published or never written) value, so two writers never
 * write one slot at once. */
struct rc2fx_jackerr_slot {
    _Atomic uint64_t seq;
    _Atomic uint64_t t_ns;
    _Atomic uint64_t kind_flags;   /* kind:16 flags:16 len1:8 len2:8 */
    _Atomic int64_t value1;
    _Atomic int64_t value2;
    _Atomic uint64_t string1[RC2FX_JACKERR_STRING1_WORDS];
    _Atomic uint64_t string2[RC2FX_JACKERR_STRING2_WORDS];
};
_Static_assert(sizeof(struct rc2fx_jackerr_header) == 64, "header layout");
_Static_assert(sizeof(struct rc2fx_jackerr_slot) == 96, "slot layout");
#endif
