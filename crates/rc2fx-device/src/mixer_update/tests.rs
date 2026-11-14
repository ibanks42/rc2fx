use super::*;
use aws_lc_rs::{rand::SystemRandom, signature::Ed25519KeyPair};
use serde_json::json;
use std::{collections::HashMap, fs, num::NonZeroU64, path::PathBuf, sync::Arc};
use tough::{
    editor::{RepositoryEditor, signed::SignedRole},
    key_source::{KeySource, LocalKeySource},
    schema::{KeyHolder, RoleKeys, RoleType, Root},
};

const CATALOG: &str = include_str!("../../../../config/firmware-profiles.json");
const EXPIRES: &str = "2099-01-01T00:00:00Z";

fn block<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}

fn nonzero(value: u64) -> NonZeroU64 {
    NonZeroU64::new(value).unwrap()
}

fn qualified_profiles() -> Vec<String> {
    let catalog = rc2fx_firmware::Catalog::parse(CATALOG).unwrap();
    let mut ids: Vec<String> = catalog
        .profiles
        .iter()
        .filter(|profile| verify::qualified(profile))
        .map(|profile| profile.id.clone())
        .collect();
    ids.sort();
    ids
}

fn pro_ii_profile() -> String {
    let catalog = rc2fx_firmware::Catalog::parse(CATALOG).unwrap();
    catalog
        .profiles
        .iter()
        .find(|p| p.model == rc2fx_firmware::Model::ProIi.id() && verify::qualified(p))
        .map(|p| p.id.clone())
        .expect("the shipped catalog has a qualified Pro II profile")
}

/// A signed test repository in a temporary directory. Its keys are generated
/// per test and never leave the directory.
struct Repository {
    dir: tempfile::TempDir,
    root: Vec<u8>,
    key: PathBuf,
    metadata: verify::MixerTarget,
    bytes: Vec<u8>,
}

impl Repository {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let key = dir.path().join("test-only-key.pk8");
        fs::write(
            &key,
            Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
                .unwrap()
                .as_ref(),
        )
        .unwrap();
        let source = LocalKeySource { path: key.clone() };
        let public = source.as_sign().await.unwrap().tuf_key();
        let id = public.key_id().unwrap();
        let roles = [
            RoleType::Root,
            RoleType::Targets,
            RoleType::Snapshot,
            RoleType::Timestamp,
        ]
        .into_iter()
        .map(|role| {
            (
                role,
                RoleKeys {
                    keyids: vec![id.clone()],
                    threshold: nonzero(1),
                    _extra: HashMap::new(),
                },
            )
        })
        .collect();
        let root = Root {
            spec_version: "1.0.1".into(),
            consistent_snapshot: false,
            version: nonzero(1),
            expires: EXPIRES.parse().unwrap(),
            keys: HashMap::from([(id, public)]),
            roles,
            _extra: HashMap::new(),
        };
        let signed = SignedRole::new(
            root.clone(),
            &KeyHolder::Root(root),
            &[Box::new(source)],
            &SystemRandom::new(),
        )
        .await
        .unwrap();
        let root = signed.buffer().clone();
        fs::write(dir.path().join("root.json"), &root).unwrap();
        let metadata = verify::MixerTarget {
            version: "0.2.0".into(),
            payload_format: 1,
            protocol_version: 1,
            minimum_app_version: "0.1.0".into(),
            firmware_profiles: qualified_profiles(),
        };
        Self {
            dir,
            root,
            key,
            metadata,
            bytes: b"the signed mixer payload".to_vec(),
        }
    }

    async fn publish(&self, sequence: u64) {
        let path = self.dir.path().join("targets").join("payload");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, &self.bytes).unwrap();
        let (_, mut target) = RepositoryEditor::build_target(&path).await.unwrap();
        target.custom.insert(
            "rc2fxMixer".into(),
            serde_json::to_value(&self.metadata).unwrap(),
        );
        let mut editor = RepositoryEditor::new(self.dir.path().join("root.json"))
            .await
            .unwrap();
        let expires: jiff::Timestamp = EXPIRES.parse().unwrap();
        editor.add_target("payload", target).unwrap();
        editor
            .targets_version(nonzero(sequence))
            .unwrap()
            .targets_expires(expires)
            .unwrap()
            .snapshot_version(nonzero(sequence))
            .snapshot_expires(expires)
            .timestamp_version(nonzero(sequence))
            .timestamp_expires(expires);
        let repository = editor
            .sign(&[Box::new(LocalKeySource {
                path: self.key.clone(),
            })])
            .await
            .unwrap();
        repository
            .write(self.dir.path().join("metadata"))
            .await
            .unwrap();
    }

    fn input(&self) -> verify::MetadataInput {
        let dir = self.dir.path().join("metadata");
        let read = |name: &str| fs::read_to_string(dir.join(name)).unwrap();
        verify::MetadataInput {
            root: vec![String::from_utf8(self.root.clone()).unwrap()],
            timestamp: read("timestamp.json"),
            snapshot: read("snapshot.json"),
            targets: read("targets.json"),
        }
    }

    fn payload(&self) -> PathBuf {
        let path = self.dir.path().join("upload");
        fs::write(&path, &self.bytes).unwrap();
        path
    }

    fn request(&self) -> verify::ReleaseRequest {
        verify::ReleaseRequest {
            version: self.metadata.version.clone(),
            size: self.bytes.len() as u64,
            sha256: install::sum(&self.bytes),
            allow_downgrade: false,
        }
    }
}

