use super::*;
use crate::{controller::Controller, coordinator::Coordinator, store::Store};
use rc2fx_core::{Config, StockInventory};

fn number(value: usize) -> Vec<u8> {
    let mut bytes = vec![4];
    bytes.extend((value as u32).to_le_bytes());
    bytes
}
fn text(value: &str) -> Vec<u8> {
    let mut bytes = vec![5];
    bytes.extend(value.as_bytes());
    bytes.push(0);
    bytes
}
fn integer(value: i32) -> Vec<u8> {
    let mut bytes = vec![1];
    bytes.extend(value.to_le_bytes());
    bytes
}
fn node(name: &str, properties: Vec<(&str, Vec<u8>)>, children: Vec<Vec<u8>>) -> Vec<u8> {
    let mut bytes = name.as_bytes().to_vec();
    bytes.push(0);
    bytes.extend(number(properties.len()));
    for (key, value) in properties {
        bytes.extend(key.as_bytes());
        bytes.push(0);
        bytes.extend(number(value.len()));
        bytes.extend(value);
    }
    bytes.extend(number(children.len()));
    for child in children {
        bytes.extend(child);
    }
    bytes
}
fn show(name: &str) -> Vec<u8> {
    show_with(name, [3, -1, -1, -1, -1, -1, -1, -1, -1], &[3])
}

fn show_with(name: &str, faders: [i32; 9], sources: &[i32]) -> Vec<u8> {
    let mut children = vec![node(
        "CURRENTSHOW",
        vec![("currentShowName", text(name))],
        vec![],
    )];
    for id in faders {
        children.push(node(
            "CHANNEL",
            vec![("channelInputSource", integer(id))],
            vec![],
        ));
    }
    for &id in sources {
        children.push(node(
            "INPUTSOURCE",
            vec![("inputId", integer(id)), ("inputColor", text("fffdba0e"))],
            vec![],
        ));
    }
    node("Rodecaster", vec![], children)
}

fn selected_mic4_config() -> Config {
    let mut config = Config {
        selected_targets: vec!["mic-1".into()],
        ..Config::default()
    };
    let mic4 = config
        .targets
        .iter_mut()
        .find(|target| target.id == "mic-1")
        .unwrap();
    mic4.highpass.enabled = true;
    mic4.highpass.frequency = 120.0;
    mic4.gain.enabled = true;
    mic4.gain.db = -7.5;
    config.validate().unwrap();
    config
}

fn inventory(reply: Reply) -> StockInventory {
    assert_eq!(reply.status, 200);
    serde_json::from_str(reply.body.get()).unwrap()
}

#[tokio::test]
async fn inputs_read_the_current_show_without_changing_settings() {
    let dir = tempfile::tempdir().unwrap();
    let show_path = dir.path().join("show.bin");
    std::fs::write(&show_path, show("Before")).unwrap();
    let store = Store::open(dir.path().join("settings"), Config::default()).unwrap();
    let (handle, owner) = Coordinator::new(Controller::new(store), None).unwrap();
    let owner = std::thread::spawn(move || owner.run());
    let options = Options {
        stock_config: Some(show_path.clone()),
        catalog: crate::catalog::physical_catalog().to_vec(),
    };
    let mut api = Api::new(handle.clone(), options);
    let inputs = || Request {
        method: "GET".into(),
        path: "api/v1/inputs".into(),
        body: None,
    };
    let before = receive(handle.state()).await.unwrap();
    let read = inventory(api.request(inputs()).await);
    assert!(read.available);
    assert_eq!(read.show_name, "Before");
    assert_eq!(read.inputs[0].target_id, "mic-1");
    std::fs::write(&show_path, show("After")).unwrap();
    assert_eq!(inventory(api.request(inputs()).await).show_name, "After");
    // A real read failure must remain unavailable, not return the prior layout.
    std::fs::write(&show_path, b"partial save").unwrap();
    let failed = inventory(api.request(inputs()).await);
    assert!(!failed.available);
    assert!(failed.inputs.is_empty() && failed.revision.is_empty());
    assert_eq!(receive(handle.state()).await.unwrap(), before);
    handle.close();
    handle.shutdown().await.unwrap();
    owner.join().unwrap();
}

