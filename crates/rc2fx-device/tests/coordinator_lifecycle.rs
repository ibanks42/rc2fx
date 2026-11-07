#![cfg(unix)]
//! Typed owner-fact/transaction gates only. Actual JACK proof is separately
//! exercised by service_generation; these software frames claim no hardware.
use rc2fx_core::{Config, ConfigUpdate, EngineMode, RoutingPhase, Target, TargetKind};
use rc2fx_device::{
    audio::{Chain, Group},
    controller::Controller,
    coordinator::{
        Coordinator, Error, GenerationPhase, GenerationUpdate, Handle, Runtime, RuntimeDescriptor,
    },
    store::Store,
};
use std::time::{Duration, Instant};

fn config() -> Config {
    Config {
        revision: 7,
        targets: vec![Target::new("usb-main", "USB", TargetKind::Playback, 2)],
        selected_targets: vec!["usb-main".into()],
        ..Config::default()
    }
}
fn descriptor(generation: u64) -> RuntimeDescriptor {
    RuntimeDescriptor {
        generation,
        mode: EngineMode::Jack,
        sample_rate: 48_000,
        block_size: 112,
        progress_timeout: Duration::from_secs(2),
    }
}
struct Close(Handle);
impl Drop for Close {
    fn drop(&mut self) {
        self.0.close();
    }
}
fn update(
    handle: &Handle,
    generation: u64,
    phase: GenerationPhase,
) -> Result<rc2fx_core::DeviceState, Error> {
    handle
        .lifecycle(GenerationUpdate { generation, phase })
        .unwrap()
        .blocking_recv()
        .unwrap()
}

#[test]
fn validation_failure_returns_original_locked_store_without_mutation() {
    let directory = tempfile::tempdir().unwrap();
    let config = config();
    let mut group = Group::new(
        vec![Chain::new(&config.targets[0], [None, None]).unwrap()],
        7,
    )
    .unwrap();
    let (control, _callbacks) = group.split().unwrap();
    let controller = Controller::new(Store::open(directory.path(), config.clone()).unwrap());
    let returned = match Coordinator::try_new(
        controller,
        Some(Runtime {
            control,
            observations: None,
            descriptor: descriptor(0),
        }),
    ) {
        Err((Error::InvalidRuntime, controller)) => controller,
        _ => panic!("invalid generation must return Store ownership"),
    };
    assert_eq!(returned.state().config, config);
    assert!(
        Store::open(directory.path(), Config::default()).is_err(),
        "original lock retained"
    );
    drop(returned);
    assert_eq!(
        Store::open(directory.path(), Config::default())
            .unwrap()
            .config(),
        &config
    );
}

#[test]
fn typed_lifecycle_rejects_stale_facts_and_blocks_all_mutations_while_starting() {
    let directory = tempfile::tempdir().unwrap();
    let config = config();
    let mut group = Group::new(
        vec![Chain::new(&config.targets[0], [None, None]).unwrap()],
        7,
    )
    .unwrap();
    let (control, mut callbacks) = group.split().unwrap();
    let (handle, coordinator) = Coordinator::new(
        Controller::new(Store::open(directory.path(), config.clone()).unwrap()),
        Some(Runtime {
            control,
            observations: None,
            descriptor: descriptor(9),
        }),
    )
    .unwrap();
    std::thread::scope(|scope| {
        // Admission is closed before the first explicit Starting fact, not
        // merely after a manager thread happens to enqueue that fact.
        let early_apply = handle
            .apply(ConfigUpdate {
                expected_revision: 7,
                targets: config.targets.clone(),
            })
            .unwrap();
        let early_selection = handle
            .selection(rc2fx_core::SelectionRequest {
                expected_revision: 7,
                expected_generation: 9,
                target_ids: vec![],
            })
            .unwrap();
        let owner = scope.spawn(move || coordinator.run());
        let close = Close(handle.clone());
        assert!(matches!(
            early_apply.blocking_recv().unwrap(),
            Err(Error::RuntimeFault)
        ));
        assert!(matches!(
            early_selection.blocking_recv().unwrap(),
            Err(Error::RuntimeFault)
        ));
        assert!(matches!(
            update(&handle, 8, GenerationPhase::Starting),
            Err(Error::InvalidRuntime)
        ));
        assert!(matches!(
            update(&handle, 9, GenerationPhase::Active),
            Err(Error::InvalidRuntime)
        ));
        let state = update(&handle, 9, GenerationPhase::Starting).unwrap();
        assert_eq!(state.routing.status, RoutingPhase::Starting);
        state.validate().unwrap();
        assert!(matches!(
            handle
                .apply(ConfigUpdate {
                    expected_revision: 7,
                    targets: config.targets.clone(),
                })
                .unwrap()
                .blocking_recv()
                .unwrap(),
            Err(Error::RuntimeFault)
        ));
        assert!(matches!(
            handle
                .create_preset("blocked".into(), 7)
                .unwrap()
                .blocking_recv()
                .unwrap(),
            Err(Error::RuntimeFault)
        ));
        assert!(matches!(
            handle
                .delete_preset("unknown".into())
                .unwrap()
                .blocking_recv()
                .unwrap(),
            Err(Error::RuntimeFault)
        ));
        assert!(matches!(
            handle
                .apply_preset("unknown".into(), 7)
                .unwrap()
                .blocking_recv()
                .unwrap(),
            Err(Error::RuntimeFault)
        ));
        let end = Instant::now() + Duration::from_secs(1);
        loop {
            callbacks[0]
                .process(
                    [&[0.2; 112], &[0.2; 112]],
                    [&mut [0.0; 112], &mut [0.0; 112]],
                )
                .unwrap();
            if handle
                .state()
                .unwrap()
                .blocking_recv()
                .unwrap()
                .unwrap()
                .engine
                .audio_advancing
            {
                break;
            }
            assert!(Instant::now() < end);
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(
            update(&handle, 9, GenerationPhase::Active)
                .unwrap()
                .routing
                .status,
            RoutingPhase::Active
        );
        handle
            .create_preset("allowed".into(), 7)
            .unwrap()
            .blocking_recv()
            .unwrap()
            .unwrap();
        update(
            &handle,
            9,
            GenerationPhase::Fault("specific owner fault".into()),
        )
        .unwrap();
        assert!(matches!(
            update(&handle, 9, GenerationPhase::Active),
            Err(Error::InvalidRuntime)
        ));
        assert!(matches!(
            handle
                .apply(ConfigUpdate {
                    expected_revision: 7,
                    targets: config.targets.clone(),
                })
                .unwrap()
                .blocking_recv()
                .unwrap(),
            Err(Error::RuntimeFault)
        ));
        let final_state = handle.state().unwrap().blocking_recv().unwrap().unwrap();
        assert_eq!(final_state.engine.message, "specific owner fault");
        assert_eq!(final_state.config, config);
        close.0.close();
        let returned = owner.join().unwrap();
        assert_eq!(returned.presets().len(), 1);
    });
}
