use super::*;
use crate::{controller::Controller, coordinator::Request as ControlRequest, store::Store};
use rc2fx_core::{ConfigUpdate, RoutingRequest, SelectionRequest};
use rc2fx_native_control::{AudioMeter, Request, Response, Session, VERSION};
use std::{
    collections::HashMap,
    os::unix::net::UnixDatagram,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
};

#[test]
fn initial_stream_readiness_is_distinct_from_a_running_path_stall() {
    let start = Instant::now();
    let mut path = Bound {
        id: "usb-main".into(),
        context: 1,
        expected: [0; 32],
        mode: PathMode::Processed,
        kind: TargetKind::Playback,
        channels: 2,
        publication: 1,
        frames: 0,
        last_progress: start,
        progressed: false,
    };
    let deadline = Duration::from_secs(2);
    assert!(!path.timed_out(start + Duration::from_secs(7), start, deadline));
    assert!(path.timed_out(start + INITIAL_PROGRESS_TIMEOUT, start, deadline));
    path.progressed = true;
    path.last_progress = start + Duration::from_secs(7);
    assert!(!path.timed_out(start + Duration::from_secs(8), start, deadline));
    assert!(path.timed_out(start + Duration::from_secs(9), start, deadline));
}

#[derive(Clone, Debug)]
struct Event {
    prepare: bool,
    transaction: Transaction,
    mode: PathMode,
}
struct FakePath {
    target: rc2fx_core::Target,
    status: TargetStatus,
}
struct Peer {
    directory: tempfile::TempDir,
    stop: Arc<AtomicBool>,
    drop_commit: Arc<AtomicBool>,
    hold_window: Arc<AtomicBool>,
    failed_worker: Arc<AtomicBool>,
    events: Arc<Mutex<Vec<Event>>>,
    task: Option<thread::JoinHandle<()>>,
}
impl Peer {
    fn new(config: &Config) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let socket = UnixDatagram::bind(directory.path().join("control.sock")).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        let session = Session::local().unwrap();
        let mut paths: Vec<_> = config
            .targets
            .iter()
            .filter(|target| config.selected_targets.contains(&target.id))
            .map(|target| FakePath {
                target: target.clone(),
                status: TargetStatus {
                    id: target.id.clone(),
                    mode: PathMode::Processed,
                    runtime_epoch: 1,
                    retired_workers: vec![],
                    retired_worker_count: 0,
                    published: 1,
                    seen: 1,
                    applied: 1,
                    fingerprint: fingerprint(target).unwrap(),
                    worker: None,
                    meter: Some(AudioMeter {
                        revision: 1,
                        callbacks: 0,
                        processed_frames: 0,
                        input_peak: 0.25,
                        output_peak: 0.125,
                        rms: 0.1,
                        gain_reduction: 0.0,
                        voice: 0.0,
                        gate_open: 0.0,
                        clipped: 0,
                        latency_samples: 0,
                        dropped_windows: 0,
                    }),
                    lates: None,
                    misses: None,
                },
            })
            .collect();
        let stop = Arc::new(AtomicBool::new(false));
        let drop_commit = Arc::new(AtomicBool::new(false));
        let events = Arc::new(Mutex::new(Vec::new()));
        let hold_window = Arc::new(AtomicBool::new(false));
        let failed_worker = Arc::new(AtomicBool::new(false));
        let (holding, failing) = (hold_window.clone(), failed_worker.clone());
        let (stopping, dropping, trace) = (stop.clone(), drop_commit.clone(), events.clone());
        let task = thread::spawn(move || {
            let mut buffer = vec![0; rc2fx_native_control::MAX_REQUEST + 1];
            let mut prepared =
                HashMap::<String, (Transaction, rc2fx_core::Target, PathMode)>::new();
            let mut completed = HashMap::<String, Transaction>::new();
            while !stopping.load(Ordering::Acquire) {
                let (length, address) = match socket.recv_from(&mut buffer) {
                    Ok(value) => value,
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ) =>
                    {
                        continue;
                    }
                    Err(error) => panic!("{error}"),
                };
                let request: Request = serde_json::from_slice(&buffer[..length]).unwrap();
                assert_eq!(request.version, VERSION);
                let reply = match request.operation {
                    Operation::Hello => Reply::Hello,
                    Operation::Inventory => {
                        for path in &mut paths {
                            let meter = path.status.meter.as_mut().unwrap();
                            if !holding.load(Ordering::Acquire) {
                                meter.callbacks += 100;
                                meter.processed_frames += 11_200;
                                meter.revision = path.status.published;
                            }
                            if path.status.id == "mic-1" && failing.load(Ordering::Acquire) {
                                path.status.worker = Some(rc2fx_native_control::WorkerMeter {
                                    failed: true,
                                    ..Default::default()
                                });
                            }
                            path.status.seen = path.status.published;
                            path.status.applied = path.status.published;
                        }
                        Reply::Inventory {
                            contexts: vec![ContextStatus {
                                context: 42,
                                targets: paths.iter().map(|path| path.status.clone()).collect(),
                                timing: None,
                                counters: Default::default(),
                                events: Vec::new(),
                            }],
                        }
                    }
                    Operation::Prepare {
                        transaction,
                        target,
                        mode,
                    } => {
                        assert!(transaction.valid());
                        trace.lock().unwrap().push(Event {
                            prepare: true,
                            transaction: transaction.clone(),
                            mode,
                        });
                        let path = paths
                            .iter()
                            .find(|path| path.status.id == target.id)
                            .unwrap();
                        assert!(transaction.revision > path.status.published);
                        prepared
                            .insert(transaction.id.clone(), (transaction.clone(), *target, mode));
                        Reply::Prepared { transaction }
                    }
                    Operation::Commit { transaction } => {
                        if let Some((_, target, mode)) = prepared.remove(&transaction.id) {
                            let path = paths
                                .iter_mut()
                                .find(|path| path.status.id == target.id)
                                .unwrap();
                            let reset = rc2fx_dsp::Prepared::new(&path.target, 48_000)
                                .unwrap()
                                .requires_reset(
                                    &rc2fx_dsp::Prepared::new(&target, 48_000).unwrap(),
                                );
                            if mode == PathMode::Passthrough {
                                path.status.runtime_epoch = 0;
                            } else if path.status.mode != mode || reset {
                                path.status.runtime_epoch = transaction.revision;
                            }
                            path.target = target;
                            path.status.fingerprint = fingerprint(&path.target).unwrap();
                            path.status.mode = mode;
                            path.status.published = transaction.revision;
                            completed.insert(transaction.id.clone(), transaction.clone());
                        } else {
                            assert_eq!(completed.get(&transaction.id), Some(&transaction));
                        }
                        let mode = paths
                            .iter()
                            .find(|path| path.status.id == transaction.target_id)
                            .unwrap()
                            .status
                            .mode;
                        trace.lock().unwrap().push(Event {
                            prepare: false,
                            transaction: transaction.clone(),
                            mode,
                        });
                        if dropping.swap(false, Ordering::AcqRel) {
                            continue;
                        }
                        Reply::Published { transaction }
                    }
                    Operation::Cancel { transaction } => {
                        prepared.remove(&transaction.id);
                        Reply::Cancelled { transaction }
                    }
                    Operation::Status { .. } => unreachable!(),
                };
                let response = Response {
                    version: VERSION,
                    request_id: request.request_id,
                    session: session.clone(),
                    reply,
                };
                socket
                    .send_to_addr(&serde_json::to_vec(&response).unwrap(), &address)
                    .unwrap();
            }
        });
        Self {
            directory,
            stop,
            drop_commit,
            hold_window,
            failed_worker,
            events,
            task: Some(task),
        }
    }
    fn path(&self) -> PathBuf {
        self.directory.path().join("control.sock")
    }
    fn preparations(&self) -> Vec<Event> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter(|event| event.prepare)
            .cloned()
            .collect()
    }
}
impl Drop for Peer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.task.take().unwrap().join().unwrap();
    }
}

