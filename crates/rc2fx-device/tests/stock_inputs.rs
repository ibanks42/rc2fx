use rc2fx_core::Config;
use rc2fx_device::stock_inputs::{CatalogChannel, decode, read};

fn count(n: usize) -> Vec<u8> {
    let mut b = vec![4];
    b.extend((n as u32).to_le_bytes());
    b
}
fn text(s: &str) -> Vec<u8> {
    let mut b = vec![5];
    b.extend(s.as_bytes());
    b.push(0);
    b
}
fn int(n: i32) -> Vec<u8> {
    let mut b = vec![1];
    b.extend(n.to_le_bytes());
    b
}
fn node(name: &str, properties: Vec<(&str, Vec<u8>)>, children: Vec<Vec<u8>>) -> Vec<u8> {
    let mut b = name.as_bytes().to_vec();
    b.push(0);
    b.extend(count(properties.len()));
    for (key, value) in properties {
        b.extend(key.as_bytes());
        b.push(0);
        b.extend(count(value.len()));
        b.extend(value);
    }
    b.extend(count(children.len()));
    for child in children {
        b.extend(child);
    }
    b
}
fn fixture(ids: &[i32]) -> Vec<u8> {
    fixture_with_sources(ids, &[0, 3, 4, 5, 6, 7, 8, 9, 10, 11, 16, 17, 18, 19, 20])
}
fn fixture_with_sources(ids: &[i32], sources: &[i32]) -> Vec<u8> {
    let mut children = vec![node(
        "CURRENTSHOW",
        vec![("currentShowName", text("Test show"))],
        vec![],
    )];
    for id in ids {
        children.push(node(
            "CHANNEL",
            vec![("channelInputSource", int(*id))],
            vec![],
        ));
    }
    for &id in sources {
        children.push(node(
            "INPUTSOURCE",
            vec![("inputId", int(id)), ("inputColor", text("fffdba0e"))],
            vec![],
        ));
    }
    node(
        "Rodecaster",
        vec![("private-account", text("do-not-publish"))],
        children,
    )
}
#[test]
fn duo_has_seven_faders_and_a_headset_source_without_pro_ii_aliases() {
    use rc2fx_firmware::Model;
    let sources = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 16, 17, 18, 19, 20];
    let bytes = fixture_with_sources(&[0, 1, 2, 7, 8, 9, 10], &sources);
    assert!(decode(&bytes).is_err(), "Pro II requires nine faders");
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("show");
    std::fs::write(&file, bytes).unwrap();
    let config = rc2fx_device::catalog::default_config(Model::Duo);
    let inventory = read(
        &file,
        &config,
        rc2fx_device::catalog::physical_catalog_for(Model::Duo),
    );
    assert!(inventory.available);
    assert_eq!(
        inventory
            .inputs
            .iter()
            .filter(|s| s.fader.is_some())
            .count(),
        7
    );
    let headset = inventory.inputs.iter().find(|s| s.source_id == 2).unwrap();
    assert_eq!(headset.name, "Headset mic");
    assert_eq!(headset.target_id, "duo-headset-mic");
    assert_eq!(headset.fader, Some(3));
    assert!(
        inventory
            .inputs
            .iter()
            .all(|s| ![3, 5, 6].contains(&s.source_id))
    );
    assert_eq!(
        inventory
            .inputs
            .iter()
            .find(|s| s.source_id == 7)
            .unwrap()
            .target_id,
        "duo-usb-main"
    );
}

#[test]
fn vendor_virtual_source_names_and_unknown_faders_are_model_independent() {
    use rc2fx_firmware::Model;
    let sources = [12, 13, 14, 15, 21, 29, 42];
    for model in Model::ALL {
        let ids = if model == Model::Duo {
            vec![12, 14, 13, 15, 21, 29, 42]
        } else {
            vec![12, 14, 13, 15, 21, 29, 42, -1, -1]
        };
        let inventory = rc2fx_device::stock_inputs::decode_for_model(
            &fixture_with_sources(&ids, &sources),
            model,
        )
        .unwrap();
        assert_eq!(
            inventory
                .inputs
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>(),
            [
                "Game",
                "Virtual A",
                "Music",
                "Virtual B",
                "RCV Channel 1",
                "RCV Channel 9",
                "Fader 7"
            ]
        );
        assert!(inventory.inputs.iter().all(|s| s.target_id.is_empty()));
        let mut moved = ids;
        moved.swap(0, 6);
        let inventory = rc2fx_device::stock_inputs::decode_for_model(
            &fixture_with_sources(&moved, &sources),
            model,
        )
        .unwrap();
        assert_eq!(inventory.inputs[0].source_id, 42);
        assert_eq!(inventory.inputs[0].name, "Fader 1");
        assert_eq!(inventory.inputs[6].source_id, 12);
        assert_eq!(inventory.inputs[6].name, "Game");
    }
}

