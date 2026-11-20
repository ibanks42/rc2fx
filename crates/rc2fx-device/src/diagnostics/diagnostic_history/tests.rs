use super::*;
use rc2fx_core::{Config, EngineMode, EnginePhase, RoutingPhase, SelectionPhase};
use rc2fx_core::{MeterType, Timing};
use serde_json::Value;

fn native_payload(callbacks: u64) -> (Meter, Vec<rc2fx_native_control::ContextStatus>) {
    use rc2fx_native_control::{
        AudioMeter, CallbackTiming, ContextStatus, PathMode, TargetStatus, WorkerMeter,
    };
    let mut value = meter(9, 12, callbacks);
    value.targets[0].performance = None;
    let template = value.targets[0].clone();
    value.targets = [
        "mic-1",
        "usb-main",
        "usb-secondary",
        "headphones-2",
        "send-chat",
    ]
    .into_iter()
    .map(|id| {
        let mut target = template.clone();
        target.id = id.into();
        target
    })
    .collect();
    let targets = value
        .targets
        .iter()
        .map(|target| TargetStatus {
            id: target.id.clone(),
            mode: PathMode::Processed,
            runtime_epoch: 1,
            retired_workers: vec![],
            retired_worker_count: 0,
            published: 12,
            seen: 12,
            applied: 12,
            fingerprint: [7; 32],
            meter: Some(AudioMeter {
                revision: 12,
                callbacks,
                processed_frames: callbacks * 112,
                input_peak: target.input_peak,
                output_peak: target.output_peak,
                rms: target.rms,
                gain_reduction: 0.0,
                voice: 0.0,
                gate_open: 0.0,
                clipped: 0,
                latency_samples: 1120,
                dropped_windows: 0,
            }),
            worker: (target.id == "mic-1").then_some(WorkerMeter {
                completed_frames: callbacks / 4,
                ..Default::default()
            }),
            lates: None,
            misses: None,
            worker_recent_max_ns: None,
        })
        .collect();
    (
        value,
        vec![ContextStatus {
            context: 42,
            targets,
            counters: Default::default(),
            events: Vec::new(),
            timing: Some(CallbackTiming {
                attempts: callbacks,
                cpu_sample_every: 16,
                ..Default::default()
            }),
            recent_callback_wall_ns: None,
        }],
    )
}

#[test]
fn native_shared_xruns_do_not_flood_per_target_fault_history() {
    let mut history = History::new(Instant::now());
    history.state(&state(9, 12));
    for xruns in 0..=2 {
        let (mut value, mut native) = native_payload(100 + xruns);
        native[0].counters.xruns = xruns;
        for target in &mut value.targets {
            let mut performance = meter(9, 12, 100).targets[0].performance.clone().unwrap();
            performance.xruns = xruns;
            performance.callback = Timing::default();
            performance.worker_wall = Timing::default();
            performance.worker_cpu = Timing::default();
            target.performance = Some(performance);
        }
        // Target-specific faults still need their own event.
        value.targets[0].missed_deadlines = u64::from(xruns == 2);
        history.native_meter(value, native);
    }
    let exported = report(&history);
    let events = exported["events"].as_array().unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|e| e["kind"] == "native-engine-fault")
            .count(),
        2
    );
    let targets: Vec<_> = events
        .iter()
        .filter(|e| e["kind"] == "audio-fault")
        .collect();
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0]["counters"]["id"], "mic-1");
}

