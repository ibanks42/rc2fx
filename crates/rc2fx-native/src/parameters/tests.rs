use super::*;
use rc2fx_core::TargetKind;

fn target() -> Target {
    Target::new("headphones", "Headphones", TargetKind::Output, 2)
}
/// Arithmetic for disjoint worker totals. The live lookahead-switch test
/// covers retirement at the real call site, including archive feedback.
#[test]
fn timing_merge_sums_disjoint_workers() {
    let first = rc2fx_native_control::Timing {
        calls: 10,
        total_ns: 1_000,
        max_ns: 200,
        overruns: 2,
        clock_errors: 1,
        histogram: [1, 0, 0, 0, 0, 0, 0, 0],
    };
    let second = rc2fx_native_control::Timing {
        calls: 5,
        total_ns: 400,
        max_ns: 300,
        overruns: 1,
        clock_errors: 0,
        histogram: [0, 2, 0, 0, 0, 0, 0, 0],
    };
    let once = merge_timing(first, second);
    assert_eq!(
        (
            once.calls,
            once.total_ns,
            once.max_ns,
            once.overruns,
            once.clock_errors,
            once.histogram,
        ),
        (15, 1_400, 300, 3, 1, [1, 2, 0, 0, 0, 0, 0, 0])
    );
    // A worker that has not run contributes nothing.
    assert_eq!(merge_timing(once, ZERO_TIMING), once);
}

#[test]
fn paired_worker_histories_preserve_empty_and_absent_wire_fields() {
    let mut target = Target::new("source", "Source", TargetKind::Mic, 1);
    let (mut control, _reader) = channel(&target, Prepared::new(&target, 48_000).unwrap());
    assert_eq!(control.worker_histories(), None);
    let status = crate::target_status(&mut control).unwrap();
    assert_eq!((status.misses, status.lates), (None, None));

    target.suppression.enabled = true;
    let (mut control, _reader) = channel(&target, Prepared::new(&target, 48_000).unwrap());
    let empty = rc2fx_native_control::NativeHistory::default();
    assert_eq!(
        control.worker_histories(),
        Some((1, empty.clone(), empty.clone()))
    );
    let status = crate::target_status(&mut control).unwrap();
    assert_eq!(
        (status.misses, status.lates),
        (Some(empty.clone()), Some(empty))
    );
    assert_eq!(status.runtime_epoch, 1);
}

#[test]
fn paired_histories_preserve_both_rings_loss_unpublished_and_clock_failures() {
    use rc2fx_native_control::{NativeEventRecord, NativeHistory};
    use rc2fx_suppression::{EventHistory, EventRecord};
    let histories = (
        EventHistory {
            total: 8,
            capacity: 2,
            events: vec![
                EventRecord {
                    sequence: 1,
                    at_ns: 123,
                },
                EventRecord {
                    sequence: 2,
                    at_ns: 0,
                },
            ],
            lost: 5,
            unpublished: 1,
        },
        EventHistory {
            total: 4,
            capacity: 2,
            events: vec![
                EventRecord {
                    sequence: 7,
                    at_ns: 456,
                },
                EventRecord {
                    sequence: 11,
                    at_ns: 789,
                },
            ],
            lost: 0,
            unpublished: 2,
        },
    );
    assert_eq!(
        native_histories(histories),
        (
            NativeHistory {
                total: 8,
                lost: 5,
                events: vec![
                    NativeEventRecord {
                        sequence: 1,
                        at_ns: 123
                    },
                    NativeEventRecord {
                        sequence: 2,
                        at_ns: 0
                    },
                ],
            },
            NativeHistory {
                total: 4,
                lost: 0,
                events: vec![
                    NativeEventRecord {
                        sequence: 7,
                        at_ns: 456
                    },
                    NativeEventRecord {
                        sequence: 11,
                        at_ns: 789
                    },
                ],
            },
        )
    );
}

