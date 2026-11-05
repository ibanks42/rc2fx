/* Host test harness for rc2fx_jackerr.c (built with -DRC2FX_JACKERR_TESTING):
 * calls jack_error() the way the vendor's libjackserver and jack_alsa.so do.
 *   formats       one call per known format, plus edge cases
 *   threads N     N threads x 2000 not-finished calls
 *   vendor        the stand-in vendor library calls its own jack_error()
 *   stall         a writer stalled in a slot: a later lap must drop, not overwrite
 *   newer         a reservation older than the slot's record must drop
 *   errno         jack_error() must leave errno unchanged (exit 1 if not)
 *   clockfail     a failed clock read is flagged, the record kept
 *   live MS       2 writers for MS milliseconds (for a concurrent snapshot)
 * The test hooks are found with dlsym, so a production library is usable too. */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

void jack_error(const char *format, ...);
void vendor_check_xrun(const char *client);

#define NOT_FINISHED "JackEngine::XRun: client = %s was not finished, state = %s"
static _Atomic int stop;

static void *burst(void *arg) {
    (void)arg;
    for (int i = 0; i < 2000; i++) jack_error(NOT_FINISHED, "usb1out", "Running");
    return NULL;
}
static void *forever(void *arg) {
    const char *client = arg;
    while (!atomic_load(&stop)) jack_error(NOT_FINISHED, client, "Running");
    return NULL;
}

static _Atomic uint64_t *slot_seq(uint64_t index) {
    void *(*slot)(uint64_t) = (void *(*)(uint64_t))dlsym(RTLD_DEFAULT, "rc2fx_jackerr_test_slot");
    return slot ? (_Atomic uint64_t *)slot(index) : NULL; /* seq is the first field */
}

int main(int argc, char **argv) {
    const char *mode = argc > 1 ? argv[1] : "formats";
    if (strcmp(mode, "fault-hook") == 0) {
        uint64_t (*calls)(void) = (uint64_t (*)(void))dlsym(RTLD_DEFAULT, "rc2fx_jackerr_test_fault_calls");
        uint64_t (*published)(void) = (uint64_t (*)(void))dlsym(RTLD_DEFAULT, "rc2fx_jackerr_test_fault_published");
        if (!calls || !published) return 3;
        jack_error("unknown message");
        jack_error("ALSA: poll interrupt");
        if (calls() != 0) return 4;
        jack_error(NOT_FINISHED, "usb1out", "Running");
        jack_error("internal xrun detected: frame_offset = %ld", 10L);
        return calls() == 2 && published() == 2 ? 0 : 5;
    }
    if (strcmp(mode, "threads") == 0) {
        int n = argc > 2 ? atoi(argv[2]) : 4;
        pthread_t threads[16];
        for (int i = 0; i < n && i < 16; i++) pthread_create(&threads[i], NULL, burst, NULL);
        for (int i = 0; i < n && i < 16; i++) pthread_join(threads[i], NULL);
        return 0;
    }
    if (strcmp(mode, "vendor") == 0) { vendor_check_xrun("sip-audio-out"); return 0; }
    if (strcmp(mode, "stall") == 0 || strcmp(mode, "newer") == 0) {
        /* Records 0..9, then make slot 5 look stalled (odd: a writer inside)
         * or newer (a later lap's record), then write until index 4096 + 5
         * lands on that slot again. */
        for (int i = 0; i < 10; i++) jack_error(NOT_FINISHED, "early", "Running");
        _Atomic uint64_t *seq = slot_seq(5);
        if (!seq) return 3;
        uint64_t marked = strcmp(mode, "stall") == 0 ? 2 * 5 + 1 : 2 * (5 + 2 * 4096) + 2;
        atomic_store(seq, marked);
        for (int i = 10; i < 4096 + 10; i++) jack_error(NOT_FINISHED, "later", "Running");
        return atomic_load(seq) == marked ? 0 : 4; /* untouched by the later writer */
    }
    if (strcmp(mode, "errno") == 0) {
        errno = 1234;
        jack_error(NOT_FINISHED, "usb1out", "Running");
        jack_error("unknown %d", 1);
        jack_error((const char *)0);
        return errno == 1234 ? 0 : 1;
    }
    if (strcmp(mode, "clockfail") == 0) {
        void (*fail)(int) = (void (*)(int))dlsym(RTLD_DEFAULT, "rc2fx_jackerr_test_fail_clock");
        if (!fail) return 3;
        fail(1);
        jack_error(NOT_FINISHED, "usb1out", "Running");
        fail(0);
        return 0;
    }
    if (strcmp(mode, "live") == 0) {
        long ms = argc > 2 ? atol(argv[2]) : 500;
        struct timespec t = {ms / 1000, (ms % 1000) * 1000000L};
        pthread_t a, b;
        pthread_create(&a, NULL, forever, "usb1out");
        pthread_create(&b, NULL, forever, "usb1-chat-out");
        nanosleep(&t, NULL);
        atomic_store(&stop, 1);
        pthread_join(a, NULL);
        pthread_join(b, NULL);
        return 0;
    }
    jack_error(NOT_FINISHED, "usb1out", "Running");
    jack_error("JackEngine::XRun: client %s finished after current callback", "usb1-chat-out");
    jack_error("JackTimedDriver::Process XRun = %ld usec", 4321L);
    jack_error("**** alsa_pcm: xrun of at least %.3f msecs", 1.25);
    jack_error("ALSA XRun wait_status = %d", -32);
    jack_error("internal xrun detected: frame_offset = %ld", 77L);
    jack_error("ALSA: poll call failed (%s)", "Interrupted system call");
    jack_error("ALSA: poll time out, polled for %lu usecs, Reached max retry cnt = %d, Exiting", 5000000UL, 5);
    jack_error("ALSA: poll time out, polled for %lu usecs, Retrying with a recovery, retry cnt = %d", 2000000UL, 2);
    jack_error("ALSA: poll time out, recovery failed with status = %d", -5);
    jack_error("ALSA: poll interrupt");
    jack_error(NOT_FINISHED, "a-client-name-that-is-much-longer-than-forty-bytes-in-total", (const char *)0);
    jack_error("Some other message %d with %s that must not be formatted", 3, "x");
    jack_error((const char *)0);
    return 0;
}
