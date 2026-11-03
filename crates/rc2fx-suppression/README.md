# rc2fx-suppression

The noise suppression worker. It contains the audio-frame adapter, startup
preparation, the worker lifecycle, bounded queues, dry/wet alignment and
telemetry, and it implements `rc2fx_dsp::Suppression`.

`VendorModel` runs DeepFilterNet3 from `rc2fx-dfn`. It is plain Rust and
included in every build.

## Models and delay

| Model | Endpoint | Frame | Intrinsic | Worker allowance | Total samples |
|---|---|---:|---:|---:|---:|
| DeepFilterNet3, lookahead 0 | worker | 240 | 120 | 320 | 680 (14.2 ms) |
| DeepFilterNet3, lookahead 1 | worker | 240 | 360 | 320 | 920 (19.2 ms) |

`Endpoint::new` chooses the endpoint from the algorithm's admitted geometry in
`rc2fx-core`.

A nonzero worker allowance selects the threaded `Worker`. The allowance is a
whole number of 112-sample device periods, sized from the model's worst frame
time measured on the mixer. A result that misses it plays dry until it
arrives. The mixer's own period is
unchanged.

A zero allowance selects `Inline`. The model runs on the calling thread as
each frame completes. On the mixer that thread is the DSP helper, which has a
whole device period on its own core, so the output needs no scheduling margin.
This only works for a model whose worst frame fits in what remains of the
helper's period after its start offset and the other paths' DSP. None of the
admitted algorithms uses the inline endpoint. It
is kept for a model with a shorter hop. `deep_filter_from_weights`, which
loads a candidate model for benchmarking, builds a zero-lead geometry.

## Threaded worker

Construct `Worker` on the control thread, keep its `Telemetry`, and move its
box into `Dsp`. The audio callback has mutable access to that DSP and only
does bounded frame copies, atomic operations and mixing. Each model and its
recurrent history belong to one worker. The 8-slot queues have a single
producer endpoint and a single consumer endpoint, neither cloneable, and
acquire/release indices pass ownership of each slot.

The model is created and prepared before the worker is published. Any
per-thread or one-time setup runs on the new worker thread before it reports
ready, so it doesn't land on the first live frame.

A gap in the frame sequence recreates the model on the worker and invalidates
the next `ceil(intrinsic / frame)` results, three frames for DeepFilterNet3. Recovery doesn't repeat the warm-up.

### Thread placement

CPU masks and scheduling priorities are constructor options. On Linux the new
thread applies and verifies them before it is published. Requested CPUs must
be a subset of the creator's mask. Options that aren't set are inherited from
the creator, so priority is raised only on request. An explicit priority
requests SCHED_RR 1–20. An explicit nice value requests SCHED_OTHER at that
value. The two are mutually exclusive.

`PRODUCTION_WORKER` (RR 5 on CPU 0) is the mixer policy used by the service
and by the native plan defaults. Its doc comment records the measurements
behind it.

### Shutdown

Once the callback has stopped, call `finish` from control code, or retire the
DSP from the control side.

- An explicit `finish` can drain complete admitted frames. It returns the
  joined accounting, including the unprocessed partial tail. Failures and
  output queue overflows are reported.
- The first `finish` closes queue submission permanently, even if it times out.
- After a timeout the caller keeps the worker object and handle. It can
  observe and retry, but must not start a replacement.
- If no explicit retirement has started, `Drop` on the control side requests
  cancellation and waits up to two seconds for the thread before detaching it.
  A thread that is still running keeps its own `Arc` queues and model, so its
  memory stays valid.

`Telemetry::termination()` must be `Joined` before the service treats
retirement as complete. `TimedOut` stays unresolved even if the thread exits
later, and `StartupTerminationUnresolved` also blocks a replacement. Rust can't
safely kill a single inference thread, so a model call that never returns
needs an OS-level process supervisor.

`Dsp::drain_suppression` reaches the worker's drain path through the shared
provider trait after callbacks have returned. The first stop mode is kept.
Later calls to `finish`, retries and `Drop` don't change it.

`Telemetry::terminal()` returns the first joined `Terminal`, which is
immutable, or `None` before the join. Its `retirement_timed_out` field stays
true after a late join, so a missed deadline is still recorded once the
thread's resources can be released. Live snapshots and
`termination() == Joined` on their own don't prove the drained frame counts.

An already finished worker can be joined with a zero wait. After any wait,
completion has to be observed strictly before the deadline. A late wakeup of
the control thread can't certify that the worker finished in time.

## Inline endpoint

`Inline` has no thread. The model runs inside `process_sample` as each frame
completes, and the result plays over the next frame. Its single warm-up runs
on the first call, which is the only time it allocates. It reports through the
same `Telemetry` type (frame CPU and wall time, completed frames, failure),
which starts in the joined state, and `finish` publishes the terminal receipt
immediately.

## Audio handling

When a result is missing, stale, invalidated by recovery, nonfinite or failed,
the callback outputs the dry audio with the same delay. A worker result that
arrives after its frame has started is used for the rest of that frame. It is
counted both as a late result and as a missed deadline. Turning wet
processing off keeps the model history and the delay.

Normalization clamps finite input to ±1 and uses the vendor's 32768 scale.
Offline, a whole frame is validated before any model or output state changes,
and frames with the wrong length or nonfinite samples are rejected. In the
callback, nonfinite input is replaced with zero.

## Measurements

Worker CPU time uses `CLOCK_THREAD_CPUTIME_ID` and wall time uses
`CLOCK_MONOTONIC`. Histograms are relative to the frame's compute budget.
`allowance_overruns` separately compares wall time with the shorter playback
allowance. A good mean or a zero overrun count doesn't qualify the queue,
wakeup or callback deadlines.

Each live snapshot is atomic on its own. Joined snapshots are taken after the
worker has stopped. On non-Linux test builds, CPU time readings return clock
errors.

## Tests

`cargo test -p rc2fx-suppression` runs on every platform. The tests cover model state and reset, dry/wet sample alignment, zero
allocations on the callback thread, fixed queue capacity and wraparound, stale
and late results, timeouts, and partial final frames.