#[test]
fn virtual_sources_bind_only_their_exact_model_specific_stereo_ports() {
    use rc2fx_firmware::Model;
    for model in Model::ALL {
        let ids = if model == Model::Duo {
            vec![12, 14, 13, 15, 11, -1, -1]
        } else {
            vec![12, 14, 13, 15, 11, -1, -1, -1, -1]
        };
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("show.bin");
        std::fs::write(&file, fixture_with_sources(&ids, &[11, 12, 13, 14, 15])).unwrap();
        let config = rc2fx_device::catalog::default_config(model);
        let catalog = rc2fx_device::catalog::physical_catalog_for(model);
        let inventory = read(&file, &config, catalog);
        assert!(inventory.available);
        let prefix = if model == Model::Duo { "duo-" } else { "" };
        for (source, suffix) in [(12, "game"), (13, "music"), (14, "a"), (15, "b")] {
            let input = inventory
                .inputs
                .iter()
                .find(|s| s.source_id == source)
                .unwrap();
            assert_eq!(input.target_id, format!("{prefix}usb-{suffix}"));
        }
        assert!(
            inventory
                .inputs
                .iter()
                .find(|s| s.source_id == 11)
                .unwrap()
                .target_id
                .is_empty(),
            "SMART Pads are not a USB source"
        );
        let mut missing = catalog.to_vec();
        missing.retain(|(_, ports)| ports[1] != "JUCEJack:in_14");
        let inventory = read(&file, &config, &missing);
        assert!(
            inventory
                .inputs
                .iter()
                .find(|s| s.source_id == 12)
                .unwrap()
                .target_id
                .is_empty(),
            "half of a stereo pair cannot bind"
        );
        std::fs::write(
            &file,
            fixture_with_sources(&vec![-1; ids.len()], &[12, 13, 14, 15]),
        )
        .unwrap();
        let unassigned = read(&file, &config, catalog);
        assert_eq!(unassigned.inputs.len(), 4);
        assert!(
            unassigned
                .inputs
                .iter()
                .all(|s| s.fader.is_none() && !s.target_id.is_empty())
        );
        assert_eq!(
            unassigned
                .inputs
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>(),
            ["Game", "Music", "Virtual A", "Virtual B"]
        );
    }
}

#[test]
fn assigned_targets_follow_physical_coverage_not_faders_or_inventory_target_ids() {
    use rc2fx_device::stock_inputs::{assigned_targets, unavailable};
    use rc2fx_firmware::Model;
    let config = Config::default();
    let catalog = rc2fx_device::catalog::physical_catalog_for(Model::ProIi);
    let show = |ids: &[i32]| {
        decode(&fixture_with_sources(
            ids,
            &[0, 1, 2, 3, 4, 5, 6, 7, 12, 21, 22],
        ))
        .unwrap()
    };
    let source = show(&[3, 7, 12, -1, -1, -1, -1, -1, -1]);
    let expected = vec!["mic-1".to_string(), "usb-main".into(), "usb-game".into()];
    assert_eq!(
        assigned_targets(&source, &config, catalog),
        Some(expected.clone())
    );
    let moved = show(&[12, 3, 7, -1, -1, -1, -1, -1, -1]);
    assert_eq!(assigned_targets(&moved, &config, catalog), Some(expected));
    // Linked Mic 3+4 retains both physical chains. RCV aliases cover existing
    // USB buffers without constructing a second chain over those buffers.
    let linked_aliases = show(&[6, 21, 22, -1, -1, -1, -1, -1, -1]);
    assert_eq!(
        assigned_targets(&linked_aliases, &config, catalog),
        Some(vec![
            "mic-1".into(),
            "analog-3".into(),
            "usb-main".into(),
            "usb-game".into()
        ])
    );
    let empty = show(&[-1; 9]);
    assert_eq!(assigned_targets(&empty, &config, catalog), Some(vec![]));
    assert_eq!(
        assigned_targets(&unavailable("not ready"), &config, catalog),
        None
    );
    let mut invalid = source.clone();
    invalid.inputs[0].fader = Some(20);
    assert_eq!(assigned_targets(&invalid, &config, catalog), None);
    let mut untrusted = source;
    untrusted.inputs[0].target_id = "send-chat".into();
    assert!(
        assigned_targets(&untrusted, &config, catalog)
            .unwrap()
            .iter()
            .all(|id| id != "send-chat")
    );
}

