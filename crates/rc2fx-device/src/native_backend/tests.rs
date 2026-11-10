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
        atomic::{AtomicBool, AtomicU64, Ordering},
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
    /// Answer every inventory with Busy, as the library does while RØDE's
    /// mixer registers or closes ports.
    busy: Arc<AtomicBool>,
    /// Inventory requests received, answered or refused.
    inventories: Arc<AtomicU64>,
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
        let busy = Arc::new(AtomicBool::new(false));
        let inventories = Arc::new(AtomicU64::new(0));
        let (refusing, counted) = (busy.clone(), inventories.clone());
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
                    Operation::Inventory if refusing.load(Ordering::Acquire) => {
                        counted.fetch_add(1, Ordering::AcqRel);
                        Reply::Error {
                            code: rc2fx_native_control::ErrorCode::Busy,
                        }
                    }
                    Operation::Inventory => {
                        counted.fetch_add(1, Ordering::AcqRel);
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
            busy,
            inventories,
            events,
            task: Some(task),
        }
    }
    fn inventories(&self) -> u64 {
        self.inventories.load(Ordering::Acquire)
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
    fixture_with(Duration::from_secs(2))
}
fn fixture_with(progress_timeout: Duration) -> (tempfile::TempDir, Peer, Controller, Runtime) {
    let config = Config {
        selected_targets: vec!["mic-1".into(), "headphones-2".into()],
        ..Config::default()
    };
    let peer = Peer::new(&config);
    let directory = tempfile::tempdir().unwrap();
    let mut controller = Controller::new(Store::open(directory.path(), config).unwrap());
    let mut runtime =
        Runtime::connect(&peer.path(), controller.config(), progress_timeout).unwrap();
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

/// The idle interval comes from the depth of the native meter ring, which is
/// a constant in another crate. It always leaves the ring its slack, and it
/// stays under the history's one-second sample.
#[test]
fn the_idle_interval_follows_the_meter_ring_and_leaves_it_room() {
    let ms = Duration::from_millis;
    let span = |windows: usize| ms(windows as u64 * METER_WINDOW_FRAMES * 1000 / 48_000);
    assert_eq!(span(8), ms(400));
    assert_eq!(idle_poll_interval(8), ms(250));
    assert_eq!(idle_poll_interval(16), ms(650));
    assert_eq!(idle_poll_interval(32), IDLE_POLL_LIMIT);
    assert_eq!(idle_poll_interval(1024), IDLE_POLL_LIMIT);
    for windows in 0..=64 {
        let idle = idle_poll_interval(windows);
        assert!(
            (POLL_INTERVAL..=IDLE_POLL_LIMIT).contains(&idle),
            "{windows}"
        );
        // Slower than the watched rate only where the ring has the room.
        assert!(
            idle == POLL_INTERVAL || idle + RING_SLACK <= span(windows),
            "{windows}"
        );
    }
    assert_eq!(IDLE_POLL_INTERVAL, idle_poll_interval(METER_RING_WINDOWS));
    assert!(IDLE_POLL_INTERVAL + RING_SLACK <= span(METER_RING_WINDOWS));
    // One read at least between two history samples, a second apart.
    assert!(IDLE_POLL_LIMIT < Duration::from_secs(1));
    // The watched rate is nine reads a second, not ten.
    assert_eq!(
        Duration::from_secs(1).as_millis() / POLL_INTERVAL.as_millis(),
        9
    );
}

#[test]
fn the_idle_interval_is_at_most_half_the_stall_deadline() {
    let ms = Duration::from_millis;
    for (timeout, expected) in [
        (Duration::from_secs(2), IDLE_POLL_INTERVAL),
        (ms(400), ms(200).min(IDLE_POLL_INTERVAL)),
        (ms(300), ms(150).min(IDLE_POLL_INTERVAL)),
        // Never faster than the watched rate.
        (ms(200), POLL_INTERVAL),
    ] {
        let (_directory, _peer, _controller, mut runtime) = fixture_with(timeout);
        assert_eq!(runtime.idle_interval, expected, "{timeout:?}");
        assert!(runtime.idle_interval <= (timeout / 2).max(POLL_INTERVAL));
        // The read schedules what the runtime holds, not the constant.
        runtime.next_poll = Instant::now();
        let before = Instant::now();
        assert!(runtime.poll(|| false).is_some());
        let scheduled = runtime.next_poll.saturating_duration_since(before);
        assert!(scheduled <= expected + ms(50), "{timeout:?}: {scheduled:?}");
        assert_eq!(runtime.idle, expected > POLL_INTERVAL);
    }
}

#[test]
fn a_client_attaching_during_an_idle_wait_is_read_at_once() {
    let (_directory, peer, _controller, mut runtime) = fixture();
    runtime.next_poll = Instant::now();
    assert!(runtime.poll(|| false).is_some());
    assert!(runtime.idle, "a settled unwatched engine waits idle");
    let idle_until = runtime.next_poll;
    let read = peer.inventories();
    // Not due, and still nobody watching: nothing is read.
    assert!(runtime.poll(|| false).is_none());
    assert_eq!(peer.inventories(), read);
    assert_eq!(runtime.next_poll, idle_until);
    // A client subscribes: the engine is read now and the fast rate resumes.
    let before = Instant::now();
    assert!(runtime.poll(|| true).is_some());
    assert_eq!(peer.inventories(), read + 1);
    assert!(!runtime.idle);
    assert!(runtime.next_poll < idle_until);
    assert!(runtime.next_poll >= before + POLL_INTERVAL);
    assert!(runtime.next_poll <= Instant::now() + POLL_INTERVAL);
    // Being watched does not make every pass a read.
    assert!(runtime.poll(|| true).is_none());
    assert_eq!(peer.inventories(), read + 1);
}

#[test]
fn a_busy_engine_after_an_idle_wait_is_not_a_stall() {
    let (_directory, peer, _controller, mut runtime) = fixture();
    let timeout = runtime.progress_timeout;
    runtime.next_poll = Instant::now();
    assert!(runtime.poll(|| false).is_some());
    assert!(runtime.idle);
    // The idle wait has passed, so the last progress seen is that old. The
    // library then answers Busy, as it does while ports are registered.
    let aged = |runtime: &mut Runtime, age: Duration| {
        let now = Instant::now();
        for path in &mut runtime.paths {
            path.last_progress = now - age;
        }
        runtime.next_poll = now;
    };
    peer.busy.store(true, Ordering::Release);
    let idle = runtime.idle_interval;
    aged(&mut runtime, idle);
    let read = peer.inventories();
    assert!(runtime.poll(|| false).is_none(), "Busy is not a fault");
    assert_eq!(peer.inventories(), read + 1);
    // It is asked again at the fast rate, not after another idle wait.
    assert!(!runtime.idle);
    assert!(runtime.next_poll <= Instant::now() + POLL_INTERVAL);
    // Busy is tolerated up to the stall deadline, which the idle wait used
    // at most half of.
    assert!(idle <= timeout / 2);
    aged(&mut runtime, timeout - Duration::from_millis(200));
    assert!(runtime.poll(|| false).is_none());
    aged(&mut runtime, timeout);
    let stalled = runtime.poll(|| false).expect("a real stall is reported");
    assert!(stalled.failure);
    // The engine answers again: one read clears it.
    peer.busy.store(false, Ordering::Release);
    runtime.next_poll = Instant::now();
    let recovered = runtime.poll(|| false).unwrap();
    assert!(!recovered.failure && recovered.advancing);
}

/// The controller's loop with the real event hub, USB session and API. The
/// tests above pass a literal for "someone is watching"; these let the
/// subscription decide.
#[cfg(feature = "native-pipeline")]
mod coordinator_loop {
    use super::*;
    use crate::{
        api::Options,
        coordinator::{Coordinator, Handle},
        usb_control::{Server, testing::*},
    };
    use rc2fx_usb::{Kind, REPORT_SIZE};

    const SESSION: u64 = 0xfeed;

    fn start() -> (tempfile::TempDir, Peer, Handle, Coordinator<'static>) {
        let (directory, peer, controller, runtime) = fixture();
        let (handle, owner) = Coordinator::new_native(controller, runtime).unwrap();
        (directory, peer, handle, owner)
    }
    async fn event(server: &mut Server, id: u32) -> serde_json::Value {
        call(server, SESSION, id, "GET", "api/v1/events").await.body
    }
    fn samples(handle: &Handle) -> Vec<serde_json::Value> {
        let report: serde_json::Value =
            serde_json::from_slice(&handle.diagnostic_report().unwrap()).unwrap();
        report["samples"].as_array().unwrap().clone()
    }

    #[tokio::test(start_paused = true)]
    async fn a_usb_client_that_goes_silent_returns_the_engine_to_idle_polling() {
        let (_directory, peer, handle, mut owner) = start();
        let (socket, _peer_socket) = tokio::net::UnixDatagram::pair().unwrap();
        let mut server = Server::new(handle.clone(), Options::default());
        let mut bytes = [0; REPORT_SIZE + 1];
        // Nobody attached: the first pass reads the engine and waits idle.
        let wait = owner.turn();
        assert!(
            wait > POLL_INTERVAL && wait <= IDLE_POLL_INTERVAL,
            "{wait:?}"
        );
        assert!(owner.native_mut().idle);

        // The app attaches. Its subscription ends the idle wait: the next
        // pass reads the engine without waiting for the idle deadline, and
        // the fast rate is back.
        assert_eq!(answer(&mut server, &hello(SESSION)).await.0, Kind::Ack);
        assert_eq!(event(&mut server, 1).await["event"], "state");
        assert!(handle.has_clients());
        let read = peer.inventories();
        assert!(owner.pass(), "the subscription woke the loop");
        assert_eq!(peer.inventories(), read + 1);
        assert!(!owner.native_mut().idle);
        let wait = owner.turn();
        assert!(wait <= POLL_INTERVAL, "{wait:?}");
        // That read's meter is already waiting for the client.
        assert_eq!(event(&mut server, 2).await["event"], "meter");

        // The app goes away without a word. Five seconds later its
        // subscription is gone, and the next read schedules the idle interval.
        let silent =
            tokio::time::timeout(Duration::from_secs(6), server.receive(&socket, &mut bytes));
        assert!(silent.await.is_err());
        assert!(!handle.has_clients());
        owner.native_mut().next_poll = Instant::now();
        let before = Instant::now();
        let wait = owner.turn();
        assert!(
            wait > POLL_INTERVAL && wait <= IDLE_POLL_INTERVAL,
            "{wait:?}"
        );
        let native = owner.native_mut();
        assert!(native.idle);
        assert!(native.next_poll >= before + IDLE_POLL_INTERVAL);
        assert!(native.next_poll <= Instant::now() + IDLE_POLL_INTERVAL);

        // The app comes back on the same session: it starts with the state,
        // and the engine is read again at once and at the fast rate.
        assert_eq!(event(&mut server, 3).await["event"], "state");
        let read = peer.inventories();
        assert!(owner.pass());
        assert_eq!(peer.inventories(), read + 1);
        assert!(owner.turn() <= POLL_INTERVAL);
        assert_eq!(event(&mut server, 4).await["event"], "meter");
    }

    #[test]
    fn the_loop_sleeps_until_the_next_read_or_sample_not_on_a_tick() {
        let (_directory, peer, handle, mut owner) = start();
        let read = peer.inventories();
        // The first pass reads and samples; the next read is the idle one.
        let wait = owner.turn();
        assert_eq!(peer.inventories(), read + 1);
        assert_eq!(samples(&handle).len(), 1);
        assert!(
            wait > IDLE_POLL_INTERVAL - Duration::from_millis(50),
            "{wait:?}"
        );
        assert!(wait <= IDLE_POLL_INTERVAL, "{wait:?}");
        // A pass before the deadline does no work.
        let again = owner.turn();
        assert!(again <= wait && again > POLL_INTERVAL, "{again:?}");
        assert_eq!(peer.inventories(), read + 1);
        assert_eq!(samples(&handle).len(), 1);
        // With the read far away the loop wakes for the one-second sample.
        let native = owner.native_mut();
        native.next_poll = Instant::now() + Duration::from_secs(30);
        native.idle = false;
        let wait = owner.turn();
        assert!(wait > Duration::from_millis(800), "{wait:?}");
        assert!(wait <= Duration::from_secs(1), "{wait:?}");
        // A read that is due is done on the pass itself.
        owner.native_mut().next_poll = Instant::now();
        owner.turn();
        assert_eq!(peer.inventories(), read + 2);
    }

    #[test]
    fn a_command_ends_the_wait_and_the_read_after_a_save_is_immediate() {
        let (_directory, peer, handle, mut owner) = start();
        owner.turn();
        owner.native_mut().idle_interval = Duration::from_secs(30);
        owner.native_mut().next_poll = Instant::now();
        owner.turn();
        // The loop now has nothing to do before the one-second sample. A
        // command sent while it waits is run at once.
        assert!(owner.native_mut().enabled());
        let sender = handle.clone();
        let command = thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            let state = sender.state().unwrap().blocking_recv().unwrap().unwrap();
            sender
                .routing(RoutingRequest {
                    enabled: false,
                    expected_revision: state.config.revision,
                    expected_generation: state.engine.generation,
                })
                .unwrap()
        });
        let started = Instant::now();
        assert!(owner.pass());
        let waited = started.elapsed();
        // That one pass ran the command: it did not end on a tick before the
        // command came, and it did not sleep on to its deadline after it.
        let mut reply = command.join().unwrap();
        assert!(
            matches!(reply.try_recv(), Ok(Ok(_))),
            "the pass ended without running the command"
        );
        assert!(
            waited < Duration::from_millis(700),
            "the command waited for a deadline: {waited:?}"
        );
        assert!(!owner.native_mut().enabled());
        // The save made the next read due now, long before the idle deadline:
        // the following pass reads the engine.
        let read = peer.inventories();
        owner.turn();
        assert_eq!(peer.inventories(), read + 1);
    }

    #[test]
    fn a_subscription_ends_an_idle_wait_in_progress() {
        let (_directory, peer, handle, mut owner) = start();
        owner.native_mut().idle_interval = Duration::from_secs(30);
        let task = thread::spawn(move || owner.run());
        // The loop has read the engine and sampled, and now sleeps until the
        // sample a second later.
        thread::sleep(Duration::from_millis(150));
        let read = peer.inventories();
        let started = Instant::now();
        let _subscription = handle.subscribe().unwrap();
        while peer.inventories() == read && started.elapsed() < Duration::from_millis(600) {
            thread::sleep(Duration::from_millis(5));
        }
        let waited = started.elapsed();
        handle.close();
        task.join().unwrap();
        assert!(
            waited < Duration::from_millis(600),
            "the new subscriber waited for the loop's next deadline"
        );
    }

    #[test]
    fn closing_admission_ends_the_loop_without_waiting_for_a_deadline() {
        let (_directory, _peer, handle, mut owner) = start();
        owner.native_mut().idle_interval = Duration::from_secs(30);
        let task = thread::spawn(move || owner.run());
        thread::sleep(Duration::from_millis(100));
        let started = Instant::now();
        handle.close();
        let _controller = task.join().unwrap();
        assert!(
            started.elapsed() < Duration::from_millis(700),
            "the loop waited for a deadline: {:?}",
            started.elapsed()
        );
    }

    /// The real loop on its own thread and on the real clock. A subscriber is
    /// read nine times a second, as it was when a 100 ms interval was rounded
    /// up to a 20 ms tick: not ten, and not the idle rate. When its
    /// subscription is dropped, which sends the loop nothing, the next read
    /// falls back to the idle rate by itself. The lower bounds are loose
    /// because a loaded machine reads late; the upper bounds are exact.
    #[test]
    fn the_running_loop_reads_nine_times_a_second_watched_and_at_the_idle_rate_alone() {
        let (_directory, peer, handle, owner) = start();
        let task = thread::spawn(move || owner.run());
        let subscription = handle.subscribe().unwrap();
        thread::sleep(Duration::from_millis(200));
        let window = Duration::from_millis(2200);
        let read = peer.inventories();
        thread::sleep(window);
        let watched = peer.inventories() - read;
        drop(subscription);
        // At most one more fast read finds the subscriber gone.
        thread::sleep(Duration::from_millis(400));
        let read = peer.inventories();
        thread::sleep(window);
        let idle = peer.inventories() - read;
        handle.close();
        task.join().unwrap();
        let most = |interval: Duration| (window.as_millis() / interval.as_millis()) as u64 + 1;
        assert!(
            // Nine a second and no more, whatever the constant says.
            (14..=21).contains(&watched),
            "{watched} watched reads in {window:?}"
        );
        assert!(
            (5..=most(IDLE_POLL_INTERVAL)).contains(&idle),
            "{idle} idle reads in {window:?}"
        );
    }

    /// The history samples once a second and keeps the newest meter window
    /// read since its last sample. With the idle read under a second apart,
    /// every sample has one, at the longest idle interval a deep ring allows.
    #[test]
    fn every_idle_history_sample_has_a_fresh_meter_window() {
        let (_directory, peer, handle, mut owner) = start();
        owner.native_mut().idle_interval = IDLE_POLL_LIMIT;
        let read = peer.inventories();
        let task = thread::spawn(move || owner.run());
        thread::sleep(Duration::from_millis(3300));
        let samples = samples(&handle);
        let reads = peer.inventories() - read;
        handle.close();
        task.join().unwrap();
        assert!(samples.len() >= 3, "{} samples", samples.len());
        for (index, sample) in samples.iter().enumerate() {
            assert_eq!(sample["telemetryStatus"], "fresh", "sample {index}");
            assert!(!sample["telemetry"].is_null(), "sample {index}");
        }
        // Idle the whole time: about one read per 900 ms, never the fast rate.
        assert!((3..=6).contains(&reads), "{reads} reads in 3.3 s");
    }
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
