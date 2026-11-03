// Host test program for gui_pacing.c (built with -DRC2FX_PACING_TEST), run
// under LD_AUDIT. Its functions stand in for the pinned GUI sites by name:
// test_pump_lock_site = the frame pump's pthread_mutex_lock, test_event_wait
// = WaitableEvent::wait's condition_variable::wait, test_repaint_a/_b = the
// repaint callers, test_other = any other caller.
#include <cerrno>
#include <chrono>
#include <condition_variable>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <mutex>
#include <pthread.h>
#include <sys/wait.h>
#include <unistd.h>
#include <atomic>
#include <algorithm>
#include <cstdint>
#include <thread>
#include <time.h>
#include <vector>
#include <fcntl.h>
#include <linux/videodev2.h>
#include <poll.h>
#include <sys/ioctl.h>
#include <sys/mman.h>

static pthread_mutex_t repaint_mutex = PTHREAD_MUTEX_INITIALIZER;

static unsigned long long now_ns() {
    timespec t{};
    clock_gettime(CLOCK_MONOTONIC, &t);
    return (unsigned long long)t.tv_sec * 1000000000ull + (unsigned long long)t.tv_nsec;
}

extern "C" __attribute__((noinline)) int test_pump_lock_site(pthread_mutex_t *m) {
    int r = pthread_mutex_lock(m);
    asm volatile("" ::: "memory");
    return r;
}
extern "C" int drmModeAtomicCommit(int, void *, uint32_t, void *);
extern "C" __attribute__((noinline)) int test_pump_unlock_site(pthread_mutex_t *m) {
    int r = pthread_mutex_unlock(m);
    asm volatile("" ::: "memory");
    return r;
}
extern "C" __attribute__((noinline)) int test_commit_site(void) {
    int r = drmModeAtomicCommit(7, nullptr, 1, nullptr);
    asm volatile("" ::: "memory");
    return r;
}
extern "C" __attribute__((noinline)) void test_other_lock(pthread_mutex_t *m) {
    pthread_mutex_lock(m);
    asm volatile("" ::: "memory");
}

// The dirty-region build (-DRC2FX_PACING_DIRTY): the repaint callbacks'
// frames as the GUI lays them out. x29 (here rbp) equals sp, and the dirty
// list and its count sit at +0x60 / +0x6c (path A) or +0x50 / +0x5c (B)
// when the repaint mutex is locked.
#if defined(__x86_64__)
extern "C" int test_paint_a(const int32_t *list, int count, pthread_mutex_t *m);
extern "C" int test_paint_b(const int32_t *list, int count, pthread_mutex_t *m);
asm(R"(
    .text
    .globl test_paint_a
    .type test_paint_a, @function
test_paint_a:
    push %rbp
    sub $0x70, %rsp
    mov %rsp, %rbp
    mov %rdi, 0x60(%rbp)
    mov %esi, 0x6c(%rbp)
    mov %rdx, %rdi
    call pthread_mutex_lock@PLT
    add $0x70, %rsp
    pop %rbp
    ret
    .size test_paint_a, .-test_paint_a
    .globl test_paint_b
    .type test_paint_b, @function
test_paint_b:
    push %rbp
    sub $0x60, %rsp
    mov %rsp, %rbp
    mov %rdi, 0x50(%rbp)
    mov %esi, 0x5c(%rbp)
    mov %rdx, %rdi
    call pthread_mutex_lock@PLT
    add $0x60, %rsp
    pop %rbp
    ret
    .size test_paint_b, .-test_paint_b
)");
#endif
// Stands in for global 0x12ec610: the current destination descriptor
// (DMABUF fd at +0x24, FB ID at +0x20).
extern "C" {
__attribute__((visibility("default"))) const uint8_t *test_current_destination = nullptr;
}

