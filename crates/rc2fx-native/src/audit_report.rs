//! Periodic and shutdown `audit.json` report construction and atomic publication.
use super::*;

pub(crate) fn write_report() {
    let held = publishing();
    if REPORT_SELECTED_ONLY.load(Ordering::Relaxed) && !SELECTOR_SEEN.load(Ordering::Relaxed) {
        return;
    }
    let Some(path) = REPORT.get() else {
        return;
    };
    let report = build_report();
    let _ = publish_report(&held, path, &report, |partial| {
        std::fs::File::create(partial)
    });
}
/// The audit report. Report thread and finalization only.
pub(crate) fn build_report() -> serde_json::Value {
    let read = |counter: &AtomicU64| counter.load(Ordering::Relaxed);
    // Bench-only measurements are null without the gauges, never a zero that
    // could be read as a measured value.
    let gauged = gauges::enabled();
    let retained = CONTEXTS
        .iter()
        .filter(|context| context.slot.load(Ordering::Acquire).is_claimed())
        .count();
    let mut report = serde_json::json!({"registrations":read(&REGISTRATIONS),"callbacks":read(&CYCLE.callbacks),"buffersInside":read(&CYCLE.buffer_calls_inside),
        "buffersOutside":gauged.then(|| read(&BUFFER_CALLS_OUTSIDE)),"directBindings":read(&DIRECT_BINDINGS),"dlsymBindings":read(&DLSYM_BINDINGS),
        "bindingsDuringCallback":read(&CALLBACK_BINDINGS),"conflicts":read(&CONFLICTS),"capacityFailures":read(&CAPACITY_FAILURES),
        "teardownForwards":read(&TEARDOWN_FORWARDS),
        "reclaimedContexts":read(&RECLAIMED),"retainedContexts":retained,"callbackAllocations":read(&CALLBACK_ALLOCATIONS),
        "callbackFrees":read(&CALLBACK_FREES),"callbackReallocations":read(&CALLBACK_REALLOCATIONS),
        "planErrors":read(&PLAN_ERRORS),"pipelinesCreated":read(&PIPELINES_CREATED),"pipelinesRetired":read(&PIPELINES_RETIRED),
        "retainedPorts":CONTROL.lock().map(|registry|registry.ports.len()).unwrap_or(usize::MAX)});
    report["vendorJobRepair"] = vendor_job::status();
    report["jackBufferCache"] = jack_cache::status();
    report["vendorWakeOne"] = vendor_wake::status();
    report["firmwareProfile"] = FIRMWARE_PROFILE.get().cloned().into();
    report["controlServerErrors"] = read(&SERVER_ERRORS).into();
    report["controlRequired"] = CONTROL_ADMISSION.required().into();
    report["processPid"] = std::process::id().into();
    report["clientTiming"] = client_timing::snapshot();
    report["srcFast"] = src_fast::snapshot();
    report["denoiserHold"] = denoiser_hold::report();
    if gauged {
        report["gauges"] = gauges::snapshot();
    }
    report["mainCallbackTail"] = MAIN_TAIL.snapshot();
    report["retiredCallbackTiming"] =
        serde_json::to_value(RETIRED_TIMING.snapshot()).unwrap_or(serde_json::Value::Null);
    report["callbackTiming"] = CONTEXTS
        .iter()
        .filter(|context| context.slot.load(Ordering::Acquire).is_live())
        .map(|context| serde_json::to_value(context.timing.snapshot()).unwrap_or_default())
        .collect();
    report["helper"] = serde_json::json!({
        "frames": read(&pipeline::HELPER_TIME.frames),
        "totalNs": read(&pipeline::HELPER_TIME.total_ns),
        "maxNs": read(&pipeline::HELPER_TIME.max_ns),
        "stalls": read(&pipeline::HELPER_STALLS),
        "late": read(&pipeline::HELPER_LATE),
        "minMarginNs": read(&pipeline::HELPER_MIN_MARGIN_NS),
    });
    report["sources"] = pipeline::sources();
    // Live processing counters, read without touching a pipeline.
    report["processingCounters"] = pipeline::processing_counters();
    report["pathCostNs"] = gauged
        .then(|| pipeline::PATH_COST_NS.iter().map(read).collect::<Vec<_>>())
        .into();
    report["resolveNs"] = gauged.then(|| read(&pipeline::RESOLVE_NS)).into();
    report["callbackGaps"] = CYCLE.gaps.iter().map(read).collect();
    report["callbackGapsOutsideBand"] = serde_json::json!({
        "short": read(&CYCLE.gaps_short),
        "long": read(&CYCLE.gaps_long),
        "band": "0.9-1.1 periods",
        "ring": "gap events >= 1.5 periods",
    });
    report["maxCallbackGapNs"] = read(&CYCLE.max_gap).into();
    report["monotonicNs"] = timing::monotonic().into();
    let mut preparation_workers = Vec::new();
    let mut unavailable = 0usize;
    for (context_slot, context) in CONTEXTS.iter().enumerate() {
        if !context.slot.load(Ordering::Acquire).is_live() {
            continue;
        }
        let Ok(controls) = context.parameters.try_lock() else {
            unavailable += 1;
            continue;
        };
        for (target_slot, control) in controls.iter().enumerate() {
            match control.worker_telemetry() {
                Ok(Some((runtime, telemetry))) => {
                    preparation_workers.push(serde_json::json!({
                        "id": control.id(), "contextSlot": context_slot,
                        "targetSlot": target_slot, "runtime": runtime,
                        "preparation": suppression::preparation_report(telemetry.preparation()),
                    }));
                }
                Ok(None) => {}
                Err(_) => unavailable += 1,
            }
        }
    }
    report["workerPreparation"] = preparation_workers.into();
    // Failed context/path reads are explicit: an empty array with failed
    // reads is not an idle worker.
    report["workerPreparationUnavailableReads"] = unavailable.into();
    report["workers"] = CONTEXTS
        .iter()
        .filter(|context| context.slot.load(Ordering::Acquire).is_live())
        .filter_map(|context| context.parameters.try_lock().ok())
        .flat_map(|controls| {
            controls
                .iter()
                .filter_map(|control| {
                    control
                        .worker_status()
                        .map(|worker| serde_json::json!({"id": control.id(), "worker": worker}))
                })
                .collect::<Vec<_>>()
        })
        .collect();
    report["workerOutliers"] = CONTEXTS
        .iter()
        .filter(|context| context.slot.load(Ordering::Acquire).is_live())
        .filter_map(|context| context.parameters.try_lock().ok())
        .flat_map(|controls| {
            controls
                .iter()
                .filter_map(|control| {
                    control.worker_outliers().map(
                        |outliers| serde_json::json!({"id": control.id(), "outliers": outliers}),
                    )
                })
                .collect::<Vec<_>>()
        })
        .collect();
    report["controlSkippedCallbacks"] = CONTROL_ADMISSION.skipped().into();
    if let serde_json::Value::Object(probe) = probe::snapshot() {
        for (key, value) in probe {
            report[key] = value;
        }
    }
    report["chatVoice"] = chat_mix::status();
    report["placement"] = pipeline::placement();
    report["suppression"] =
        serde_json::to_value(suppression::stats()).unwrap_or(serde_json::Value::Null);
    if let Some(server) = SERVER.get()
        && let Ok(server) = server.lock()
        && let Some(server) = server.as_ref()
        && let Ok(status) = serde_json::to_value(server.status())
    {
        report["controlServer"] = status;
    }
    report
}
/// Report publication, by the refresh thread and at finalization (never a
/// real-time thread). Held from before a report is built until it is
/// published, so each published report was built after the previous one was
/// published, and the partial file is always the holder's own.
static PUBLISHING: Mutex<()> = Mutex::new(());
/// Proof that PUBLISHING is held.
struct Publishing {
    _held: MutexGuard<'static, ()>,
}
fn publishing() -> Publishing {
    Publishing {
        _held: PUBLISHING
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    }
}
/// Room for a whole report, so it reaches the file in one write instead of
/// one per JSON token.
const REPORT_BUFFER: usize = 64 * 1024;
/// The report as one line of JSON, buffered; a failed flush is returned, not
/// dropped.
fn write_json<W: Write>(writer: W, report: &serde_json::Value) -> io::Result<W> {
    let mut buffered = io::BufWriter::with_capacity(REPORT_BUFFER, writer);
    serde_json::to_writer(&mut buffered, report)?;
    writeln!(buffered)?;
    buffered
        .into_inner()
        .map_err(io::IntoInnerError::into_error)
}
/// Replaces the report whole, so a reader never sees a partial one: written
/// and flushed to a partial file, renamed over the previous report only if
/// all of that succeeded. On failure the previous report stays.
fn publish_report<W: Write>(
    _held: &Publishing,
    path: &Path,
    report: &serde_json::Value,
    open: impl FnOnce(&Path) -> io::Result<W>,
) -> io::Result<()> {
    let partial = path.with_extension("partial");
    let result = open(&partial)
        .and_then(|file| write_json(file, report))
        .and_then(|_| std::fs::rename(&partial, path));
    if result.is_err() {
        let _ = std::fs::remove_file(&partial);
    }
    result
}
/// Seconds after start in which the report stays at one per second: the
/// install commit check and the GUI's readiness wait watch `callbacks` advance.
const REPORT_STARTUP_SECONDS: u64 = 180;
/// Afterwards, without a diagnostic marker. Must stay well inside the bench's
/// five-second telemetry limit, which a run without a marker also applies.
const REPORT_EVERY_SECONDS: u64 = 3;
fn report_due(elapsed_seconds: u64, diagnostic: bool) -> bool {
    diagnostic
        || elapsed_seconds < REPORT_STARTUP_SECONDS
        || elapsed_seconds.is_multiple_of(REPORT_EVERY_SECONDS)
}

