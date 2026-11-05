# Xrun flight recorder

Kernel scheduler timestamps must pass a cadence check before the analyzer
attributes durations, CPU occupancy or cross-CPU ordering. Invalid or incomplete
kernel timing is reported explicitly; native monotonic histories can still be
examined separately. Task selection uses `set_event_pid` rather than event-field
filters to avoid the kernel event-discard path. Nearby callbacks alone do not
prove which JACK cycle owned a fault.

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
The tool uses `mono_raw`, validates 32 bracketed clock samples across four CPUs
on every arm, and aligns the frozen trace to a monotonic timestamp carried by
the fault hook. Alignment retains user-to-probe latency uncertainty.

The mixer kernel's uprobe `traceoff` affects global tracing even when installed
in an instance. This tool therefore requires exclusive use of an idle global
tracer, refuses existing tracing instances, saves the prior buffer/clock/on/off
settings, and restores them on `clear`. Capture uses a rolling 4 MiB buffer per
CPU, scheduling events and syscalls filtered to the measured USB callback, JACK
driver and external SIP process. Kernel tracing adds overhead: this is an instrumented qualification,
not a claim that observation is free. There are no per-callback uprobes.

Set the usual `RC2FX_MIXER_IP`, `RC2FX_MIXER_KEY`,
`RC2FX_MIXER_KNOWN_HOSTS`, and `RC2FX_MIXER_HOST_KEY_ALIAS` environment variables.
Use an exact build bundle whose recorder hash matches the running mixer:

```sh
python3 tools/xrun_trace/run.py prepare
# Explicitly restart/install audio with the normal mixer workflow once.
python3 tools/xrun_trace/bench.py --keep-going --bundle /path/to/verified/mixer-bundle --out /path/to/new-receipt-directory
```

The bench plays six quiet, distinct modulated stereo signals through System,
Game, Music, Virtual A, Virtual B and Chat. It reads the mixer every 30 seconds by default (configurable with `--interval`),
checks process/boot identity, telemetry freshness, callback progress, xruns and
other error counters, and stops its own playback on completion. Run it in a
persistent foreground session or service. It does not publish releases or touch
the desktop window. With `--keep-going`, faults are recorded immediately and
playback continues for the requested duration. Any observed fault still makes
the final qualification fail, even if later checkpoints look clean. The first
kernel trace stays frozen; subsequent counters and event reports continue.
The first frozen native audit is saved from the existing checkpoint without an
extra device read, and matched to the capture token during final analysis.
The frozen trace is collected when the run ends. Without `--keep-going`, the
bench retains its earlier stop-on-fault behavior. Playback or transport failure
can still prevent a complete observation run.

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

Wake attribution uses `sched_waking`, before the target becomes runnable.
`sched_wakeup` can be logged after the target has already run on another CPU;
a late wake notification must never turn an executing callback into a runnable
delay. Dispatch lookup also rejects wakes from an earlier completed callback.

`bench.py --avoid-driver` is a reversible scheduling experiment. After reading
live thread identities and affinity masks, it restricts the JACK FIFO-51 driver
to CPU 1 and restricts the two USB output threads to CPU 3, intersecting
their existing masks. The main callback remains on CPU 2.
The original masks, process IDs and thread start times are saved with the
capture. `clear` restores them only if the same process/thread still exists.
This is an experimental bench policy, not a production fix. The ordinary bench
leaves affinity unchanged. Diagnostic tracing uses `set_event_pid` for mixer,
JACK and SIP tasks. Event field filters are disabled to avoid the known discard path.
See the [kernel PID-selection documentation](https://docs.kernel.org/trace/ftrace.html#set-event-pid).

The default profile records transitions involving selected mixer, JACK and SIP
tasks through PID selection, and omits syscall events. `--deep` adds syscall
capture for those tasks, including external SIP semaphore identity. The minimal profile cannot establish
continuous occupancy by an intermediate competitor; analysis marks that scope
and reports intervals since observed switches. Use the deep profile when the
minimal evidence does not distinguish the mechanism.

All report and map downloads use device-side `gzip -1` with shell `pipefail`,
then host-side decompression. Uploads stay plain. This applies to baseline and
checkpoint reads as well as arm verification: plain SSH report transfers were
already shown to interfere with audio. Capture archives are already compressed.
The default trace clock is `mono_raw`, validated across four CPUs on each arm
and aligned using the fault hook's monotonic timestamp.

`--interval 7200 --seconds 7200` requests baseline/end-only reads for a two-hour
run. The final read includes five seconds of allowance for report cadence; the
receipt must still cover the requested duration in both host and device time.

For an observation-overhead control, disable the optional client-timing marker
with the launcher's `xrun-trace unprepare`, explicitly restart audio using the
installed launcher, then run `bench.py --no-trace --seconds 300 --interval 30`.
The control refuses a baseline with the optional timing probe still enabled.
It uses the same audio, compressed checkpoint transport and error-counter
checks, but does not arm or collect a kernel trace. Restarting is explicit;
`unprepare` itself only removes the diagnostic marker for the next audio start.

Settled controls use `--no-trace --settle-seconds 60 --seconds 300 --trials 3
--interval 30`. They require at least three minutes of audio callback progress
and a flat event counter for 60 seconds, retain continuous six-channel playback
between the five-minute trials, and stop at the first failed checkpoint.
A startup burst cannot be counted as a steady-state control.