// The partial-conversion build: the vendor's conversion (0x7d0560), with
// its three ioctl sites, against the fake GE2D in fake_v4l2.c.
extern "C" int fake_v4l2_open(uint32_t **src);
extern "C" void fake_v4l2_add_dest(int fd, uint32_t *p, size_t pitch);
extern "C" unsigned long long fake_jobs, fake_pixels;
extern "C" __attribute__((noinline)) int test_capture_qbuf_site(int fd, v4l2_buffer *b) {
    int r = ioctl(fd, VIDIOC_QBUF, b);
    asm volatile("" ::: "memory");
    return r;
}
extern "C" __attribute__((noinline)) int test_output_qbuf_site(int fd, v4l2_buffer *b) {
    int r = ioctl(fd, VIDIOC_QBUF, b);
    asm volatile("" ::: "memory");
    return r;
}
extern "C" __attribute__((noinline)) int test_capture_dqbuf_site(int fd, v4l2_buffer *b) {
    int r = ioctl(fd, VIDIOC_DQBUF, b);
    asm volatile("" ::: "memory");
    return r;
}
extern "C" __attribute__((noinline)) int test_output_dqbuf_site(int fd, v4l2_buffer *b) {
    int r = ioctl(fd, VIDIOC_DQBUF, b);
    asm volatile("" ::: "memory");
    return r;
}
// As the vendor: capture QBUF (DMABUF), output QBUF (MMAP), wait, DQBUF both.
static int test_convert(int video, int dest_fd) {
    v4l2_buffer cap{};
    cap.type = V4L2_BUF_TYPE_VIDEO_CAPTURE, cap.memory = V4L2_MEMORY_DMABUF, cap.m.fd = dest_fd;
    if (test_capture_qbuf_site(video, &cap) != 0) return -1;
    v4l2_buffer out{};
    out.type = V4L2_BUF_TYPE_VIDEO_OUTPUT, out.memory = V4L2_MEMORY_MMAP, out.bytesused = 1280 * 720 * 4;
    errno = 77;
    if (test_output_qbuf_site(video, &out) != 0) return -2;
    if (errno != 77) return -5;  // a successful call leaves the program's errno alone
    pollfd p{video, POLLIN, 0};
    poll(&p, 1, 2000);
    v4l2_buffer dc{};
    dc.type = V4L2_BUF_TYPE_VIDEO_CAPTURE, dc.memory = V4L2_MEMORY_DMABUF;
    if (test_capture_dqbuf_site(video, &dc) != 0) return -3;
    v4l2_buffer dout{};
    dout.type = V4L2_BUF_TYPE_VIDEO_OUTPUT, dout.memory = V4L2_MEMORY_MMAP;
    return test_output_dqbuf_site(video, &dout) != 0 ? -4 : 0;
}

struct Event {
    std::mutex m;
    std::condition_variable cv;
    bool flag = false;
};
extern "C" __attribute__((noinline)) void test_event_wait(Event *e) {
    std::unique_lock<std::mutex> lock(e->m);
    while (!e->flag) e->cv.wait(lock);
    e->flag = false;
    asm volatile("" ::: "memory");
}
extern "C" __attribute__((noinline)) void test_repaint_a(Event *e) { test_event_wait(e); asm volatile("" ::: "memory"); }
extern "C" __attribute__((noinline)) void test_repaint_b(Event *e) { test_event_wait(e); asm volatile("" ::: "memory"); }
extern "C" __attribute__((noinline)) void test_other(Event *e) { test_event_wait(e); asm volatile("" ::: "memory"); }

static void signal_after(Event *e, int ms) {
    std::this_thread::sleep_for(std::chrono::milliseconds(ms));
    std::lock_guard<std::mutex> g(e->m);
    e->flag = true;
    e->cv.notify_all();
}

