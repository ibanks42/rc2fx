/* Bounded, explicitly invoked diagnostic fault injection. Never in runtime. */
#define _GNU_SOURCE
#include <sched.h>
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
    if (argc != 2) return 2;
    char *end; long cpu = strtol(argv[1], &end, 10);
    if (*end || cpu < 0 || cpu >= sysconf(_SC_NPROCESSORS_ONLN) || cpu >= CPU_SETSIZE) return 2;
    cpu_set_t set; CPU_ZERO(&set); CPU_SET(cpu, &set);
    if (sched_setaffinity(0, sizeof(set), &set)) return 3;
    prctl(PR_SET_NAME, "rc2fx-test-hog", 0, 0, 0);
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