#[test]
fn deadline_histories_report_their_accounting_and_no_invented_times() {
    use rc2fx_suppression::{EventHistory, EventRecord, Outliers, Recorded};
    let events: Vec<_> = (0..64)
        .map(|sequence| EventRecord {
            sequence,
            at_ns: if sequence == 2 { 0 } else { 100 + sequence },
        })
        .collect();
    let outliers = Outliers {
        worst: Recorded::Empty,
        latest_slow: Recorded::Empty,
        slow_frames: 0,
        above: [3, 2, 1, 0],
        last_miss: Recorded::Stable(EventRecord {
            sequence: 69,
            at_ns: 500,
        }),
        last_late: Recorded::Empty,
        miss_history: EventHistory {
            total: 70,
            capacity: 64,
            events,
            lost: 6,
            unpublished: 0,
        },
        late_history: EventHistory {
            capacity: 64,
            ..Default::default()
        },
    };
    let json = outliers_json(outliers);
    let misses = &json["missHistory"];
    for key in [
        "capacity",
        "total",
        "recorded",
        "lost",
        "unpublished",
        "complete",
        "events",
    ] {
        assert!(misses.get(key).is_some(), "missing {key}");
    }
    assert_eq!(
        (misses["total"].as_u64(), misses["recorded"].as_u64()),
        (Some(70), Some(64))
    );
    assert_eq!(misses["lost"], 6);
    assert_eq!(misses["complete"], false);
    assert_eq!(
        misses["events"][0],
        serde_json::json!({"sequence": 0, "atNs": 100})
    );
    // A failed clock read is null, not epoch 0.
    assert_eq!(
        misses["events"][2],
        serde_json::json!({"sequence": 2, "atNs": null})
    );
    let lates = &json["lateHistory"];
    assert_eq!(
        (lates["total"].as_u64(), lates["recorded"].as_u64()),
        (Some(0), Some(0))
    );
    assert_eq!(lates["complete"], true);
    assert_eq!(lates["events"], serde_json::json!([]));
    // The latest records are unchanged alongside.
    assert_eq!(json["lastMiss"]["sequence"], 69);
    assert_eq!(json["lastLate"]["state"], "empty");
}
#[test]
fn compatible_updates_keep_an_enabled_classic_provider_signature() {
    let mut target = Target::new("source", "Source", TargetKind::Mic, 1);
    target.suppression.enabled = true;
    let (mut control, mut reader) = channel(&target, Prepared::new(&target, 48_000).unwrap());
    assert_eq!(control.status().unwrap().runtime_epoch, 1);
    target.eq.input_gain_db = -6.0;
    let staged = control.prepare(&target, 2).unwrap();
    control.reserve(staged).unwrap().commit();
    assert!(reader.begin());
    reader.finish(true);
    assert_eq!(control.status().unwrap().applied, 2);
    assert_eq!(control.status().unwrap().runtime_epoch, 1);
    target.suppression.enabled = false;
    let staged = control.prepare(&target, 3).unwrap();
    control.reserve(staged).unwrap().commit();
    assert_eq!(control.status().unwrap().runtime_epoch, 2);
    assert!(control.worker_status().is_none());
    assert!(reader.begin());
    reader.finish(true);
    control.status().unwrap();
    assert_eq!(control.retired_worker_count(), 1);
    let retired = control.retired_workers();
    assert_eq!(retired[0].runtime_epoch, 1);
    assert!(retired[0].joined && retired[0].drained);
}
#[test]
fn old_frame_remains_valid_and_updates_backpressure_until_new_frame_finishes() {
    let target = target();
    let (mut control, mut reader) = channel(&target, Prepared::new(&target, 48_000).unwrap());
    assert!(reader.begin());
    control.publish(&target, 2).unwrap();
    assert_eq!(reader.prepared().unwrap().channels(), 2);
    assert_eq!(control.publish(&target, 3), Err(Error::Busy));
    reader.finish(true);
    assert!(reader.prepared().is_none());
    assert_eq!(control.status().unwrap().applied, 1);
    assert_eq!(control.publish(&target, 3), Err(Error::Busy));
    assert!(reader.begin());
    reader.finish(true);
    control.publish(&target, 3).unwrap();
    assert_eq!(control.status().unwrap().published, 3);
    assert_eq!(control.status().unwrap().applied, 2);
}
#[test]
fn reader_retains_storage_after_control_is_dropped() {
    let target = target();
    let (control, mut reader) = channel(&target, Prepared::new(&target, 48_000).unwrap());
    assert!(reader.begin());
    drop(control);
    assert_eq!(reader.prepared().unwrap().channels(), 2);
    reader.finish(true);
    assert!(reader.begin());
    reader.finish(true);
}
#[test]
fn rejected_updates_leave_current_revision_unchanged() {
    let mut target = target();
    let (mut control, _reader) = channel(&target, Prepared::new(&target, 48_000).unwrap());
    assert_eq!(control.publish(&target, 1), Err(Error::StaleRevision));
    target.channels = 1;
    assert_eq!(control.publish(&target, 2), Err(Error::RequiresReset));
    target.id = "another-path".into();
    assert_eq!(control.publish(&target, 2), Err(Error::WrongTarget));
    assert_eq!(control.status().unwrap().published, 1);
}
#[test]
fn canceling_staged_settings_keeps_audio_live_and_bounds_candidate_storage() {
    let target = target();
    let (mut control, mut reader) = channel(&target, Prepared::new(&target, 48_000).unwrap());
    let staged = control.prepare(&target, 2).unwrap();
    assert_eq!(control.status().unwrap().published, 1);
    assert!(matches!(control.prepare(&target, 3), Err(Error::Busy)));
    assert!(reader.begin());
    reader.finish(true);
    assert_eq!(control.status().unwrap().applied, 1);
    drop(staged);
    let staged = control.prepare(&target, 3).unwrap();
    drop(staged);
    assert_eq!(control.status().unwrap().published, 1);
}
#[test]
fn commit_loan_does_not_block_reader_and_drop_aborts_publication() {
    let target = target();
    let (mut control, mut reader) = channel(&target, Prepared::new(&target, 48_000).unwrap());
    let staged = control.prepare(&target, 2).unwrap();
    let reservation = control.reserve(staged).unwrap();
    assert!(reader.begin());
    assert_eq!(reader.prepared().unwrap().channels(), 2);
    reader.finish(true);
    drop(reservation);
    assert_eq!(control.status().unwrap().published, 1);
    assert_eq!(control.status().unwrap().applied, 1);
    let staged = control.prepare(&target, 3).unwrap();
    control.reserve(staged).unwrap().commit();
    assert_eq!(control.status().unwrap().published, 3);
    assert!(reader.begin());
    reader.finish(true);
    assert_eq!(control.status().unwrap().applied, 3);
}
#[test]
fn replacement_endpoint_refuses_old_staging_and_releases_its_original_slot() {
    let target = target();
    let prepared = Prepared::new(&target, 48_000).unwrap();
    let (mut original, _original_reader) = channel(&target, prepared.clone());
    let (mut replacement, _replacement_reader) = channel(&target, prepared);
    let staged = original.prepare(&target, 2).unwrap();
    assert!(matches!(
        replacement.reserve(staged),
        Err(Error::WrongTarget)
    ));
    assert_eq!(replacement.status().unwrap().published, 1);
    assert!(original.prepare(&target, 3).is_ok());
}

