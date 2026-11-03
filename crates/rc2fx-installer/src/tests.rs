use super::*;
use crate::{
    files::{Algorithm, file_hash, sum},
    firmware::{location, patch_archive, valid_url, walk_archive},
    runtime::{Entry, backup_settings, image_entries, readiness_error},
    usb::{NORMAL_PIDS, UPDATE_PID, vendor_usage},
};
use std::{
    collections::HashMap,
    fs,
    io::Read,
    sync::{Arc, Mutex},
};

const VOLUME_TOTAL: u64 = 283_189_248;

fn put(path: &Path, data: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, data).unwrap();
}

fn test_tar(path: &Path, entries: &[(tar::Header, Vec<u8>)]) {
    let file = fs::File::create(path).unwrap();
    let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
        file,
        flate2::Compression::default(),
    ));
    for (header, data) in entries {
        builder.append(header, data.as_slice()).unwrap();
    }
    builder.into_inner().unwrap().finish().unwrap();
}

fn header(name: &str, kind: tar::EntryType, size: usize) -> tar::Header {
    let mut h = tar::Header::new_gnu();
    h.set_path(name).unwrap();
    h.set_entry_type(kind);
    h.set_mode(0o755);
    h.set_uid(4);
    h.set_gid(5);
    h.set_mtime(123);
    h.set_size(size as u64);
    h.set_cksum();
    h
}

fn sample_profile() -> Profile {
    Profile {
        id: "fixture".into(),
        model: "rodecaster-pro-ii".into(),
        version: "1.7.7".into(),
        mixer_sha: "a".repeat(64),
        launcher_sha: "b".repeat(64),
        runtime: Some(RuntimeProfile {
            callback_abi: "jack-process-v1".into(),
            route_layout: "pro-ii-25-v1".into(),
            source_enum: "pro-ii-sources-v1".into(),
            sample_rate: 48000,
            block_frames: 112,
            job_lifetime: JobLifetime {
                abi: "capture-values-v1".into(),
            },
        }),
        ..Default::default()
    }
}

#[derive(Default)]
struct Fake {
    items: Vec<Hid>,
    volumes: Vec<Volume>,
    present: bool,
    opens: usize,
    writes: usize,
    ejects: usize,
    ack: Vec<u8>,
    /// Bytes the OS reports written; `None` means the full report.
    written: Option<usize>,
    write_error: bool,
    eject_error: bool,
    /// The command log at the moment of each HID write.
    log_at_write: Vec<Vec<CommandRecord>>,
    reports: Vec<Vec<u8>>,
    work: PathBuf,
}

#[derive(Clone)]
struct FakePlatform(Arc<Mutex<Fake>>);

struct FakeReports(Arc<Mutex<Fake>>);

impl Reports for FakeReports {
    fn write(&mut self, report: &[u8], _: Duration) -> Result<usize> {
        let mut f = self.0.lock().unwrap();
        f.writes += 1;
        f.reports.push(report.to_vec());
        let log = load_saved(&f.work).unwrap().commands;
        f.log_at_write.push(log);
        if f.write_error {
            bail!("device disconnected after write");
        }
        Ok(f.written.unwrap_or(report.len()))
    }
    fn read(&mut self, buf: &mut [u8], _: Duration) -> Result<usize> {
        let f = self.0.lock().unwrap();
        if f.ack.is_empty() {
            bail!("timed out");
        }
        buf[..f.ack.len()].copy_from_slice(&f.ack);
        Ok(f.ack.len())
    }
}

impl Platform for FakePlatform {
    fn inventory(&self) -> Result<Vec<Hid>> {
        Ok(self.0.lock().unwrap().items.clone())
    }
    fn open(&self, _: &Hid) -> Result<Box<dyn Reports>> {
        self.0.lock().unwrap().opens += 1;
        Ok(Box::new(FakeReports(self.0.clone())))
    }
    fn volumes(&self) -> Result<Vec<Volume>> {
        Ok(self.0.lock().unwrap().volumes.clone())
    }
    fn mount(&self, v: Volume) -> Result<Volume> {
        Ok(v)
    }
    fn media_present(&self, _: &Volume) -> Result<bool> {
        Ok(self.0.lock().unwrap().present)
    }
    fn eject(&self, _: &Volume) -> Result<()> {
        let mut f = self.0.lock().unwrap();
        f.ejects += 1;
        if f.eject_error {
            bail!("busy");
        }
        f.present = false;
        Ok(())
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    engine: Engine,
    prepared: Prepared,
    p: Arc<Mutex<Fake>>,
}

const SERIAL: &str = "fixture-device";

impl Fixture {
    fn fake(&self) -> std::sync::MutexGuard<'_, Fake> {
        self.p.lock().unwrap()
    }
    fn counts(&self) -> (usize, usize, usize) {
        let f = self.fake();
        (f.opens, f.writes, f.ejects)
    }
    fn saved(&self) -> Saved {
        load_saved(&self.engine.work).unwrap()
    }
    fn update_mode(&self) {
        self.fake().items[0].pid = UPDATE_PID;
    }
    fn drive(&self, serial: &str) -> PathBuf {
        let usb = self.root.join("usb");
        fs::create_dir_all(&usb).unwrap();
        self.fake().volumes = vec![Volume {
            path: usb.to_string_lossy().into(),
            device: "fixture-disk".into(),
            serial: serial.into(),
            fs: "FAT".into(),
            total: VOLUME_TOTAL,
            free: 200 << 20,
        }];
        self.fake().present = true;
        usb
    }
}

fn write_runtime(root: &Path, catalog: &[u8]) {
    let mut bundle: HashMap<&str, Vec<u8>> = HashMap::new();
    bundle.insert("firmware-profiles.json", catalog.to_vec());
    for n in [
        "librc2fx_native.so",
        "rc2fxd",
        "rc2fx-firmware",
        "librc2fx_gui.so",
        "librc2fx_jackerr.so",
        "rc2fx_jackerr_snapshot",
    ] {
        let mut b = vec![0u8; 64];
        b[..6].copy_from_slice(&[127, b'E', b'L', b'F', 2, 1]);
        b[18] = 0xb7;
        bundle.insert(n, b);
    }
    let mut manifest = String::new();
    for n in [
        "librc2fx_native.so",
        "rc2fxd",
        "rc2fx-firmware",
        "firmware-profiles.json",
        "librc2fx_gui.so",
        "librc2fx_jackerr.so",
        "rc2fx_jackerr_snapshot",
    ] {
        manifest.push_str(&format!("{}  {n}\n", sum(&bundle[n])));
        put(&root.join("runtime").join(n), &bundle[n]);
    }
    put(
        &root.join("runtime").join("SHA256SUMS"),
        manifest.as_bytes(),
    );
    for n in [
        "rc2fx.sh",
        "rc2fx-shim.sh",
        "rc2fx-gui-shim.sh",
        "install.sh",
    ] {
        // The launcher says what kind of build it is; the payload checks that.
        let data: &[u8] = if n == "rc2fx.sh" {
            b"#!/bin/sh\r\nrc2fx_build=development\r\nexit 0\r\n"
        } else {
            b"#!/bin/sh\r\nexit 0\r\n"
        };
        put(&root.join("xtask").join("mixer").join(n), data);
    }
}