fn verify_release(
    repository: &Repository,
    profile: Option<&str>,
    prior: &install::StoredState,
) -> Result<verify::VerifiedRelease, String> {
    let work = repository.dir.path().join("work");
    fs::create_dir_all(&work).unwrap();
    verify::verify(
        &repository.root,
        &repository.input(),
        &repository.request(),
        &repository.payload(),
        profile,
        Some("0.1.5"),
        prior,
        &work,
    )
}

#[test]
fn a_signed_release_is_accepted_on_the_mixer() {
    let repository = block(Repository::new());
    block(repository.publish(1));
    let profile = pro_ii_profile();
    let verified = verify_release(&repository, Some(&profile), &Default::default()).unwrap();
    assert_eq!(verified.version, "0.2.0");
    assert_eq!(verified.target, "payload");
    assert_eq!(verified.from.as_deref(), Some("0.1.5"));
    assert_eq!(verified.metadata_versions.targets_version, 1);
    assert_eq!(verified.metadata_versions.snapshot_version, 1);
    assert_eq!(verified.metadata_versions.timestamp_version, 1);
    assert_eq!(verified.metadata_versions.root_version, 1);
    assert_eq!(verified.metadata_versions.targets_digest.len(), 64);
    // The same release checked again, as after a refused or retried install:
    // the same signed bytes are the same targets, never "changed".
    for _ in 0..3 {
        let again =
            verify_release(&repository, Some(&profile), &verified.metadata_versions).unwrap();
        assert_eq!(
            again.metadata_versions.targets_digest,
            verified.metadata_versions.targets_digest
        );
    }
}

#[test]
fn tampered_or_replaced_signed_metadata_is_refused() {
    let repository = block(Repository::new());
    block(repository.publish(1));
    let profile = pro_ii_profile();
    // A changed length without a new signature: the signature no longer holds.
    let mut input = repository.input();
    let mut targets: serde_json::Value = serde_json::from_str(&input.targets).unwrap();
    targets["signed"]["targets"]["payload"]["length"] = json!(1);
    input.targets = targets.to_string();
    let work = repository.dir.path().join("tampered");
    fs::create_dir_all(&work).unwrap();
    let error = verify::verify(
        &repository.root,
        &input,
        &repository.request(),
        &repository.payload(),
        Some(&profile),
        Some("0.1.5"),
        &Default::default(),
        &work,
    )
    .unwrap_err();
    assert!(error.contains("refused"), "{error}");
    // Another tree's root cannot replace the one compiled in.
    let impostor = block(Repository::new());
    block(impostor.publish(1));
    let mut input = repository.input();
    input.root = vec![String::from_utf8(impostor.root.clone()).unwrap()];
    input.timestamp = impostor.input().timestamp;
    input.snapshot = impostor.input().snapshot;
    input.targets = impostor.input().targets;
    let error = verify::verify(
        &repository.root,
        &input,
        &repository.request(),
        &repository.payload(),
        Some(&profile),
        Some("0.1.5"),
        &Default::default(),
        &work,
    )
    .unwrap_err();
    assert!(error.contains("refused"), "{error}");
}