#[test]
fn mode_descriptor_is_fixed_for_the_frame_and_retirement_waits_for_newer_release() {
    let target = target();
    let (mut control, mut reader) = channel(&target, Prepared::new(&target, 48_000).unwrap());
    let old = Arc::downgrade(
        control
            .shared
            .versions
            .lock()
            .unwrap()
            .current
            .get()
            .runtime
            .as_ref()
            .unwrap(),
    );
    assert!(reader.begin());
    let staged = control
        .prepare_mode(&target, 2, PathMode::Passthrough)
        .unwrap();
    control.reserve(staged).unwrap().commit();
    assert_eq!(reader.mode(), Some(PathMode::Processed));
    assert_eq!(control.status().unwrap().runtime_epoch, 0);
    assert!(old.upgrade().is_some());
    reader.finish(true);
    control.status().unwrap();
    assert!(
        old.upgrade().is_some(),
        "old frame release is not the newer descriptor fence"
    );
    assert!(reader.begin());
    assert_eq!(reader.mode(), Some(PathMode::Passthrough));
    assert_eq!(reader.latency_samples(), 0);
    reader.finish(true);
    control.status().unwrap();
    assert!(old.upgrade().is_none());
    let staged = control
        .prepare_mode(&target, 3, PathMode::Processed)
        .unwrap();
    control.reserve(staged).unwrap().commit();
    assert_eq!(control.status().unwrap().runtime_epoch, 2);
}

