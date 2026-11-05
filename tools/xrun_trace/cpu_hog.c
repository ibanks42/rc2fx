/* Bounded, explicitly invoked diagnostic fault injection. Never in runtime. */
#define _GNU_SOURCE
#include <sched.h>
#include <fcntl.h>
#include <string.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/prctl.h>
#include <time.h>
#include <unistd.h>
static long long ns(void) {
    struct timespec t;
    if (clock_gettime(CLOCK_MONOTONIC, &t)) _exit(3);
    return (long long)t.tv_sec * 1000000000 + t.tv_nsec;
}
int main(int argc, char **argv) {
    if (argc != 2 && (argc != 3 || strcmp(argv[2], "--trace-pid"))) return 2;
    char *end; long cpu = strtol(argv[1], &end, 10);
    if (*end || cpu < 0 || cpu >= sysconf(_SC_NPROCESSORS_ONLN) || cpu >= CPU_SETSIZE) return 2;
    cpu_set_t set; CPU_ZERO(&set); CPU_SET(cpu, &set);
    if (sched_setaffinity(0, sizeof(set), &set)) return 3;
    prctl(PR_SET_NAME, "rc2fx-test-hog", 0, 0, 0);
    if (argc == 3) {
        int fd = open("/sys/kernel/debug/tracing/set_event_pid", O_WRONLY | O_APPEND);
        if (fd < 0) return 3;
        char pid[32]; int size = snprintf(pid, sizeof pid, "%ld\n", (long)getpid());
        if (write(fd, pid, size) != size || close(fd)) return 3;
        /* Make the injector's switch-out/in visible before taking FIFO80. */
        struct timespec pause = {.tv_nsec = 1000000};
        if (nanosleep(&pause, NULL)) return 3;
    }
    alarm(1); /* Independent upper bound even if the diagnostic loop misbehaves. */
    struct sched_param priority = {.sched_priority = 80};
    if (sched_setscheduler(0, SCHED_FIFO, &priority)) return 3;
    long long began = ns(), now;
    do { now = ns(); } while (now - began < 10000000);
    priority.sched_priority = 0;
    if (sched_setscheduler(0, SCHED_OTHER, &priority)) return 3;
    alarm(0);
    printf("controlled CPU contention cpu=%ld start_ns=%lld end_ns=%lld\n", cpu, began, now);
    return 0;
}
