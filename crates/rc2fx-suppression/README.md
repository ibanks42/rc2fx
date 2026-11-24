# rc2fx-suppression

The noise suppression worker crate. It implements `rc2fx_dsp::Suppression` and handles audio-frame adaptation, startup warm-up, worker lifecycle, bounded queues, dry/wet alignment, and telemetry.

`VendorModel` wraps DeepFilterNet3 from `rc2fx-dfn` and is compiled into every build.

## Models and delay

| Model | Endpoint | Frame | Intrinsic | Worker allowance | Total samples |
|---|---|---:|---:|---:|---:|
| DeepFilterNet3, lookahead 0 | worker | 240 | 120 | 320 | 680 (14.2 ms) |
| DeepFilterNet3, lookahead 1 | worker | 240 | 360 | 320 | 920 (19.2 ms) |

`Endpoint::new` pulls its geometry from the admitted geometry in `rc2fx-core`, and every endpoint runs inference on the threaded `Worker`.

The worker allowance is an integer multiple of the 112-sample device period, sized from the model's worst-case frame time on hardware. If a frame misses its deadline, the callback plays dry audio until the result shows up, keeping the mixer's period fixed. `deep_filter_from_weights`, used for benchmarking candidate models, sets up a zero-lead geometry.

## Threaded worker

Build `Worker` on the control thread, hang onto its `Telemetry`, and move its boxed endpoint into `Dsp`. The audio callback gets mutable access to that `Dsp` and only performs bounded copies, atomics, and mixing. Each model instance and its recurrent state belong exclusively to one worker. The 8-slot SPSC queues expose non-cloneable producer and consumer ends, passing slot ownership via acquire/release indices.

Model initialization and warm-up finish on the worker thread before it signals ready, keeping one-time setup off the first live audio frame.

If a gap occurs in the frame sequence, the worker rebuilds the model and invalidates the next `ceil(intrinsic / frame)` outputs (three frames for DeepFilterNet3) without re-running startup warm-up.

### Thread placement

CPU affinity masks and scheduling priorities are passed at construction. On Linux, the new worker thread applies and verifies them before reporting ready. Requested CPUs must be a subset of the creator's mask; unset options are inherited, so priority is only bumped when explicitly asked for. An explicit priority sets `SCHED_RR` 1–20, while an explicit nice value sets `SCHED_OTHER` at that niceness. The two are mutually exclusive.

`PRODUCTION_WORKER` (`SCHED_RR` 5 on CPU 0) is the mixer policy used by both the service and the native launch plan defaults. Its doc comment has the benchmark numbers behind that choice.

### Shutdown

After the audio callback has stopped, call `finish` from the control thread or retire the DSP from the control side.

- Calling `finish` can drain any complete admitted frames and returns the joined accounting, including the unprocessed partial tail, failures, and output-queue overflows.
- The first `finish` call permanently closes queue submission, even if it times out.
- After a timeout, keep the worker object and handle. You can inspect status and retry joining, but never spin up a replacement.
- If nothing initiated retirement explicitly, `Drop` on the control side requests cancellation and waits up to two seconds before detaching the thread. A still-running worker retains its own `Arc` handles to the queues and model so its memory stays valid.

Don't consider retirement done until `Telemetry::termination()` returns `Joined`. `TimedOut` remains unresolved even if the thread eventually exits, and `StartupTerminationUnresolved` also blocks starting a replacement. Since Rust has no safe way to kill a stuck inference thread, a hung model call has to be handled by an OS-level process supervisor.

`Dsp::drain_suppression` invokes the worker's drain path through the shared provider trait once callbacks have returned. Whichever stop mode was chosen first sticks; later `finish` calls, retries, and `Drop` won't alter it.

`Telemetry::terminal()` returns `None` until the thread is joined, after which it returns the immutable `Terminal` record. Its `retirement_timed_out` flag remains true even after a late join so missed deadlines stay visible once resources can be freed. Keep in mind that live snapshots and `termination() == Joined` alone don't guarantee drained frame counts.

An already-exited worker joins immediately with zero wait. When waiting, completion must be observed strictly before the deadline; a late wakeup on the control thread doesn't count as finishing in time.

## Audio handling

Whenever a result is missing, stale, invalidated by recovery, nonfinite, or errored, the callback substitutes dry audio at the exact same delay. If a worker result shows up partway through its frame, it's used for the remainder of that frame and counted as both a late result and a missed deadline. Disabling wet processing leaves both model history and latency intact.

Normalization clamps finite samples to ±1 and applies the vendor's 32768 scaling. Offline, entire frames are validated before touching model or output state, rejecting wrong lengths or nonfinite samples. Inside the audio callback, nonfinite inputs are zeroed out.

## Measurements

Worker CPU time comes from `CLOCK_THREAD_CPUTIME_ID` and wall time from `CLOCK_MONOTONIC`. Histograms are bucketed against the frame's compute budget, while `allowance_overruns` compares wall time against the tighter playback allowance. A healthy mean or zero overrun count alone doesn't prove queue, wakeup, or callback deadlines are being met.

Individual live snapshots are atomic; joined snapshots are captured after the worker exits.

## Tests

`cargo test -p rc2fx-suppression` runs on Linux (the only platform this crate builds for). Tests cover model state and resets, dry/wet alignment, zero callback allocations, fixed queue capacity and wraparound, stale/late results, timeouts, and partial final frames.
