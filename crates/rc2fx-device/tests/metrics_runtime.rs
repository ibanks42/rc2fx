#![cfg(unix)]
use rc2fx_core::{Config, EngineMode, Target, TargetKind};
use rc2fx_device::{
    audio::{Chain, Group},
    controller::Controller,
    coordinator::{
        Coordinator, Error, Runtime, RuntimeDescriptor, TargetObservation, WorkerSource,
    },
    metrics::Slot,
    store::Store,
};
use std::time::Duration;

#[test]
fn observation_identity_count_budget_and_latency_are_checked_before_attachment() {
    for case in 0..5 {
        let directory = tempfile::tempdir().unwrap();
        let initial = Config {
            targets: vec![
                Target::new("one", "One", TargetKind::Playback, 2),
                Target::new("two", "Two", TargetKind::Output, 2),
            ],
            selected_targets: vec!["one".into(), "two".into()],
            ..Config::default()
        };
        let mut group = Group::new(
            initial
                .targets
                .iter()
                .map(|t| Chain::new(t, [None, None]).unwrap())
                .collect(),
            1,
        )
        .unwrap();
        let (control, callbacks) = group.split().unwrap();
        let mut slots = [
            Slot::new(if case == 3 { 1 } else { 2_333_333 }, false).unwrap(),
            Slot::new(2_333_333, false).unwrap(),
        ];
        let mut observations: Vec<_> = slots
            .iter_mut()
            .enumerate()
            .map(|(index, slot)| {
                let (_, consumer) = slot.split();
                TargetObservation {
                    target_id: initial.targets[index].id.clone(),
                    consumer,
                    latency_samples: callbacks[index].latency_samples() as u32,
                    worker: WorkerSource::Absent,
                    diagnostics: None,
                }
            })
            .collect();
        match case {
            0 => {
                observations.pop();
            }
            1 => observations[1].target_id = "one".into(),
            2 => observations[1].target_id = "unselected".into(),
            4 => observations[0].latency_samples = u32::MAX,
            _ => {}
        }
        let result = Coordinator::new(
            Controller::new(Store::open(directory.path(), initial).unwrap()),
            Some(Runtime {
                control,
                observations: Some(observations),
                descriptor: RuntimeDescriptor {
                    generation: 1,
                    mode: EngineMode::Simulation,
                    sample_rate: 48_000,
                    block_size: 112,
                    progress_timeout: Duration::from_secs(2),
                },
            }),
        );
        assert!(
            matches!(result, Err(Error::InvalidRuntime)),
            "invalid case {case}"
        );
    }
}
