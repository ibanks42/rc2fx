# Xrun flight recorder

This is opt-in diagnostic tooling for the RØDECaster Pro II. It does not make
xruns harmless or treat a clean run as proof that every cause has been removed.
It preserves evidence at the first fault, independently of host polling.

The JACK recorder publishes its fault record, then reaches a normally empty
`rc2fx_jackerr_fault` hook. An optional kernel uprobe freezes ftrace there.
A separate bounded native ring retains callback entry/exit times and freezes
on the first xrun notification. Audio callbacks perform no diagnostic file I/O.
The report thread reads the arm token. Changing that token explicitly starts a
new capture; reading reports cannot accidentally unfreeze an existing capture.

The mixer kernel's `mono` trace clock failed cross-CPU timestamp calibration.
The tool uses the calibrated hardware `local` clock, validates 32 bracketed clock samples across four CPUs
on every arm, and aligns the frozen trace to a monotonic timestamp carried by
the fault hook. Alignment retains user-to-probe latency uncertainty.

The mixer kernel's uprobe `traceoff` affects global tracing even when installed
in an instance. This tool therefore requires exclusive use of an idle global
tracer, refuses existing tracing instances, saves the prior buffer/clock/on/off
settings, and restores them on `clear`. Capture uses a rolling 4 MiB buffer per
CPU, scheduling events and syscalls filtered to the measured USB callback. Kernel tracing adds overhead: this is an instrumented qualification,
not a claim that observation is free. There are no per-callback uprobes.

Set the usual `RC2FX_MIXER_IP`, `RC2FX_MIXER_KEY`,
`RC2FX_MIXER_KNOWN_HOSTS`, and `RC2FX_MIXER_HOST_KEY_ALIAS` environment variables.
Use an exact build bundle whose recorder hash matches the running mixer:

```sh
python3 tools/xrun_trace/run.py prepare
# Explicitly restart/install audio with the normal mixer workflow once.
python3 tools/xrun_trace/bench.py --bundle /path/to/verified/mixer-bundle --out /path/to/new-receipt-directory
```

The bench plays six quiet, distinct modulated stereo signals through System,
Game, Music, Virtual A, Virtual B and Chat. It reads the mixer every 900 seconds,
checks process/boot identity, telemetry freshness, callback progress, xruns and
other error counters, and stops its own playback on completion. Run it in a
persistent foreground session or service. It does not publish releases or touch
the desktop window. On failure it collects the frozen trace automatically.

For manual collection:

```sh
python3 tools/xrun_trace/run.py status
# Use stop only to end a capture manually; faults freeze automatically.
python3 tools/xrun_trace/run.py collect --out /path/to/new-capture-directory
python3 tools/xrun_trace/analyze.py /path/to/new-capture-directory
python3 tools/xrun_trace/run.py clear
```

`clear` preserves the device receipt in a timestamped saved directory and
removes this recorder's probe. It must be explicit after evidence collection.
A failed setup likewise requires inspection and explicit cleanup.

The analysis distinguishes execution, runnable scheduling delay, blocking
syscalls, and an outstanding wake before callback entry. It names observed
competitors and records syscall numbers/arguments, IRQ spans, and wake sources.
Missing identity, missing intervals or lost events are reported as limitations;
a scheduler mechanism alone does not establish which source-code defect caused
it. The raw trace, native audit, JACK binary snapshot, process maps, thread
priorities, boot identity and buffer-loss counters remain available for deeper
analysis.

## Controlled validation

`cpu_hog.c` is a diagnostic injector, never part of the runtime. The host command
`python3 tools/xrun_trace/run.py self-test --out /path/to/new-self-test`
cross-compiles it with Zig, uploads it into RAM, injects the fault, collects the
capture and requires attribution to the hog. The launcher command temporarily pins the measured USB callback to CPU 3 and injects 10 ms
of FIFO-80 work there. It restores the original affinity and has a one-second
watchdog. This intentionally interrupts audio and is only for an authorized
bench. A validated capture must name that hog as the thread delaying the USB
callback, not merely observe that the xrun count increased.

On 2026-11-05 the final hardware validation captured `usb1out` TID 1879 waking
at approximately 9638.601195 s and waiting runnable until JACK reported the
fault at 9638.602240 s. The trace identified the injected hog occupying CPU 3
for that entire 1.045 ms interval. Both trace and callback history froze
without host polling. There were no commit overruns or dropped trace events.

Host checks:

```sh
python3 -m unittest discover -s tools/xrun_trace -p 'test_*.py'
python3 -m unittest discover -s crates/rc2fx-native/jackerr/tests -p test_jackerr.py
```

While armed, callback recording uses wall-clock boundaries only. Per-client
legacy CPU timing counters pause to avoid synchronized CPU-clock syscalls.
Main callback history, including native-work boundaries, freezes separately.
IRQ and broad syscall tracing are intentionally omitted from the qualification
profile because they materially perturb this mixer. A capture may establish
CPU occupancy without distinguishing hard IRQ time from execution; that
limitation requires a targeted follow-up rather than an invented conclusion.