/// An installer folder with a runtime bundle, a key and prepared firmware,
/// and one normal-mode mixer on USB.
fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let root = std::path::absolute(dir.path()).unwrap();
    let work = root.join("work");
    fs::create_dir_all(&work).unwrap();
    let catalog = serde_json::to_vec(&Catalog {
        schema: 1,
        profiles: vec![sample_profile()],
    })
    .unwrap();
    put(
        &root.join("config").join("firmware-profiles.json"),
        &catalog,
    );
    write_runtime(&root, &catalog);
    let key_path = root.join("keys").join("id");
    let (key, public) = ssh::ensure_key(&key_path.to_string_lossy()).unwrap();
    let data = b"fixture firmware";
    put(&work.join("archive.tar.gz"), data);
    let md = file_hash(&work.join("archive.tar.gz"), Algorithm::Md5).unwrap();
    put(&work.join("archive.md5"), md.as_bytes());
    let prepared = Prepared {
        model: None, // legacy Pro II cache
        version: "1.7.7".into(),
        channel: "beta".into(),
        profile: "fixture".into(),
        archive_sha: sum(data),
        archive_md5: md,
        key: key.to_string_lossy().into(),
        key_sha: sum(&public),
        host_key: String::new(),
        runtime: image_entries(
            &runtime::bundle_files(&root).unwrap(),
            &sample_profile(),
            None,
        )
        .unwrap()
        .1,
        settings: String::new(),
        stock: false,
    };
    let fake = Arc::new(Mutex::new(Fake {
        items: vec![Hid {
            path: "fake".into(),
            serial: SERIAL.into(),
            pid: NORMAL_PIDS[0],
            usage: 0xff00,
            input: 256,
            output: 256,
            control: false,
        }],
        ack: vec![2, b'A'],
        work: work.clone(),
        ..Default::default()
    }));
    let mut engine = Engine::with_platform(&root, &work, Box::new(FakePlatform(fake.clone())));
    engine.key = key;
    engine
        .save(&Saved {
            prepared: Some(prepared.clone()),
            ..Default::default()
        })
        .unwrap();
    Fixture {
        _dir: dir,
        root,
        engine,
        prepared,
        p: fake,
    }
}

#[test]
fn usb_mode_is_logged_before_the_single_write() {
    let mut f = fixture();
    f.engine.enter_usb_mode("").unwrap();
    {
        let fake = f.fake();
        assert_eq!(fake.writes, 1);
        assert_eq!(&fake.reports[0][..2], &[1, b'M']);
        assert_eq!(fake.reports[0].len(), 256);
        let log = &fake.log_at_write[0];
        assert_eq!(log.len(), 1, "logged before writing");
        assert!(log[0].command == "usb-mode" && !log[0].acknowledged && log[0].serial == SERIAL);
    }
    let c = f.saved().commands.pop().unwrap();
    assert!(c.acknowledged && c.error.is_empty());
}

#[test]
fn padded_windows_write_is_accepted_but_short_write_and_missing_ack_are_not() {
    // Regression for the 2026-10-24 incident: Windows padded a 64-byte
    // report and returned 256.
    let mut f = fixture();
    f.fake().items[0].output = 64;
    f.fake().written = Some(256);
    f.engine.enter_usb_mode("").unwrap();

    for (written, ack, error) in [
        (Some(63), vec![2, b'A'], false),
        (None, vec![], false),
        (None, vec![2, b'N'], false),
        (None, vec![2, b'A'], true),
    ] {
        let mut f = fixture();
        f.fake().items[0].output = 64;
        f.fake().written = written;
        f.fake().ack = ack;
        f.fake().write_error = error;
        assert!(f.engine.enter_usb_mode("").is_err());
        assert_eq!(f.fake().writes, 1, "never retried");
        let c = f.saved().commands.pop().unwrap();
        assert!(
            !c.acknowledged && !c.error.is_empty(),
            "outcome recorded as unknown"
        );
    }
}

#[test]
fn damaged_preparation_blocks_usb_before_opening_the_device() {
    for damage in [
        "firmware",
        "md5",
        "runtime",
        "profile",
        "missing-key",
        "changed-key",
    ] {
        let mut f = fixture();
        let mut saved = f.saved();
        let p = saved.prepared.as_mut().unwrap();
        match damage {
            "missing-key" => fs::remove_file(&p.key).unwrap(),
            "changed-key" => {
                let (key, _) =
                    ssh::ensure_key(&f.root.join("other-key").to_string_lossy()).unwrap();
                p.key = key.to_string_lossy().into();
            }
            "firmware" => put(&f.engine.work.join("archive.tar.gz"), b"changed"),
            "md5" => put(&f.engine.work.join("archive.md5"), b"changed"),
            "runtime" => put(&f.root.join("runtime").join("rc2fxd"), b"changed"),
            _ => p.profile = "unknown".into(),
        }
        f.engine.save(&saved).unwrap();
        assert!(f.engine.enter_usb_mode("").is_err(), "{damage} accepted");
        assert_eq!(f.counts(), (0, 0, 0), "{damage}: device accessed");
    }
    let mut f = fixture();
    f.engine.save(&Saved::default()).unwrap();
    assert!(f.engine.enter_usb_mode("").is_err(), "unprepared");
    assert_eq!(f.counts(), (0, 0, 0));
}

#[test]
fn stock_firmware_requires_catalog_pins_but_no_runtime_or_key() {
    let mut f = fixture();
    let mut saved = f.saved();
    let p = saved.prepared.as_mut().unwrap();
    p.stock = true;
    p.runtime.clear();
    p.key.clear();
    let mut profile = sample_profile();
    profile.archive_sha = p.archive_sha.clone();
    profile.archive_md5 = p.archive_md5.clone();
    let catalog = serde_json::to_vec(&Catalog {
        schema: 1,
        profiles: vec![profile],
    })
    .unwrap();
    put(&f.root.join("config/firmware-profiles.json"), &catalog);
    write_runtime(&f.root, &catalog);
    f.engine.save(&saved).unwrap();
    fs::remove_dir_all(f.root.join("runtime")).unwrap();
    f.engine.enter_usb_mode("").unwrap();
    assert_eq!(f.fake().writes, 1);

    put(&f.engine.work.join("archive.tar.gz"), b"changed");
    assert!(f.engine.enter_usb_mode("").is_err(), "damaged stock image");
    assert_eq!(f.fake().writes, 1);
}