const IDS: [i32; 9] = [3, 7, 9, 8, 10, 11, -1, -1, -1];

const CATALOG: [CatalogChannel<'static>; 3] = [
    (
        "mic-1",
        ["system:capture_4", "JUCEJack:in_4", "unused", "unused"],
    ),
    (
        "usb-main",
        ["usb1in:capture_1", "JUCEJack:in_7", "unused", "unused"],
    ),
    (
        "usb-main",
        ["usb1in:capture_2", "JUCEJack:in_8", "unused", "unused"],
    ),
];

#[test]
fn complete_inventory_is_not_a_claim_of_live_transport_or_fader_assignment() {
    let empty = decode(&fixture(&[-1; 9])).unwrap();
    assert_eq!(empty.inputs.len(), 15);
    assert!(
        empty
            .inputs
            .iter()
            .all(|s| s.fader.is_none() && s.target_id.is_empty())
    );
    for id in [10, 11, 16, 17, 18, 19, 20] {
        assert!(empty.inputs.iter().any(|s| s.source_id == id));
    }
    let mut children: Vec<_> = (0..9)
        .map(|_| node("CHANNEL", vec![("channelInputSource", int(-1))], vec![]))
        .collect();
    for id in 0..31 {
        children.push(node(
            "INPUTSOURCE",
            vec![("inputId", int(id)), ("inputColor", text("ff123456"))],
            vec![],
        ));
    }
    let all_enum_values = decode(&node("Rodecaster", vec![], children)).unwrap();
    assert_eq!(
        all_enum_values
            .inputs
            .iter()
            .map(|s| s.source_id)
            .collect::<Vec<_>>(),
        (0..=20).collect::<Vec<_>>()
    );
    // RCV aliases do not add independent paths. Virtual USB names are known,
    // but an enum record alone still never provisions a processing binding.
    assert!(
        all_enum_values
            .inputs
            .iter()
            .all(|s| s.fader.is_none() && s.target_id.is_empty())
    );
}

#[test]
fn projection_preserves_source_identity_not_fader_and_excludes_private_values() {
    let data = fixture(&IDS);
    let result = decode(&data).unwrap();
    assert_eq!(result.show_name, "Test show");
    assert_eq!(result.inputs.len(), 15);
    assert_eq!(
        result
            .inputs
            .iter()
            .filter(|s| s.fader.is_some())
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>(),
        ["Mic 4", "USB 1", "USB 2", "Chat", "Bluetooth", "SMART Pads"]
    );
    assert_eq!(
        result
            .inputs
            .iter()
            .filter_map(|s| s.fader)
            .collect::<Vec<_>>(),
        [1, 2, 3, 4, 5, 6]
    );
    let public = serde_json::to_string(&result).unwrap();
    assert!(!public.contains("private-account"));
    assert!(!public.contains("do-not-publish"));
    let mut ids = IDS;
    ids.swap(0, 1);
    let moved = decode(&fixture(&ids)).unwrap();
    assert_eq!(moved.inputs[1].id, result.inputs[0].id);
    assert_eq!(moved.inputs[1].fader, Some(2));
    assert_ne!(moved.revision, result.revision);
}