#[test]
fn a_payload_that_differs_from_its_signature_is_refused() {
    let repository = block(Repository::new());
    block(repository.publish(1));
    let profile = pro_ii_profile();
    let work = repository.dir.path().join("wrong");
    fs::create_dir_all(&work).unwrap();
    // Same length, different bytes.
    let path = repository.dir.path().join("swapped");
    let mut bytes = repository.bytes.clone();
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    fs::write(&path, &bytes).unwrap();
    let error = verify::verify(
        &repository.root,
        &repository.input(),
        &repository.request(),
        &path,
        Some(&profile),
        Some("0.1.5"),
        &Default::default(),
        &work,
    )
    .unwrap_err();
    assert!(error.contains("does not match"), "{error}");
    // A different length.
    let path = repository.dir.path().join("longer");
    let mut bytes = repository.bytes.clone();
    bytes.push(0);
    fs::write(&path, &bytes).unwrap();
    let error = verify::verify(
        &repository.root,
        &repository.input(),
        &repository.request(),
        &path,
        Some(&profile),
        Some("0.1.5"),
        &Default::default(),
        &work,
    )
    .unwrap_err();
    assert!(error.contains("bytes"), "{error}");
}

#[test]
fn older_metadata_is_refused_without_relying_on_the_mixers_clock() {
    let repository = block(Repository::new());
    block(repository.publish(2));
    let profile = pro_ii_profile();
    let prior = install::StoredState {
        schema: 1,
        installed_version: Some("0.1.5".into()),
        targets_version: 2,
        snapshot_version: 2,
        timestamp_version: 2,
        targets_digest: String::new(),
        ..Default::default()
    };
    let verified = verify_release(&repository, Some(&profile), &prior).unwrap();
    assert_eq!(verified.metadata_versions.targets_version, 2);
    // A newer timestamp but older targets is still a rollback.
    let prior = install::StoredState {
        targets_version: 3,
        ..Default::default()
    };
    let error = verify_release(&repository, Some(&profile), &prior).unwrap_err();
    assert!(error.contains("older"), "{error}");
    // The same targets version with different content is refused too.
    let mut prior = install::StoredState {
        targets_version: 2,
        snapshot_version: 2,
        timestamp_version: 2,
        ..Default::default()
    };
    prior.targets_digest = "0".repeat(64);
    let error = verify_release(&repository, Some(&profile), &prior).unwrap_err();
    assert!(error.contains("changed without a new version"), "{error}");
}

#[test]
fn an_older_release_needs_an_explicit_downgrade() {
    let repository = block(Repository::new());
    block(repository.publish(1));
    let profile = pro_ii_profile();
    let prior = install::StoredState {
        installed_version: Some("0.3.0".into()),
        ..Default::default()
    };
    let error = verify_release(&repository, Some(&profile), &prior).unwrap_err();
    assert!(error.contains("older than the installed one"), "{error}");
    let work = repository.dir.path().join("downgrade");
    fs::create_dir_all(&work).unwrap();
    let mut request = repository.request();
    request.allow_downgrade = true;
    let verified = verify::verify(
        &repository.root,
        &repository.input(),
        &request,
        &repository.payload(),
        Some(&profile),
        Some("0.1.5"),
        &prior,
        &work,
    )
    .unwrap();
    assert_eq!(verified.version, "0.2.0");
    // The same release again is not an update.
    let prior = install::StoredState {
        installed_version: Some("0.2.0".into()),
        ..Default::default()
    };
    let error = verify_release(&repository, Some(&profile), &prior).unwrap_err();
    assert!(error.contains("already installed"), "{error}");
}