#[tokio::test]
async fn show_reads_never_change_selected_mic4_or_its_effects() {
    let dir = tempfile::tempdir().unwrap();
    let show_path = dir.path().join("show.bin");
    // Start with a successful show in which Mic 4 has moved off fader 1.
    std::fs::write(
        &show_path,
        show_with("Moved", [7, 3, -1, -1, -1, -1, -1, -1, -1], &[7, 3]),
    )
    .unwrap();
    let config = selected_mic4_config();
    let expected = config.clone();
    let store = Store::open(dir.path().join("settings"), config).unwrap();
    let (handle, owner) = Coordinator::new(Controller::new(store), None).unwrap();
    let owner = std::thread::spawn(move || owner.run());
    let options = Options {
        stock_config: Some(show_path.clone()),
        catalog: crate::catalog::physical_catalog().to_vec(),
    };
    let mut api = Api::new(handle.clone(), options);
    let inputs = || Request {
        method: "GET".into(),
        path: "api/v1/inputs".into(),
        body: None,
    };

    let before = receive(handle.state()).await.unwrap();
    assert_eq!(before.config, expected);
    assert_eq!(before.config.selected_targets, ["mic-1"]);
    let mic4 = before
        .config
        .targets
        .iter()
        .find(|target| target.id == "mic-1")
        .unwrap();
    assert!(mic4.highpass.enabled && mic4.highpass.frequency == 120.0);
    assert!(mic4.gain.enabled && mic4.gain.db == -7.5);

    let moved = inventory(api.request(inputs()).await);
    assert!(moved.available, "{}", moved.message);
    assert_eq!(
        moved
            .inputs
            .iter()
            .find(|source| source.source_id == 3)
            .unwrap()
            .target_id,
        "mic-1"
    );
    assert_eq!(
        moved
            .inputs
            .iter()
            .find(|source| source.source_id == 7)
            .unwrap()
            .target_id,
        "usb-main"
    );
    assert_eq!(receive(handle.state()).await.unwrap().config, before.config);

    // Mic 4 is unassigned, and USB 1 keeps fader 1.
    std::fs::write(
        &show_path,
        show_with("Replaced", [7, -1, -1, -1, -1, -1, -1, -1, -1], &[7, 3]),
    )
    .unwrap();
    let replaced = inventory(api.request(inputs()).await);
    assert!(replaced.available, "{}", replaced.message);
    let mic4 = replaced
        .inputs
        .iter()
        .find(|source| source.source_id == 3)
        .unwrap();
    assert_eq!(mic4.fader, None);
    assert_eq!(mic4.target_id, "mic-1");
    assert_eq!(
        replaced
            .inputs
            .iter()
            .find(|source| source.source_id == 7)
            .unwrap()
            .target_id,
        "usb-main"
    );
    assert_eq!(receive(handle.state()).await.unwrap().config, before.config);

    // Mic 4 is absent from INPUTSOURCE entirely; USB 1 keeps fader 1.
    std::fs::write(
        &show_path,
        show_with("Removed", [7, -1, -1, -1, -1, -1, -1, -1, -1], &[7]),
    )
    .unwrap();
    let removed = inventory(api.request(inputs()).await);
    assert!(removed.available, "{}", removed.message);
    assert!(removed.inputs.iter().all(|source| source.source_id != 3));
    assert!(removed
        .inputs
        .iter()
        .all(|source| source.target_id != "mic-1"));
    assert_eq!(
        removed
            .inputs
            .iter()
            .find(|source| source.source_id == 7)
            .unwrap()
            .target_id,
        "usb-main"
    );
    assert_eq!(receive(handle.state()).await.unwrap().config, before.config);

    // A malformed or missing saved show is unavailable, never a settings edit.
    std::fs::write(&show_path, b"partial save").unwrap();
    let partial = inventory(api.request(inputs()).await);
    assert!(!partial.available);
    assert!(partial.inputs.is_empty());
    assert_eq!(receive(handle.state()).await.unwrap().config, before.config);

    std::fs::remove_file(&show_path).unwrap();
    let missing = inventory(api.request(inputs()).await);
    assert!(!missing.available);
    assert!(missing.inputs.is_empty());
    assert_eq!(receive(handle.state()).await.unwrap().config, before.config);

    handle.close();
    handle.shutdown().await.unwrap();
    owner.join().unwrap();
}

#[tokio::test]
async fn cancelled_or_timed_out_blocking_read_keeps_its_permit_until_it_finishes() {
    for cancel in [false, true] {
        let permits = Arc::new(Semaphore::new(1));
        let permit = permits.clone().acquire_owned().await.unwrap();
        let (started, begin) = tokio::sync::oneshot::channel();
        let (release, wait) = std::sync::mpsc::channel();
        // The production helper with an injected stalled reader.
        let task = tokio::spawn(read_inventory(
            tokio::time::Instant::now() + Duration::from_millis(100),
            permit,
            move || {
                let _ = started.send(());
                wait.recv_timeout(Duration::from_secs(3)).unwrap();
                crate::stock_inputs::unavailable("test reader completed")
            },
        ));
        begin.await.unwrap();
        if cancel {
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
        } else {
            assert!(!inventory(task.await.unwrap()).available);
        }
        assert_eq!(permits.available_permits(), 0);
        assert!(permits.clone().try_acquire_owned().is_err());
        release.send(()).unwrap();
        let returned = tokio::time::timeout(Duration::from_secs(1), permits.acquire())
            .await
            .unwrap()
            .unwrap();
        drop(returned);
        assert_eq!(permits.available_permits(), 1);
    }
}

#[tokio::test]
async fn an_expired_request_does_not_start_a_blocking_read() {
    let permits = Arc::new(Semaphore::new(1));
    let permit = permits.clone().acquire_owned().await.unwrap();
    let reply = read_inventory(tokio::time::Instant::now(), permit, || {
        panic!("an expired request must not start filesystem work")
    })
    .await;
    assert!(!inventory(reply).available);
    assert_eq!(permits.available_permits(), 1);
}