#[test]
fn unpinned_or_misidentified_cached_firmware_never_opens_usb() {
    for stock in [false, true] {
        for change in ["version", "profile", "stock-hash"] {
            if !stock && change == "stock-hash" {
                continue;
            }
            let mut f = fixture();
            let mut saved = f.saved();
            let p = saved.prepared.as_mut().unwrap();
            p.stock = stock;
            match change {
                "version" => p.version = "1.7.8".into(),
                "profile" => p.profile.clear(),
                _ => {} // fixture archive hashes are not official catalog pins
            }
            f.engine.save(&saved).unwrap();
            assert!(f.engine.enter_usb_mode("").is_err(), "{stock} {change}");
            assert_eq!(f.counts(), (0, 0, 0));
        }
    }
}

#[test]
fn usb_mode_needs_exactly_one_normal_mixer() {
    let mut f = fixture();
    f.update_mode();
    assert!(
        f.engine.enter_usb_mode("").is_err(),
        "already in update mode"
    );
    let mut f = fixture();
    let second = Hid {
        serial: "other".into(),
        ..f.fake().items[0].clone()
    };
    f.fake().items.push(second);
    assert!(f.engine.enter_usb_mode("").is_err(), "ambiguous");
    f.engine.enter_usb_mode(SERIAL).unwrap();
    // The USB 2 interface (no serial, PID 0x26) is never a candidate.
    let mut f = fixture();
    f.fake().items[0].pid = 0x26;
    assert!(f.engine.enter_usb_mode("").is_err());
    assert_eq!(f.counts(), (0, 0, 0));
}

#[test]
fn flash_copies_verifies_ejects_then_sends_u() {
    let mut f = fixture();
    f.update_mode();
    let usb = f.drive(SERIAL);
    put(
        &usb.join("archive.tar.gz"),
        b"left over from an earlier attempt",
    );
    put(&usb.join(".rc2fx-archive.tar.gz.part"), b"partial");
    f.engine.flash("").unwrap();
    crate::usb::verify_archive(&usb, &f.prepared).unwrap();
    assert!(!usb.join(".rc2fx-archive.tar.gz.part").exists());
    assert_eq!(f.counts(), (1, 1, 1));
    assert_eq!(&f.fake().reports[0][..2], &[1, b'U']);
    let c = f.saved().commands.pop().unwrap();
    assert!(c.command == "flash" && c.acknowledged);
}

#[test]
fn flash_never_writes_another_drive_or_flashes_mounted_media() {
    let mut f = fixture();
    f.update_mode();
    let usb = f.drive("another-mixer");
    assert!(f.engine.flash("").is_err());
    assert!(!usb.join("archive.tar.gz").exists(), "wrong drive written");
    assert_eq!(f.counts(), (0, 0, 0));

    let mut f = fixture();
    f.update_mode();
    f.drive(SERIAL);
    f.fake().eject_error = true;
    assert!(f.engine.flash("").is_err());
    assert_eq!(f.fake().writes, 0, "flash after failed ejection");

    let mut f = fixture();
    let usb = f.drive(SERIAL);
    assert!(
        f.engine.flash("").is_err(),
        "normal mode is not update mode"
    );
    assert!(!usb.join("archive.tar.gz").exists());

    let mut f = fixture();
    f.update_mode();
    let usb = f.drive(SERIAL);
    fs::create_dir_all(usb.join("archive.md5")).unwrap();
    assert!(
        f.engine.flash("").is_err(),
        "a directory where a file belongs"
    );
    assert_eq!(f.fake().writes, 0);
}

#[test]
fn probe_reports_usb_and_cached_state_without_touching_the_device() {
    let mut f = fixture();
    let status = f.engine.probe().unwrap();
    assert_eq!(
        status.usb,
        [UsbMixer {
            model: rc2fx_firmware::Model::ProIi,
            serial: SERIAL.into(),
            update_mode: false
        }]
    );
    assert!(status.prepared_present);
    assert_eq!(f.counts(), (0, 0, 0));
}

