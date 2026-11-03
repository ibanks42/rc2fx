#![cfg(unix)]
use rc2fx_core::{
    Config, ConfigUpdate, EngineMode, EnginePhase, SCHEMA_VERSION, Target, TargetKind,
};
use rc2fx_device::{
    audio::{Chain, Group},
    controller::Controller,
    coordinator::{Coordinator, Error, QUEUED_COMMANDS, Runtime, RuntimeDescriptor, receive},
    store::Store,
};
use std::time::Duration;

fn config() -> Config {
    Config {
        schema_version: SCHEMA_VERSION,
        revision: 1,
        selected_targets: vec!["usb-main".into()],
        targets: vec![Target::new("usb-main", "USB", TargetKind::Playback, 2)],

    }
}
fn descriptor() -> RuntimeDescriptor {
    RuntimeDescriptor {
        generation: 1,
        mode: EngineMode::Simulation,
        sample_rate: 48_000,
        block_size: 112,
        progress_timeout: Duration::from_secs(2),
    }
}

#[tokio::test]
async fn bounded_admission_cancelled_replies_and_shutdown_drain() {
    let directory = tempfile::tempdir().unwrap();
    let initial = Config::default();
    let (handle, coordinator) = Coordinator::new(
        Controller::new(Store::open(directory.path(), initial.clone()).unwrap()),
        None,
    )
    .unwrap();
    // Enqueue before starting the ordinary owner thread, deterministically
    // filling the queue without disk sleeps or an unbounded executor backlog.
    let subscription = handle.subscribe().unwrap();
    for revision in 1..=QUEUED_COMMANDS as u64 {
        let mut targets = initial.targets.clone();
        targets[0].gain.db = -(revision as f64);
        let abandoned = handle
            .apply(ConfigUpdate {
                expected_revision: revision,
                targets,

            })
            .unwrap();
        drop(abandoned);
    }
    // Reads and subscriptions remain responsive even with a full write queue.
    assert_eq!(
        handle
            .state()
            .unwrap()
            .await
            .unwrap()
            .unwrap()
            .config
            .revision,
        1
    );
    assert!(matches!(
        handle.apply(ConfigUpdate {
            expected_revision: 99,
            targets: initial.targets.clone(),
        }),
        Err(Error::Busy)
    ));
    handle.close();
    assert!(matches!(
        handle.apply(ConfigUpdate {
            expected_revision: 1,
            targets: initial.targets.clone(),
        }),
        Err(Error::Stopped)
    ));
    let owner = std::thread::spawn(move || coordinator.run());
    tokio::time::timeout(Duration::from_secs(5), handle.shutdown())
        .await
        .unwrap()
        .unwrap();
    let mut subscription = subscription.await.unwrap().unwrap();
    assert!(
        subscription.next().await.is_none(),
        "shutdown disconnects subscribers, including its queued state"
    );
    let stopped = owner.join().unwrap();
    assert_eq!(stopped.state().config.revision, QUEUED_COMMANDS as u64 + 1);
    assert_eq!(
        stopped.state().config.targets[0].gain.db,
        -(QUEUED_COMMANDS as f64)
    );
    drop(stopped);
    let reopened = Store::open(directory.path(), initial).unwrap();
    assert_eq!(reopened.config().revision, QUEUED_COMMANDS as u64 + 1);
}

#[test]
fn runtime_descriptor_rejects_mismatched_storage_or_allocation() {
    for case in 0..8 {
        let directory = tempfile::tempdir().unwrap();
        let mut initial = config();
        let group_revision = if case == 0 { 2 } else { 1 };
        let mut runtime = descriptor();
        match case {
            1 => runtime.generation = 0,
            2 => runtime.sample_rate = 44_100,
            3 => runtime.block_size = 0,
            4 => runtime.mode = EngineMode::Offline,
            5 => runtime.progress_timeout = Duration::ZERO,
            6 => initial.selected_targets.clear(),
            7 => initial.selected_targets = vec!["other".into()],
            _ => {}
        }
        if case == 7 {
            initial
                .targets
                .push(Target::new("other", "Other", TargetKind::Output, 2));
        }
        let mut group = Group::new(
            vec![Chain::new(&initial.targets[0], [None, None]).unwrap()],
            group_revision,
        )
        .unwrap();
        let (control, _) = group.split().unwrap();
        let controller = Controller::new(Store::open(directory.path(), initial).unwrap());
        assert!(matches!(
            Coordinator::new(
                controller,
                Some(Runtime {
                    control,
                    descriptor: runtime,
                    observations: None,
                })
            ),
            Err(Error::InvalidRuntime)
        ));
    }
}