/// A callback stand-in: blocks of a constant `level` through `reader`
/// until `stop`, logging each block's revision and first channel. A
/// passthrough path's block is left as it came.
fn audio(
    mut reader: Reader,
    channels: usize,
    level: f32,
    stop: Arc<std::sync::atomic::AtomicBool>,
) -> std::thread::JoinHandle<Vec<(u64, Vec<f32>)>> {
    std::thread::spawn(move || {
        let mut log = Vec::new();
        while !stop.load(Ordering::Acquire) {
            let mut left = [level; 112];
            let mut right = [level; 112];
            assert!(reader.begin());
            let revision = reader.descriptor().unwrap().revision;
            if reader.mode() == Some(PathMode::Processed) {
                let right: &mut [f32] = if channels == 2 { &mut right } else { &mut [] };
                reader.output([&mut left, right]).unwrap();
            }
            reader.finish(true);
            log.push((revision, left.to_vec()));
            std::thread::sleep(Duration::from_micros(200));
        }
        log
    })
}
/// The largest step from one sample to the next across a log.
fn largest_step(log: &[(u64, Vec<f32>)]) -> f32 {
    let samples: Vec<f32> = log.iter().flat_map(|(_, block)| block.clone()).collect();
    samples
        .windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0, f32::max)
}
/// Prepares and publishes `target` as `revision`, retrying while the
/// previous publication is still being released.
fn publish_live(control: &mut Control, target: &Target, revision: u64, mode: PathMode) {
    let deadline = Instant::now() + Duration::from_secs(10);
    let staged = loop {
        match control.prepare_mode(target, revision, mode) {
            Ok(staged) => break staged,
            Err(Error::Busy) if Instant::now() < deadline => std::thread::yield_now(),
            Err(error) => panic!("prepare {revision}: {error:?}"),
        }
    };
    control.reserve(staged).unwrap().commit();
}
/// Waits until the reader has applied `revision` and played `blocks` of it.
fn play(control: &mut Control, revision: u64, millis: u64) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while control.status().unwrap().applied < revision {
        assert!(Instant::now() < deadline, "revision {revision} not applied");
        std::thread::yield_now();
    }
    std::thread::sleep(Duration::from_millis(millis));
}

/// Switching suppression replaces the runtime. The running one fades to
/// silence before the new one is published; the new one stays silent for
/// its delay and then fades in. No step anywhere.
#[test]
fn a_reset_fades_the_old_runtime_out_before_the_new_one_fades_in() {
    let mut target = Target::new("mic", "Mic", TargetKind::Mic, 1);
    target.gain.enabled = true;
    target.gain.db = -6.0;
    let (mut control, reader) = channel(&target, Prepared::new(&target, 48_000).unwrap());
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let thread = audio(reader, 1, 0.25, Arc::clone(&stop));
    play(&mut control, 1, 20);
    // Dry only, so the new runtime's output is its delayed input.
    target.suppression.enabled = true;
    target.suppression.wet = 0.0;
    publish_live(&mut control, &target, 2, PathMode::Processed);
    play(&mut control, 2, 200);
    stop.store(true, Ordering::Release);
    let log = thread.join().unwrap();
    let level = 0.25 * 10_f32.powf(-6.0 / 20.0);
    let last_old = log
        .iter()
        .rposition(|(revision, _)| *revision == 1)
        .unwrap();
    assert!(
        log[..last_old]
            .iter()
            .any(|(_, block)| (block[0] - level).abs() < 1e-6)
    );
    assert_eq!(
        log[last_old].1[111], 0.0,
        "the old runtime had not faded out"
    );
    assert_eq!(
        log[last_old + 1].1[0],
        0.0,
        "the new runtime did not start silent"
    );
    assert!((log.last().unwrap().1[111] - level).abs() < 1e-5);
    let step = largest_step(&log);
    assert!(step < 2e-3, "a step of {step}; a cut is {level}");
}

/// On a path processed in place (as a callback-owned output is), switching
/// processing off fades to the dry signal, which is what the unprocessed
/// path plays; switching it back on fades from it. No dip.
#[test]
fn an_aligned_mode_change_fades_through_the_dry_signal() {
    let mut target = target();
    target.gain.enabled = true;
    target.gain.db = -6.0;
    let (mut control, reader) = channel(&target, Prepared::new(&target, 48_000).unwrap());
    reader.pass_fade().set_aligned(true);
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let thread = audio(reader, 2, 0.25, Arc::clone(&stop));
    play(&mut control, 1, 20);
    publish_live(&mut control, &target, 2, PathMode::Passthrough);
    play(&mut control, 2, 20);
    publish_live(&mut control, &target, 3, PathMode::Processed);
    play(&mut control, 3, 20);
    stop.store(true, Ordering::Release);
    let log = thread.join().unwrap();
    let last_processed = log
        .iter()
        .rposition(|(revision, _)| *revision == 1)
        .unwrap();
    assert_eq!(
        log[last_processed].1[111], 0.25,
        "had not reached the dry signal"
    );
    let first_again = log.iter().position(|(revision, _)| *revision == 3).unwrap();
    assert!(
        log[first_again].1[0] > 0.249,
        "did not start from the dry signal"
    );
    let level = 0.25 * 10_f32.powf(-6.0 / 20.0);
    assert!((log.last().unwrap().1[111] - level).abs() < 1e-6);
    let step = largest_step(&log);
    assert!(step < 2e-3, "a step of {step}");
}