fn fixture() -> (tempfile::TempDir, Peer, Controller, Runtime) {
    let config = Config {
        selected_targets: vec!["mic-1".into(), "headphones-2".into()],
        ..Config::default()
    };
    let peer = Peer::new(&config);
    let directory = tempfile::tempdir().unwrap();
    let mut controller = Controller::new(Store::open(directory.path(), config).unwrap());
    let mut runtime =
        Runtime::connect(&peer.path(), controller.config(), Duration::from_secs(2)).unwrap();
    controller.attach_native(&mut runtime).unwrap();
    advance(&mut controller, &mut runtime);
    (directory, peer, controller, runtime)
}
fn advance(controller: &mut Controller, runtime: &mut Runtime) {
    let observation = runtime.observe(Instant::now()).unwrap();
    assert!(observation.applied && observation.advancing);
    controller.native_progress(observation);
}
fn toggle(controller: &mut Controller, runtime: &mut Runtime, enabled: bool) {
    let state = controller.state();
    controller
        .native_transition(
            ControlRequest::Routing(RoutingRequest {
                enabled,
                expected_revision: state.config.revision,
                expected_generation: state.engine.generation,
            }),
            runtime,
        )
        .unwrap();
}

#[cfg(feature = "native-pipeline")]
#[tokio::test]
async fn show_removal_selection_is_committed_by_the_coordinator_handle() {
    use crate::{
        coordinator::{Coordinator, receive},
        show_watch::Watch,
    };
    let (directory, _peer, controller, runtime) = fixture();
    let (handle, owner) = Coordinator::new_native(controller, runtime).unwrap();
    let task = thread::spawn(move || owner.run());
    let state = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let state = receive(handle.state()).await.unwrap();
            if state.selection.allowed {
                break state;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let mut watcher = Watch::default();
    let assigned = || Some(["mic-1".to_owned()].into_iter().collect());
    assert!(watcher.observe(&state, assigned()).is_none());
    assert!(watcher.observe(&state, assigned()).is_none());
    let request = watcher.observe(&state, Some(Default::default())).unwrap();
    let reply = receive(handle.selection(request)).await.unwrap();
    assert_eq!(reply.config.selected_targets, ["headphones-2"]);
    assert_eq!(reply.config.targets, state.config.targets);
    handle.close();
    handle.shutdown().await.unwrap();
    let stopped = task.join().unwrap();
    assert_eq!(stopped.config().selected_targets, ["headphones-2"]);
    drop(stopped);
    let reopened = Store::open(directory.path(), Config::default()).unwrap();
    assert_eq!(reopened.config().selected_targets, ["headphones-2"]);
    assert_eq!(reopened.config().targets, state.config.targets);
}

#[test]
fn show_removal_uses_latest_native_selection_and_retains_effects_and_presets() {
    use crate::{show_watch::Watch, store::StoreError};
    use std::collections::HashSet;
    let assigned = |ids: &[&str]| -> Option<HashSet<String>> {
        Some(ids.iter().map(|id| (*id).to_owned()).collect())
    };
    let (directory, peer, mut controller, mut runtime) = fixture();
    let initial = controller.state();
    let mut watcher = Watch::default();
    assert!(watcher.observe(&initial, assigned(&["mic-1"])).is_none());
    assert!(watcher.observe(&initial, assigned(&["mic-1"])).is_none());
    let stale = watcher.observe(&initial, assigned(&[])).unwrap();
    assert_eq!(stale.target_ids, ["headphones-2"]);

    // A desktop edit wins before the observer's removal is admitted. The
    // observer may not overwrite it with its older revision or effect values.
    let mut targets = initial.config.targets.clone();
    targets
        .iter_mut()
        .find(|t| t.id == "mic-1")
        .unwrap()
        .gain
        .db = -7.5;
    controller
        .apply_native(
            ConfigUpdate {
                expected_revision: initial.config.revision,
                targets,
            },
            &mut runtime,
        )
        .unwrap();
    advance(&mut controller, &mut runtime);
    controller
        .create_preset("Preserved effects", controller.config().revision)
        .unwrap();
    let path = directory.path().join("state.json");
    let before_bytes = std::fs::read(&path).unwrap();
    let before: serde_json::Value = serde_json::from_slice(&before_bytes).unwrap();
    let before_events = peer.preparations().len();
    assert!(matches!(
        controller.native_transition(ControlRequest::Selection(stale), &mut runtime,),
        Err(Error::Store(StoreError::Conflict))
    ));
    assert_eq!(std::fs::read(&path).unwrap(), before_bytes);
    assert_eq!(peer.preparations().len(), before_events);

    // Unavailable reads cannot consume pending debt or cause a selection.
    let current = controller.state();
    assert!(watcher.observe(&current, None).is_none());
    let fresh = watcher.observe(&current, assigned(&[])).unwrap();
    assert_eq!(fresh.expected_revision, current.config.revision);
    assert_eq!(fresh.expected_generation, current.engine.generation);
    assert_eq!(fresh.target_ids, ["headphones-2"]);
    let expected_effects = current.config.targets.clone();
    controller
        .native_transition(ControlRequest::Selection(fresh), &mut runtime)
        .unwrap();
    advance(&mut controller, &mut runtime);
    let applied = controller.state();
    assert_eq!(applied.config.selected_targets, ["headphones-2"]);
    assert_eq!(applied.config.targets, expected_effects);
    assert_eq!(applied.engine.generation, initial.engine.generation);
    let preparations = peer.preparations();
    assert_eq!(preparations.len(), before_events + 1);
    assert_eq!(preparations.last().unwrap().transaction.target_id, "mic-1");
    assert_eq!(preparations.last().unwrap().mode, PathMode::Passthrough);
    let after: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(after["presets"], before["presets"]);
    assert_eq!(after["config"]["targets"], before["config"]["targets"]);
    assert!(watcher.observe(&applied, assigned(&["mic-1"])).is_none());
    assert_eq!(controller.config().selected_targets, ["headphones-2"]);
    let presets = controller.presets();
    drop(controller);
    let reopened = Store::open(directory.path(), Config::default()).unwrap();
    assert_eq!(reopened.config().selected_targets, ["headphones-2"]);
    assert_eq!(reopened.config().targets, expected_effects);
    assert_eq!(reopened.presets(), presets);
}

#[test]
fn global_toggles_use_native_publications_without_consuming_config_revision_or_generation() {
    let (_directory, peer, mut controller, mut runtime) = fixture();
    let initial = controller.state();
    for _ in 0..3 {
        toggle(&mut controller, &mut runtime, false);
        advance(&mut controller, &mut runtime);
        assert!(controller.state().can_enable_processing());
        assert!(controller.state().selection.active_target_ids.is_empty());
        toggle(&mut controller, &mut runtime, true);
        advance(&mut controller, &mut runtime);
        assert!(controller.state().is_applied());
    }
    assert_eq!(controller.config().revision, initial.config.revision);
    assert_eq!(
        controller.state().engine.generation,
        initial.engine.generation
    );
    let before = peer.preparations().len();
    let mut targets = controller.config().targets.clone();
    targets
        .iter_mut()
        .find(|target| target.id == "headphones-2")
        .unwrap()
        .eq
        .input_gain_db = -6.0;
    controller
        .apply_native(
            ConfigUpdate {
                expected_revision: initial.config.revision,
                targets,
            },
            &mut runtime,
        )
        .unwrap();
    advance(&mut controller, &mut runtime);
    let events = peer.preparations();
    assert_eq!(events.len(), before + 1);
    assert_eq!(events.last().unwrap().transaction.target_id, "headphones-2");
    assert!(events.last().unwrap().transaction.revision > controller.config().revision);
    assert_eq!(controller.config().revision, initial.config.revision + 1);
    assert_eq!(
        controller.state().engine.generation,
        initial.engine.generation
    );
}

#[test]
fn bound_path_selection_and_structural_replacement_touch_only_changed_paths() {
    let (_directory, peer, mut controller, mut runtime) = fixture();
    let initial = controller.state();
    controller
        .native_transition(
            ControlRequest::Selection(SelectionRequest {
                expected_revision: initial.config.revision,
                expected_generation: initial.engine.generation,
                target_ids: vec!["mic-1".into()],
            }),
            &mut runtime,
        )
        .unwrap();
    advance(&mut controller, &mut runtime);
    assert_eq!(
        controller.state().selection.active_target_ids,
        vec!["mic-1"]
    );
    assert_eq!(
        controller.state().selection.qualified_target_ids,
        vec!["mic-1", "headphones-2"]
    );
    let events = peer.preparations();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].transaction.target_id, "headphones-2");
    assert_eq!(events[0].mode, PathMode::Passthrough);
    let mut targets = controller.config().targets.clone();
    targets
        .iter_mut()
        .find(|target| target.id == "mic-1")
        .unwrap()
        .suppression
        .enabled = true;
    controller
        .apply_native(
            ConfigUpdate {
                expected_revision: controller.config().revision,
                targets,
            },
            &mut runtime,
        )
        .unwrap();
    advance(&mut controller, &mut runtime);
    assert_eq!(peer.preparations().len(), 2);
    assert_eq!(peer.preparations()[1].transaction.target_id, "mic-1");
    let state = controller.state();
    controller
        .native_transition(
            ControlRequest::Selection(SelectionRequest {
                expected_revision: state.config.revision,
                expected_generation: state.engine.generation,
                target_ids: vec!["mic-1".into(), "headphones-2".into()],
            }),
            &mut runtime,
        )
        .unwrap();
    advance(&mut controller, &mut runtime);
    assert_eq!(peer.preparations().len(), 3);
    assert_eq!(peer.preparations()[2].transaction.target_id, "headphones-2");
    assert_eq!(peer.preparations()[2].mode, PathMode::Processed);
    assert_eq!(
        controller.state().engine.generation,
        initial.engine.generation
    );
}