fn duo_fixture() -> Fixture {
    let mut f = fixture();
    let model = DeviceModel::Duo;
    let mut profile = sample_profile();
    profile.id = "duo-fixture".into();
    profile.model = model.id().into();
    profile.runtime.as_mut().unwrap().route_layout = model.route_layout().into();
    profile.runtime.as_mut().unwrap().source_enum = model.source_enum().into();
    let contents = [
        ("etc/hostname", model.hostname().as_bytes().to_vec()),
        (
            "boot/extlinux/extlinux.conf",
            format!("FDT {}\n", model.board_dtb()).into_bytes(),
        ),
        (&model.board_dtb()[1..], b"fixture dtb".to_vec()),
        ("usr/bin/rc_audio_mixer", b"fixture mixer".to_vec()),
        ("usr/bin/audio-start.sh", b"fixture launcher".to_vec()),
    ];
    let entries: Vec<_> = contents
        .into_iter()
        .map(|(name, data)| (header(name, tar::EntryType::Regular, data.len()), data))
        .collect();
    let archive = f.engine.work.join("archive.tar.gz");
    test_tar(&archive, &entries);
    profile.archive_sha = file_hash(&archive, Algorithm::Sha256).unwrap();
    profile.archive_md5 = file_hash(&archive, Algorithm::Md5).unwrap();
    let catalog = serde_json::to_vec(&Catalog {
        schema: 1,
        profiles: vec![profile.clone()],
    })
    .unwrap();
    put(&f.root.join("config/firmware-profiles.json"), &catalog);
    write_runtime(&f.root, &catalog);
    let bundle = runtime::bundle_files(&f.root).unwrap();
    let (image, runtime) = image_entries(&bundle, &profile, None).unwrap();
    assert!(image.iter().any(|e| matches!(e, Entry::File { path, data, .. } if path == "opt/rc2fx/experimental-duo" && data == b"1\n")));
    assert!(
        image_entries(
            &bundle,
            &profile,
            Some(br#"{"targets":[{"id":"analog-1"}]}"#)
        )
        .is_err()
    );
    f.prepared.model = Some(model);
    f.prepared.profile = profile.id;
    f.prepared.runtime = runtime;
    f.prepared.archive_sha = profile.archive_sha;
    f.prepared.archive_md5 = profile.archive_md5;
    put(
        &f.engine.work.join("archive.md5"),
        f.prepared.archive_md5.as_bytes(),
    );
    f.engine
        .save(&Saved {
            prepared: Some(f.prepared.clone()),
            ..Default::default()
        })
        .unwrap();
    f.fake().items[0].pid = model.normal_pids()[0];
    f.engine.probe().unwrap();
    f
}

#[test]
fn duo_opt_in_is_required_and_revocation_blocks_a_prepared_image() {
    let mut f = duo_fixture();
    assert!(f.engine.enter_usb_mode(SERIAL).is_err());
    assert_eq!(f.counts(), (0, 0, 0));
    f.engine
        .handle(Request::AllowExperimentalDuo(true))
        .unwrap();
    f.engine.enter_usb_mode(SERIAL).unwrap();
    assert_eq!(f.fake().reports[0][1], b'M');
    f.fake().items[0].pid = DeviceModel::Duo.update_pid();
    f.drive(SERIAL);
    f.engine
        .handle(Request::AllowExperimentalDuo(false))
        .unwrap();
    let before = f.counts();
    assert!(f.engine.flash(SERIAL).is_err());
    assert_eq!(f.counts(), before);
    f.engine
        .handle(Request::AllowExperimentalDuo(true))
        .unwrap();
    f.engine.flash(SERIAL).unwrap();
    assert_eq!(f.fake().reports.last().unwrap()[1], b'U');
    assert_eq!(f.fake().ejects, 1);
}

#[test]
fn duo_opt_in_never_bypasses_model_serial_or_archive_identity() {
    for update in [false, true] {
        let mut f = fixture();
        f.engine
            .handle(Request::AllowExperimentalDuo(true))
            .unwrap();
        f.fake().items[0].pid = if update {
            DeviceModel::Duo.update_pid()
        } else {
            DeviceModel::Duo.normal_pids()[0]
        };
        assert!(
            if update {
                f.engine.flash(SERIAL)
            } else {
                f.engine.enter_usb_mode(SERIAL)
            }
            .is_err()
        );
        assert_eq!(f.counts(), (0, 0, 0), "Pro II image must not reach Duo");
        let mut f = duo_fixture();
        f.engine
            .handle(Request::AllowExperimentalDuo(true))
            .unwrap();
        f.fake().items[0].pid = if update { UPDATE_PID } else { NORMAL_PIDS[0] };
        assert!(
            if update {
                f.engine.flash(SERIAL)
            } else {
                f.engine.enter_usb_mode(SERIAL)
            }
            .is_err()
        );
        assert_eq!(f.counts(), (0, 0, 0), "Duo image must not reach Pro II");
    }
    let mut f = duo_fixture();
    f.engine
        .handle(Request::AllowExperimentalDuo(true))
        .unwrap();
    assert!(f.engine.enter_usb_mode("wrong-serial").is_err());
    assert_eq!(f.counts(), (0, 0, 0));
    // A forged cache model/profile cannot legitimize a Pro II archive.
    let mut f = fixture();
    f.engine
        .handle(Request::AllowExperimentalDuo(true))
        .unwrap();
    let mut p = f.prepared.clone();
    p.model = Some(DeviceModel::Duo);
    p.stock = true;
    assert!(f.engine.verify_prepared(&p).is_err());
    assert_eq!(f.counts(), (0, 0, 0));
    let mut f = duo_fixture();
    f.engine
        .handle(Request::AllowExperimentalDuo(true))
        .unwrap();
    let second = Hid {
        pid: NORMAL_PIDS[0],
        serial: "PRO".into(),
        ..f.fake().items[0].clone()
    };
    f.fake().items.push(second);
    assert!(
        f.engine.enter_usb_mode(SERIAL).is_err(),
        "mixed models remain ambiguous"
    );
    assert_eq!(f.counts(), (0, 0, 0));
}

#[test]
fn duo_stock_recovery_checks_its_own_pinned_archive() {
    let mut f = duo_fixture();
    let mut p = f.prepared.clone();
    p.stock = true;
    f.engine
        .handle(Request::AllowExperimentalDuo(true))
        .unwrap();
    f.engine.verify_prepared(&p).unwrap();
    p.archive_sha = "0".repeat(64);
    assert!(f.engine.verify_prepared(&p).is_err());
    assert_eq!(f.counts(), (0, 0, 0));
}

#[test]
fn duo_is_recognized_but_never_opened_or_flashed_without_opt_in() {
    use rc2fx_firmware::Model;
    for pid in Model::Duo
        .normal_pids()
        .iter()
        .copied()
        .chain([Model::Duo.update_pid()])
    {
        let mut f = fixture();
        f.fake().items[0].pid = pid;
        let status = f.engine.probe().unwrap();
        assert_eq!(status.usb.len(), 1);
        assert_eq!(status.usb[0].model, Model::Duo);
        assert!(status.usb_error.unwrap().contains("experimental"));
        assert!(
            f.engine
                .enter_usb_mode(SERIAL)
                .unwrap_err()
                .0
                .contains("experimental")
        );
        assert!(
            f.engine
                .flash(SERIAL)
                .unwrap_err()
                .0
                .contains("experimental")
        );
        assert_eq!(f.counts(), (0, 0, 0));
    }
    let mut f = fixture();
    let mut duo = f.fake().items[0].clone();
    duo.pid = Model::Duo.normal_pids()[0];
    duo.serial = "DUO".into();
    f.fake().items.push(duo);
    assert!(
        f.engine.enter_usb_mode("").is_err(),
        "mixed models must not auto-select"
    );
    assert_eq!(f.counts(), (0, 0, 0));
    for stock in [false, true] {
        let mut f = fixture();
        let mut p = f.prepared.clone();
        p.model = Some(Model::Duo);
        p.stock = stock;
        assert!(
            f.engine
                .verify_prepared(&p)
                .unwrap_err()
                .0
                .contains("experimental")
        );
        assert_eq!(f.counts(), (0, 0, 0));
    }
}

#[test]
fn release_discovery_keeps_same_version_models_separate() {
    use rc2fx_firmware::Model;
    let catalog: Catalog = serde_json::from_str(rc2fx_firmware::DEFAULT_CATALOG).unwrap();
    for model in Model::ALL {
        let releases = firmware::releases_from_catalog(model, &catalog).unwrap();
        assert_eq!(releases.len(), 2);
        assert_eq!(releases[0].version, "1.7.7");
        assert_eq!(releases[1].version, "1.7.6");
        for r in &releases {
            assert_eq!(r.model, model.id());
            assert!(r.supported);
            assert_eq!(r.channel, "stable");
            assert_eq!(
                r.url,
                format!(
                    "https://update.rode.com/{}/{}/os.tar.gz",
                    model.firmware_directory(),
                    r.version
                )
            );
            let other = if model == Model::ProIi {
                Model::Duo
            } else {
                Model::ProIi
            };
            assert!(firmware::location_for_model(other, &r.url, "os.tar.gz", &r.version).is_err());
        }
    }
}

#[test]
fn key_generation_is_reused_and_never_half_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("id").to_string_lossy().into_owned();
    let (key, public) = ssh::ensure_key(&path).unwrap();
    let (_, again) = ssh::ensure_key(&key.to_string_lossy()).unwrap();
    assert_eq!(public, again, "existing key replaced");
    let line = String::from_utf8(public).unwrap();
    assert!(
        line.starts_with("ssh-ed25519 ") && line.ends_with('\n') && line.split(' ').count() == 2
    );
    assert!(
        fs::read_to_string(&key)
            .unwrap()
            .starts_with("-----BEGIN OPENSSH PRIVATE KEY-----")
    );
    fs::remove_file(format!("{path}.pub")).unwrap();
    assert!(ssh::ensure_key(&path).is_err());
}