/// The hand-off's wait holds no lock: another caller of the control (a
/// status read, standing in for the IPC status and the lifecycle calls
/// that share the parameter lock) gets it at once throughout, as
/// `LiveEndpoints::commit` waits.
#[test]
fn the_hand_off_wait_holds_no_lock() {
    let mut target = Target::new("mic", "Mic", TargetKind::Mic, 1);
    target.gain.enabled = true;
    target.gain.db = -6.0;
    let (control, mut reader) = channel(&target, Prepared::new(&target, 48_000).unwrap());
    let control = Arc::new(Mutex::new(control));
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    // A slow callback: about 25 ms for the fade's five blocks.
    let audio_stop = Arc::clone(&stop);
    let thread = std::thread::spawn(move || {
        while !audio_stop.load(Ordering::Acquire) {
            let mut x = [0.25_f32; 112];
            assert!(reader.begin());
            reader.output([&mut x, &mut []]).unwrap();
            reader.finish(true);
            std::thread::sleep(Duration::from_millis(5));
        }
    });
    let mut suppressed = target.clone();
    suppressed.suppression.enabled = true;
    // Building the new runtime is prepare's work, before the commit.
    let staged = control.lock().unwrap().prepare(&suppressed, 2).unwrap();
    let committer = {
        let control = Arc::clone(&control);
        std::thread::spawn(move || {
            let handing = control.lock().unwrap().hand_off(&staged).unwrap();
            let started = Instant::now();
            assert!(handing.wait(), "the fade did not finish");
            let waited = started.elapsed();
            let mut control = control.lock().unwrap();
            control.reserve_after(staged, handing).unwrap().commit();
            waited
        })
    };
    let mut slowest = Duration::ZERO;
    while !committer.is_finished() {
        let started = Instant::now();
        let status = control.lock().unwrap().status().unwrap();
        slowest = slowest.max(started.elapsed());
        assert!(status.published <= 2);
        std::thread::sleep(Duration::from_micros(500));
    }
    let waited = committer.join().unwrap();
    stop.store(true, Ordering::Release);
    thread.join().unwrap();
    assert!(waited >= Duration::from_millis(10), "waited {waited:?}");
    assert!(
        slowest < waited / 2,
        "a status read waited {slowest:?} of {waited:?}"
    );
    assert_eq!(control.lock().unwrap().status().unwrap().published, 2);
}

/// The control (and its reader) can go away while a hand-off waits, as on
/// close: nothing deadlocks, and the old runtime is released when the
/// hand-off is, on the control side.
#[test]
fn closing_during_a_hand_off_releases_the_old_runtime() {
    let target = Target::new("mic", "Mic", TargetKind::Mic, 1);
    let (mut control, reader) = channel(&target, Prepared::new(&target, 48_000).unwrap());
    let old = Arc::downgrade(
        control
            .shared
            .versions
            .lock()
            .unwrap()
            .current
            .get()
            .runtime
            .as_ref()
            .unwrap(),
    );
    let mut suppressed = target.clone();
    suppressed.suppression.enabled = true;
    let staged = control.prepare(&suppressed, 2).unwrap();
    let handing = control.hand_off(&staged).unwrap();
    drop(staged);
    drop(control);
    drop(reader);
    assert!(old.upgrade().is_some(), "held by the hand-off");
    drop(handing);
    assert!(old.upgrade().is_none());
}

/// With no callbacks to finish the fade (the engine stopped), publishing
/// waits out the timeout and goes ahead.
#[test]
fn a_replacement_without_callbacks_is_published_after_the_timeout() {
    let target = target();
    let (mut control, _reader) = channel(&target, Prepared::new(&target, 48_000).unwrap());
    control.set_handoff_timeout(Duration::from_millis(30));
    let staged = control
        .prepare_mode(&target, 2, PathMode::Passthrough)
        .unwrap();
    let started = Instant::now();
    control.reserve(staged).unwrap().commit();
    assert!(started.elapsed() >= Duration::from_millis(30));
    assert_eq!(control.status().unwrap().published, 2);
}