int main(int argc, char **argv) {
    if (argc < 2) return 2;
    if (!strcmp(argv[1], "pump")) {
        // pump N GAP_MS: N passes, GAP_MS of "work" between them
        int n = atoi(argv[2]), gap = atoi(argv[3]);
        for (int i = 0; i < n; ++i) {
            test_pump_lock_site(&repaint_mutex);
            printf("pass %llu\n", now_ns());
            pthread_mutex_unlock(&repaint_mutex);
            if (gap) std::this_thread::sleep_for(std::chrono::milliseconds(gap));
        }
        // Other lock sites are never paced.
        unsigned long long t0 = now_ns();
        for (int i = 0; i < 20; ++i) {
            test_other_lock(&repaint_mutex);
            pthread_mutex_unlock(&repaint_mutex);
        }
        printf("other_ns %llu\n", now_ns() - t0);
        return 0;
    }
    if (!strcmp(argv[1], "wait")) {
        // repaint_a blocks ~30 ms, repaint_b ~10 ms, other ~20 ms (not counted)
        // Each signaller starts just before its wait, so every wait blocks.
        Event a, b, o;
        std::thread ta(signal_after, &a, 30);
        test_repaint_a(&a);
        std::thread tb(signal_after, &b, 10);
        test_repaint_b(&b);
        std::thread to(signal_after, &o, 20);
        test_other(&o);
        ta.join(), tb.join(), to.join();
        // Already signalled: no wait at all, as an unblocked repaint would be.
        Event ready;
        ready.flag = true;
        test_repaint_a(&ready);
        printf("done\n");
        return 0;
    }
    if (!strcmp(argv[1], "handshake")) {
        // handshake N: the GUI's 1:1 paint/present loop. The message thread
        // requests a pass and waits (as a repaint) until the pump has made it;
        // the pump waits for each request outside any instrumented site.
        static int n;
        n = atoi(argv[2]);
        static Event request, done;
        std::thread pump([] {
            for (int i = 0; i < n; ++i) {
                {
                    std::unique_lock<std::mutex> lock(request.m);
                    while (!request.flag) request.cv.wait(lock);
                    request.flag = false;
                }
                test_pump_lock_site(&repaint_mutex);
                pthread_mutex_unlock(&repaint_mutex);
                std::lock_guard<std::mutex> g(done.m);
                done.flag = true;
                done.cv.notify_all();
            }
        });
        for (int i = 0; i < n; ++i) {
            {
                std::lock_guard<std::mutex> g(request.m);
                request.flag = true;
                request.cv.notify_all();
            }
            test_repaint_a(&done);
        }
        pump.join();
        printf("handshakes %d\n", n);
        return 0;
    }
#if defined(__x86_64__)
    if (!strcmp(argv[1], "dirty")) {
        // Paints through both paths, each followed by a presented frame into
        // alternating destinations; then paints the module must reject.
        alignas(16) static int32_t a[3][4] = {{10, 20, 30, 40}, {100, 200, 50, 60}, {1270, 700, 20, 30}};
        alignas(16) static int32_t b[1][4] = {{0, 0, 1280, 720}};
        alignas(16) static int32_t many[40][4];
        for (int i = 0; i < 40; ++i) many[i][0] = i, many[i][1] = 2 * i, many[i][2] = 3, many[i][3] = 4;
        alignas(16) static uint8_t descriptors[2][0x28];
        for (int d = 0; d < 2; ++d) {
            int32_t fd = 41 + d, fb = 7 + d;
            memcpy(descriptors[d] + 0x24, &fd, 4);
            memcpy(descriptors[d] + 0x20, &fb, 4);
        }
        for (int i = 0; i < 4; ++i) {
            if (i == 3) test_paint_b(&b[0][0], 1, &repaint_mutex);
            else test_paint_a(&a[0][0], 3, &repaint_mutex);
            pthread_mutex_unlock(&repaint_mutex);
            test_current_destination = descriptors[i & 1];
            test_pump_lock_site(&repaint_mutex);
            test_pump_unlock_site(&repaint_mutex);
            test_commit_site();
        }
        test_paint_a(&many[0][0], 40, &repaint_mutex);  // more than are listed
        pthread_mutex_unlock(&repaint_mutex);
        test_paint_a(&a[0][0], 0, &repaint_mutex);  // rejected: empty
        pthread_mutex_unlock(&repaint_mutex);
        test_paint_b(&a[0][1], 1, &repaint_mutex);  // rejected: misaligned list
        pthread_mutex_unlock(&repaint_mutex);
        test_paint_b(nullptr, 1, &repaint_mutex);  // rejected: no list
        pthread_mutex_unlock(&repaint_mutex);
        errno = 55;
        test_paint_a(&a[0][0], 1, &repaint_mutex);
        printf("errno_after_paint %d\n", errno);
        pthread_mutex_unlock(&repaint_mutex);
        printf("dirty done\n");
        return 0;
    }
    if (!strcmp(argv[1], "race")) {
        // race N: N paints on this thread while another thread commits N
        // frames, so two threads log at once (the log rotation's single
        // rotator, and the lines the others drop meanwhile).
        int n = atoi(argv[2]);
        alignas(16) static int32_t a[3][4] = {{10, 20, 30, 40}, {100, 200, 50, 60}, {1270, 700, 20, 30}};
        alignas(16) static uint8_t descriptors[2][0x28];
        for (int d = 0; d < 2; ++d) {
            int32_t fd = 41 + d, fb = 7 + d;
            memcpy(descriptors[d] + 0x24, &fd, 4);
            memcpy(descriptors[d] + 0x20, &fb, 4);
        }
        test_current_destination = descriptors[0];
        // RC2FX_TEST_CANARY: a third thread opens, appends to and closes a
        // file of its own in a loop, so a descriptor the logging code frees
        // would be handed to it (and log lines would land in that file).
        const char *canary = getenv("RC2FX_TEST_CANARY");
        std::atomic<bool> stop{false};
        std::thread opener([canary, &stop] {
            if (!canary) return;
            for (int i = 0; i < 2000000 && !stop.load(); ++i) {
                int fd = open(canary, O_WRONLY | O_CREAT | O_APPEND, 0600);
                if (fd < 0) continue;
                ssize_t written = write(fd, "canary\n", 7);
                (void)written;
                close(fd);
            }
        });
        // Strict ping-pong: a frame after each paint, then the next paint, so
        // both writers are busy throughout and the tail holds both kinds.
        std::atomic<int> painted{0}, committed{0};
        std::thread pump([n, &painted, &committed] {
            for (int i = 0; i < n; ++i) {
                while (painted.load() <= i) std::this_thread::yield();
                test_current_destination = (i & 1) ? descriptors[1] : descriptors[0];
                test_pump_lock_site(&repaint_mutex);
                test_pump_unlock_site(&repaint_mutex);
                test_commit_site();
                committed = i + 1;
            }
        });
        for (int i = 0; i < n; ++i) {
            while (committed.load() < i) std::this_thread::yield();
            test_paint_a(&a[0][0], 3, &repaint_mutex);
            pthread_mutex_unlock(&repaint_mutex);
            painted = i + 1;
        }
        pump.join();
        // A few paints end the log, so the surviving window has both kinds.
        for (int i = 0; i < 3; ++i) {
            test_paint_a(&a[0][0], 3, &repaint_mutex);
            pthread_mutex_unlock(&repaint_mutex);
        }
        stop = true;
        opener.join();
        printf("raced %d\n", n);
        return 0;
    }
#endif
#if defined(__x86_64__)
    if (!strcmp(argv[1], "partial")) {
        // partial FRAMES: meter-like paints into a fake source, each followed
        // by a conversion into alternating destinations; after each, the
        // destination must equal the rotated source everywhere.
        // PARTIAL_ROGUE_AT=f writes one source pixel outside any paint at
        // frame f; PARTIAL_KILL_AT=f creates PARTIAL_KILL_FILE at frame f.
        const int frames = atoi(argv[2]), SW = 1280, SH = 720;
        const int rogue = getenv("PARTIAL_ROGUE_AT") ? atoi(getenv("PARTIAL_ROGUE_AT")) : -1;
        const int kill_at = getenv("PARTIAL_KILL_AT") ? atoi(getenv("PARTIAL_KILL_AT")) : -1;
        const bool ccw = getenv("FAKE_GE2D_DIR") && !strcmp(getenv("FAKE_GE2D_DIR"), "ccw");
        // PARTIAL_SKIP makes the module's verify take one of its skip paths.
        const char *skip = getenv("PARTIAL_SKIP") ? getenv("PARTIAL_SKIP") : "";
        int skip_at = getenv("PARTIAL_SKIP_AT") ? atoi(getenv("PARTIAL_SKIP_AT")) : 0;
        uint32_t *src;
        int video = fake_v4l2_open(&src);
        if (video < 0) return 3;
        std::vector<uint32_t> dest[2] = {std::vector<uint32_t>(SW * SH), std::vector<uint32_t>(SW * SH)};
        alignas(16) static uint8_t descriptors[2][0x28];
        int dest_fds[2] = {9, 10};
        uint32_t *dest_ptrs[2] = {dest[0].data(), dest[1].data()};
        for (int d = 0; d < 2; ++d) {
            if (getenv("PARTIAL_REAL_DEST_FDS")) {
                dest_fds[d] = memfd_create("fake-destination", MFD_CLOEXEC);
                if (dest_fds[d] < 0 || ftruncate(dest_fds[d], SW * SH * 4)) return 4;
                dest_ptrs[d] = (uint32_t *)mmap(nullptr, SW * SH * 4, PROT_READ | PROT_WRITE,
                                              MAP_SHARED, dest_fds[d], 0);
                if (dest_ptrs[d] == MAP_FAILED) return 4;
            }
            int32_t fd = dest_fds[d], fb = 31 + d;
            uint32_t *p = dest_ptrs[d];
            memcpy(descriptors[d] + 0x18, &p, sizeof p);
            memcpy(descriptors[d] + 0x20, &fb, 4);
            memcpy(descriptors[d] + 0x24, &fd, 4);
            fake_v4l2_add_dest(fd, p, SH * 4);
        }
        for (int i = 0; i < SW * SH; ++i) src[i] = 0x01000193u * (uint32_t)i;
        int bad_frames = 0;
        for (int f = 0; f < frames; ++f) {
            alignas(16) int32_t rects[48][4] = {
                {193 + f % 5, 370 + (f * 3) % 40, 28, 9}, {193, 384, 10, 8}, {211, 383, 10, 8},
                {985 + f % 20, 45, 3, 30}, {1003, 67, 7, 8}, {131, 0, 274, 120}};
            int n = f % 10 == 7 ? 6 : 5;
            // Exact pending paints reconstructed from Follow-up 10: each
            // destination has been initialised before these are submitted.
            const char *event = getenv("PARTIAL_EVENT_CASE");
            if (event) {
                static const int32_t a[][4] = {
                    {167,362,28,3},{167,341,10,7},{185,341,10,7},{411,455,28,9},
                    {411,444,10,8},{429,444,10,8},{1010,45,3,30},{1018,45,7,8},{1019,67,7,8},
                    {167,365,28,3},{167,342,10,8},{185,342,10,8},{411,463,28,9},
                    {411,446,10,8},{429,446,10,7},{1010,45,2,30}};
                static const int32_t b[][4] = {
                    {131,0,274,120},{411,466,28,20},{411,453,10,8},{429,453,10,7},
                    {968,45,7,30},{979,45,7,8},{979,67,8,8},{411,449,28,26},
                    {411,444,10,5},{429,444,10,5},{972,45,9,30},{981,45,8,8},{981,67,8,8}};
                n = 0;
                if (f == 2) {
                    if (!strcmp(event, "A")) n = (int)(sizeof a / sizeof a[0]), memcpy(rects, a, sizeof a);
                    else if (!strcmp(event, "B")) n = (int)(sizeof b / sizeof b[0]), memcpy(rects, b, sizeof b);
                    else return 5;
                    std::vector<uint8_t> covered(SW * SH, 0);
                    for (int r = 0; r < n; ++r)
                        for (int y = rects[r][1]; y < rects[r][1] + rects[r][3]; ++y)
                            for (int x = rects[r][0]; x < rects[r][0] + rects[r][2]; ++x)
                                covered[y * SW + x] = 1;
                    printf("pending_union %ld\n", (long)std::count(covered.begin(), covered.end(), 1));
                }
            }
            if (!event && getenv("PARTIAL_CAPACITY")) {
                n = 40;
                for (int i = 0; i < n; i++) {
                    rects[i][0] = (i * 29) % (SW - 21), rects[i][1] = (i * 17) % (SH - 19);
                    rects[i][2] = 21, rects[i][3] = 19;
                }
            }
            if (!event && getenv("PARTIAL_RANDOM") && !strcmp(getenv("PARTIAL_RANDOM"), "1")) {
                uint32_t rng = 0x9e3779b9u * (unsigned)(f + 1);
                auto next = [&]() { rng ^= rng << 13; rng ^= rng >> 17; rng ^= rng << 5; return rng; };
                n = 6;
                for (int i = 0; i < n; i++) {
                    int x = (int)(next() % SW), y = (int)(next() % SH);
                    rects[i][0] = x, rects[i][1] = y;
                    rects[i][2] = 1 + (int)(next() % (unsigned)std::min(61, SW - x));
                    rects[i][3] = 1 + (int)(next() % (unsigned)std::min(35, SH - y));
                }
                // Token intersection, non-aligned tails, and all four edges.
                int32_t edge[4][4] = {{0, 0, 19, 23}, {1279, 719, 1, 1},
                                     {0, 701, 3, 19}, {1263, 0, 17, 1}};
                memcpy(rects[f % n], edge[f % 4], sizeof edge[0]);
            }
            // PARTIAL_WIDE adds a far-corner rect, so a bounding box over the
            // paint spans most of the frame.
            if (getenv("PARTIAL_WIDE")) {
                rects[n][0] = 1260, rects[n][1] = 700, rects[n][2] = 20, rects[n][3] = 20;
                n++;
            }
            if (n && f % 25 == 3) test_paint_b(&rects[0][0], n, &repaint_mutex);
            else if (n) test_paint_a(&rects[0][0], n, &repaint_mutex);
            for (int r = 0; r < n; ++r)  // the "renderer" draws inside the clip
                for (int y = rects[r][1]; y < rects[r][1] + rects[r][3]; ++y)
                    for (int x = rects[r][0]; x < rects[r][0] + rects[r][2]; ++x)
                        src[y * SW + x] = (uint32_t)(f * 2654435761u) ^ (uint32_t)(y * SW + x);
            if (n) pthread_mutex_unlock(&repaint_mutex);
            if (getenv("PARTIAL_INVALID") && f == 4) {
                test_paint_a(nullptr, 1, &repaint_mutex);
                pthread_mutex_unlock(&repaint_mutex);
            }
            if (f == rogue) src[700 * SW + 1270] ^= 0xffffffffu;  // written outside any paint
            if (f == kill_at) close(open(getenv("PARTIAL_KILL_FILE"), O_CREAT | O_WRONLY, 0600));
            int d = f & 1;
            uintptr_t keep;
            memcpy(&keep, descriptors[d] + 0x18, sizeof keep);
            test_current_destination = descriptors[d];
            if (f >= skip_at && !strcmp(skip, "descriptor-null")) test_current_destination = nullptr;
            if (f >= skip_at && !strcmp(skip, "dest-mismatch")) test_current_destination = descriptors[d ^ 1];
            if (f >= skip_at && !strcmp(skip, "dst-null")) memset(descriptors[d] + 0x18, 0, sizeof(uintptr_t));
            test_pump_lock_site(&repaint_mutex);
            std::vector<uint32_t> before;
            if (getenv("PARTIAL_REAL_DEST_FDS")) before.assign(dest_ptrs[d], dest_ptrs[d] + SW * SH);
            int r = test_convert(video, dest_fds[d]);
            if (r == -2 && !before.empty())
                printf("failed_qbuf_cpu_writes %d\n", memcmp(before.data(), dest_ptrs[d], SW * SH * 4) != 0);
            test_pump_unlock_site(&repaint_mutex);
            test_commit_site();
            if (!strcmp(skip, "dst-null")) memcpy(descriptors[d] + 0x18, &keep, sizeof keep);  // the module only reads
            long mismatches = 0;
            for (int row = 0; row < SW; ++row)
                for (int col = 0; col < SH; ++col) {
                    int x = ccw ? SW - 1 - row : row, y = ccw ? col : SH - 1 - col;
                    mismatches += dest_ptrs[d][(size_t)row * SH + col] != src[(size_t)y * SW + x];
                }
            bad_frames += mismatches != 0;
            printf("frame %d dest %d convert %d mismatches %ld\n", f, 9 + d, r, mismatches);
        }
        printf("partial frames %d bad_frames %d jobs %llu pixels %llu\n", frames, bad_frames, fake_jobs,
               fake_pixels);
        return 0;
    }
#endif
    if (!strcmp(argv[1], "errno")) {
        test_pump_lock_site(&repaint_mutex);
        pthread_mutex_unlock(&repaint_mutex);
        errno = 4321;
        test_pump_lock_site(&repaint_mutex);  // paced (back to back)
        printf("errno %d\n", errno);
        pthread_mutex_unlock(&repaint_mutex);
        Event e;
        std::thread t(signal_after, &e, 5);
        errno = 777;
        test_repaint_a(&e);
        printf("errno_after_wait %d\n", errno);
        t.join();
        return 0;
    }
    if (!strcmp(argv[1], "frames")) {
        // frames N: the pump's full sequence per frame
        int n = atoi(argv[2]);
        for (int i = 0; i < n; ++i) {
            test_pump_lock_site(&repaint_mutex);
            std::this_thread::sleep_for(std::chrono::milliseconds(3));  // "conversion"
            test_pump_unlock_site(&repaint_mutex);
            errno = 99;
            int r = test_commit_site();
            printf("commit %d errno %d\n", r, errno);
        }
        return 0;
    }
    if (!strcmp(argv[1], "cancel-write")) {
        // A cancel is already pending when the pump passes a log write
        // (its first pass writes "first-pass"): the write must not act on it.
        static std::atomic<int> go{0}, reached{0};
        pthread_t thread;
        pthread_create(&thread, nullptr, [](void *) -> void * {
            while (!go.load()) {}  // no cancellation point while the cancel arrives
            test_pump_lock_site(&repaint_mutex);
            reached = 1;
            pthread_mutex_unlock(&repaint_mutex);
            for (;;) pthread_testcancel();
            return nullptr;
        }, nullptr);
        pthread_cancel(thread);
        go = 1;
        void *result;
        pthread_join(thread, &result);
        printf("cancelled %d reached %d\n", result == PTHREAD_CANCELED, reached.load());
        return 0;
    }
    if (!strcmp(argv[1], "fork")) {
        // A forked helper inherits the module: it must never pace or log.
        test_pump_lock_site(&repaint_mutex);
        pthread_mutex_unlock(&repaint_mutex);
        fflush(stdout);
        pid_t child = fork();
        if (child == 0) {
            unsigned long long t0 = now_ns();
            for (int i = 0; i < 5; ++i) {
                test_pump_lock_site(&repaint_mutex);
                pthread_mutex_unlock(&repaint_mutex);
            }
            printf("child_ms %.1f\n", (double)(now_ns() - t0) / 1e6);
            fflush(stdout);
            _exit(0);
        }
        int status;
        waitpid(child, &status, 0);
        return 0;
    }
    if (!strcmp(argv[1], "cancel")) {
        // A pass waiting inside the pacing is cancelled: the cancel must take
        // effect only at the program's own next cancellation point, after the
        // lock was taken (no new cancellation point in the pump).
        static unsigned long long locked_at = 0;
        pthread_t thread;
        pthread_create(&thread, nullptr, [](void *) -> void * {
            test_pump_lock_site(&repaint_mutex);
            pthread_mutex_unlock(&repaint_mutex);
            test_pump_lock_site(&repaint_mutex);  // paced: waits ~66 ms
            locked_at = now_ns();
            pthread_mutex_unlock(&repaint_mutex);
            for (;;) pthread_testcancel();
            return nullptr;
        }, nullptr);
        std::this_thread::sleep_for(std::chrono::milliseconds(10));
        unsigned long long cancelled_at = now_ns();
        pthread_cancel(thread);
        void *result;
        pthread_join(thread, &result);
        printf("cancelled %d locked_after_cancel_ms %.1f\n", result == PTHREAD_CANCELED,
               // Signed: a lock taken before the cancel must show as negative.
               locked_at ? (double)((long long)locked_at - (long long)cancelled_at) / 1e6 : -1.0);
        return 0;
    }
    return 2;
}
