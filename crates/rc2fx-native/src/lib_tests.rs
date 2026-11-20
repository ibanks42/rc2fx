use super::*;

#[cfg(test)]
mod hook_tests {
    use super::*;

    /// The tests here drive the process wrapper and the buffer hook, which
    /// count into process-wide statics: one at a time.
    static HOOKS: Mutex<()> = Mutex::new(());
    /// What the stand-in vendor callback saw of the published count while it
    /// was still inside its frame.
    static SEEN_INSIDE: AtomicU64 = AtomicU64::new(0);

    fn serial() -> (MutexGuard<'static, ()>, MutexGuard<'static, ()>) {
        let hooks = HOOKS.lock().unwrap_or_else(|poison| poison.into_inner());
        // Callbacks a few microseconds apart count as short gaps, which the
        // gap test reads.
        let events = probe::EVENT_TESTS
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        READ_MOSTLY
            .get_buffer
            .store(real_get_buffer as *const () as usize, Ordering::Release);
        (hooks, events)
    }
    /// The real getter: a pointer that names its port and frame count.
    unsafe extern "C" fn real_get_buffer(port: *mut c_void, frames: u32) -> *mut c_void {
        (port as usize * 1024 + frames as usize) as *mut c_void
    }
    /// The vendor callback: fetches `argument` buffers through the hook.
    unsafe extern "C" fn vendor(frames: u32, argument: *mut c_void) -> c_int {
        for port in 1..=argument as usize {
            let got = unsafe { get_buffer(port as *mut c_void, frames) };
            assert_eq!(got as usize, port * 1024 + frames as usize);
        }
        SEEN_INSIDE.store(
            CYCLE.buffer_calls_inside.load(Ordering::Relaxed),
            Ordering::Relaxed,
        );
        0
    }
    fn context(calls: usize) -> &'static Context {
        let context = Box::leak(Box::new(Context::new()));
        context
            .callback
            .store(vendor as *const () as usize, Ordering::Release);
        context.argument.store(calls, Ordering::Relaxed);
        context
    }
    fn run(context: &'static Context) -> c_int {
        unsafe { process(112, (context as *const Context).cast_mut().cast()) }
    }
    fn clock_reads() -> [u64; 2] {
        timing::CLOCK_READS.with(Cell::get)
    }

    /// N callbacks of K hooked buffer calls each publish N x K, once per
    /// callback at its end, with no pipeline attached.
    #[test]
    fn buffer_calls_inside_a_callback_are_published_once_when_it_ends() {
        let _serial = serial();
        const CALLS: usize = 68;
        const CALLBACKS: u64 = 25;
        let context = context(CALLS);
        assert!(context.pipeline.load(Ordering::Relaxed).is_null());
        let inside = || CYCLE.buffer_calls_inside.load(Ordering::Relaxed);
        let (before, callbacks) = (inside(), CYCLE.callbacks.load(Ordering::Relaxed));
        let outside = BUFFER_CALLS_OUTSIDE.load(Ordering::Relaxed);
        for done in 0..CALLBACKS {
            assert_eq!(run(context), 0);
            // Nothing was published while the callback was inside its frame.
            assert_eq!(
                SEEN_INSIDE.load(Ordering::Relaxed),
                before + done * CALLS as u64
            );
            assert_eq!(inside(), before + (done + 1) * CALLS as u64);
        }
        assert_eq!(
            CYCLE.callbacks.load(Ordering::Relaxed),
            callbacks + CALLBACKS
        );
        assert_eq!(BUFFER_CALLS_OUTSIDE.load(Ordering::Relaxed), outside);
        // The thread is outside any frame again, with nothing left to publish.
        assert!(!super::inside());
        assert_eq!(INSIDE.with(|inside| inside.buffer_calls.get()), 0);
        assert_eq!(
            context.timing.snapshot().callback_wall.calls,
            CALLBACKS,
            "every callback was timed"
        );
    }

    /// A call from a thread that is in no callback frame (a USB client's
    /// thread) returns exactly what the real getter returns and writes no
    /// counter, unless the bench gauges are on.
    #[test]
    fn a_buffer_call_outside_a_callback_is_forwarded_and_counted_only_with_the_gauges() {
        let _serial = serial();
        let inside = CYCLE.buffer_calls_inside.load(Ordering::Relaxed);
        let outside = BUFFER_CALLS_OUTSIDE.load(Ordering::Relaxed);
        std::thread::spawn(move || {
            for port in 1..=40usize {
                let got = unsafe { get_buffer(port as *mut c_void, 112) };
                assert_eq!(got as usize, port * 1024 + 112);
            }
            assert_eq!(INSIDE.with(|inside| inside.buffer_calls.get()), 0);
        })
        .join()
        .unwrap();
        assert_eq!(BUFFER_CALLS_OUTSIDE.load(Ordering::Relaxed), outside);
        assert_eq!(CYCLE.buffer_calls_inside.load(Ordering::Relaxed), inside);
        READ_MOSTLY.gauges.store(true, Ordering::Relaxed);
        for port in 1..=40usize {
            let got = unsafe { get_buffer(port as *mut c_void, 112) };
            assert_eq!(got as usize, port * 1024 + 112);
        }
        READ_MOSTLY.gauges.store(false, Ordering::Relaxed);
        assert_eq!(BUFFER_CALLS_OUTSIDE.load(Ordering::Relaxed), outside + 40);
        assert_eq!(CYCLE.buffer_calls_inside.load(Ordering::Relaxed), inside);
    }

    /// A frame inside another publishes its own calls when it ends; the
    /// enclosing frame keeps its own count and publishes it at its own end.
    #[test]
    fn a_nested_frame_publishes_only_its_own_buffer_calls() {
        let _serial = serial();
        let published = || CYCLE.buffer_calls_inside.load(Ordering::Relaxed);
        let call = |times: usize| {
            for port in 1..=times {
                unsafe { get_buffer(port as *mut c_void, 112) };
            }
        };
        let frame = |owner| CallbackFrame {
            owner,
            processing: false,
            pipeline: 0,
            started: 0,
            jack: jack_cache::Snapshot::INACTIVE,
        };
        let before = published();
        let outer = enter_frame(frame(0x10));
        call(3);
        let inner = enter_frame(frame(0x20));
        call(2);
        assert_eq!(published(), before);
        leave_frame(inner);
        assert_eq!(published(), before + 2, "the inner frame's two");
        assert_eq!(INSIDE.with(|inside| inside.get().owner), 0x10);
        call(1);
        leave_frame(outer);
        assert_eq!(published(), before + 2 + 4, "the outer frame's four");
        assert!(!super::inside());
        // A frame with no buffer call publishes nothing.
        leave_frame(enter_frame(frame(0x30)));
        assert_eq!(published(), before + 6);
    }

    /// Several threads in callback frames at once, each with its own number
    /// of buffer calls, while other threads fetch buffers outside any frame
    /// (as the USB clients do). Each callback publishes exactly its own
    /// calls, no thread's count reaches another, and a call from outside a
    /// frame is counted nowhere while the gauges are off.
    #[test]
    fn concurrent_callbacks_and_outside_callers_keep_their_own_counts() {
        let _serial = serial();
        const THREADS: usize = 6;
        const CALLBACKS: u64 = 300;
        let inside = CYCLE.buffer_calls_inside.load(Ordering::Relaxed);
        let callbacks = CYCLE.callbacks.load(Ordering::Relaxed);
        let outside = BUFFER_CALLS_OUTSIDE.load(Ordering::Relaxed);
        let stop = AtomicBool::new(false);
        let outside_calls = AtomicU64::new(0);
        let expected: u64 = std::thread::scope(|scope| {
            for _ in 0..3 {
                scope.spawn(|| {
                    let mut port = 1usize;
                    let mut calls = 0;
                    while !stop.load(Ordering::Relaxed) {
                        let got = unsafe { get_buffer(port as *mut c_void, 112) };
                        assert_eq!(got as usize, port * 1024 + 112);
                        port = port % 40 + 1;
                        calls += 1;
                    }
                    assert_eq!(INSIDE.with(|inside| inside.buffer_calls.get()), 0);
                    outside_calls.fetch_add(calls, Ordering::Relaxed);
                });
            }
            let workers: Vec<_> = (0..THREADS)
                .map(|thread| {
                    scope.spawn(move || {
                        let calls = 3 + 13 * thread;
                        let context = context(calls);
                        for _ in 0..CALLBACKS {
                            assert_eq!(run(context), 0);
                            assert_eq!(INSIDE.with(|inside| inside.buffer_calls.get()), 0);
                            assert!(!super::inside());
                        }
                        assert_eq!(context.timing.snapshot().callback_wall.calls, CALLBACKS);
                        calls as u64 * CALLBACKS
                    })
                })
                .collect();
            let expected = workers
                .into_iter()
                .map(|worker| worker.join().unwrap())
                .sum();
            stop.store(true, Ordering::Relaxed);
            expected
        });
        assert!(outside_calls.load(Ordering::Relaxed) > 0);
        assert_eq!(
            CYCLE.buffer_calls_inside.load(Ordering::Relaxed),
            inside + expected
        );
        assert_eq!(
            CYCLE.callbacks.load(Ordering::Relaxed),
            callbacks + THREADS as u64 * CALLBACKS
        );
        assert_eq!(BUFFER_CALLS_OUTSIDE.load(Ordering::Relaxed), outside);
    }

    unsafe extern "C" fn ten_frames_in(_: *const c_void) -> u32 {
        10
    }

    /// The wrapper's own clock reads: five per callback without a cycle clock
    /// and six with one, and never the thread CPU clock or the second
    /// cycle-start estimate unless the bench gauges are on.
    #[test]
    fn the_wrapper_reads_no_diagnostic_clock_without_the_gauges() {
        let _serial = serial();
        const CALLBACKS: u64 = 40;
        let context = context(4);
        // A callback the host deschedules for milliseconds records a gap or
        // an overrun event, which stamps itself: those reads are not the
        // wrapper's, and no other test records events while this one runs.
        let reads = |context: &'static Context| {
            let (before, events) = (clock_reads(), probe::events_recorded());
            for _ in 0..CALLBACKS {
                assert_eq!(run(context), 0);
            }
            let after = clock_reads();
            let events = probe::events_recorded() - events;
            [after[0] - before[0] - events, after[1] - before[1]]
        };
        // No client, so no cycle clock: start, timing entry, before and after
        // the original callback, exit.
        assert_eq!(reads(context), [5 * CALLBACKS, 0]);
        // With a cycle clock: one more read, for the cycle start.
        let clock = probe::CYCLE_CLOCK.swap(ten_frames_in as *const () as usize, Ordering::AcqRel);
        context.client.store(0x1234, Ordering::Relaxed);
        let drift = probe::ESTIMATE_DRIFT_MAX_NS.load(Ordering::Relaxed);
        assert_eq!(reads(context), [6 * CALLBACKS, 0]);
        assert_eq!(probe::ESTIMATE_DRIFT_MAX_NS.load(Ordering::Relaxed), drift);
        assert_eq!(context.timing.snapshot().callback_cpu.calls, 0);
        // Gauges on: the second estimate (one read per callback that ends in
        // the first half of its cycle, as all of these do), and four CPU
        // clock reads on the one sampled callback.
        let fresh = self::context(4);
        fresh.client.store(0x1234, Ordering::Relaxed);
        READ_MOSTLY.gauges.store(true, Ordering::Relaxed);
        let gauged = reads(fresh);
        READ_MOSTLY.gauges.store(false, Ordering::Relaxed);
        probe::CYCLE_CLOCK.store(clock, Ordering::Release);
        // A callback descheduled past half a period skips the second
        // estimate, so on a loaded host the count can fall short of seven.
        assert!(
            gauged[0] > 6 * CALLBACKS && gauged[0] <= 7 * CALLBACKS,
            "{gauged:?}"
        );
        assert_eq!(gauged[1], 4);
        assert_eq!(fresh.timing.snapshot().callback_cpu.calls, 1);
        assert_eq!(fresh.timing.snapshot().owned_cpu.calls, 1);
    }

    /// Every counter this change moved still reaches the report under its own
    /// key, and what only a bench run measures is null without the gauges.
    #[test]
    fn the_report_keeps_every_key_and_nulls_bench_only_fields_without_the_gauges() {
        let _serial = serial();
        let number = |value: &serde_json::Value| value.as_u64().expect("a number");
        // Marks: each counter gets its own amount, far above anything other
        // tests add meanwhile, so a swapped key shows as the wrong multiple.
        const MARK: u64 = 1 << 40;
        let marked = |after: &serde_json::Value, before: &serde_json::Value| {
            (number(after) - number(before)) / MARK
        };
        let before = build_report();
        CYCLE.callbacks.fetch_add(MARK, Ordering::Relaxed);
        CYCLE
            .buffer_calls_inside
            .fetch_add(2 * MARK, Ordering::Relaxed);
        CYCLE.gaps_short.fetch_add(3 * MARK, Ordering::Relaxed);
        CYCLE.gaps_long.fetch_add(4 * MARK, Ordering::Relaxed);
        for (bin, gap) in CYCLE.gaps.iter().enumerate() {
            gap.fetch_add((5 + bin as u64) * MARK, Ordering::Relaxed);
        }
        pipeline::HELPER_TIME
            .frames
            .fetch_add(12 * MARK, Ordering::Relaxed);
        pipeline::HELPER_TIME
            .total_ns
            .fetch_add(13 * MARK, Ordering::Relaxed);
        BUFFER_CALLS_OUTSIDE.fetch_add(14 * MARK, Ordering::Relaxed);
        pipeline::RESOLVE_NS.fetch_add(15 * MARK, Ordering::Relaxed);
        pipeline::PATH_COST_NS[3].fetch_add(16 * MARK, Ordering::Relaxed);
        let after = build_report();
        assert_eq!(marked(&after["callbacks"], &before["callbacks"]), 1);
        assert_eq!(marked(&after["buffersInside"], &before["buffersInside"]), 2);
        let (band, band_before) = (
            &after["callbackGapsOutsideBand"],
            &before["callbackGapsOutsideBand"],
        );
        assert_eq!(marked(&band["short"], &band_before["short"]), 3);
        assert_eq!(marked(&band["long"], &band_before["long"]), 4);
        let gaps = after["callbackGaps"].as_array().unwrap();
        assert_eq!(gaps.len(), 7);
        for (bin, gap) in gaps.iter().enumerate() {
            assert_eq!(marked(gap, &before["callbackGaps"][bin]), 5 + bin as u64);
        }
        assert_eq!(
            marked(&after["helper"]["frames"], &before["helper"]["frames"]),
            12
        );
        assert_eq!(
            marked(&after["helper"]["totalNs"], &before["helper"]["totalNs"]),
            13
        );
        for key in ["maxNs", "stalls", "late", "minMarginNs"] {
            assert!(after["helper"][key].is_u64(), "helper.{key}");
        }
        assert!(after["maxCallbackGapNs"].is_u64());
        for key in [
            "processed",
            "processedUnacknowledged",
            "rawHelperUnready",
            "rawNoHandoff",
            "rawNotDone",
            "rawInvalid",
            "unobserved",
            "rawReturns",
        ] {
            assert!(after["sources"][key].is_u64(), "sources.{key}");
        }
        for key in ["startPos", "endPos", "helperPos"] {
            assert_eq!(
                after["cycle"][key]["histogram"].as_array().unwrap().len(),
                8
            );
        }
        // Bench-only: present as keys, null without the gauges, and no gauges
        // section.
        let object = after.as_object().unwrap();
        for key in ["buffersOutside", "pathCostNs", "resolveNs"] {
            assert!(object.contains_key(key), "{key}");
            assert!(after[key].is_null(), "{key} without the gauges");
        }
        let cycle = after["cycle"].as_object().unwrap();
        assert!(cycle.contains_key("estimateDriftMaxNs"));
        assert!(cycle["estimateDriftMaxNs"].is_null());
        assert!(!object.contains_key("gauges"));
        assert_eq!(after["retiredCallbackTiming"]["cpuSampleEvery"], 0);
        assert_eq!(
            after["retiredCallbackTiming"]["callbackCpu"]["calls"], 0,
            "the wire format keeps its CPU fields"
        );
        // The launcher greps the first `"callbacks":` of the file for the
        // callback count: it must still be the top-level one.
        let text = serde_json::to_string(&after).unwrap();
        let first = text.find("\"callbacks\":").unwrap();
        let count: String = text[first + 12..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        assert_eq!(count.parse::<u64>().unwrap(), number(&after["callbacks"]));
        // With the gauges, the same keys carry the measurements.
        READ_MOSTLY.gauges.store(true, Ordering::Relaxed);
        let gauged = build_report();
        READ_MOSTLY.gauges.store(false, Ordering::Relaxed);
        assert_eq!(number(&gauged["buffersOutside"]) / MARK, 14);
        assert_eq!(number(&gauged["resolveNs"]) / MARK, 15);
        let costs = gauged["pathCostNs"].as_array().unwrap();
        assert_eq!(costs.len(), rc2fx_native_control::MAX_PATHS);
        assert_eq!(number(&costs[3]) / MARK, 16);
        assert_eq!(number(&costs[2]) / MARK, 0);
        assert!(gauged["cycle"]["estimateDriftMaxNs"].is_u64());
        assert_eq!(
            gauged["retiredCallbackTiming"]["cpuSampleEvery"],
            timing::CPU_SAMPLE_EVERY
        );
        let wake = &gauged["gauges"]["helperWake"];
        assert_eq!(wake["boundsUs"].as_array().unwrap().len(), 7);
        for key in ["wakeCall", "wakeLatency", "end", "place"] {
            assert_eq!(wake[key]["histogram"].as_array().unwrap().len(), 8, "{key}");
            for field in ["calls", "totalNs", "maxNs"] {
                assert!(wake[key][field].is_u64(), "{key}.{field}");
            }
        }
    }

    /// The statics the callback, the helper and every hooked call use are on
    /// lines of their own.
    #[test]
    fn hot_statics_do_not_share_cache_lines() {
        let line = |address: usize| address / 64;
        let read_mostly = &raw const READ_MOSTLY as usize;
        let cycle = &raw const CYCLE as usize;
        let outside = &raw const BUFFER_CALLS_OUTSIDE as usize;
        let admission = &raw const CONTROL_ADMISSION as usize;
        let helper = &raw const pipeline::HELPER_TIME as usize;
        let positions = [
            &raw const probe::START_POS as usize,
            &raw const probe::END_POS as usize,
            &raw const probe::HELPER_POS as usize,
        ];
        let mut lines = vec![
            line(read_mostly),
            line(cycle),
            line(cycle + 64),
            line(outside),
            line(admission),
            line(helper),
        ];
        for start in positions {
            assert_eq!(start % 64, 0);
            assert_eq!(std::mem::size_of::<probe::Positions>(), 128);
            lines.extend([line(start), line(start + 64)]);
        }
        for address in [read_mostly, cycle, outside, admission, helper] {
            assert_eq!(address % 64, 0);
        }
        let distinct: std::collections::HashSet<_> = lines.iter().collect();
        assert_eq!(distinct.len(), lines.len());
    }
}