/// A refused reset never touches the running output, and a reservation
/// dropped uncommitted fades it back in.
#[test]
fn refused_and_abandoned_replacements_leave_the_output_playing() {
    let mut target = Target::new("mic", "Mic", TargetKind::Mic, 1);
    target.gain.enabled = true;
    target.gain.db = -6.0;
    let (mut control, mut reader) = channel(&target, Prepared::new(&target, 48_000).unwrap());
    control.set_handoff_timeout(Duration::from_millis(5));
    let mut block = || {
        let mut x = [0.25_f32; 112];
        assert!(reader.begin());
        reader.output([&mut x, &mut []]).unwrap();
        reader.finish(true);
        x
    };
    let before = block();
    let mut suppressed = target.clone();
    suppressed.suppression.enabled = true;
    assert_eq!(
        control
            .prepare_with(&suppressed, 2, PathMode::Processed, ResetPolicy::Refuse)
            .err(),
        Some(Error::RequiresReset)
    );
    assert_eq!(block(), before);
    let staged = control
        .prepare_with(&suppressed, 2, PathMode::Processed, ResetPolicy::Allow)
        .unwrap();
    let reservation = control.reserve(staged).unwrap();
    let fading = block();
    assert!(fading[111] < before[111] && fading[0] > fading[111]);
    drop(reservation);
    let mut previous = fading[111];
    for _ in 0..8 {
        let next = block();
        assert!(next.iter().all(|v| *v >= previous - 1e-7));
        previous = next[111];
    }
    assert_eq!(block(), before);
    assert_eq!(control.status().unwrap().published, 1);
}

/// A refusing prepare (a live edit, or the daemon restoring saved settings
/// at connect) hands the running processing new parameters and never
/// replaces it; a save may.
#[test]
fn a_refusing_prepare_never_builds_a_new_runtime() {
    let target = Target::new("mic", "Mic", TargetKind::Mic, 1);
    let (mut control, mut reader) = channel(&target, Prepared::new(&target, 48_000).unwrap());
    // Suppression switched on needs a new runtime: refused, nothing staged.
    let mut suppressed = target.clone();
    suppressed.suppression.enabled = true;
    assert_eq!(
        control
            .prepare_with(&suppressed, 2, PathMode::Processed, ResetPolicy::Refuse)
            .err(),
        Some(Error::RequiresReset)
    );
    // A mode change is refused as well.
    assert_eq!(
        control
            .prepare_with(&target, 2, PathMode::Passthrough, ResetPolicy::Refuse)
            .err(),
        Some(Error::RequiresReset)
    );
    assert_eq!(control.status().unwrap().runtime_epoch, 1);
    assert_eq!(control.status().unwrap().published, 1);
    assert_eq!(control.retired_worker_count(), 0, "no worker was started");
    // Parameters only: the same runtime carries them.
    let mut louder = target.clone();
    louder.gain.db = -6.0;
    let staged = control
        .prepare_with(&louder, 2, PathMode::Processed, ResetPolicy::Refuse)
        .unwrap();
    control.reserve(staged).unwrap().commit();
    assert_eq!(control.status().unwrap().published, 2);
    assert_eq!(control.status().unwrap().runtime_epoch, 1);
    assert!(reader.begin());
    reader.finish(true);
    // A save of the same suppression change builds the new runtime.
    let staged = control
        .prepare_with(&suppressed, 3, PathMode::Processed, ResetPolicy::Allow)
        .unwrap();
    control.reserve(staged).unwrap().commit();
    assert_eq!(control.status().unwrap().runtime_epoch, 2);
    assert!(reader.begin());
    reader.finish(true);
    // And going back off from there is a reset again.
    assert_eq!(
        control
            .prepare_with(&louder, 4, PathMode::Processed, ResetPolicy::Refuse)
            .err(),
        Some(Error::RequiresReset)
    );
}