/// Refresh the report while the mixer runs, so it can be read without stopping
/// audio. Events still reach their log every second.
pub(crate) fn start_report_refresh() {
    static STARTED: AtomicBool = AtomicBool::new(false);
    if REPORT.get().is_none() || STARTED.swap(true, Ordering::Relaxed) {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("rc2fx-report".into())
        .spawn(|| {
            let diagnostic = client_timing::enabled() || gauges::enabled();
            let mut elapsed_seconds = 0u64;
            loop {
                std::thread::sleep(std::time::Duration::from_secs(1));
                elapsed_seconds += 1;
                if report_due(elapsed_seconds, diagnostic) {
                    write_report();
                }
                probe::flush_log();
            }
        });
}

#[cfg(test)]
mod report_tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn the_report_slows_only_after_startup_and_without_a_marker() {
        let writes = |range: std::ops::Range<u64>, diagnostic| {
            range.filter(|tick| report_due(*tick, diagnostic)).count()
        };
        assert_eq!(writes(1..REPORT_STARTUP_SECONDS, false), 179);
        assert_eq!(writes(180..300, true), 120);
        assert_eq!(writes(180..300, false), 40);
        // No gap comes near the bench's five-second telemetry limit.
        let due: Vec<_> = (170..300).filter(|tick| report_due(*tick, false)).collect();
        assert!(due.windows(2).all(|pair| pair[1] - pair[0] <= 3));
    }

    /// Counts the writes and bytes that reach it; with `fail_after`, writes
    /// at most that many bytes in total (a short write at the limit) and then
    /// fails.
    struct Counting<W> {
        inner: W,
        writes: usize,
        bytes: usize,
        fail_after: Option<usize>,
    }
    impl<W: Write> Counting<W> {
        fn new(inner: W, fail_after: Option<usize>) -> Self {
            Self {
                inner,
                writes: 0,
                bytes: 0,
                fail_after,
            }
        }
    }
    impl<W: Write> Write for Counting<W> {
        fn write(&mut self, data: &[u8]) -> io::Result<usize> {
            let data = match self.fail_after {
                Some(limit) if self.bytes >= limit => {
                    return Err(io::Error::other("injected write failure"));
                }
                Some(limit) => &data[..data.len().min(limit - self.bytes)],
                None => data,
            };
            let written = self.inner.write(data)?;
            self.writes += 1;
            self.bytes += written;
            Ok(written)
        }
        fn flush(&mut self) -> io::Result<()> {
            self.inner.flush()
        }
    }

    /// A report shaped like the mixer's (about 4.3 KB of nested objects,
    /// arrays, strings and numbers; the mixer's is larger), told apart by
    /// `writer` and `sequence`.
    fn report(writer: u64, sequence: u64) -> serde_json::Value {
        let events: Vec<_> = (0..64)
            .map(|index| {
                serde_json::json!({"kind": "helperBusy", "sequence": index, "atNs": 1_000_000 + index, "valueNs": 2 * index})
            })
            .collect();
        serde_json::json!({
            "writer": writer, "sequence": sequence, "probeOnly": true, "callbacks": 123_456_789,
            "workers": [{"id": "mic-1", "worker": {"completedFrames": 99, "cpu": {"histogram": [0, 0, 16_684, 594, 0]}}}],
            "events": events,
            "placement": {"samples": 3, "deferred": 0, "history": [{"cycle": 64, "callbackCpus": 1}]},
        })
    }
    /// What the unbuffered writer produced, byte for byte.
    fn old_bytes(report: &serde_json::Value) -> Vec<u8> {
        let mut bytes = serde_json::to_vec(report).unwrap();
        bytes.push(b'\n');
        bytes
    }
    fn scratch(name: &str) -> PathBuf {
        let directory =
            std::env::temp_dir().join(format!("rc2fx-report-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        directory
    }

    #[test]
    fn the_report_is_byte_identical_in_a_few_writes() {
        let report = report(0, 0);
        let mut unbuffered = Counting::new(Vec::new(), None);
        serde_json::to_writer(&mut unbuffered, &report).unwrap();
        writeln!(unbuffered).unwrap();
        let buffered = write_json(Counting::new(Vec::new(), None), &report).unwrap();
        assert_eq!(buffered.inner, old_bytes(&report));
        assert!(
            unbuffered.writes > 1000,
            "{} unbuffered writes",
            unbuffered.writes
        );
        assert!(buffered.writes <= 2, "{} buffered writes", buffered.writes);
    }

    /// Gaps outside 0.9 to 1.1 periods are counted in both directions; the
    /// event ring keeps only 1.5 periods and more, with the gap as its size.
    #[test]
    fn gaps_outside_the_period_band_are_counted_and_only_long_ones_keep_an_event() {
        let _events = probe::EVENT_TESTS
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let period = timing::BUDGET_NS;
        let counts = |read: &AtomicU64| read.load(Ordering::Relaxed);
        let events =
            |report: &serde_json::Value| report["events"]["total"].as_u64().unwrap_or_default();
        let total = || events(&probe::snapshot());
        let before = (
            counts(&CYCLE.gaps_short),
            counts(&CYCLE.gaps_long),
            counts(&CYCLE.gaps[2]),
            total(),
        );
        // Inside the band: neither direction, and no event (the size bins
        // count every gap, so this one lands in the 0.9-1.1 bin).
        record_gap(period * 19 / 20);
        assert_eq!(
            (counts(&CYCLE.gaps_short), counts(&CYCLE.gaps_long), total()),
            (before.0, before.1, before.3)
        );
        assert_eq!(counts(&CYCLE.gaps[2]) - before.2, 1);
        // Early (0.5 period) and late (1.3 periods): counted, no event.
        record_gap(period / 2);
        record_gap(period * 13 / 10);
        assert_eq!(
            (
                counts(&CYCLE.gaps_short) - before.0,
                counts(&CYCLE.gaps_long) - before.1,
                total() - before.3
            ),
            (1, 1, 0)
        );
        // Two periods: counted and kept, with its size.
        record_gap(period * 2);
        assert_eq!(counts(&CYCLE.gaps_long) - before.1, 2);
        assert_eq!(total() - before.3, 1);
        let report = probe::snapshot();
        let recent = report["events"]["recent"].as_array().unwrap();
        let last = recent.last().unwrap();
        assert_eq!(last["kind"], "gap");
        assert_eq!(last["valueNs"], period * 2);
    }

    /// Publishes through the real seam with a writer that fails after
    /// `fail_after` bytes; the previous report must survive.
    fn publish_failing(name: &str, fail_after: usize) {
        let directory = scratch(name);
        let path = directory.join("audit.json");
        let previous = report(1, 1);
        publish_report(&publishing(), &path, &previous, |partial| {
            std::fs::File::create(partial)
        })
        .unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), old_bytes(&previous));
        // A second handle on the partial file shows what reached it, even
        // after it is removed.
        let mut partial_file = None;
        let failed = publish_report(&publishing(), &path, &report(1, 2), |partial| {
            let file = std::fs::File::create(partial)?;
            partial_file = Some(file.try_clone()?);
            Ok(Counting::new(file, Some(fail_after)))
        });
        assert!(failed.is_err());
        let reached = partial_file.unwrap().metadata().unwrap().len();
        assert_eq!(reached, fail_after as u64, "bytes in the failed partial");
        assert!(old_bytes(&report(1, 2)).len() > fail_after);
        assert_eq!(std::fs::read(&path).unwrap(), old_bytes(&previous));
        assert!(
            !path.with_extension("partial").exists(),
            "partial left behind"
        );
        let next = report(1, 3);
        publish_report(&publishing(), &path, &next, |partial| {
            std::fs::File::create(partial)
        })
        .unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), old_bytes(&next));
        assert!(!path.with_extension("partial").exists());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn a_failure_while_flushing_keeps_the_previous_report() {
        publish_failing("flush", 0);
    }

    #[test]
    fn a_failure_part_way_keeps_the_previous_report() {
        publish_failing("partway", 4096);
    }

    /// As write_report: hold the publishing lock from before the report is
    /// built until it is published.
    fn write_like_write_report(path: &Path, writer: u64, sequence: u64) {
        let held = publishing();
        let report = report(writer, sequence);
        publish_report(&held, path, &report, |partial| {
            std::fs::File::create(partial)
        })
        .unwrap();
    }

    #[test]
    fn concurrent_publishers_only_ever_publish_whole_reports_in_order() {
        let directory = scratch("concurrent");
        let path = directory.join("audit.json");
        write_like_write_report(&path, 0, 0);
        let done = AtomicBool::new(false);
        std::thread::scope(|scope| {
            let reader = scope.spawn(|| {
                let (mut reads, mut published) = (0, 0);
                let mut latest = [0; 3];
                while !done.load(Ordering::Acquire) {
                    let bytes = std::fs::read(&path).unwrap();
                    let report: serde_json::Value = serde_json::from_slice(&bytes)
                        .unwrap_or_else(|error| panic!("torn report after {reads} reads: {error}"));
                    let expected = super::report_tests::report(
                        report["writer"].as_u64().unwrap(),
                        report["sequence"].as_u64().unwrap(),
                    );
                    assert_eq!(bytes, old_bytes(&expected));
                    // Never an older report of a writer after a newer one.
                    let (writer, sequence) = (
                        report["writer"].as_u64().unwrap() as usize,
                        report["sequence"].as_u64().unwrap(),
                    );
                    assert!(sequence >= latest[writer], "writer {writer} went back");
                    latest[writer] = sequence;
                    reads += 1;
                    published += usize::from(writer != 0);
                }
                (reads, published)
            });
            let publishers: Vec<_> = (1..=2)
                .map(|writer| {
                    let path = &path;
                    scope.spawn(move || {
                        for sequence in 1..=150 {
                            write_like_write_report(path, writer, sequence);
                        }
                    })
                })
                .collect();
            for publisher in publishers {
                publisher.join().unwrap();
            }
            done.store(true, Ordering::Release);
            let (reads, published) = reader.join().unwrap();
            assert!(
                reads > 0 && published > 0,
                "{reads} reads, {published} published"
            );
        });
        // The last report published is whole: one writer's final sequence.
        let last: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(last["sequence"], 150);
        assert!(matches!(last["writer"].as_u64(), Some(1 | 2)));
        assert!(
            !path.with_extension("partial").exists(),
            "partial left behind"
        );
        std::fs::remove_dir_all(directory).unwrap();
    }
}