#[test]
fn lost_global_commit_reply_retries_same_receipt_without_another_prepare_or_save() {
    let (_directory, peer, mut controller, mut runtime) = fixture();
    let initial = controller.state();
    peer.drop_commit.store(true, Ordering::Release);
    toggle(&mut controller, &mut runtime, false);
    assert_eq!(runtime.pending.len(), 1);
    let receipt = runtime.pending[0].clone();
    advance(&mut controller, &mut runtime);
    assert!(runtime.pending.is_empty());
    let events = peer.events.lock().unwrap();
    assert_eq!(events.iter().filter(|event| event.prepare).count(), 2);
    assert_eq!(
        events
            .iter()
            .filter(|event| !event.prepare && event.transaction == receipt)
            .count(),
        2
    );
    assert_eq!(controller.config().revision, initial.config.revision);
    assert_eq!(
        controller.state().engine.generation,
        initial.engine.generation
    );
}

#[test]
fn unbound_selection_is_rejected_without_touching_disk_or_audio() {
    let (directory, peer, mut controller, mut runtime) = fixture();
    let initial = controller.state();
    let bytes = std::fs::read(directory.path().join("state.json")).unwrap();
    let result = controller.native_transition(
        ControlRequest::Selection(SelectionRequest {
            expected_revision: initial.config.revision,
            expected_generation: initial.engine.generation,
            target_ids: vec!["usb-main".into()],
        }),
        &mut runtime,
    );
    assert!(matches!(result, Err(Error::NativeUnsupported(_))));
    assert_eq!(controller.state(), initial);
    assert_eq!(
        std::fs::read(directory.path().join("state.json")).unwrap(),
        bytes
    );
    assert!(peer.preparations().is_empty());
}