#[test]
fn wireless_and_callme_names_preserve_assigned_faders_without_processing_bindings() {
    // An empty fader is retained as a gap, and the tenth channel is not a fader.
    // IDs 4..6 are linked combo sources, not the wireless/call source IDs.
    let data = fixture(&[20, -1, 16, 19, 18, 17, 4, 5, 6, 7]);
    let result = decode(&data).unwrap();
    let expected = [
        (20, 1, "Wireless 2"),
        (16, 3, "CallMe 1"),
        (19, 4, "Wireless 1"),
        (18, 5, "CallMe 3"),
        (17, 6, "CallMe 2"),
        (4, 7, "Mic 1 + 2"),
        (5, 8, "Mic 2 + 3"),
        (6, 9, "Mic 3 + 4"),
    ];
    assert_eq!(
        result.inputs.iter().filter(|s| s.fader.is_some()).count(),
        expected.len()
    );
    for (input, (id, fader, name)) in result
        .inputs
        .iter()
        .filter(|s| s.fader.is_some())
        .zip(expected)
    {
        assert_eq!(input.source_id, id);
        assert_eq!(input.id, format!("input-{id}"));
        assert_eq!(input.fader, Some(fader));
        assert_eq!(input.name, name);
        assert_eq!(input.color, "fffdba0e");
        assert!(input.target_id.is_empty());
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("show.bin");
    let config = Config::default();
    let before = serde_json::to_vec(&config).unwrap();
    std::fs::write(&path, &data).unwrap();
    // Actual read/binding projection still leaves all these sources unbound.
    let readback = read(&path, &config, &CATALOG);
    assert!(readback.available);
    assert!(
        readback
            .inputs
            .iter()
            .filter(|s| s.fader.is_some())
            .all(|s| s.target_id.is_empty())
    );
    // Unassigned Mic 4 and USB 1 still retain their genuine saved DSP bindings.
    assert_eq!(
        readback
            .inputs
            .iter()
            .find(|s| s.source_id == 3)
            .unwrap()
            .target_id,
        "mic-1"
    );
    assert_eq!(std::fs::read(&path).unwrap(), data);
    assert_eq!(serde_json::to_vec(&config).unwrap(), before);
}

#[test]
fn truncation_ambiguity_and_excess_are_rejected_without_partial_layout() {
    let data = fixture(&IDS);
    for end in 0..data.len() {
        assert!(decode(&data[..end]).is_err(), "truncation {end}");
    }
    let mut trailing = data.clone();
    trailing.push(0);
    assert!(decode(&trailing).is_err());
    assert!(decode(&fixture(&[3, 7, 9])).is_err());
    let mut ids = IDS;
    ids[1] = 3;
    assert!(decode(&fixture(&ids)).is_err());
    ids[1] = 254;
    assert!(decode(&fixture(&ids)).is_err());
    assert!(decode(&vec![0; (2 << 20) + 1]).is_err());
    let duplicate = node("Rodecaster", vec![("x", int(1)), ("x", int(2))], vec![]);
    assert!(decode(&duplicate).is_err());
    let mut nested = node("leaf", vec![], vec![]);
    for _ in 0..34 {
        nested = node("child", vec![], vec![nested]);
    }
    assert!(decode(&node("Rodecaster", vec![], vec![nested])).is_err());
}

#[test]
fn reads_are_fresh_and_only_complete_qualified_physical_pairs_bind() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("show.bin");
    let config = Config::default();
    let before = serde_json::to_vec(&config).unwrap();
    assert!(!read(&path, &config, &CATALOG).available);
    let data = fixture(&IDS);
    std::fs::write(&path, &data).unwrap();
    let result = read(&path, &config, &CATALOG);
    assert!(result.available);
    assert_eq!(result.inputs[0].target_id, "mic-1");
    assert_eq!(result.inputs[1].target_id, "usb-main");
    assert!(result.inputs[2..].iter().all(|s| s.target_id.is_empty()));
    assert!(
        read(&path, &config, &CATALOG[..2]).inputs[1]
            .target_id
            .is_empty()
    );
    let mut reversed = CATALOG;
    reversed.swap(1, 2);
    assert!(
        read(&path, &config, &reversed).inputs[1]
            .target_id
            .is_empty()
    );
    let mut duplicate = CATALOG.to_vec();
    duplicate.push(CATALOG[0]);
    assert!(
        read(&path, &config, &duplicate).inputs[0]
            .target_id
            .is_empty()
    );
    let mut wrong_kind = config.clone();
    wrong_kind
        .targets
        .iter_mut()
        .find(|t| t.id == "usb-main")
        .unwrap()
        .kind = rc2fx_core::TargetKind::Output;
    assert!(
        read(&path, &wrong_kind, &CATALOG).inputs[1]
            .target_id
            .is_empty()
    );
    assert_eq!(std::fs::read(&path).unwrap(), data);
    assert_eq!(serde_json::to_vec(&config).unwrap(), before);
    std::fs::write(&path, b"partial save").unwrap();
    let missing = read(&path, &config, &CATALOG);
    assert!(!missing.available);
    assert!(missing.inputs.is_empty());
    assert!(missing.revision.is_empty());
    missing.validate().unwrap();
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

#[test]
fn mic4_effects_stay_on_combo4_when_its_fader_moves_or_is_replaced() {
    let config = selected_mic4_config();
    let before = serde_json::to_vec(&config).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("show.bin");

    // Mic 4 moves from fader 1 to fader 2; USB 1 takes fader 1.
    std::fs::write(
        &path,
        fixture_with_sources(&[7, 3, -1, -1, -1, -1, -1, -1, -1], &[7, 3]),
    )
    .unwrap();
    let moved = read(&path, &config, &CATALOG);
    assert!(moved.available, "{}", moved.message);
    let mic4 = moved.inputs.iter().find(|s| s.source_id == 3).unwrap();
    assert_eq!(mic4.fader, Some(2));
    assert_eq!(mic4.target_id, "mic-1");
    let usb = moved.inputs.iter().find(|s| s.source_id == 7).unwrap();
    assert_eq!(usb.fader, Some(1));
    assert_eq!(usb.target_id, "usb-main");
    assert_ne!(usb.target_id, mic4.target_id);
    assert_eq!(serde_json::to_vec(&config).unwrap(), before);

    // Mic 4 is no longer on a fader; USB 1 keeps fader 1.
    std::fs::write(
        &path,
        fixture_with_sources(&[7, -1, -1, -1, -1, -1, -1, -1, -1], &[7, 3]),
    )
    .unwrap();
    let replaced = read(&path, &config, &CATALOG);
    assert!(replaced.available, "{}", replaced.message);
    let mic4 = replaced.inputs.iter().find(|s| s.source_id == 3).unwrap();
    assert_eq!(mic4.fader, None);
    assert_eq!(mic4.target_id, "mic-1");
    let usb = replaced.inputs.iter().find(|s| s.source_id == 7).unwrap();
    assert_eq!(usb.fader, Some(1));
    assert_eq!(usb.target_id, "usb-main");
    assert_ne!(usb.target_id, mic4.target_id);
    assert_eq!(serde_json::to_vec(&config).unwrap(), before);
}

#[test]
fn removing_mic4_source_keeps_selected_effects_and_never_moves_them() {
    let config = selected_mic4_config();
    let before = serde_json::to_vec(&config).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("show.bin");

    // USB 1 takes fader 1 and Mic 4 is absent from INPUTSOURCE entirely.
    std::fs::write(
        &path,
        fixture_with_sources(&[7, -1, -1, -1, -1, -1, -1, -1, -1], &[7]),
    )
    .unwrap();
    let removed = read(&path, &config, &CATALOG);
    assert!(removed.available, "{}", removed.message);
    assert!(removed.inputs.iter().all(|s| s.source_id != 3));
    assert!(removed.inputs.iter().all(|s| s.target_id != "mic-1"));
    assert_eq!(
        removed
            .inputs
            .iter()
            .find(|s| s.source_id == 7)
            .unwrap()
            .target_id,
        "usb-main"
    );
    assert_eq!(serde_json::to_vec(&config).unwrap(), before);

    // Incomplete and missing reads do not change the selected effects either.
    std::fs::write(&path, b"partial save").unwrap();
    let partial = read(&path, &config, &CATALOG);
    assert!(!partial.available);
    assert!(partial.inputs.is_empty());
    assert_eq!(serde_json::to_vec(&config).unwrap(), before);

    std::fs::remove_file(&path).unwrap();
    let missing = read(&path, &config, &CATALOG);
    assert!(!missing.available);
    assert!(missing.inputs.is_empty());
    assert_eq!(serde_json::to_vec(&config).unwrap(), before);
}

/// Private firmware capture remains outside source control. No arbitrary stock
/// properties or bytes are printed even if parsing fails.
#[test]
#[ignore = "requires RC2FX_TEST_STOCK_CAPTURE private device capture"]
fn private_capture_projection() {
    let path = std::env::var_os("RC2FX_TEST_STOCK_CAPTURE").expect("set private capture path");
    let bytes = std::fs::read(path).unwrap();
    let result = decode(&bytes).expect("private capture must decode");
    assert_eq!(
        result
            .inputs
            .iter()
            .filter(|input| input.fader.is_some())
            .map(|input| input.source_id)
            .collect::<Vec<_>>(),
        [3, 7, 9, 8, 10, 11]
    );
    result.validate().unwrap();
}

#[cfg(unix)]
#[test]
fn special_files_and_symlinks_do_not_block_or_read_another_file() {
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("show.bin");
    std::fs::write(&target, fixture(&IDS)).unwrap();
    let link = dir.path().join("link");
    symlink(&target, &link).unwrap();
    assert!(!read(&link, &Config::default(), &CATALOG).available);
    let fifo = dir.path().join("fifo");
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
    assert!(!read(&fifo, &Config::default(), &CATALOG).available);
}
