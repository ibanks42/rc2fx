# Xrun flight recorder

Kernel scheduler timestamps must pass a cadence check before the analyzer
attributes durations, CPU occupancy, or cross-CPU ordering. Invalid or
incomplete kernel timing is surfaced explicitly; native monotonic histories can
still be inspected on their own. Task selection uses `set_event_pid` rather
than event-field filters to avoid the kernel's event-discard path. Nearby
callbacks alone never prove which JACK cycle owned a fault.

## Measurement levels

Every level of instrumentation adds audio overhead. Measured on `main` with six
streams playing:

| Level | How to run it | Main mean | Slow callbacks per hour |
| --- | --- | --- | --- |
| 1. Always-on counters | `bench.py --no-trace`, client timing off | 884 us | about 50 |
| 2. Plus client timing | `run.py prepare`, then restart audio | 907 us | about 180 |
| 3. Plus kernel tracing | `run.py prepare`, restart audio, then `bench.py` | 940 us | about 1,860 |

A callback counts as slow when `main` finishes past 0.75 of the period.

Pass/fail qualification always uses level 1 via the settled control described
below. Level 3 acts as a magnifier for root-cause work: its extra overhead
triggers faults that wouldn't happen in an uninstrumented run. (The one
two-hour xrun occurred while tracing was active, with zero faults in the 63
untraced minutes after the trace froze.)

The trade-off is that a level 1 fault carries no kernel trace. To diagnose it,
reproduce with tracing enabled.

The audit report refreshes every second for the first three minutes (and
whenever client timing or gauges are enabled), and every three seconds
otherwise. Keep `--interval` above three seconds during level 1 runs.

`status.json` writes `measurementLevel` plus a `measurement` block covering the
entire window since baseline, derived purely from level 1 counters:
`slowCallbacks` (growth of `audit.cycle.endPos.histogram[4:]`),
`slowCallbacksPerHour`, and `meanCallbackWallUs`, `meanNativeWallUs`, and
`meanOwnedWallUs` from `callbackTiming[0]`. Means stabilize within five
minutes, while rates do not: level 1 produces only about 4 slow callbacks per
five minutes, so judge rates over at least 30 minutes.

## Bench gauges

`run.py gauges-on` creates the RAM marker `/run/rc2fx-gauges`. It takes effect
on the next audio restart, operates independently of client timing, and clears
on reboot (`gauges-off` removes it). Without the marker, `libasound` and
`libsamplerate` stay uninstrumented and the launcher passes `RC2FX_GAUGES=0`,
so no wrappers bind and no `gauges` block appears in the audit. Because gauges
add load, they are never used for pass/fail checks, and a run rejects them
unless `--gauges` is explicitly passed.

`bench.py --gauges` resets min/max trackers after settling and adds a `gauges`
block to `measurement`:

- `handles[]`, one entry per USB ALSA handle: `lowestFillFrames`, calculated
  the same way the vendor does (`ring - avail - frames` for playback,
  `avail - frames` for capture). Silence is only written when fill drops below
  0, and target fill is 146 frames. `--ring-frames` defaults to 784 (4 periods
  of 196 frames on the S32 gadget); adjust it if testing another USB mode.
- `srcMeanUsPerCycle`, `srcMaxCycleUs`, and `srcCallsPerCycle`: per-client
  resampler time measured around each `src_process` call.
- `vendorJob`: where RØDE's monitor job finished within its cycle (bucketed
  into `endPos` histogram bins), plus `pastJackdRead` for jobs finishing after
  jackd's read (roughly 110 us before the next estimated cycle start, where
  cycle start is estimated at one-frame resolution, 21 us).

## How it works

This is opt-in diagnostic tooling for the RØDECaster Pro II. It neither makes
xruns harmless nor proves from a clean run that every bug is gone. Its job is
to freeze evidence at the first fault without depending on host polling.