#[test]
fn external_publications_do_not_turn_live_frames_into_stalls_and_exact_restore_reconciles() {
    let (_directory, peer, mut controller, mut runtime) = fixture();
    let initial = controller.state();
    let mut client = Client::connect(&peer.path()).unwrap();
    let target = controller
        .config()
        .targets
        .iter()
        .find(|target| target.id == "mic-1")
        .unwrap()
        .clone();
    let change = |client: &mut Client, revision, mode| {
        let transaction = client.transaction(42, "mic-1", revision).unwrap();
        assert!(matches!(
            client
                .request(Operation::Prepare {
                    transaction: transaction.clone(),
                    target: Box::new(target.clone()),
                    mode
                })
                .unwrap()
                .reply,
            Reply::Prepared { .. }
        ));
        assert!(matches!(
            client.commit_saved(&transaction),
            Publication::Published(_)
        ));
    };
    change(&mut client, 2, PathMode::Passthrough);
    runtime.paths[0].last_progress = Instant::now() - Duration::from_secs(3);
    let observation = runtime.observe(Instant::now()).unwrap();
    assert!(observation.advancing && !observation.applied);
    assert!(!observation.failure);
    change(&mut client, 3, PathMode::Processed);
    // Applied acknowledgement can precede the next ~50ms meter window. Do not
    // relabel old amplitudes as the restored publication or report a stall.
    peer.hold_window.store(true, Ordering::Release);
    let observation = runtime
        .observe(Instant::now() + Duration::from_millis(50))
        .unwrap();
    assert!(observation.applied && observation.advancing);
    assert!(observation.meter.is_none());
    peer.hold_window.store(false, Ordering::Release);
    advance(&mut controller, &mut runtime);
    assert!(controller.state().is_applied());
    assert_eq!(controller.config(), &initial.config);
    assert_eq!(
        controller.state().engine.generation,
        initial.engine.generation
    );
    assert_eq!(runtime.paths[0].publication, 3);
}