#[test]
fn a_release_for_another_firmware_is_refused() {
    let repository = block(Repository::new());
    block(repository.publish(1));
    let error = verify_release(
        &repository,
        Some("another-mixer-9.9.9"),
        &Default::default(),
    )
    .unwrap_err();
    assert!(error.contains("does not support"), "{error}");
    let error = verify_release(&repository, None, &Default::default()).unwrap_err();
    assert!(error.contains("profile is unknown"), "{error}");
}

// ---- upload sessions -------------------------------------------------------

fn test_paths() -> (tempfile::TempDir, install::Paths) {
    let dir = tempfile::tempdir().unwrap();
    let paths = install::Paths {
        opt: dir.path().join("opt"),
        run: dir.path().join("run"),
        shims: dir.path().join("shims"),
        stock_mixer: dir.path().join("stock-mixer"),
        audio_launcher: dir.path().join("audio-start.sh"),
    };
    fs::create_dir_all(&paths.opt).unwrap();
    fs::create_dir_all(&paths.shims).unwrap();
    fs::create_dir_all(&paths.run).unwrap();
    fs::write(&paths.stock_mixer, b"stock").unwrap();
    fs::write(&paths.audio_launcher, b"launcher").unwrap();
    (dir, paths)
}

fn new_update(paths: install::Paths) -> Arc<MixerUpdate> {
    Arc::new(MixerUpdate::new(
        paths,
        Some(pro_ii_profile()),
        Some("0.1.5".into()),
    ))
}

fn start_request(size: u64) -> StartRequest {
    StartRequest {
        version: "0.2.0".into(),
        size,
        sha256: "0".repeat(64),
        metadata: verify::MetadataInput {
            root: vec!["{}".into()],
            timestamp: "{}".into(),
            snapshot: "{}".into(),
            targets: "{}".into(),
        },
        allow_downgrade: false,
    }
}

#[test]
fn uploads_append_at_offsets_and_resume_after_a_lost_reply() {
    let (_dir, paths) = test_paths();
    let update = new_update(paths);
    let started = update.start(start_request(9)).unwrap();
    assert_eq!(started.state, "accepting");
    assert_eq!(started.id.len(), 16);
    let base64 = |bytes: &[u8]| crate::mixer_update::encode_base64_for_test(bytes);

    let first = update
        .append(
            &started.id,
            Chunk {
                offset: 0,
                data: base64(b"abcd"),
            },
        )
        .unwrap();
    assert_eq!(first.received, 4);
    assert_eq!(first.state, "accepting");
    // A lost reply: the same bytes at the same offset are accepted again.
    let replay = update
        .append(
            &started.id,
            Chunk {
                offset: 0,
                data: base64(b"abcd"),
            },
        )
        .unwrap();
    assert_eq!(replay.received, 4);
    // A different chunk where the upload already is: resume point returned.
    let error = update
        .append(
            &started.id,
            Chunk {
                offset: 0,
                data: base64(b"zzzz"),
            },
        )
        .unwrap_err();
    assert_eq!(error, UpdateError::Offset { expected: 4 });
    // Skipping ahead is refused the same way.
    let error = update
        .append(
            &started.id,
            Chunk {
                offset: 6,
                data: base64(b"abc"),
            },
        )
        .unwrap_err();
    assert_eq!(error, UpdateError::Offset { expected: 4 });
    let last = update
        .append(
            &started.id,
            Chunk {
                offset: 4,
                data: base64(b"efghi"),
            },
        )
        .unwrap();
    assert_eq!(last.received, 9);
    assert_eq!(last.state, "complete");
    // Past the declared size is refused.
    let error = update
        .append(
            &started.id,
            Chunk {
                offset: 9,
                data: base64(b"x"),
            },
        )
        .unwrap_err();
    assert!(matches!(error, UpdateError::Conflict(_)), "{error}");
    update.abort(&started.id).unwrap();
    assert_eq!(
        update.progress(&started.id).unwrap_err(),
        UpdateError::NotFound
    );
}