When a fault occurs, the JACK recorder publishes its fault record and hits a
normally empty `rc2fx_jackerr_fault` hook, where an optional kernel uprobe
freezes ftrace. A separate bounded native ring records callback entry/exit
times and freezes on the first xrun notification. Audio callbacks do zero
diagnostic file I/O. The report thread watches an arm token: changing the token
explicitly arms a fresh capture, so reading reports can never accidentally
unfreeze an existing one.

Because the mixer kernel's `mono` trace clock failed cross-CPU timestamp
calibration, this tool uses `mono_raw`, validates 32 bracketed clock samples
across all four CPUs on every arm, and aligns the frozen trace to the monotonic
timestamp passed by the fault hook (retaining user-to-probe latency
uncertainty).

Because the mixer kernel's uprobe `traceoff` stops global tracing even when
attached to a sub-instance, this tool demands exclusive access to an idle
global tracer, refuses to run if other tracing instances exist, saves prior
buffer/clock/on/off state, and restores it on `clear`. Capture runs with a
rolling 4 MiB per-CPU buffer covering scheduling events and syscalls filtered to
the measured USB callback, JACK driver, and external SIP process. Kernel
tracing isn't free: this is an instrumented investigation tool, not a
zero-overhead monitor. There are no per-callback uprobes.

Export the usual `RC2FX_MIXER_IP`, `RC2FX_MIXER_KEY`,
`RC2FX_MIXER_KNOWN_HOSTS`, and `RC2FX_MIXER_HOST_KEY_ALIAS` variables, and pass
a verified build bundle whose recorder hash matches the mixer:

```sh
python3 tools/xrun_trace/run.py prepare
# Explicitly restart/install audio with the normal mixer workflow once.
python3 tools/xrun_trace/bench.py --keep-going --bundle /path/to/verified/mixer-bundle --out /path/to/new-receipt-directory
```

The bench plays six quiet, distinct modulated stereo signals across System,
Game, Music, Virtual A, Virtual B, and Chat. By default it polls the mixer
every 30 seconds (`--interval`), verifying process and boot identity, telemetry
freshness, callback progress, xruns, and other error counters, and stops
playback when done. Run it in a persistent foreground session or service; it
never publishes releases or touches the desktop UI. With `--keep-going`, faults
are logged immediately while playback continues for the full duration, though
any fault still fails final qualification even if subsequent checkpoints are
clean. The first kernel trace remains frozen while counters and events keep
updating; the first frozen native audit is saved from the existing checkpoint
without an extra device read and matched to the capture token during analysis,
and the frozen kernel trace is pulled at the end of the run. Without
`--keep-going`, the bench stops on the first fault. Playback or transport
errors can still abort an observation run.

For manual capture and cleanup:

```sh
python3 tools/xrun_trace/run.py status
# Use stop only to end a capture manually; faults freeze automatically.
python3 tools/xrun_trace/run.py collect --out /path/to/new-capture-directory
python3 tools/xrun_trace/analyze.py /path/to/new-capture-directory
python3 tools/xrun_trace/run.py clear
```

`clear` archives the device receipt into a timestamped directory and removes
the uprobe. Cleanup is always explicit after collection (including after a
failed setup).

The analyzer separates execution time, runnable scheduling delay, blocking
syscalls, and pending wakes prior to callback entry, naming observed
competitors along with syscall numbers/arguments, IRQ spans, and wake sources.
Missing identities, gaps, or dropped events are called out as limitations, and
a scheduler symptom by itself does not prove which source-code bug caused it.
The raw trace, native audit, JACK binary snapshot, process maps, thread
priorities, boot identity, and buffer-loss counters are all preserved for
deeper inspection.

## Controlled validation

`cpu_hog.c` is a diagnostic fault injector that never ships in the runtime.
Running `python3 tools/xrun_trace/run.py self-test --out /path/to/new-self-test`
cross-compiles it with Zig, uploads it to RAM, triggers a fault, collects the
capture, and asserts that the analyzer blames the hog. The launcher temporarily
pins the measured USB callback to CPU 3, injects 10 ms of FIFO-80 work on that
CPU, restores the original affinity, and enforces a one-second watchdog. Because
this deliberately interrupts audio, only run it on an authorized bench. A valid
self-test must identify the hog as the thread delaying the USB callback, not
just observe a bumped xrun counter.