#[test]
fn worker_failure_is_not_hidden_by_an_old_meter_window() {
    let (_directory, peer, mut controller, mut runtime) = fixture();
    peer.hold_window.store(true, Ordering::Release);
    peer.failed_worker.store(true, Ordering::Release);
    toggle(&mut controller, &mut runtime, false);
    assert!(
        matches!(runtime.observe(Instant::now()), Err(Error::Native(message)) if message.contains("worker failed"))
    );
}

/// Whether the read just made scheduled the next one at the slow cadence.
fn polls_slowly(runtime: &mut Runtime, watched: bool) -> bool {
    runtime.next_poll = Instant::now();
    let before = Instant::now();
    assert!(runtime.poll(|| watched).is_some());
    runtime.next_poll.saturating_duration_since(before) > POLL_INTERVAL * 2
}

#[test]
fn only_a_settled_unwatched_engine_polls_slowly() {
    let (_directory, peer, _controller, mut runtime) = fixture();
    assert!(polls_slowly(&mut runtime, false));
    assert!(!polls_slowly(&mut runtime, true));
    // A processed path that stops advancing returns to fast polling at once,
    // so its stall deadline is not stretched by the slower cadence.
    peer.hold_window.store(true, Ordering::Release);
    assert!(!polls_slowly(&mut runtime, false));
    peer.hold_window.store(false, Ordering::Release);
    assert!(polls_slowly(&mut runtime, false));
}