#[test]
fn no_progress_fault_is_latched_and_never_claims_hardware_routes() {
    let directory = tempfile::tempdir().unwrap();
    let initial = config();
    let mut group = Group::new(
        vec![Chain::new(&initial.targets[0], [None, None]).unwrap()],
        1,
    )
    .unwrap();
    let (control, mut callbacks) = group.split().unwrap();
    let mut desc = descriptor();
    desc.progress_timeout = Duration::from_millis(40);
    let (handle, coordinator) = Coordinator::new(
        Controller::new(Store::open(directory.path(), initial.clone()).unwrap()),
        Some(Runtime {
            control,
            descriptor: desc,
            observations: None,
        }),
    )
    .unwrap();
    std::thread::scope(|scope| {
        let owner = scope.spawn(move || coordinator.run());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    let state = receive(handle.state()).await.unwrap();
                    if state.engine.status == EnginePhase::Error {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .unwrap();
            callbacks[0]
                .process([&[0.2], &[0.2]], [&mut [0.0], &mut [0.0]])
                .unwrap();
            tokio::time::sleep(Duration::from_millis(25)).await;
            let state = receive(handle.state()).await.unwrap();
            assert_eq!(state.engine.status, EnginePhase::Error);
            assert!(!state.engine.audio_advancing);
            assert_eq!(state.routing.status, rc2fx_core::RoutingPhase::Unmanaged);
            assert!(!state.is_applied());
            assert!(matches!(
                receive(handle.apply(ConfigUpdate {
                    expected_revision: 1,
                    targets: initial.targets.clone(),
                }))
                .await,
                Err(Error::RuntimeFault)
            ));
            assert_eq!(receive(handle.state()).await.unwrap().config.revision, 1);
            handle.shutdown().await.unwrap();
        });
        drop(owner.join().unwrap());
    });
}

#[test]
fn sustained_underruns_fault_even_when_audio_callbacks_keep_advancing() {
    use rc2fx_device::{
        coordinator::{TargetObservation, WorkerSource},
        diagnostics,
        metrics::Slot,
    };
    use std::time::Instant;
    let directory = tempfile::tempdir().unwrap();
    let initial = config();
    let mut group = Group::new(
        vec![Chain::new(&initial.targets[0], [None, None]).unwrap()],
        1,
    )
    .unwrap();
    let (control, mut callbacks) = group.split().unwrap();
    let mut slot = Slot::new(112 * 1_000_000_000 / 48_000, false).unwrap();
    let (_producer, consumer) = slot.split();
    let (faults, reader) = diagnostics::channel();
    let mut desc = descriptor();
    desc.mode = EngineMode::Jack;
    let (handle, coordinator) = Coordinator::new(
        Controller::new(Store::open(directory.path(), initial.clone()).unwrap()),
        Some(Runtime {
            control,
            descriptor: desc,
            observations: Some(vec![TargetObservation {
                target_id: "usb-main".into(),
                consumer,
                latency_samples: 0,
                worker: WorkerSource::Absent,
                diagnostics: Some(reader),
            }]),
        }),
    )
    .unwrap();
    std::thread::scope(|scope| {
        let owner = scope.spawn(move || coordinator.run());
        struct Close(rc2fx_device::coordinator::Handle);
        impl Drop for Close {
            fn drop(&mut self) {
                self.0.close();
            }
        }
        let close = Close(handle.clone());
        let start = Instant::now();
        loop {
            callbacks[0]
                .process([&[0.2], &[0.2]], [&mut [0.0], &mut [0.0]])
                .unwrap();
            for _ in 0..4 {
                faults.xrun();
            }
            let state = handle.state().unwrap().blocking_recv().unwrap().unwrap();
            if state.engine.status == EnginePhase::Error {
                assert!(state.engine.message.contains("Sustained audio overload"));
                assert!(!state.engine.audio_advancing);
                assert_eq!(state.config, initial);
                break;
            }
            assert!(
                start.elapsed() < Duration::from_secs(6),
                "overload did not fault"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        drop(close);
        let controller = owner.join().unwrap();
        assert_eq!(controller.state().config, initial);
    });
}