#[test]
fn canceled_provider_is_joined_and_does_not_replace_current_runtime() {
    let mut target = Target::new("mic", "Mic", TargetKind::Mic, 1);
    let (mut control, mut reader) = channel(&target, Prepared::new(&target, 48_000).unwrap());
    target.suppression.enabled = true;
    let staged = control
        .prepare_mode(&target, 2, PathMode::Processed)
        .unwrap();
    assert_eq!(control.status().unwrap().runtime_epoch, 1);
    assert!(reader.begin());
    reader.finish(true);
    drop(staged);
    assert_eq!(control.status().unwrap().runtime_epoch, 1);
    assert_eq!(control.retired_worker_count(), 1);
    let retired = control.retired_workers();
    assert_eq!(retired[0].runtime_epoch, 2);
    assert!(retired[0].joined && retired[0].drained);
    let staged = control
        .prepare_mode(&target, 3, PathMode::Processed)
        .unwrap();
    control.reserve(staged).unwrap().commit();
    assert_eq!(control.status().unwrap().runtime_epoch, 3);
}

#[test]
fn active_and_staged_descriptors_admit_only_one_mono_microphone_target() {
    let admission = Admission::default();
    let first = Target::new("first", "First", TargetKind::Mic, 1);
    let mut second = Target::new("second", "Second", TargetKind::Mic, 1);
    let (mut a, mut ar) = channel_with_options(
        &first,
        Prepared::new(&first, 48_000).unwrap(),
        rc2fx_suppression::WorkerOptions::default(),
        admission.clone(),
        PathMode::Processed,
    )
    .unwrap();
    let (mut b, _br) = channel_with_options(
        &second,
        Prepared::new(&second, 48_000).unwrap(),
        rc2fx_suppression::WorkerOptions::default(),
        admission,
        PathMode::Processed,
    )
    .unwrap();
    let mut enabled_first = first.clone();
    enabled_first.suppression.enabled = true;
    let staged = a
        .prepare_mode(&enabled_first, 2, PathMode::Processed)
        .unwrap();
    second.suppression.enabled = true;
    assert!(matches!(
        b.prepare_mode(&second, 2, PathMode::Processed),
        Err(Error::SuppressionInUse)
    ));
    drop(staged);
    let staged = b.prepare_mode(&second, 3, PathMode::Passthrough).unwrap();
    b.reserve(staged).unwrap().commit();
    assert!(
        matches!(
            a.prepare_mode(&enabled_first, 3, PathMode::Processed),
            Err(Error::SuppressionInUse)
        ),
        "a saved enable keeps its claim even while its path is unprocessed"
    );
    assert!(ar.begin());
    ar.finish(true);
}

#[test]
fn chat_owns_one_worker_and_excludes_an_enabled_microphone() {
    let admission = Admission::default();
    let mut chat = Target::new("send-chat", "Chat", TargetKind::Output, 2);
    chat.suppression.enabled = true;
    let (control, mut reader) = channel_with_options(
        &chat,
        Prepared::new(&chat, 48_000).unwrap(),
        rc2fx_suppression::WorkerOptions::default(),
        admission.clone(),
        PathMode::Processed,
    )
    .unwrap();
    assert!(control.worker_status().is_some());
    let mut mic = Target::new("mic-1", "Mic", rc2fx_core::TargetKind::Mic, 1);
    mic.suppression.enabled = true;
    assert!(matches!(
        channel_with_options(
            &mic,
            Prepared::new(&mic, 48_000).unwrap(),
            rc2fx_suppression::WorkerOptions::default(),
            admission,
            PathMode::Processed
        ),
        Err(Error::SuppressionInUse)
    ));
    assert!(reader.begin());
    let mut left = [0.25; 112];
    let mut right = [-0.25; 112];
    assert_eq!(
        reader.output([&mut left, &mut right]).unwrap_err(),
        rc2fx_dsp::DspError::MissingChatStem
    );
    assert_eq!(left, [0.25; 112]);
    assert_eq!(right, [-0.25; 112]);
    reader
        .chat_output([&[0.; 112], &[0.; 112]], [&mut left, &mut right])
        .unwrap();
    reader.finish(true);
}