#[test]
fn native_fault_baselines_follow_resets_and_generation_not_settings_revision() {
    let start = Instant::now();
    let mut history = History::new(start);
    for (index, generation, revision, xruns) in [
        (1, 9, 12, 7),
        (2, 9, 13, 7),
        (3, 9, 13, 0),
        (4, 9, 13, 1),
        (5, 10, 13, 1),
    ] {
        history.state(&state(generation, revision));
        let (mut meter, mut native) = native_payload(index * 100);
        meter.generation = generation;
        meter.revision = revision;
        native[0].counters.xruns = xruns;
        history.native_meter(meter, native);
    }
    let exported = report(&history);
    let faults: Vec<_> = exported["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|event| event["kind"] == "native-engine-fault")
        .collect();
    assert_eq!(faults.len(), 3);
    assert!(faults[0]["previousCounters"].is_null());
    assert_eq!(faults[1]["previousCounters"]["xruns"], 0);
    assert_eq!(faults[1]["counters"]["xruns"], 1);
    assert!(faults[2]["previousCounters"].is_null());
    assert_eq!(faults[2]["generation"], 10);
}

#[test]
fn native_fault_timing_survives_sample_eviction_without_duplicate_snapshots() {
    let start = Instant::now();
    let mut history = History::new(start);
    history.state(&state(9, 12));
    for second in 1..=310 {
        let (meter, mut native) = native_payload(second * 100);
        native[0].counters.xruns = u64::from(second >= 2);
        history.native_meter(meter, native);
        sample(&mut history, start + Duration::from_secs(second));
    }
    let exported = report(&history);
    let faults: Vec<_> = exported["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|event| event["kind"] == "native-engine-fault")
        .collect();
    assert_eq!(faults.len(), 1);
    assert_eq!(faults[0]["counters"]["xruns"], 1);
    assert_eq!(faults[0]["previousCounters"]["xruns"], 0);
    assert_eq!(faults[0]["timing"]["attempts"], 200);
    assert_eq!(faults[0]["workers"][0]["id"], "mic-1");
    assert!(history.retention.evicted_samples > 0);
    assert!(history.event_bytes <= MAX_EVENT_HISTORY_BYTES);
}

#[test]
fn full_native_catalog_retains_timing_with_bounded_history() {
    let start = Instant::now();
    let mut history = History::new(start);
    history.state(&state(9, 12));
    for second in 1..=10 {
        let (mut meter, mut native) = native_payload(second * 100);
        let mut meter_template = meter.targets[0].clone();
        // Native meters also carry the shared callback counters on each path.
        // Include those repeated fields, as in the failing device export.
        meter_template.performance = self::meter(9, 12, second * 100).targets[0]
            .performance
            .clone();
        let target_template = native[0].targets[0].clone();
        meter.targets = (0..29)
            .map(|index| {
                let mut target = meter_template.clone();
                target.id = format!("path-{index}");
                target
            })
            .collect();
        native[0].targets = meter
            .targets
            .iter()
            .map(|meter| {
                let mut target = target_template.clone();
                target.id = meter.id.clone();
                target
            })
            .collect();
        history.native_meter(meter, native);
        sample(&mut history, start + Duration::from_secs(second));
    }
    let exported = report(&history);
    let samples = exported["samples"].as_array().unwrap();
    assert!(!samples.is_empty());
    for sample in samples {
        assert!(sample["telemetry"]["nativeDiagnostics"][0]["timing"].is_object());
        assert_eq!(
            sample["telemetry"]["nativeDiagnostics"][0]["targets"]
                .as_array()
                .unwrap()
                .len(),
            29
        );
    }
    assert_eq!(history.retention.rejected_records, 0);
    assert!(history.bytes <= MAX_HISTORY_BYTES);
    assert!(serde_json::to_vec(&exported).unwrap().len() <= MAX_RESPONSE_BYTES);
}

/// The largest state the controller admits (a full catalog, five selected
/// paths and maximum-length messages) with saturated timings on every path
/// still exports as a fresh sample that a strict client can parse.
#[test]
fn maximum_admitted_paths_and_full_catalog_fit_the_export_and_strict_client_parser() {
    let start = Instant::now();
    let mut history = History::new(start);
    let mut current = state(9, 12);
    current.config.targets = (0..24)
        .map(|index| {
            rc2fx_core::Target::new(
                format!("path-{index}"),
                "Saved path",
                match index {
                    0 => rc2fx_core::TargetKind::Mic,
                    1..=3 => rc2fx_core::TargetKind::Playback,
                    _ => rc2fx_core::TargetKind::Output,
                },
                if index == 0 { 1 } else { 2 },
            )
        })
        .collect();
    current.config.selected_targets = (0..5).map(|i| format!("path-{i}")).collect();
    current.selection.active_target_ids = current.config.selected_targets.clone();
    current.engine.message = "界".repeat(300);
    current.routing.message = "x".repeat(1024);
    current.selection.message = "y".repeat(1024);
    history.state(&current);
    let (_, mut native) = native_payload(100);
    let mut value = meter(9, 12, 100);
    value.targets = (0..5)
        .map(|index| {
            let mut t = value.targets[0].clone();
            t.id = format!("path-{index}");
            t.spectrum = vec![f64::MAX; 64];
            if let Some(p) = &mut t.performance {
                for timing in [&mut p.callback, &mut p.worker_wall, &mut p.worker_cpu] {
                    timing.calls = u64::MAX;
                    timing.total_ns = u64::MAX;
                    timing.max_ns = u64::MAX;
                    timing.overruns = u64::MAX;
                    timing.clock_errors = u64::MAX;
                    timing.histogram = [u64::MAX; 8];
                }
            }
            t
        })
        .collect();
    let target_template = native[0].targets[0].clone();
    native[0].targets = value
        .targets
        .iter()
        .map(|meter| {
            let mut target = target_template.clone();
            target.id = meter.id.clone();
            target
        })
        .collect();
    history.native_meter(value, native);
    sample(&mut history, start);
    let exported = report(&history);
    assert_eq!(exported["samples"][0]["telemetryStatus"], "fresh");
    assert_eq!(
        exported["samples"][0]["telemetry"]["meter"]["targets"]
            .as_array()
            .unwrap()
            .len(),
        5
    );
    assert_eq!(exported["report"]["retention"]["rejectedRecords"], 0);
    assert!(exported["report"]["state"].get("config").is_none());
}

#[test]
fn native_pending_encodes_only_latest_window_and_keeps_fault_detection_at_poll_rate() {
    let start = Instant::now();
    let mut history = History::new(start);
    history.state(&state(9, 12));
    for index in 1..=10 {
        let (mut meter, native) = native_payload(index * 100);
        // A transient failed interval must remain visible even if a later
        // pending window replaces it before the one-second sample.
        if index == 3 {
            meter.targets[0].missed_deadlines = 1;
        }
        history.native_meter(meter, native);
    }
    assert_eq!(history.retention.native_windows_serialized, 0);
    assert_eq!(history.retention.replaced_pending_native_windows, 9);
    assert!(
        report(&history)["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["kind"] == "audio-fault")
    );
    sample(&mut history, start + Duration::from_secs(1));
    assert_eq!(history.retention.native_windows_serialized, 1);
    let exported = report(&history);
    let telemetry = &exported["samples"][0]["telemetry"];
    assert_eq!(telemetry["meter"]["targets"][0]["callbacks"], 1000);
    assert!(
        telemetry["at"]["elapsedMs"].as_u64().unwrap()
            < exported["samples"][0]["observation"]["at"]["elapsedMs"]
                .as_u64()
                .unwrap()
    );
    sample(&mut history, start + Duration::from_secs(2));
    assert_eq!(history.retention.native_windows_serialized, 1);
    assert_eq!(
        report(&history)["samples"][1]["telemetryStatus"],
        "noFreshWindow"
    );
}

#[test]
fn lazy_native_epoch_changes_and_oversize_records_never_export_stale_telemetry() {
    let start = Instant::now();
    let mut history = History::new(start);
    history.state(&state(9, 12));
    let (meter, native) = native_payload(100);
    history.native_meter(meter, native);
    history.state(&state(9, 13));
    sample(&mut history, start);
    assert_eq!(history.retention.native_windows_serialized, 0);
    assert_eq!(
        report(&history)["samples"][0]["telemetryStatus"],
        "epochChanged"
    );
    let (mut meter, mut native) = native_payload(200);
    meter.revision = 13;
    // An oversized internal peer response still must respect the stricter
    // per-history-record bound when its pending window is eventually encoded.
    native[0].targets[0].retired_workers = vec![
        rc2fx_native_control::RetiredWorker {
            runtime_epoch: 1,
            worker: Default::default(),
            joined: true,
            drained: true
        };
        1000
    ];
    history.native_meter(meter, native);
    sample(&mut history, start + Duration::from_secs(1));
    let exported = report(&history);
    assert_eq!(
        exported["samples"][1]["telemetryStatus"],
        "oversizeRejected"
    );
    assert!(exported["samples"][1]["telemetry"].is_null());
    assert_eq!(history.retention.rejected_records, 1);
}

#[test]
fn native_exports_preserve_worker_and_callback_clocks_without_main_callback_fabrication() {
    use rc2fx_native_control::{CallbackTiming, ContextStatus, TargetStatus, Timing, WorkerMeter};
    let start = Instant::now();
    let mut history = History::new(start);
    history.state(&state(9, 12));
    let mut value = meter(9, 12, 100);
    value.targets[0].performance = None;
    value.targets[0].missed_deadlines = 3;
    let native = vec![ContextStatus {
        context: 42,
        counters: Default::default(),
        targets: vec![TargetStatus {
            id: "mic-1".into(),
            mode: rc2fx_native_control::PathMode::Processed,
            runtime_epoch: 1,
            retired_workers: vec![],
            retired_worker_count: 0,
            published: 12,
            seen: 12,
            applied: 12,
            fingerprint: [7; 32],
            meter: None,
            worker: Some(WorkerMeter {
                missed_deadlines: 3,
                frame_budget_ns: 10_000_000,
                worker_allowance_ns: 7_000_000,
                queue_wait: Timing {
                    calls: 10,
                    total_ns: 12_000_000,
                    max_ns: 2_000_000,
                    ..Default::default()
                },
                ..Default::default()
            }),
            lates: None,
            misses: None,
            worker_recent_max_ns: None,
        }],
        timing: Some(CallbackTiming {
            cpu_sample_every: 16,
            callback_wall: Timing {
                calls: 100,
                total_ns: 20_000_000,
                ..Default::default()
            },
            callback_cpu: Timing {
                calls: 6,
                total_ns: 1_000_000,
                ..Default::default()
            },
            ..Default::default()
        }),
        events: Vec::new(),
        recent_callback_wall_ns: None,
    }];
    history.native_meter(value.clone(), native.clone());
    sample(&mut history, start);
    let exported = report(&history);
    assert_eq!(exported["samples"][0]["telemetryStatus"], "fresh");
    let telemetry = &exported["samples"][0]["telemetry"];
    assert_eq!(telemetry["source"], "native-mixer");
    assert!(telemetry.get("callbackDiagnostics").is_none());
    assert_eq!(
        telemetry["nativeDiagnostics"][0]["targets"][0]["worker"]["queueWait"]["totalNs"],
        12_000_000
    );
    assert_eq!(
        telemetry["nativeDiagnostics"][0]["timing"]["callbackCpu"]["calls"],
        6
    );
    assert_eq!(
        telemetry["nativeDiagnostics"][0]["timing"]["callbackWall"]["calls"],
        100
    );
    // A completed old worker's faults remain protected events even after its
    // replacement resets live counters and sample history rolls over.
    let mut retired = native.clone();
    retired[0].targets[0].retired_worker_count = 1;
    retired[0].targets[0].retired_workers = vec![rc2fx_native_control::RetiredWorker {
        runtime_epoch: 1,
        worker: retired[0].targets[0].worker.unwrap(),
        joined: true,
        drained: true,
    }];
    retired[0].targets[0].runtime_epoch = 2;
    history.native_meter(value.clone(), retired.clone());
    history.native_meter(value.clone(), retired.clone());
    let incidents = report(&history)["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|event| event["kind"] == "native-worker-retired")
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(incidents.len(), 1);
    assert_eq!(
        incidents[0]["retiredWorkers"][0]["worker"]["missedDeadlines"],
        3
    );
    assert_eq!(incidents[0]["retiredWorkers"][0]["runtimeEpoch"], 1);
    assert_eq!(incidents[0]["retiredWorkers"][0]["joined"], true);
    assert!(
        exported["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["kind"] == "initial-fault-counters")
    );
}

#[test]
fn controller_and_native_process_resources_are_separate_and_pid_reuse_is_rejected() {
    let start = Instant::now();
    let mut history = History::new(start);
    history.state(&state(9, 12));
    let ticket = history.begin_sample(start).unwrap();
    history.finish_sample_with_native(
        ticket,
        ProcessResources::Unavailable {
            message: "controller accounting".into(),
        },
        rc2fx_core::UsbStatus {
            usb1: rc2fx_core::UsbLinkState::Unavailable,
        },
        Some(ProcessResources::Unavailable {
            message: "native accounting".into(),
        }),
    );
    let exported = report(&history);
    let observation = &exported["samples"][0]["observation"];
    assert_eq!(observation["resources"]["message"], "controller accounting");
    assert_eq!(
        observation["nativeMixerResources"]["message"],
        "native accounting"
    );
    {
        let process = NativeProcess::capture(std::process::id());
        assert!(matches!(
            process.resources(),
            ProcessResources::Measured { .. }
        ));
        let start = process.start().expect("a measured process start");
        assert_eq!(start.pid, std::process::id());
        assert!(start.start_ticks > 0 && start.at_ns > 0);
        assert!(
            start.running_for_ns(crate::diagnostics_log::monotonic_ns()) > 0,
            "this test process started before now"
        );
        let wrong = NativeProcess {
            start_ticks: process.start_ticks.map(|ticks| ticks + 1),
            ..process
        };
        assert!(
            matches!(wrong.resources(), ProcessResources::Unavailable { message } if message.contains("identity changed"))
        );
    }
}

#[test]
fn rejected_controls_record_bounded_metadata_without_settings() {
    let start = Instant::now();
    let mut history = History::new(start);
    history.state(&state(9, 12));
    history.rejection(
        9,
        12,
        "apply-effects",
        Some(11),
        &["mic-1".into()],
        &"x".repeat(2000),
    );
    let exported = report(&history);
    let event = exported["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|event| event["kind"] == "control-rejected")
        .unwrap();
    assert_eq!(event["expectedRevision"], 11);
    assert_eq!(event["reason"].as_str().unwrap().len(), 1024);
    assert_eq!(event["targetIds"][0], "mic-1");
    assert!(event.get("config").is_none());
}

fn state(generation: u64, revision: u64) -> DeviceState {
    let config = Config {
        revision,
        selected_targets: vec!["mic-1".into()],
        ..Config::default()
    };
    DeviceState {
        config,
        engine: EngineStatus {
            generation,
            restart_allowed: false,
            mode: EngineMode::Simulation,
            status: EnginePhase::Running,
            applied_revision: revision,
            sample_rate: 48000,
            block_size: 112,
            message: "Observed callbacks".into(),
            audio_advancing: true,
        },
        routing: RoutingStatus {
            status: RoutingPhase::Unmanaged,
            channels: 0,
            recovered: 0,
            message: "Synthetic routing".into(),
        },
        selection: SelectionStatus {
            channel_capacity: None,
            status: SelectionPhase::Idle,
            allowed: false,
            qualified_target_ids: vec![],
            active_target_ids: vec!["mic-1".into()],
            pending_target_ids: vec![],
            message: "Allocated".into(),
        },
    }
}
fn timing() -> Timing {
    Timing {
        calls: 10,
        total_ns: 100,
        max_ns: 10,
        overruns: 0,
        clock_errors: 0,
        histogram: [0, 0, 10, 0, 0, 0, 0, 0],
    }
}
fn meter(generation: u64, revision: u64, callbacks: u64) -> Meter {
    Meter {
        message_type: MeterType::Meter,
        generation,
        revision,
        synthetic: true,
        targets: vec![TargetMeter {
            id: "mic-1".into(),
            input_peak: 0.25,
            output_peak: 0.125,
            rms: 0.1,
            gain_reduction: 0.,
            voice: 0.,
            gate_open: 0.,
            clipped: 0,
            spectrum: vec![],
            callbacks,
            processed_frames: callbacks * 112,
            suppression_frames: callbacks / 4,
            missed_deadlines: 0,
            stale_results: 0,
            input_drops: 0,
            output_drops: 0,
            latency_samples: 1184,
            worker_failed: false,
            performance: Some(Performance {
                callback_budget_ns: 2333333,
                worker_budget_ns: 10000000,
                xruns: 0,
                callback: timing(),
                worker_wall: timing(),
                worker_cpu: timing(),
                helper_late: Some(2),
                helper_stalls: Some(1),
                callback_recent_max_ns: None,
                worker_recent_max_ns: None,
            }),
        }],
        diagnostics_epoch: 0,
        running_for_ns: None,
    }
}
/// One native context holding every target of `meter`, with no events.
fn contexts(meter: &Meter) -> Vec<rc2fx_native_control::ContextStatus> {
    use rc2fx_native_control::{ContextStatus, PathMode, TargetStatus};
    vec![ContextStatus {
        context: 42,
        targets: meter
            .targets
            .iter()
            .map(|target| TargetStatus {
                id: target.id.clone(),
                mode: PathMode::Processed,
                runtime_epoch: 1,
                retired_workers: vec![],
                retired_worker_count: 0,
                published: meter.revision,
                seen: meter.revision,
                applied: meter.revision,
                fingerprint: [7; 32],
                meter: None,
                worker: None,
                lates: None,
                misses: None,
                worker_recent_max_ns: None,
            })
            .collect(),
        counters: Default::default(),
        events: Vec::new(),
        timing: None,
        recent_callback_wall_ns: None,
    }]
}
fn report(history: &History) -> Value {
    let bytes = history.snapshot().encode().unwrap();
    assert!(bytes.len() <= MAX_RESPONSE_BYTES);
    rc2fx_core::from_json(std::str::from_utf8(&bytes).unwrap()).unwrap()
}
fn sample(history: &mut History, now: Instant) {
    let ticket = history.begin_sample(now).unwrap();
    history.finish_sample(
        ticket,
        ProcessResources::Unavailable {
            message: "Fixture unavailable".into(),
        },
        rc2fx_core::UsbStatus {
            usb1: rc2fx_core::UsbLinkState::Unavailable,
        },
    );
}
fn kinds(value: &Value) -> Vec<&str> {
    value["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["kind"].as_str().unwrap())
        .collect()
}

#[test]
fn sample_consumes_only_fresh_meter_and_does_not_catch_up_or_relabel() {
    let start = Instant::now();
    let mut history = History::new(start);
    history.state(&state(7, 11));
    let first = meter(7, 11, 10);
    history.native_meter(first.clone(), contexts(&first));
    sample(&mut history, start);
    assert!(
        history
            .begin_sample(start + Duration::from_millis(999))
            .is_none()
    );
    history.native_meter(first.clone(), contexts(&first));
    sample(&mut history, start + Duration::from_secs(6));
    let value = report(&history);
    let samples = value["samples"].as_array().unwrap();
    assert_eq!(samples.len(), 2);
    assert_eq!(samples[0]["telemetry"]["meter"]["generation"], 7);
    assert_eq!(samples[0]["telemetry"]["meter"]["synthetic"], true);
    assert!(samples[1]["telemetry"].is_null());
    assert_eq!(samples[1]["telemetryStatus"], "noFreshWindow");
    assert_eq!(samples[1]["observation"]["intervalMs"], 6000);
    assert_eq!(value["report"]["retention"]["duplicateMeters"], 1);
    let next = meter(7, 11, 20);
    history.native_meter(next.clone(), contexts(&next));
    history.state(&state(8, 11));
    sample(&mut history, start + Duration::from_secs(7));
    let report = report(&history);
    assert_eq!(report["samples"][2]["telemetryStatus"], "epochChanged");
    assert!(report["samples"][2]["telemetry"].is_null());
    assert_eq!(report["report"]["state"]["engine"]["generation"], 8);
}

#[test]
fn idle_bound_path_does_not_make_advancing_windows_duplicates() {
    let start = Instant::now();
    let mut history = History::new(start);
    history.state(&state(7, 11));
    let with_idle = |callbacks| {
        let mut value = meter(7, 11, callbacks);
        let mut idle = value.targets[0].clone();
        idle.id = "bluetooth".into();
        idle.callbacks = 0;
        idle.processed_frames = 0;
        value.targets.push(idle);
        value
    };
    let first = with_idle(10);
    history.native_meter(first.clone(), contexts(&first));
    sample(&mut history, start);
    // The microphone advances while Bluetooth stays idle: a new window.
    let next = with_idle(20);
    history.native_meter(next.clone(), contexts(&next));
    sample(&mut history, start + Duration::from_secs(1));
    // Nothing advanced: still a duplicate.
    history.native_meter(next.clone(), contexts(&next));
    sample(&mut history, start + Duration::from_secs(2));
    let value = report(&history);
    let samples = value["samples"].as_array().unwrap();
    assert_eq!(samples[1]["telemetryStatus"], "fresh");
    assert_eq!(samples[2]["telemetryStatus"], "noFreshWindow");
    assert_eq!(value["report"]["retention"]["duplicateMeters"], 1);
}

#[test]
fn fault_deltas_preserve_baselines_across_effects_revision_and_reset_on_replacement() {
    let mut history = History::default();
    history.state(&state(1, 4));
    let mut value = meter(1, 4, 10);
    value.targets[0].missed_deadlines = 2;
    history.native_meter(value.clone(), contexts(&value));
    assert!(kinds(&report(&history)).contains(&"initial-fault-counters"));
    value.targets[0].callbacks = 20;
    value.targets[0].processed_frames = 2240;
    history.native_meter(value.clone(), contexts(&value));
    history.state(&state(1, 5));
    value.revision = 5;
    value.targets[0].callbacks = 30;
    value.targets[0].processed_frames = 3360;
    history.native_meter(value.clone(), contexts(&value));
    assert!(!kinds(&report(&history)).contains(&"audio-fault"));
    value.targets[0].missed_deadlines = 3;
    value.targets[0].callbacks = 40;
    value.targets[0].processed_frames = 4480;
    history.native_meter(value.clone(), contexts(&value));
    assert_eq!(
        kinds(&report(&history))
            .iter()
            .filter(|kind| **kind == "audio-fault")
            .count(),
        1
    );
    value.targets[0].missed_deadlines = 0;
    value.targets[0].suppression_frames = 0;
    value.targets[0].callbacks = 50;
    value.targets[0].processed_frames = 5600;
    history.native_meter(value.clone(), contexts(&value));
    assert!(kinds(&report(&history)).contains(&"counter-reset"));
    history.state(&state(2, 5));
    value.generation = 2;
    value.targets[0].missed_deadlines = 9;
    history.native_meter(value.clone(), contexts(&value));
    assert_eq!(
        kinds(&report(&history))
            .iter()
            .filter(|kind| **kind == "audio-fault")
            .count(),
        1
    );
}

#[test]
fn resource_ticket_retains_original_identity_across_off_lock_probe() {
    let mut history = History::default();
    history.state(&state(4, 12));
    let ticket = history.begin_sample(Instant::now()).unwrap();
    history.state(&state(5, 13));
    history.finish_sample(
        ticket,
        ProcessResources::Unavailable {
            message: "No /proc".into(),
        },
        rc2fx_core::UsbStatus {
            usb1: rc2fx_core::UsbLinkState::NotAttached,
        },
    );
    let value = report(&history);
    assert_eq!(value["report"]["state"]["revision"], 13);
    assert_eq!(value["samples"][0]["observation"]["revision"], 12);
    assert_eq!(value["samples"][0]["observation"]["generation"], 4);
    assert_eq!(
        value["samples"][0]["observation"]["usb"]["usb1"],
        "notAttached"
    );
    assert_eq!(
        value["samples"][0]["observation"]["resources"]["status"],
        "unavailable"
    );
}

#[test]
fn process_parser_uses_final_comm_delimiter_and_rejects_invalid_units_numbers_and_truncation() {
    let mut fields = vec!["0".to_owned(); 22];
    fields[0] = "S".into();
    fields[11] = "7".into();
    fields[12] = "11".into();
    fields[17] = "4".into();
    fields[19] = "987".into();
    fields[20] = "65536".into();
    fields[21] = "3".into();
    let input = |fields: &[String]| format!("123 (odd ) ( process name) {}", fields.join(" "));
    let value =
        serde_json::to_value(parse_process(input(&fields).as_bytes(), 123, 100, 4096).unwrap())
            .unwrap();
    assert_eq!(value["cpuTicks"], 18);
    assert_eq!(value["rssBytes"], 12288);
    assert_eq!(value["virtualBytes"], 65536);
    assert_eq!(value["threads"], 4);
    assert_eq!(value["startTicks"], 987);
    assert!(parse_process(input(&fields).as_bytes(), 124, 100, 4096).is_err());
    assert!(parse_process(input(&fields).as_bytes(), 123, 0, 4096).is_err());
    for (at, bad) in [
        (21, "-1"),
        (11, "18446744073709551615"),
        (17, "0"),
        (19, "bad"),
        (20, "0"),
    ] {
        let mut altered = fields.clone();
        altered[at] = bad.into();
        assert!(parse_process(input(&altered).as_bytes(), 123, 100, 4096).is_err());
    }
    assert!(parse_process(b"123 (name) S 1 2", 123, 100, 4096).is_err());
    fields[21] = i64::MAX.to_string();
    assert!(parse_process(input(&fields).as_bytes(), 123, 100, 4096).is_err());
}

#[test]
fn real_linux_process_resource_probe_is_measured_with_current_identity() {
    match process_resources() {
        ProcessResources::Measured {
            pid,
            threads,
            virtual_bytes,
            ticks_per_second,
            start_ticks,
            ..
        } => {
            assert_eq!(pid, std::process::id());
            assert!(threads > 0 && virtual_bytes > 0 && ticks_per_second > 0 && start_ticks > 0);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn count_and_byte_eviction_and_writer_capacity_are_bounded() {
    let start = Instant::now();
    let mut history = History::new(start);
    history.state(&state(1, 1));
    for second in 0..350 {
        sample(&mut history, start + Duration::from_secs(second));
    }
    assert_eq!(history.samples.len(), MAX_SAMPLES);
    assert_eq!(history.retention.evicted_samples, 50);
    for index in 0..600 {
        let mut changed = state(1, index + 2);
        changed.engine.message = "é".repeat(512);
        history.state(&changed);
    }
    assert!(history.events.len() <= MAX_EVENTS);
    assert!(history.bytes <= MAX_HISTORY_BYTES);
    assert!(history.retention.evicted_events > 0);
    let value = report(&history);
    assert!(value["report"]["historyBytes"].as_u64().unwrap() <= MAX_HISTORY_BYTES as u64);
    let mut out = Limited::new(127);
    for _ in 0..127 {
        out.raw(b"x").unwrap();
        assert!(out.bytes.capacity() <= 127);
    }
    assert!(out.raw(b"x").is_err());
    assert_eq!(out.bytes.len(), 127);
    let snapshot = history.snapshot();
    for index in 0..100 {
        history.state(&state(2, index + 1));
    }
    assert!(snapshot.encode().unwrap().len() <= MAX_RESPONSE_BYTES);
}

#[test]
fn seven_minutes_of_large_native_style_samples_cannot_evict_incident_events() {
    let start = Instant::now();
    let mut history = History::new(start);
    history.state(&state(9, 12));
    history.rejection(
        9,
        12,
        "apply-effects",
        Some(12),
        &["mic-1".into()],
        "native busy",
    );
    let saved_events: Vec<_> = history
        .events
        .iter()
        .map(|record| record.bytes.clone())
        .collect();
    assert!(
        report(&history)["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["kind"] == "control-rejected")
    );
    // Reproduce the measured ~6KiB native sample size and seven-minute churn.
    for second in 0..420 {
        history.push(
            true,
            encoded(
                &serde_json::json!({"second": second, "nativeDiagnostics": "x".repeat(6 * 1024)}),
                MAX_RECORD_BYTES,
            ),
        );
    }
    assert!(history.retention.evicted_samples > 350);
    assert_eq!(history.retention.evicted_events, 0);
    assert_eq!(history.events.len(), saved_events.len());
    for (record, saved) in history.events.iter().zip(&saved_events) {
        assert_eq!(&record.bytes, saved);
    }
    let exported = report(&history);
    assert_eq!(exported["report"]["scope"], "rc2fx-controller");
    assert_eq!(exported["report"]["eventHistoryBytes"], history.event_bytes);
    assert_eq!(
        exported["report"]["sampleHistoryBytes"],
        history.bytes - history.event_bytes
    );
    assert!(exported["report"]["historyBytes"].as_u64().unwrap() <= MAX_HISTORY_BYTES as u64);
    assert!(history.snapshot().encode().unwrap().len() <= MAX_RESPONSE_BYTES);
}

#[test]
fn events_have_independent_fifo_byte_capacity_and_samples_borrow_unused_space() {
    let start = Instant::now();
    let mut history = History::new(start);
    history.state(&state(1, 1));
    for index in 0..100 {
        history.push(
            true,
            encoded(
                &serde_json::json!({"index": index, "padding": "s".repeat(6 * 1024)}),
                MAX_RECORD_BYTES,
            ),
        );
    }
    // No fixed sample partition wastes the unused event allowance.
    assert!(history.bytes - history.event_bytes > MAX_HISTORY_BYTES - MAX_EVENT_HISTORY_BYTES);
    let old_sample_evictions = history.retention.evicted_samples;
    for index in 0..100 {
        history.push(false, encoded(&serde_json::json!({"kind": "incident", "index": index, "reason": "e".repeat(8 * 1024)}), MAX_RECORD_BYTES));
        assert!(history.event_bytes <= MAX_EVENT_HISTORY_BYTES);
        assert!(history.bytes <= MAX_HISTORY_BYTES);
    }
    assert!(history.retention.evicted_events > 0);
    assert!(history.retention.evicted_samples > old_sample_evictions);
    let events = report(&history)["events"].as_array().unwrap().clone();
    assert_eq!(events.last().unwrap()["index"], 99);
    assert!(events.first().unwrap()["index"].as_u64().unwrap() > 0);
    let event_evictions = history.retention.evicted_events;
    for index in 0..420 {
        history.push(
            true,
            encoded(
                &serde_json::json!({"index": index, "padding": "s".repeat(6 * 1024)}),
                MAX_RECORD_BYTES,
            ),
        );
    }
    assert_eq!(history.retention.evicted_events, event_evictions);
    assert_eq!(report(&history)["events"], Value::Array(events));
    assert_eq!(
        history.event_bytes,
        history
            .events
            .iter()
            .map(|record| record.bytes.len())
            .sum::<usize>()
    );
    assert_eq!(
        history.bytes,
        history.event_bytes
            + history
                .samples
                .iter()
                .map(|record| record.bytes.len())
                .sum::<usize>()
    );
}

#[test]
fn process_start_ticks_land_on_the_event_clock_and_running_for_saturates() {
    // `_SC_CLK_TCK` is 100 here, so field 22 counts 10 ms ticks since boot.
    let start = ProcessStart::from_ticks(123, 987, 100).expect("a usable tick rate");
    assert_eq!(
        start,
        ProcessStart {
            pid: 123,
            start_ticks: 987,
            at_ns: 9_870_000_000,
        }
    );
    assert_eq!(start.running_for_ns(10_000_000_000), 130_000_000);
    assert_eq!(
        start.running_for_ns(9_000_000_000),
        0,
        "a clock read before the start never wraps"
    );
    assert_eq!(ProcessStart::from_ticks(123, 987, 0), None);
    // The product stays in 128 bits on a very long boot.
    assert!(ProcessStart::from_ticks(123, u64::MAX, 1).is_some());
}

#[test]
fn the_stat_start_time_converts_onto_the_event_clock() {
    let mut fields = vec!["0".to_owned(); 22];
    fields[0] = "S".into();
    fields[17] = "4".into();
    fields[19] = "987".into();
    fields[20] = "65536".into();
    fields[21] = "3".into();
    let line = format!("123 (rc_audio_mixer) {}", fields.join(" "));
    let ProcessResources::Measured {
        start_ticks,
        ticks_per_second,
        ..
    } = parse_process(line.as_bytes(), 123, 100, 4096).unwrap()
    else {
        panic!("a valid stat line is measured");
    };
    let start = ProcessStart::from_ticks(123, start_ticks, ticks_per_second).unwrap();
    assert_eq!(start.start_ticks, 987);
    assert_eq!(start.at_ns, 9_870_000_000);
}

#[test]
fn an_unreadable_process_stat_leaves_no_process_start() {
    let process = NativeProcess::capture(u32::MAX);
    assert!(process.start().is_none());
    assert!(matches!(
        process.resources(),
        ProcessResources::Unavailable { .. }
    ));
}