#[test]
fn an_unpublished_change_keeps_fast_polling_without_a_client() {
    let (_directory, peer, mut controller, mut runtime) = fixture();
    let settled = runtime.observe(Instant::now()).unwrap();
    assert_eq!(runtime.interval_after(&settled, false), IDLE_POLL_INTERVAL);
    peer.drop_commit.store(true, Ordering::Release);
    toggle(&mut controller, &mut runtime, false);
    assert!(!runtime.pending.is_empty());
    assert_eq!(runtime.interval_after(&settled, false), POLL_INTERVAL);
}

/// Every native path reports performance: the worker's own timings when it has
/// one, and always the engine's shared numbers. A path without a worker still
/// reports the callback timing, xruns and the helper counters, with zero worker
/// timing and a calls count of zero to say "no worker here".
#[test]
fn every_native_path_reports_performance() {
    use rc2fx_native_control::{EngineCounters, PathMode, TargetStatus, Timing, WorkerMeter};
    let timing = rc2fx_native_control::CallbackTiming {
        callback_budget_ns: 2_333_333,
        callback_wall: Timing {
            calls: 7,
            total_ns: 700,
            max_ns: 200,
            overruns: 2,
            clock_errors: 0,
            histogram: [7, 0, 0, 0, 0, 0, 0, 0],
        },
        ..Default::default()
    };
    let worker = WorkerMeter {
        worker_allowance_ns: 9_333_333,
        turnaround: Timing {
            calls: 6,
            total_ns: 600,
            max_ns: 300,
            overruns: 1,
            clock_errors: 0,
            histogram: [6, 0, 0, 0, 0, 0, 0, 0],
        },
        cpu: Timing {
            calls: 4,
            total_ns: 400,
            max_ns: 150,
            overruns: 0,
            clock_errors: 2,
            histogram: [4, 0, 0, 0, 0, 0, 0, 0],
        },
        ..Default::default()
    };
    let status = |id: &str, worker: Option<WorkerMeter>| TargetStatus {
        id: id.into(),
        mode: PathMode::Processed,
        runtime_epoch: 1,
        retired_workers: vec![],
        retired_worker_count: 0,
        published: 1,
        seen: 1,
        applied: 1,
        fingerprint: [0; 32],
        meter: None,
        worker,
        lates: None,
        misses: None,
    };
    let contexts = vec![ContextStatus {
        context: 1,
        targets: vec![status("mic-1", Some(worker)), status("bluetooth", None)],
        timing: Some(timing),
        counters: EngineCounters {
            xruns: 5,
            helper_late: 2,
            helper_stalls: 1,
        },
        events: Vec::new(),
    }];

    let with = engine_performance(&contexts, "mic-1", Some(worker));
    assert_eq!(with.callback_budget_ns, 2_333_333);
    assert_eq!(with.worker_budget_ns, 9_333_333);
    assert_eq!(with.xruns, 5);
    assert_eq!((with.helper_late, with.helper_stalls), (Some(2), Some(1)));
    assert_eq!((with.callback.calls, with.callback.overruns), (7, 2));
    assert_eq!((with.worker_wall.calls, with.worker_wall.max_ns), (6, 300));
    assert_eq!(
        (with.worker_cpu.calls, with.worker_cpu.clock_errors),
        (4, 2)
    );

    // No worker: the shared numbers are still there, the worker's are zero.
    let without = engine_performance(&contexts, "bluetooth", None);
    assert_eq!(
        (without.xruns, without.helper_late, without.helper_stalls),
        (5, Some(2), Some(1))
    );
    assert_eq!((without.callback.calls, without.callback.overruns), (7, 2));
    assert_eq!(
        (without.worker_wall.calls, without.worker_cpu.calls),
        (0, 0)
    );
    assert_eq!(without.worker_budget_ns, 0);

    // A path whose context is gone (a race between inventory and path list)
    // reports what it can rather than nothing.
    let unknown = engine_performance(&[], "mic-1", None);
    assert_eq!((unknown.xruns, unknown.callback.calls), (0, 0));
    assert!(unknown.helper_late.is_some());
}