/// A minimal SSH server: accepts the user `root` when `accept`, runs
/// "fixture" (echoing stdin) and fails anything else with exit status 3.
#[test]
fn requests_take_the_folder_lock() {
    let mut f = fixture();
    let lock = files::open_local_file(&f.engine.work.join("installer.lock"), false).unwrap();
    let guard = files::lock_action(&lock).unwrap();
    let error = f.engine.handle(Request::Probe).unwrap_err();
    assert!(error.0.contains("another RC2FX installer"));
    drop(guard);
    assert!(matches!(
        f.engine.handle(Request::Probe).unwrap(),
        Reply::Status(_)
    ));
}

#[test]
fn descriptor_and_pid_guards() {
    assert!(vendor_usage(&[0x06, 0x00, 0xff, 0x09, 0x01, 0xa1, 0x01]));
    assert!(!vendor_usage(&[0x05, 0x01, 0x09, 0x06, 0xa1, 0x01]));
    assert!(NORMAL_PIDS.contains(&0x37) && !NORMAL_PIDS.contains(&0x26));
}

#[test]
fn utc_formats_rfc3339() {
    let now = utc();
    assert_eq!(now.len(), 20);
    assert!(now.ends_with('Z') && now.as_bytes()[10] == b'T');
}

#[test]
fn patch_changes_only_authorized_key_and_keeps_long_names() {
    let dir = tempfile::tempdir().unwrap();
    let (src, dst) = (dir.path().join("source.gz"), dir.path().join("patched.gz"));
    let long = format!("./usr/share/{}/file", "n".repeat(120));
    let names = [
        format!("./{KEY_MEMBER}"),
        "./etc/hostname".into(),
        "./etc/ssh/sshd_config".into(),
        "./usr/bin/rc_audio_mixer".into(),
        "./usr/bin/audio-start.sh".into(),
        "./etc/unchanged".into(),
        long.clone(),
    ];
    let data: Vec<Vec<u8>> = vec![
        vec![],
        b"rodecaster-pro-ii\n".to_vec(),
        b"PermitRootLogin yes\nAuthorizedKeysFile .ssh/authorized_keys\n".to_vec(),
        b"mixer".to_vec(),
        b"launcher".to_vec(),
        b"retain me".to_vec(),
        b"deep".to_vec(),
    ];
    // Build with the tar crate's GNU long-name support for the last entry.
    {
        let file = fs::File::create(&src).unwrap();
        let mut b = tar::Builder::new(flate2::write::GzEncoder::new(
            file,
            flate2::Compression::default(),
        ));
        for (name, d) in names.iter().zip(&data) {
            let mut h = header("placeholder", tar::EntryType::Regular, d.len());
            b.append_data(&mut h, name, d.as_slice()).unwrap();
        }
        b.into_inner().unwrap().finish().unwrap();
    }
    let key = b"ssh-ed25519 fixture\n";
    let (mix, launcher) = patch_archive(&src, &dst, key, &[]).unwrap();
    assert_eq!((mix, launcher), (sum(b"mixer"), sum(b"launcher")));
    let mut i = 0;
    walk_archive(&dst, 1 << 20, false, |entry| {
        let mut b = Vec::new();
        entry.read_to_end(&mut b).unwrap();
        let h = entry.header();
        let (expected, mode) = if i == 0 {
            (key.to_vec(), 0o600)
        } else {
            (data[i].clone(), 0o755)
        };
        assert_eq!(b, expected, "member {i}");
        assert_eq!(h.mode().unwrap(), mode);
        assert_eq!(
            (h.uid().unwrap(), h.gid().unwrap(), h.mtime().unwrap()),
            (4, 5, 123)
        );
        assert_eq!(
            firmware::normalized(&firmware::entry_name(entry)),
            firmware::normalized(&names[i])
        );
        i += 1;
        Ok(())
    })
    .unwrap();
    assert_eq!(i, names.len());
    // A duplicate key member is refused.
    let mut entries: Vec<_> = names
        .iter()
        .take(6)
        .zip(&data)
        .map(|(n, d)| (header(n, tar::EntryType::Regular, d.len()), d.clone()))
        .collect();
    entries.push(entries[0].clone());
    test_tar(&src, &entries);
    assert!(patch_archive(&src, &dir.path().join("bad.gz"), key, &[]).is_err());
    // A non-empty authorized_keys is refused.
    entries.pop();
    entries[0] = (
        header(&names[0], tar::EntryType::Regular, 3),
        b"key".to_vec(),
    );
    test_tar(&src, &entries);
    assert!(patch_archive(&src, &dir.path().join("bad.gz"), key, &[]).is_err());
}

#[test]
fn action_lock_is_exclusive_and_released() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("lock");
    let a = files::open_local_file(&path, false).unwrap();
    let b = files::open_local_file(&path, false).unwrap();
    let guard = files::lock_action(&a).unwrap();
    assert!(
        files::lock_action(&b).is_err(),
        "concurrent action acquired lock"
    );
    drop(guard);
    drop(files::lock_action(&b).unwrap());
}

#[test]
fn releases_are_offline_and_only_offer_qualified_catalog_entries() {
    let mut catalog: Catalog = serde_json::from_str(rc2fx_firmware::DEFAULT_CATALOG).unwrap();
    catalog
        .profiles
        .iter_mut()
        .find(|p| p.model == DeviceModel::Duo.id() && p.version == "1.7.7")
        .unwrap()
        .runtime = None;
    let releases = firmware::releases_from_catalog(DeviceModel::Duo, &catalog).unwrap();
    assert_eq!(releases.len(), 1);
    assert_eq!(releases[0].version, "1.7.6");
    assert!(!releases.iter().any(|r| r.version == "1.7.8"));

    let mut f = fixture();
    let releases = f.engine.releases().unwrap();
    assert_eq!(releases.len(), 1);
    assert_eq!(releases[0].version, "1.7.7");
    assert!(
        f.engine.http.is_none(),
        "listing must not initialize a network client"
    );
    for stock in [false, true] {
        assert!(
            f.engine
                .prepare("stable", "1.7.8", "", stock, false)
                .unwrap_err()
                .0
                .contains("qualified catalog")
        );
        assert!(
            f.engine
                .prepare("beta", "1.7.7", "", stock, false)
                .unwrap_err()
                .0
                .contains("qualified catalog")
        );
    }
    assert!(
        f.engine.http.is_none(),
        "unknown releases must fail before network access"
    );
    assert_eq!(f.counts(), (0, 0, 0));
}