#[test]
fn only_the_session_start_handed_out_is_accepted() {
    let (_dir, paths) = test_paths();
    let update = new_update(paths);
    let started = update.start(start_request(9)).unwrap();
    let other = if started.id == "0000000000000000" {
        "1111111111111111"
    } else {
        "0000000000000000"
    };
    let chunk = || Chunk {
        offset: 0,
        data: crate::mixer_update::encode_base64_for_test(b"abcd"),
    };
    assert!(matches!(
        update.append(other, chunk()),
        Err(UpdateError::NotFound)
    ));
    assert!(matches!(update.progress(other), Err(UpdateError::NotFound)));
    assert!(matches!(update.abort(other), Err(UpdateError::NotFound)));
    // The real session is untouched by the wrong ids.
    assert_eq!(update.append(&started.id, chunk()).unwrap().received, 4);
}

#[test]
fn an_update_waits_for_a_developer_install_and_keeps_its_own_marker() {
    let (_dir, paths) = test_paths();
    // install.sh's transaction marker is not this updater's: a launcher must
    // never read a developer install as an interrupted in-place update.
    assert_ne!(paths.pending(), paths.install_pending());
    fs::write(paths.install_pending(), b"").unwrap();
    let update = new_update(paths.clone());
    assert!(matches!(
        update.start(start_request(9)),
        Err(UpdateError::Busy(_))
    ));
    fs::remove_file(paths.install_pending()).unwrap();
    assert!(update.start(start_request(9)).is_ok());
}

#[test]
fn a_new_upload_replaces_one_that_was_abandoned() {
    let (_dir, paths) = test_paths();
    let update = new_update(paths.clone());
    let abandoned = update.start(start_request(9)).unwrap();
    update
        .append(
            &abandoned.id,
            Chunk {
                offset: 0,
                data: crate::mixer_update::encode_base64_for_test(b"abcd"),
            },
        )
        .unwrap();
    // The app closed mid-upload and starts over: the old session and its
    // bytes in RAM go, and the new one starts from zero.
    let next = update.start(start_request(9)).unwrap();
    assert_ne!(next.id, abandoned.id);
    assert_eq!(next.received, 0);
    assert!(!paths.run.join(&abandoned.id).exists());
    assert!(matches!(
        update.progress(&abandoned.id),
        Err(UpdateError::NotFound)
    ));
}

#[test]
fn the_size_cap_and_chunk_limit_are_enforced() {
    let (_dir, paths) = test_paths();
    let update = new_update(paths);
    assert!(matches!(
        update.start(start_request(0)),
        Err(UpdateError::Invalid(_))
    ));
    assert!(matches!(
        update.start(start_request(MAX_PAYLOAD + 1)),
        Err(UpdateError::Invalid(_))
    ));
    let started = update.start(start_request(MAX_CHUNK as u64 + 1)).unwrap();
    let data = crate::mixer_update::encode_base64_for_test(&vec![0u8; MAX_CHUNK + 1]);
    let error = update
        .append(&started.id, Chunk { offset: 0, data })
        .unwrap_err();
    assert!(matches!(error, UpdateError::Invalid(_)), "{error}");
    for bad in ["", "abc", "%%%%", "AAAA=A=="] {
        let error = update
            .append(
                &started.id,
                Chunk {
                    offset: 0,
                    data: bad.into(),
                },
            )
            .unwrap_err();
        assert!(matches!(error, UpdateError::Invalid(_)), "{bad}: {error}");
    }
}

// ---- install, swap, commit and rollback -----------------------------------

fn bundle_files(tag: &str) -> Vec<(String, Vec<u8>)> {
    let mut files: Vec<(String, Vec<u8>)> = install::PAYLOAD_NAMES
        .iter()
        .map(|name| {
            let data = match *name {
                "rc2fx-firmware" => format!(
                    "#!/bin/sh\necho {0} /var/volatile/show.json\n",
                    pro_ii_profile()
                )
                .into_bytes(),
                "rc2fx.sh" => b"#!/bin/sh\nrc2fx_build=release\n  launch) : ;;\n  gui-launch) : ;;\n  update-commit) : ;;\n"
                    .to_vec(),
                _ => format!("{tag} {name}\n").into_bytes(),
            };
            ((*name).to_owned(), data)
        })
        .collect();
    let sums: String = install::PINNED
        .iter()
        .map(|name| {
            let data = &files.iter().find(|(n, _)| n.as_str() == *name).unwrap().1;
            format!("{}  {name}\n", install::sum(data))
        })
        .collect();
    files.iter_mut().find(|(n, _)| n == "SHA256SUMS").unwrap().1 = sums.into_bytes();
    files
}