#[test]
fn unresolved_cancel_retains_one_runtime_and_blocks_only_its_path() {
    let mut target = Target::new("mic", "Mic", TargetKind::Mic, 1);
    let (mut control, mut reader) = channel(&target, Prepared::new(&target, 48_000).unwrap());
    target.suppression.enabled = true;
    let staged = control
        .prepare_mode(&target, 2, PathMode::Processed)
        .unwrap();
    let runtime = staged
        .snapshot
        .as_ref()
        .unwrap()
        .get()
        .runtime
        .as_ref()
        .unwrap()
        .clone();
    runtime.block_retirement(true);
    drop(staged);
    let blocked = matches!(
        control.prepare_mode(&target, 3, PathMode::Processed),
        Err(Error::Busy)
    );
    let retained = control.shared.versions.lock().unwrap().retired.is_some();
    let archived = control.retired_worker_count();
    assert!(reader.begin());
    reader.finish(true);
    // Release the test fence before any assertion can unwind into teardown.
    runtime.block_retirement(false);
    control.status().unwrap();
    assert!(blocked && retained);
    assert_eq!(archived, 0, "unresolved work is not a completed retirement");
    assert_eq!(control.retired_worker_count(), 1);
    assert!(control.shared.versions.lock().unwrap().retired.is_none());
    assert_eq!(control.status().unwrap().runtime_epoch, 1);
    assert!(
        control
            .prepare_mode(&target, 4, PathMode::Processed)
            .is_ok()
    );
}

#[test]
fn repeated_eq_enable_changes_on_512k_callback_thread_keep_runtime_alive() {
    use std::{
        sync::atomic::AtomicBool,
        time::{Duration, Instant},
    };
    let mut target = target();
    target.limiter.enabled = false;
    target.eq.enabled = true;
    target.eq.bands = (0..20)
        .map(|index| {
            let mut band = rc2fx_core::Band::new(format!("toggle-{index}"));
            band.frequency = 40.0 * 1.3_f64.powi(index);
            band.gain = if index % 2 == 0 { 3.0 } else { -3.0 };
            band
        })
        .collect();
    let (mut control, mut reader) = channel(&target, Prepared::new(&target, 48_000).unwrap());
    let initial = Arc::downgrade(
        control
            .shared
            .versions
            .lock()
            .unwrap()
            .current
            .get()
            .runtime
            .as_ref()
            .unwrap(),
    );
    let stop = Arc::new(AtomicBool::new(false));
    let frames = Arc::new(AtomicU64::new(0));
    let audio_stop = Arc::clone(&stop);
    let audio_frames = Arc::clone(&frames);
    let stack = 524_288;
    // Runtime and prepared storage are constructed on control first. Only
    // callback work runs on the explicitly bounded audio thread stack.
    let audio = std::thread::Builder::new()
        .name("eq-toggle-audio".into())
        .stack_size(stack)
        .spawn(move || {
            let mut left = [0.01_f32; 112];
            let mut right = [-0.02_f32; 112];
            while !audio_stop.load(Ordering::Acquire) {
                left.fill(0.01);
                right.fill(-0.02);
                assert!(reader.begin());
                let meter = reader.output([&mut left, &mut right]).unwrap();
                assert!(left.iter().chain(&right).all(|sample| sample.is_finite()));
                reader.record(112, meter, reader.latency_samples() as u32);
                reader.finish(true);
                audio_frames.fetch_add(1, Ordering::Release);
            }
            reader
        })
        .unwrap();
    let result = (|| -> Result<(), String> {
        for revision in 2..=257 {
            target.eq.enabled = revision % 2 != 0;
            let deadline = Instant::now() + Duration::from_secs(10);
            let staged = loop {
                match control.prepare_mode(&target, revision, PathMode::Processed) {
                    Ok(staged) => break staged,
                    Err(Error::Busy) if Instant::now() < deadline => std::thread::yield_now(),
                    Err(error) => {
                        return Err(format!("prepare revision {revision}: {error:?}"));
                    }
                }
            };
            control
                .reserve(staged)
                .map_err(|error| format!("{error:?}"))?
                .commit();
            let mut applied_at = None;
            loop {
                let status = control.status().map_err(|error| format!("{error:?}"))?;
                if status.runtime_epoch != 1 || initial.upgrade().is_none() {
                    return Err("compatible runtime was replaced or freed".into());
                }
                let at = frames.load(Ordering::Acquire);
                if status.applied == revision {
                    applied_at.get_or_insert(at);
                }
                if applied_at.is_some_and(|start| at >= start + 8) {
                    break;
                }
                if Instant::now() >= deadline {
                    return Err(format!("audio revision {revision} did not advance"));
                }
                std::thread::yield_now();
            }
        }
        Ok(())
    })();
    stop.store(true, Ordering::Release);
    let reader = audio.join().expect("EQ callback thread failed");
    drop(reader);
    result.unwrap();
    assert_eq!(control.status().unwrap().runtime_epoch, 1);
    assert_eq!(control.retired_worker_count(), 0);
    assert!(frames.load(Ordering::Acquire) >= 256 * 8);
}