#[test]
fn catalog_url_and_readiness_guards() {
    for u in [
        "http://update.rode.com/x",
        "https://rode.com.attacker.test/x",
        "https://user@rode.com/x",
        "https://rode.com:123/x",
        "https://rode.com",
    ] {
        assert!(!valid_url(u), "{u}");
    }
    assert!(valid_url("https://update.rode.com/manifest.json"));
    assert!(
        location(
            "https://update.rode.com/duo/1.7.7/os.tar.gz",
            "os.tar.gz",
            "1.7.7"
        )
        .is_err()
    );
    assert_eq!(
        location("update.rode.com/rcp2/1.7.7/os.md5", "os.md5", "1.7.7").unwrap(),
        "https://update.rode.com/rcp2/1.7.7/os.md5"
    );
    assert!(location("https://update.rode.com/rcp2/1.7/os.md5", "os.md5", "1.7").is_err());
    let r = sample_profile().runtime.unwrap();
    let ready = r#"{"config":{"revision":42},"engine":{"generation":15,"restartAllowed":false,"mode":"native","status":"running","appliedRevision":42,"sampleRate":48000,"blockSize":112,"message":"Native audio paths applied and advancing","audioAdvancing":true}}"#;
    assert_eq!(readiness_error(ready.as_bytes(), &r), "");
    for (old, new, want) in [
        (
            r#""appliedRevision":42"#,
            r#""appliedRevision":41"#,
            "appliedRevision=41",
        ),
        (
            r#""audioAdvancing":true"#,
            r#""audioAdvancing":false"#,
            "advancing=false",
        ),
        (
            r#""mode":"native""#,
            r#""mode":"simulation""#,
            r#"mode="simulation""#,
        ),
        (r#""blockSize":112"#, r#""blockSize":128"#, "blockSize=128"),
        (
            r#""sampleRate":48000"#,
            r#""sampleRate":44100"#,
            "sampleRate=44100",
        ),
        (
            r#""revision":42"#,
            r#""other":42"#,
            "configRevision=missing",
        ),
    ] {
        let why = readiness_error(ready.replacen(old, new, 1).as_bytes(), &r);
        assert!(why.contains(want), "{want}: {why}");
    }
    assert!(readiness_error(b"{", &r).contains("invalid state JSON"));
}

fn real(name: &str) -> PathBuf {
    PathBuf::from(std::env::var(name).unwrap_or_else(|_| panic!("set {name}")))
}

#[test]
#[ignore = "offline only; needs RC2FX_DUO_ARCHIVE and RC2FX_REAL_ROOT with a freshly built runtime"]
fn real_duo_image_preserves_board_files_and_carries_explicit_runtime_opt_in() {
    let source = real("RC2FX_DUO_ARCHIVE");
    let root = real("RC2FX_REAL_ROOT");
    let model = DeviceModel::Duo;
    let inspected = inspection::inspect_archive(model, &source).unwrap();
    let catalog: Catalog =
        serde_json::from_slice(&fs::read(root.join("config/firmware-profiles.json")).unwrap())
            .unwrap();
    let profile = catalog
        .profiles
        .iter()
        .find(|p| p.model == model.id() && p.id == inspected.profile)
        .unwrap();
    let bundle = runtime::bundle_files(&root).unwrap();
    let (entries, _) = image_entries(&bundle, profile, None).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let destination = dir.path().join("duo.tar.gz");
    let (mixer, launcher) =
        firmware::patch_archive_for_model(model, &source, &destination, b"", &entries).unwrap();
    assert_eq!(mixer, profile.mixer_sha);
    assert_eq!(launcher, profile.launcher_sha);
    inspection::verify_archive_model(model, &destination).unwrap();
    assert!(inspection::verify_archive_model(DeviceModel::ProIi, &destination).is_err());
    assert!(fs::metadata(&destination).unwrap().len() <= 275 << 20);
    let expected: std::collections::BTreeMap<_, _> = entries
        .iter()
        .filter_map(|entry| {
            if let Entry::File { path, data, .. } = entry {
                Some((path.clone(), sum(data)))
            } else {
                None
            }
        })
        .collect();
    let protected = |archive: &Path| {
        let mut hashes = std::collections::BTreeMap::new();
        let mut added = std::collections::BTreeMap::new();
        walk_archive(archive, 4 << 30, false, |entry| {
            let name = firmware::normalized(&firmware::entry_name(entry)).to_owned();
            if name.starts_with("boot/")
                || name.starts_with("Application/mcu-images/")
                || name == "etc/hostname"
            {
                let mut contents = Vec::new();
                entry.read_to_end(&mut contents)?;
                hashes.insert(name, (entry.header().as_bytes().to_vec(), sum(&contents)));
            } else if expected.contains_key(&name) {
                let mut contents = Vec::new();
                entry.read_to_end(&mut contents)?;
                assert!(added.insert(name, sum(&contents)).is_none());
            }
            Ok(())
        })
        .unwrap();
        (hashes, added)
    };
    let (original, original_added) = protected(&source);
    let (patched, patched_added) = protected(&destination);
    assert_eq!(
        original, patched,
        "board/controller content and metadata must stay unchanged"
    );
    assert!(original_added.is_empty());
    assert_eq!(
        patched_added, expected,
        "all runtime files, including the opt-in marker, must actually be in the image"
    );
    assert_eq!(patched_added["opt/rc2fx/experimental-duo"], sum(b"1\n"));
}

/// Build a real image: the cached RØDE archive with this checkout's runtime
/// and the newest saved settings for `RC2FX_REAL_SERIAL`. Writes only to a
/// temporary directory.
#[test]
#[ignore = "needs RC2FX_REAL_WORK, RC2FX_REAL_ROOT and RC2FX_REAL_SERIAL"]
fn real_image_builds_within_the_update_drive() {
    let work = real("RC2FX_REAL_WORK");
    let root = real("RC2FX_REAL_ROOT");
    let serial = std::env::var("RC2FX_REAL_SERIAL").unwrap();
    let engine = Engine::with_platform(&root, &work, Box::new(FakePlatform(Default::default())));
    let profile = engine
        .catalog()
        .unwrap()
        .profiles
        .into_iter()
        .find(|p| p.version == "1.7.7")
        .unwrap();
    let saved = load_saved(&work).unwrap();
    let settings = saved
        .backup_for(&serial)
        .map(|b| backup_settings(b).unwrap());
    println!("settings: {} bytes", settings.as_ref().map_or(0, Vec::len));
    let bundle = runtime::bundle_files(&root).unwrap();
    let (entries, digest) = image_entries(&bundle, &profile, settings.as_deref()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("archive.tar.gz");
    let (mix, launcher) = patch_archive(
        &work.join("rode-original.tar.gz"),
        &out,
        b"ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIA6rWI3G2sz07DnfFlrouTcysQlj2P+jpNSOEWD9OJ3X\n",
        &entries,
    )
    .unwrap();
    assert_eq!((mix, launcher), (profile.mixer_sha, profile.launcher_sha));
    println!(
        "image {} bytes, runtime {digest}, {} entries added",
        fs::metadata(&out).unwrap().len(),
        entries.len()
    );
}

/// Read the release choices from a real local payload, without network access.
#[test]
#[ignore = "needs RC2FX_REAL_ROOT"]
fn real_payload_releases() {
    let root = real("RC2FX_REAL_ROOT");
    let dir = tempfile::tempdir().unwrap();
    let mut engine = Engine::with_platform(
        &root,
        dir.path(),
        Box::new(FakePlatform(Default::default())),
    );
    let releases = engine.releases().unwrap();
    println!("{releases:?}");
    assert!(releases.iter().any(|r| r.channel == "stable"));
}

/// Probe the host's USB. Read-only: no report channel is opened.
#[test]
#[ignore = "host USB; needs RC2FX_REAL_ROOT"]
fn live_probe() {
    let root = real("RC2FX_REAL_ROOT");
    let dir = tempfile::tempdir().unwrap();
    let mut engine = Engine::new(&root, dir.path());
    println!("{:#?}", engine.probe().unwrap());
}

/// A minimal stock root filesystem with the members the patch checks.
fn stock_image(path: &Path) {
    let names = [
        format!("./{KEY_MEMBER}"),
        "./etc/hostname".into(),
        "./etc/ssh/sshd_config".into(),
        "./usr/bin/rc_audio_mixer".into(),
        "./usr/bin/audio-start.sh".into(),
    ];
    let data: [&[u8]; 5] = [
        b"",
        b"rodecaster-pro-ii\n",
        b"PermitRootLogin yes\nAuthorizedKeysFile .ssh/authorized_keys\n",
        b"mixer",
        b"launcher",
    ];
    let entries: Vec<_> = names
        .iter()
        .zip(data)
        .map(|(n, d)| (header(n, tar::EntryType::Regular, d.len()), d.to_vec()))
        .collect();
    test_tar(path, &entries);
}

#[test]
fn image_gets_rc2fx_autostart_and_settings_and_nothing_is_replaced() {
    let f = fixture();
    let bundle = runtime::bundle_files(&f.root).unwrap();
    let settings = br#"{"config":{"revision":3,"targets":[{"id":"analog-1"}]}}"#;
    let (entries, digest) = image_entries(&bundle, &sample_profile(), Some(settings)).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (src, dst) = (dir.path().join("stock.gz"), dir.path().join("rc2fx.gz"));
    stock_image(&src);
    patch_archive(&src, &dst, b"ssh-ed25519 fixture\n", &entries).unwrap();
    let mut found: HashMap<String, (u8, u32, Vec<u8>, String)> = HashMap::new();
    walk_archive(&dst, 1 << 24, false, |entry| {
        let name = firmware::normalized(&firmware::entry_name(entry)).to_owned();
        let h = entry.header();
        let meta = (
            h.entry_type().as_byte(),
            h.mode().unwrap(),
            Vec::<u8>::new(),
            String::from_utf8_lossy(&entry.link_name_bytes().unwrap_or_default()).into_owned(),
        );
        let (uid, gid) = (h.uid().unwrap(), h.gid().unwrap());
        let mut data = Vec::new();
        entry.read_to_end(&mut data).unwrap();
        if name.starts_with("opt")
            || name == "usr/sbin/rc_audio_mixer"
            || name == "usr/sbin/rc_gui_juce"
        {
            assert_eq!((uid, gid), (0, 0), "{name} owned by root");
        }
        found.insert(name, (meta.0, meta.1, data, meta.3));
        Ok(())
    })
    .unwrap();
    assert_eq!(found["opt"].1, 0o755, "the stock image has no /opt");
    assert_eq!(found["opt/rc2fx/rc2fxd"].1, 0o700);
    assert_eq!(found["opt/rc2fx/firmware-profiles.json"].1, 0o600);
    assert_eq!(found["opt/rc2fx/install.sh"].2, b"#!/bin/sh\nexit 0\n");
    assert_eq!(
        found["opt/rc2fx/bundle.sha256"].2,
        format!("{digest}\n").as_bytes()
    );
    assert_eq!(found["opt/rc2fx/data/state.json"].2, settings);
    assert_eq!(found["opt/rc2fx/logs"].0, b'5');
    assert_eq!(found["usr/sbin/rc_audio_mixer"].1, 0o755);
    assert_eq!(found["usr/sbin/rc_audio_mixer"].2, b"#!/bin/sh\nexit 0\n");
    assert_eq!(found["usr/sbin/rc_gui_juce"].1, 0o755);
    assert_eq!(found["usr/sbin/rc_gui_juce"].2, b"#!/bin/sh\nexit 0\n");
    assert_eq!(found["opt/rc2fx/librc2fx_gui.so"].1, 0o700);
    assert_eq!(found["opt/rc2fx/librc2fx_jackerr.so"].1, 0o700);
    assert_eq!(
        found["opt/rc2fx/librc2fx_jackerr.so"].2,
        bundle["librc2fx_jackerr.so"]
    );
    assert_eq!(found["opt/rc2fx/rc2fx_jackerr_snapshot"].1, 0o700);
    assert_eq!(
        found["opt/rc2fx/rc2fx_jackerr_snapshot"].2,
        bundle["rc2fx_jackerr_snapshot"]
    );
    assert_eq!(found["opt/rc2fx/rc2fx-gui-shim.sh"].1, 0o700);
    assert_eq!(found[KEY_MEMBER].2, b"ssh-ed25519 fixture\n");
    assert_eq!(found["usr/bin/rc_audio_mixer"].2, b"mixer");

    // Without settings, no settings file is added.
    let (bare, same) = image_entries(&bundle, &sample_profile(), None).unwrap();
    assert_eq!(
        same, digest,
        "the runtime digest does not depend on settings"
    );
    assert!(!bare.iter().any(|e| e.path() == "opt/rc2fx/data/state.json"));

    // Without the SSH developer option, authorized_keys stays as shipped.
    let plain = dir.path().join("plain.gz");
    patch_archive(&src, &plain, b"", &entries).unwrap();
    walk_archive(&plain, 1 << 24, false, |entry| {
        if firmware::normalized(&firmware::entry_name(entry)) == KEY_MEMBER {
            let mut data = Vec::new();
            entry.read_to_end(&mut data).unwrap();
            assert!(data.is_empty(), "no key without the developer option");
            assert_eq!(entry.header().mode().unwrap(), 0o755, "mode unchanged");
        }
        Ok(())
    })
    .unwrap();

    // An image that already has RC2FX paths is refused rather than merged.
    let mut taken = vec![(
        header("./opt/rc2fx/rc2fxd", tar::EntryType::Regular, 1),
        b"x".to_vec(),
    )];
    walk_archive(&src, 1 << 20, false, |entry| {
        let mut data = Vec::new();
        let h = entry.header().clone();
        entry.read_to_end(&mut data).unwrap();
        taken.push((h, data));
        Ok(())
    })
    .unwrap();
    test_tar(&src, &taken);
    assert!(patch_archive(&src, &dir.path().join("bad.gz"), b"k\n", &entries).is_err());
}

#[test]
fn image_entries_refuse_a_runtime_that_does_not_support_the_firmware() {
    let f = fixture();
    let bundle = runtime::bundle_files(&f.root).unwrap();
    let mut other = sample_profile();
    other.mixer_sha = "c".repeat(64);
    assert!(image_entries(&bundle, &other, None).is_err());
    let entries = image_entries(&bundle, &sample_profile(), None).unwrap().0;
    assert!(
        entries
            .iter()
            .any(|e| matches!(e, Entry::File { path, .. } if path == "usr/sbin/rc_audio_mixer"))
    );
    assert!(entries.iter().any(
        |e| matches!(e, Entry::File { path, mode: 0o755, .. } if path == "usr/sbin/rc_gui_juce")
    ));
    assert!(entries.iter().any(
        |e| matches!(e, Entry::File { path, mode: 0o700, .. } if path == "opt/rc2fx/librc2fx_jackerr.so")
    ));
    assert!(entries.iter().any(
        |e| matches!(e, Entry::File { path, mode: 0o700, .. } if path == "opt/rc2fx/rc2fx_jackerr_snapshot")
    ));
}

#[test]
fn recorder_payloads_are_required_and_must_be_arm64_elfs() {
    let f = fixture();
    let names = ["librc2fx_jackerr.so", "rc2fx_jackerr_snapshot"];
    let catalog = fs::read(f.root.join("config").join("firmware-profiles.json")).unwrap();
    let bundle = runtime::bundle_files(&f.root).unwrap();
    for name in names {
        assert!(bundle.contains_key(name), "{name}");
    }
    for name in names {
        fs::remove_file(f.root.join("runtime").join(name)).unwrap();
        let error = runtime::bundle_files(&f.root).unwrap_err().to_string();
        assert!(error.contains(name), "missing {name}: {error}");
        write_runtime(&f.root, &catalog);
    }
    for name in names {
        let path = f.root.join("runtime").join(name);
        let good = fs::read(&path).unwrap();
        let corrupt: &[u8] = b"not an ARM64 ELF";
        fs::write(&path, corrupt).unwrap();
        let sums_path = f.root.join("runtime").join("SHA256SUMS");
        let sums = fs::read_to_string(&sums_path).unwrap();
        let old = format!("{}  {name}\n", sum(&good));
        let new = format!("{}  {name}\n", sum(corrupt));
        assert!(sums.contains(&old), "{name} manifest line");
        fs::write(&sums_path, sums.replacen(&old, &new, 1)).unwrap();
        let error = runtime::bundle_files(&f.root).unwrap_err().to_string();
        assert!(error.contains("is not ARM64 ELF"), "{name}: {error}");
        write_runtime(&f.root, &catalog);
    }
}

#[test]
fn settings_read_over_usb_are_kept_per_mixer_and_used_for_the_image() {
    let mut f = fixture();
    let body = r#"{"config":{"revision":7},"presets":[]}"#;
    let saved = f.engine.save_settings(SERIAL, body).unwrap();
    assert_eq!(saved.sha, sum(body.as_bytes()));
    assert_eq!(
        backup_settings(f.saved().backup_for(SERIAL).unwrap()).unwrap(),
        body.as_bytes()
    );
    assert!(f.saved().backup_for("another-mixer").is_none());
    assert!(
        f.saved().backup_for("").is_none(),
        "an unknown mixer gets no settings"
    );
    // A changed file is refused.
    fs::write(&saved.file, b"{}").unwrap();
    assert!(backup_settings(&saved).is_err());
    // Without a mixer serial, or with something that is not settings, nothing is kept.
    assert!(f.engine.save_settings("", body).is_err());
    assert!(f.engine.save_settings(SERIAL, "[]").is_err());
}

#[test]
fn service_follows_the_state_the_app_reads_over_usb() {
    let f = fixture();
    assert!(matches!(
        crate::runtime::service_from_state(&f.root, b"not json"),
        Service::Starting { .. }
    ));
    assert!(matches!(
        crate::runtime::service_from_state(
            &f.root,
            br#"{"engine":{"mode":"native","status":"starting"}}"#
        ),
        Service::Starting { .. }
    ));
}

#[test]
fn ssh_access_keeps_one_host_key_and_trusts_it_under_the_mixer_address() {
    let dir = tempfile::tempdir().unwrap();
    let work = dir.path();
    let (entries, public) = ssh::host_key(work).unwrap();
    let (again, same) = ssh::host_key(work).unwrap();
    assert_eq!(public, same, "the same identity every flash");
    let paths: Vec<_> = entries.iter().map(|e| e.path().to_owned()).collect();
    assert_eq!(
        paths,
        [
            "etc/ssh/ssh_host_ed25519_key",
            "etc/ssh/ssh_host_ed25519_key.pub"
        ]
    );
    assert!(matches!(
        &again[0],
        runtime::Entry::File { mode: 0o600, .. }
    ));
    std::fs::write(
        work.join("known_hosts"),
        "192.168.0.172 ssh-ed25519 OLD\nother ssh-ed25519 KEEP\n",
    )
    .unwrap();
    ssh::remember_host(work, "192.168.0.172", &public).unwrap();
    let text = std::fs::read_to_string(work.join("known_hosts")).unwrap();
    assert!(!text.contains("OLD"), "the stale key is replaced");
    assert!(text.contains("other ssh-ed25519 KEEP"));
    assert!(text.contains(&format!(
        "192.168.0.172 {}",
        String::from_utf8_lossy(&public).trim()
    )));
}