#[cfg(test)]
mod slot_tests {
    use super::*;

    const ALL: [Slot; 5] = [
        Slot::Free,
        Slot::Retained(Attach::Pending),
        Slot::Retained(Attach::Settled),
        Slot::Live(Attach::Pending),
        Slot::Live(Attach::Settled),
    ];

    #[test]
    fn every_slot_survives_its_atomic_encoding() {
        let lifecycle = Lifecycle::new();
        assert_eq!(lifecycle.load(Ordering::Acquire), Slot::Free);
        for slot in ALL {
            lifecycle.store(slot, Ordering::Release);
            assert_eq!(lifecycle.load(Ordering::Acquire), slot);
        }
        for value in 5..=u8::MAX {
            assert_eq!(Slot::from_u8(value), Slot::Free);
        }
    }

    #[test]
    fn installing_keeps_the_attach_state_and_never_claims_a_free_slot() {
        for attach in [Attach::Pending, Attach::Settled] {
            for slot in [Slot::Live(attach), Slot::Retained(attach)] {
                assert_eq!(slot.installed(true), Slot::Live(attach));
                assert_eq!(slot.installed(false), Slot::Retained(attach));
                assert!(slot.is_claimed());
            }
        }
        assert_eq!(Slot::Free.installed(true), Slot::Free);
        assert!(!Slot::Free.is_claimed() && !Slot::Free.is_live());
        assert!(!Slot::Retained(Attach::Settled).is_live());
    }
}