fn write_bundle(paths: &install::Paths, files: &[(String, Vec<u8>)]) {
    let bundle = paths.bundle();
    fs::create_dir_all(bundle.join("data")).unwrap();
    fs::create_dir_all(bundle.join("logs")).unwrap();
    fs::write(bundle.join("data").join("state.json"), b"{\"revision\":7}").unwrap();
    for (name, data) in files {
        fs::write(bundle.join(name), data).unwrap();
    }
}

#[test]
fn a_swap_keeps_settings_and_a_rollback_copy() {
    let (_dir, paths) = test_paths();
    let old = bundle_files("old");
    write_bundle(&paths, &old);
    let new = bundle_files("new");
    // The same GUI shim as the new bundle already installed: no reboot note.
    fs::write(
        paths.shims.join("rc_gui_juce"),
        new.iter()
            .find(|(n, _)| n == "rc2fx-gui-shim.sh")
            .unwrap()
            .1
            .clone(),
    )
    .unwrap();
    let staged = install::stage(&paths, "aaaaaaaaaaaaaaaa", &new).unwrap();
    let swapped = install::swap(&paths, &staged, "0.1.5", "0.2.0").unwrap();
    assert!(!swapped.gui_shim_changed);
    // The new files are live, the settings are untouched, the marker exists.
    assert_eq!(
        fs::read(paths.bundle().join("librc2fx_native.so")).unwrap(),
        b"new librc2fx_native.so\n"
    );
    assert_eq!(
        fs::read(paths.data().join("state.json")).unwrap(),
        b"{\"revision\":7}"
    );
    assert!(paths.pending().exists());
    // The rollback copy is the old bundle and passes its checks.
    assert!(install::bundle_verifies(&paths.previous()));
    assert_eq!(
        fs::read(paths.previous().join("rc2fxd")).unwrap(),
        b"old rc2fxd\n"
    );
    assert!(install::rollback_bundle(&paths).is_some());
    // The shims are installed.
    assert_eq!(
        fs::read(paths.shims.join("rc_audio_mixer")).unwrap(),
        fs::read(paths.bundle().join("rc2fx-shim.sh")).unwrap()
    );
    install::clear_pending(&paths);
    assert!(!paths.pending().exists());
}

#[test]
fn a_power_cut_mid_swap_restores_the_previous_bundle() {
    let (_dir, paths) = test_paths();
    let old = bundle_files("old");
    write_bundle(&paths, &old);
    let staged = install::stage(&paths, "bbbbbbbbbbbbbbbb", &bundle_files("new")).unwrap();
    install::swap(&paths, &staged, "0.1.5", "0.2.0").unwrap();
    // The driver is gone with RAM; the marker is what a boot finds.
    fs::write(paths.driver(), b"").unwrap();
    assert!(!install::driver_alive(&paths));
    assert_eq!(
        install::rollback_bundle(&paths).as_deref(),
        Some(paths.previous().as_path())
    );
    let update = new_update(paths.clone());
    update.recover();
    // The old files are live again, settings stayed, and the outcome is kept.
    assert_eq!(
        fs::read(paths.bundle().join("librc2fx_native.so")).unwrap(),
        b"old librc2fx_native.so\n"
    );
    assert!(!paths.pending().exists());
    let last = update.last().expect("an outcome was recorded");
    assert_eq!(last.outcome, "rolledBack");
    assert_eq!(last.to.as_deref(), Some("0.2.0"));
    assert!(last.reason.unwrap_or_default().contains("did not finish"));
}

