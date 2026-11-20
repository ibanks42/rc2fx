//! Integration tests for bounded command admission and shutdown drain on the
//! store-only coordinator.
use rc2fx_core::{Config, ConfigUpdate};
use rc2fx_device::{
    controller::Controller,
    coordinator::{Coordinator, Error, QUEUED_COMMANDS},
    store::Store,
};
use std::time::Duration;

#[tokio::test]
async fn bounded_admission_canceled_replies_and_shutdown_drain() {
    let directory = tempfile::tempdir().unwrap();
    let initial = Config::default();
    let (handle, coordinator) = Coordinator::new(Controller::new(
        Store::open(directory.path(), initial.clone()).unwrap(),
    ))
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