On 2026-11-05, final hardware validation captured `usb1out` TID 1879 waking at
roughly 9638.601195 s and sitting runnable until JACK reported the fault at
9638.602240 s, with the trace showing the injected hog holding CPU 3 across the
entire 1.045 ms window. Both the kernel trace and callback history froze
without host polling, with zero commit overruns or dropped trace events.

Host unit tests:

```sh
python3 -m unittest discover -s tools/xrun_trace -p 'test_*.py'
python3 -m unittest discover -s mixer/jackerr/tests -p test_jackerr.py
```

While armed, callback recording captures wall-clock boundaries only. Legacy
per-client CPU timing counters pause to avoid synchronized CPU-clock syscalls,
so `clientTiming.registrations[].timing` freezes at arm time and reflects
pre-playback values; use flight medians for loaded numbers. Main callback
history (including native-work boundaries) freezes independently. Broad IRQ and
syscall tracing are intentionally left out of the qualification profile because
they noticeably perturb the mixer. A capture can therefore show CPU occupancy
without separating hard IRQ time from normal execution, which calls for a
targeted follow-up rather than guessing.

Wake attribution relies on `sched_waking` (recorded before the target becomes
runnable), since `sched_wakeup` can arrive after the target is already running
on another CPU and a late wake must never misclassify an executing callback as
runnable delay. Dispatch lookup also discards wakes belonging to an earlier
completed callback.

`bench.py --avoid-driver` is a reversible scheduling experiment. After reading
live thread IDs and affinity masks, it pins the JACK FIFO-51 driver to CPU 1
and the two USB output threads to CPU 3 (intersected with their existing
masks), leaving the main callback on CPU 2. Original masks, PIDs, and thread
start times are saved with the capture, and `clear` restores them only if the
same thread is still alive. This is strictly an experimental bench policy, not
a production setting; normal bench runs leave affinities untouched. Diagnostic
tracing uses `set_event_pid` for mixer, JACK, and SIP tasks, with event-field
filters disabled to avoid the known discard path (see the
[kernel PID-selection documentation](https://docs.kernel.org/trace/ftrace.html#set-event-pid)).

The default profile logs scheduling transitions for the selected mixer, JACK,
and SIP tasks via PID selection without syscalls. `--deep` adds syscall capture
for those tasks, including external SIP semaphore identity. Because the minimal
profile can't prove continuous occupancy by an intermediate competitor, the
analyzer notes that boundary and reports intervals since observed switches;
switch to `--deep` when the minimal trace is ambiguous.

All report and map downloads stream through device-side `gzip -1` with shell
`pipefail` and decompress on the host, while uploads stay uncompressed. This
covers baseline reads, checkpoints, and arm verification, because plain SSH
report transfers were found to disturb audio. Capture archives are already
compressed. The default trace clock is `mono_raw`, calibrated across four CPUs
on each arm and aligned via the fault hook's monotonic timestamp.

`--interval 7200 --seconds 7200` runs a two-hour test with only baseline and
end reads. The final read allows five extra seconds for report cadence while
still requiring the receipt to cover the full requested duration in both host
and device time.

For an observation-overhead control, clear the optional client-timing marker
via the launcher's `xrun-trace unprepare`, explicitly restart audio through the
installed launcher, and run `bench.py --no-trace --seconds 300 --interval 30`.
The control aborts if the baseline still has the timing probe active. It runs
the same six-channel playback, compressed checkpoints, and error-counter checks
without arming or collecting a kernel trace. Restarting is always explicit;
`unprepare` only clears the marker for the next audio start.

Settled controls use `--no-trace --settle-seconds 60 --seconds 300 --trials 3
--interval 30`. They require at least three minutes of audio callback progress
and 60 seconds with a flat event counter, keep continuous six-channel playback
running between the five-minute trials, and abort on the first failing
checkpoint so startup bursts can never masquerade as steady-state controls.