#[test]
fn an_interrupted_update_without_a_rollback_copy_clears_the_marker() {
    let (_dir, paths) = test_paths();
    write_bundle(&paths, &bundle_files("old"));
    install::Pending {
        schema: 1,
        from: "0.1.5".into(),
        to: "0.2.0".into(),
    }
    .save(&paths.pending())
    .unwrap();
    let update = new_update(paths.clone());
    update.recover();
    assert!(!paths.pending().exists());
    assert_eq!(update.last().unwrap().outcome, "rolledBack");
    // The running version is what the next update is compared against.
    let state = install::StoredState::load(&paths.state());
    assert_eq!(state.installed_version.as_deref(), Some("0.1.5"));
}

#[test]
#[cfg(target_os = "linux")]
fn a_live_driver_keeps_the_new_bundle_and_its_marker() {
    let (_dir, paths) = test_paths();
    write_bundle(&paths, &bundle_files("old"));
    install::Pending {
        schema: 1,
        from: "0.1.5".into(),
        to: "0.2.0".into(),
    }
    .save(&paths.pending())
    .unwrap();
    let mut child = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg("sleep 30")
        .arg("update-commit")
        .spawn()
        .unwrap();
    // Until the child has exec'd the shell, /proc shows the test binary's
    // command line; the real driver records itself only after it runs.
    let started = std::time::Instant::now();
    while !fs::read(format!("/proc/{}/cmdline", child.id()))
        .is_ok_and(|c| c.windows(13).any(|w| w == b"update-commit"))
    {
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let stat = fs::read_to_string(format!("/proc/{}/stat", child.id())).unwrap();
    let start = stat[stat.rfind(')').unwrap() + 1..]
        .split_whitespace()
        .nth(19)
        .unwrap();
    fs::write(paths.driver(), format!("{} {start}\n", child.id())).unwrap();
    assert!(install::driver_alive(&paths));
    assert!(install::rollback_bundle(&paths).is_none());
    let update = new_update(paths.clone());
    update.recover();
    assert!(
        paths.pending().exists(),
        "the transaction must not be undone"
    );
    assert_eq!(
        fs::read(paths.bundle().join("librc2fx_native.so")).unwrap(),
        b"old librc2fx_native.so\n"
    );
    let _ = child.kill();
    let _ = child.wait();
}

// ---- the payload envelope --------------------------------------------------

fn envelope_files() -> Vec<(String, Vec<u8>)> {
    let mut elf = vec![0u8; 96];
    elf[..6].copy_from_slice(&[127, b'E', b'L', b'F', 2, 1]);
    elf[18] = 0xb7;
    let mut files: Vec<(String, Vec<u8>)> = install::PAYLOAD_NAMES
        .iter()
        .map(|name| {
            let data = match *name {
                "firmware-profiles.json" => CATALOG.as_bytes().to_vec(),
                "mixer-release.json" => {
                    br#"{"schemaVersion":1,"version":"0.2.0","protocolVersion":1}"#.to_vec()
                }
                "SHA256SUMS" => Vec::new(),
                "rc2fx.sh" => b"#!/bin/sh\nrc2fx_build=release\n  update-commit) : ;;\n".to_vec(),
                n if install::SCRIPTS.contains(&n) => b"#!/bin/sh\n".to_vec(),
                _ => elf.clone(),
            };
            ((*name).to_owned(), data)
        })
        .collect();
    let sums: String = install::PINNED
        .iter()
        .map(|name| {
            let data = &files.iter().find(|(n, _)| n.as_str() == *name).unwrap().1;
            format!("{}  {name}\n", install::sum(data))
        })
        .collect();
    files.iter_mut().find(|(n, _)| n == "SHA256SUMS").unwrap().1 = sums.into_bytes();
    files
}

fn wrap(files: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut body = Vec::new();
    for (name, data) in files {
        body.extend_from_slice(
            format!("{}  {}  {name}\n", install::sum(data), data.len()).as_bytes(),
        );
    }
    for (_, data) in files {
        body.extend_from_slice(data);
    }
    let mut out = Vec::new();
    out.extend_from_slice(payload::MAGIC.as_bytes());
    out.extend_from_slice(format!("length {}\n", body.len()).as_bytes());
    out.extend_from_slice(format!("sha256 {}\n", install::sum(&body)).as_bytes());
    out.extend_from_slice(&body);
    out
}

#[test]
fn a_release_envelope_round_trips_and_damage_is_refused() {
    let files = envelope_files();
    let bytes = wrap(&files);
    let read = payload::read(&mut bytes.as_slice()).unwrap();
    assert_eq!(read, files);
    // Wrong magic, truncation and trailing bytes.
    let mut bad = bytes.clone();
    bad[0] = b'X';
    assert!(payload::read(&mut bad.as_slice()).is_err());
    assert!(payload::read(&mut &bytes[..bytes.len() - 1]).is_err());
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert!(payload::read(&mut trailing.as_slice()).is_err());
    // A file that does not match its manifest line.
    let mut damaged = files.clone();
    damaged[0].1[0] ^= 1;
    assert!(payload::read(&mut wrap(&damaged).as_slice()).is_err());
    // A development build is not installable through this API.
    let mut development = files.clone();
    development
        .iter_mut()
        .find(|(n, _)| n == "rc2fx.sh")
        .unwrap()
        .1 = b"#!/bin/sh\nrc2fx_build=development\n  update-commit) : ;;\n".to_vec();
    let sums: String = install::PINNED
        .iter()
        .map(|name| {
            let data = &development
                .iter()
                .find(|(n, _)| n.as_str() == *name)
                .unwrap()
                .1;
            format!("{}  {name}\n", install::sum(data))
        })
        .collect();
    development
        .iter_mut()
        .find(|(n, _)| n == "SHA256SUMS")
        .unwrap()
        .1 = sums.into_bytes();
    let error = payload::read(&mut wrap(&development).as_slice()).unwrap_err();
    assert!(error.contains("development"), "{error}");
    // A release that predates in-place updates (mixer 0.1.2 is one) would be
    // installed and then never committed: refused before anything changes.
    let mut older = files.clone();
    older.iter_mut().find(|(n, _)| n == "rc2fx.sh").unwrap().1 =
        b"#!/bin/sh\nrc2fx_build=release\n  launch) : ;;\n".to_vec();
    let sums: String = install::PINNED
        .iter()
        .map(|name| {
            let data = &older.iter().find(|(n, _)| n.as_str() == *name).unwrap().1;
            format!("{}  {name}\n", install::sum(data))
        })
        .collect();
    older.iter_mut().find(|(n, _)| n == "SHA256SUMS").unwrap().1 = sums.into_bytes();
    let error = payload::read(&mut wrap(&older).as_slice()).unwrap_err();
    assert!(error.contains("full flash"), "{error}");
    // A binary that is not ARM64 ELF.
    let mut wrong = files.clone();
    wrong.iter_mut().find(|(n, _)| n == "rc2fxd").unwrap().1 = b"not an ELF".to_vec();
    let sums: String = install::PINNED
        .iter()
        .map(|name| {
            let data = &wrong.iter().find(|(n, _)| n.as_str() == *name).unwrap().1;
            format!("{}  {name}\n", install::sum(data))
        })
        .collect();
    wrong.iter_mut().find(|(n, _)| n == "SHA256SUMS").unwrap().1 = sums.into_bytes();
    assert!(payload::read(&mut wrap(&wrong).as_slice()).is_err());
}

#[test]
fn the_base64_decoder_is_strict() {
    for (text, bytes) in [
        ("", None),
        ("Zg==", Some(&b"f"[..])),
        ("Zm8=", Some(&b"fo"[..])),
        ("Zm9v", Some(&b"foo"[..])),
        ("Zm9vYmFy", Some(&b"foobar"[..])),
        ("Zg=", None),
        ("Z===", None),
        ("Zm9v\n", None),
        ("Zm 9v", None),
        ("Zm9=Zm9v", None),
    ] {
        assert_eq!(decode_base64(text).as_deref(), bytes, "{text:?}");
    }
    for length in [1usize, 2, 3, 255] {
        let bytes: Vec<u8> = (0..length).map(|i| (i * 7) as u8).collect();
        assert_eq!(
            decode_base64(&crate::mixer_update::encode_base64_for_test(&bytes)),
            Some(bytes),
            "{length}"
        );
    }
}
