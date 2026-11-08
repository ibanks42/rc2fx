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

#[test]
fn failed_bundled_selection_discards_previous_source() {
    // The installer test executable has no appended payload. Start with a
    // local source and ensure a missing bundled choice cannot retain it.
    let work = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let mut engine = Engine::with_platform(
        root.path(),
        work.path(),
        Box::new(FakePlatform(Default::default())),
    );
    assert!(!matches!(engine.source, embed::Source::Download));
    assert!(engine.handle(Request::SelectBundledMixer).is_err());
    assert!(matches!(engine.source, embed::Source::Download));
}

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

/// `profiles` as a catalog file, in the full form the strict parser needs: the
/// fields `Profile` leaves out come from the real catalog.
fn catalog_json(profiles: &[Profile]) -> Vec<u8> {
    let real: serde_json::Value =
        serde_json::from_str(include_str!("../../../config/firmware-profiles.json")).unwrap();
    let full: Vec<_> = profiles
        .iter()
        .map(|p| {
            let mut full = real["profiles"]
                .as_array()
                .unwrap()
                .iter()
                .find(|real| real["model"] == p.model.as_str())
                .unwrap()
                .clone();
            let runtime = full["runtime"].take();
            let lean = serde_json::to_value(p).unwrap();
            for (key, value) in lean.as_object().unwrap() {
                if key != "runtime" {
                    full[key] = value.clone();
                }
            }
            full["runtime"] = if p.runtime.is_some() {
                runtime
            } else {
                serde_json::Value::Null
            };
            full
        })
        .collect();
    serde_json::to_vec(&serde_json::json!({"schemaVersion": 1, "profiles": full})).unwrap()
}

fn sample_profile() -> Profile {
    Profile {
        minimum_app_version: "0.1.1".into(),
        id: "fixture".into(),
        model: "rodecaster-pro-ii".into(),
        version: "1.7.7".into(),
        mixer_sha: "a".repeat(64),
        launcher_sha: "b".repeat(64),
        archive_sha: "c".repeat(64),
        archive_md5: "d".repeat(32),
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
    }
}

#[derive(Default)]
struct Fake {
    items: Vec<Hid>,
    volumes: Vec<Volume>,
    volume_error: Option<String>,
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
        let fake = self.0.lock().unwrap();
        match &fake.volume_error {
            Some(e) => Err(e.as_str().into()),
            None => Ok(fake.volumes.clone()),
        }
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
    let release = include_bytes!("../../../config/mixer-release.json");
    bundle.insert("mixer-release.json", release.to_vec());
    put(&root.join("config").join("mixer-release.json"), release);
    for n in [
        "librc2fx_native.so",
        "rc2fxd",
        "rc2fx-supervisor",
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
        "rc2fx-supervisor",
        "rc2fx-firmware",
        "firmware-profiles.json",
        "librc2fx_gui.so",
        "librc2fx_jackerr.so",
        "rc2fx_jackerr_snapshot",
        "mixer-release.json",
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

/// An installer folder with a runtime bundle and prepared firmware,
/// and one normal-mode mixer on USB.
fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let root = std::path::absolute(dir.path()).unwrap();
    let work = root.join("work");
    fs::create_dir_all(&work).unwrap();
    let catalog = catalog_json(&[sample_profile()]);
    put(
        &root.join("config").join("firmware-profiles.json"),
        &catalog,
    );
    write_runtime(&root, &catalog);
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
        runtime: image_entries(
            &runtime::bundle_files(&root).unwrap(),
            &sample_profile(),
            None,
        )
        .unwrap()
        .1,
        ..Default::default()
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
    let engine = Engine::with_platform(&root, &work, Box::new(FakePlatform(fake.clone())));
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
    for damage in ["firmware", "md5", "runtime", "profile", "key", "host-key"] {
        let mut f = fixture();
        let mut saved = f.saved();
        let p = saved.prepared.as_mut().unwrap();
        match damage {
            // What an older installer's SSH option left in the cache.
            "key" => p.key = "/old/key".into(),
            "host-key" => p.host_key = "abc".into(),
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
fn the_helper_refuses_an_image_rewritten_together_with_its_cache_record() {
    let anchors = tempfile::tempdir().unwrap();
    let mut f = fixture();
    f.engine.anchor_dir = Some(anchors.path().into());
    f.update_mode();
    let usb = f.drive(SERIAL);

    // Nothing recorded: a cache the helper did not build is not trusted.
    assert!(f.engine.flash("").is_err(), "unrecorded image accepted");
    assert_eq!(f.counts(), (0, 0, 0));

    // A same-user process rewrites the archive and everything that describes
    // it, so every hash in the user's folder agrees.
    f.engine.anchor_prepared(&f.prepared).unwrap();
    let forged = b"forged firmware";
    let work = f.engine.work.clone();
    put(&work.join("archive.tar.gz"), forged);
    let md = file_hash(&work.join("archive.tar.gz"), Algorithm::Md5).unwrap();
    put(&work.join("archive.md5"), md.as_bytes());
    let mut saved = f.saved();
    let p = saved.prepared.as_mut().unwrap();
    p.archive_sha = sum(forged);
    p.archive_md5 = md;
    f.engine.save(&saved).unwrap();
    assert!(f.engine.flash("").is_err(), "consistent rewrite accepted");
    assert_eq!(f.counts(), (0, 0, 0));
    assert!(!usb.join("archive.tar.gz").exists(), "forged image staged");

    // The same check still lets through what the helper recorded.
    let p = saved.prepared.clone().unwrap();
    f.engine.anchor_prepared(&p).unwrap();
    f.engine.flash("").unwrap();
    assert_eq!(f.counts(), (1, 1, 1));
}

#[test]
fn only_the_helper_that_records_an_image_can_flash_it() {
    let anchors = tempfile::tempdir().unwrap();
    let f = fixture();

    // The desktop process has no root-owned folder: recording does nothing,
    // and its own check passes, so only the helper can notice.
    let desktop =
        || Engine::with_platform(&f.root, &f.engine.work, Box::new(FakePlatform(f.p.clone())));
    desktop().anchor_prepared(&f.prepared).unwrap();
    desktop().verify_prepared(&f.prepared).unwrap();

    let helper = || {
        let mut engine = desktop();
        engine.anchor_dir = Some(anchors.path().into());
        engine
    };
    let refused = helper().verify_prepared(&f.prepared).unwrap_err();
    assert!(refused.0.contains("not prepared by the installer helper"));

    // A helper's record outlives it, so a later one flashes the same image.
    helper().anchor_prepared(&f.prepared).unwrap();
    helper().verify_prepared(&f.prepared).unwrap();
}

#[test]
fn a_helper_started_before_the_mixer_appeared_prepares_for_that_mixer() {
    let mut f = fixture();
    f.fake().items[0].pid = DeviceModel::Duo.normal_pids()[0];
    for stock in [false, true] {
        let error = f
            .engine
            .handle(Request::Prepare {
                channel: "beta".into(),
                version: "1.7.7".into(),
                serial: String::new(),
                stock,
            })
            .unwrap_err();
        assert!(error.0.contains("experimental"), "{}", error.0);
    }
    assert_eq!(f.counts(), (0, 0, 0));
}

#[test]
fn a_catalog_without_the_full_pins_is_refused_from_every_source() {
    let mut f = fixture();
    let catalog = catalog_json(&[sample_profile()]);
    embed::parse_catalog(&catalog).unwrap();
    let mut catalog: serde_json::Value = serde_json::from_slice(&catalog).unwrap();
    catalog["profiles"][0]
        .as_object_mut()
        .unwrap()
        .remove("archiveSha256");
    let catalog = serde_json::to_vec(&catalog).unwrap();
    assert!(embed::parse_catalog(&catalog).is_err());

    // The same file through a folder, as an embedded payload is read.
    put(&f.root.join("config/firmware-profiles.json"), &catalog);
    assert!(f.engine.catalog().is_err());
    assert!(f.engine.enter_usb_mode("").is_err());
    assert_eq!(f.counts(), (0, 0, 0));

    let empty = sample_profile();
    let lean = serde_json::to_vec(&Catalog {
        schema: 1,
        profiles: vec![empty],
    })
    .unwrap();
    assert!(embed::parse_catalog(&lean).is_err(), "lean form accepted");
}

#[test]
fn a_download_checks_rodes_md5_and_any_sha_256_the_catalog_pins() {
    let mut f = fixture();
    let dest = f.engine.work.join("cached.tar.gz");
    put(&dest, b"cached firmware");
    let md = file_hash(&dest, Algorithm::Md5).unwrap();
    let url = "https://update.rode.com/rodecaster-pro-ii/1.7.7/os.tar.gz";
    let error = f.engine.download(url, &dest, Some(""), &md).unwrap_err();
    assert!(error.0.contains("firmware checksums"), "{}", error.0);
    let error = f.engine.download(url, &dest, None, "").unwrap_err();
    assert!(error.0.contains("firmware checksums"), "{}", error.0);
    let sha = file_hash(&dest, Algorithm::Sha256).unwrap();
    f.engine.download(url, &dest, Some(&sha), &md).unwrap();
    // Stock firmware RØDE lists without a catalog pin: RØDE's MD5 alone.
    f.engine.download(url, &dest, None, &md).unwrap();
    assert!(f.engine.http.is_none(), "a matching file is kept");
}

/// RØDE's manifest as published, with an S3 testing build, a private beta
/// and a version listed on several channels.
const RODE_MANIFEST: &str = r#"{
  "name" : "rcp2-update-manifest",
  "master-version": {"app-version": "1.7.7", "os-version": "1.7.7", "os-required": "1.7.7",
    "app-URL": "update.rode.com/rcp2/1.7.7/app.zip", "os-URL": "update.rode.com/rcp2/1.7.7/os.tar.gz",
    "app-md5": "update.rode.com/rcp2/1.7.7/app.md5", "os-md5": "update.rode.com/rcp2/1.7.7/os.md5"},
  "main-version": {"app-version": "1.7.7", "os-version": "1.7.7", "os-required": "1.7.7",
    "app-URL": "update.rode.com/rcp2/1.7.7/app.zip", "os-URL": "update.rode.com/rcp2/1.7.7/os.tar.gz",
    "app-md5": "update.rode.com/rcp2/1.7.7/app.md5", "os-md5": "update.rode.com/rcp2/1.7.7/os.md5"},
  "beta-version": {"app-version": "1.7.8", "os-version": "1.7.8", "os-required": "1.7.8",
    "app-URL": "update.rode.com/rcp2/1.7.8/app.zip", "os-URL": "update.rode.com/rcp2/1.7.8/os.tar.gz",
    "app-md5": "update.rode.com/rcp2/1.7.8/app.md5", "os-md5": "update.rode.com/rcp2/1.7.8/os.md5"},
  "testing-version": {"app-version": "2.0.0", "os-version": "2.0.0", "os-required": "2.0.0",
    "app-URL": "update-rode-com.s3.ap-southeast-2.amazonaws.com/rcp2/testing/app.zip",
    "os-URL": "update-rode-com.s3.ap-southeast-2.amazonaws.com/rcp2/testing/os.tar.gz",
    "app-md5": "update-rode-com.s3.ap-southeast-2.amazonaws.com/rcp2/testing/app.md5",
    "os-md5": "update-rode-com.s3.ap-southeast-2.amazonaws.com/rcp2/testing/os.md5"},
  "privatebeta-version": {"app-version": "1.7.6", "os-version": "1.7.6", "os-required": "1.7.6",
    "app-URL": "update.rode.com/rcp2/1.7.6/app.zip", "os-URL": "update.rode.com/rcp2/1.7.6/os.tar.gz",
    "app-md5": "update.rode.com/rcp2/1.7.6/app.md5", "os-md5": "update.rode.com/rcp2/1.7.6/os.md5"},
  "futurechannel-version": {"os-version": "9.9.9"}
}"#;

#[test]
fn stock_releases_are_what_rodes_manifest_lists_on_its_own_server() {
    let releases =
        firmware::releases_from_manifest(DeviceModel::ProIi, RODE_MANIFEST.as_bytes()).unwrap();
    let listed: Vec<_> = releases
        .iter()
        .map(|r| (r.version.as_str(), r.channel.as_str()))
        .collect();
    // Newest first, stable and beta only: master, private beta, the S3
    // testing build and an unknown channel are left out.
    assert_eq!(listed, [("1.7.8", "beta"), ("1.7.7", "stable")]);
    let r = &releases[1];
    assert_eq!(r.url, "https://update.rode.com/rcp2/1.7.7/os.tar.gz");
    assert_eq!(r.digest_url, "https://update.rode.com/rcp2/1.7.7/os.md5");
    assert!(r.supported && r.profile.is_empty());
    // Another model's files never count: the Pro II's list offers a Duo
    // nothing.
    assert!(
        firmware::releases_from_manifest(DeviceModel::Duo, RODE_MANIFEST.as_bytes())
            .unwrap()
            .is_empty()
    );
    assert!(firmware::releases_from_manifest(DeviceModel::ProIi, b"<html>").is_err());
}

/// RØDE's Duo manifest as published: the same layout, naming `rcp2-duo/`.
const RODE_DUO_MANIFEST: &str = r#"{
  "name": "rcp2-update-manifest",
  "main-version": {"app-version": "1.7.7", "os-version": "1.7.7", "os-required": "1.7.7",
    "app-URL": "", "os-URL": "update.rode.com/rcp2-duo/1.7.7/os.tar.gz",
    "app-md5": "", "os-md5": "update.rode.com/rcp2-duo/1.7.7/os.md5"},
  "beta-version": {"app-version": "1.7.7", "os-version": "1.7.7", "os-required": "1.7.7",
    "app-URL": "", "os-URL": "update.rode.com/rcp2-duo/1.7.7/os.tar.gz",
    "app-md5": "", "os-md5": "update.rode.com/rcp2-duo/1.7.7/os.md5"},
  "privatebeta-version": {"app-version": "1.7.6", "os-version": "1.7.6", "os-required": "1.7.6",
    "app-URL": "update.rode.com/rcp2-duo/1.7.6/app.zip", "os-URL": "update.rode.com/rcp2-duo/1.7.6/os.tar.gz",
    "app-md5": "update.rode.com/rcp2-duo/1.7.6/app.md5", "os-md5": "update.rode.com/rcp2-duo/1.7.6/os.md5"},
  "sp-version": {"app-version": "2.0.0", "os-version": "2.0.0", "os-required": "2.0.0",
    "app-URL": "update-rode-com.s3.ap-southeast-2.amazonaws.com/rcp2-duo/sp/app.zip",
    "os-URL": "update-rode-com.s3.ap-southeast-2.amazonaws.com/rcp2-duo/sp/os.tar.gz",
    "app-md5": "update-rode-com.s3.ap-southeast-2.amazonaws.com/rcp2-duo/sp/app.md5",
    "os-md5": "update-rode-com.s3.ap-southeast-2.amazonaws.com/rcp2-duo/sp/os.md5"}
}"#;

#[test]
fn duo_stock_releases_are_what_rodes_duo_manifest_lists() {
    assert_eq!(
        firmware::rode_manifest(DeviceModel::Duo),
        "https://update.rode.com/rcp2-mini-manifest.json"
    );
    assert_eq!(
        firmware::rode_manifest(DeviceModel::ProIi),
        "https://update.rode.com/manifest.json"
    );
    let duo =
        firmware::releases_from_manifest(DeviceModel::Duo, RODE_DUO_MANIFEST.as_bytes()).unwrap();
    let listed: Vec<_> = duo
        .iter()
        .map(|r| (r.version.as_str(), r.channel.as_str()))
        .collect();
    // One version on both public channels is listed once, as stable.
    assert_eq!(listed, [("1.7.7", "stable")]);
    assert_eq!(
        duo[0].url,
        "https://update.rode.com/rcp2-duo/1.7.7/os.tar.gz"
    );
    assert_eq!(
        duo[0].digest_url,
        "https://update.rode.com/rcp2-duo/1.7.7/os.md5"
    );
    assert_eq!(duo[0].model, DeviceModel::Duo.id());
    // A Pro II is never offered the Duo's files.
    assert!(
        firmware::releases_from_manifest(DeviceModel::ProIi, RODE_DUO_MANIFEST.as_bytes())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn unpinned_stock_firmware_is_flashed_only_as_the_helper_recorded_it() {
    let anchors = tempfile::tempdir().unwrap();
    let mut f = fixture();
    f.engine.anchor_dir = Some(anchors.path().into());
    let mut saved = f.saved();
    let p = saved.prepared.as_mut().unwrap();
    p.stock = true;
    p.profile.clear();
    p.runtime.clear();
    let p = p.clone();
    f.engine.save(&saved).unwrap();
    assert!(
        f.engine.enter_usb_mode("").is_err(),
        "unrecorded stock image"
    );
    assert_eq!(f.counts(), (0, 0, 0));

    f.engine.anchor_prepared(&p).unwrap();
    f.engine.enter_usb_mode("").unwrap();
    assert_eq!(f.fake().writes, 1);

    put(&f.engine.work.join("archive.tar.gz"), b"changed");
    assert!(f.engine.enter_usb_mode("").is_err(), "damaged stock image");
    assert_eq!(f.fake().writes, 1);
}

#[cfg(unix)]
#[test]
fn a_backup_record_cannot_point_the_helper_outside_the_cache_folder() {
    let mut f = fixture();
    let work = f.engine.work.clone();
    let settings = "{}";
    let record = f.engine.save_settings(SERIAL, settings).unwrap();
    backup_settings(&work, &record).unwrap();

    let outside = f.root.join("secret.json");
    put(&outside, settings.as_bytes());
    let elsewhere = |file: &Path| BackupRecord {
        file: file.to_string_lossy().into_owned(),
        ..record.clone()
    };
    for path in [
        outside.clone(),
        work.join("../secret.json"),
        work.join("sub/settings.json"),
    ] {
        assert!(
            backup_settings(&work, &elsewhere(&path)).is_err(),
            "{path:?}"
        );
    }
    let link = work.join("settings-link.json");
    std::os::unix::fs::symlink(&outside, &link).unwrap();
    assert!(backup_settings(&work, &elsewhere(&link)).is_err());
}

#[test]
fn stock_firmware_requires_catalog_pins_but_no_runtime_or_key() {
    let mut f = fixture();
    let mut saved = f.saved();
    let p = saved.prepared.as_mut().unwrap();
    p.stock = true;
    p.runtime.clear();
    let mut profile = sample_profile();
    profile.archive_sha = p.archive_sha.clone();
    profile.archive_md5 = p.archive_md5.clone();
    let catalog = catalog_json(&[profile]);
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
            let anchors = tempfile::tempdir().unwrap();
            let mut f = fixture();
            // A stock image without a pin is the helper's to vouch for.
            f.engine.anchor_dir = Some(anchors.path().into());
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
fn update_drive_size_is_a_ceiling_not_a_constant() {
    use crate::usb::{fits, update_size};
    // 270 MB and 224 MB drives, as Windows showed them on two Pro IIs.
    assert!(update_size(VOLUME_TOTAL) && update_size(224 << 20));
    assert!(update_size(crate::DISK_BYTES));
    assert!(
        !update_size(crate::DISK_BYTES + 1),
        "larger than RØDE makes"
    );
    assert!(!update_size(0), "the mixer's empty normal-mode drive");

    assert!(fits(175 << 20, 224 << 20).is_ok());
    assert!(fits(224 << 20, 224 << 20).is_ok());
    let e = fits((224 << 20) + 1, 224 << 20).unwrap_err();
    assert!(e.0.contains("225 MB") && e.0.contains("224 MB"), "{e}");
}

#[test]
fn a_smaller_update_drive_is_found_and_flashed() {
    let mut f = fixture();
    f.update_mode();
    let usb = f.drive(SERIAL);
    f.fake().volumes[0].total = 224 << 20;
    assert!(f.engine.probe().unwrap().update_drive);
    f.engine.flash("").unwrap();
    crate::usb::verify_archive(&usb, &f.prepared).unwrap();
    assert_eq!(f.counts(), (1, 1, 1));
}

#[test]
fn a_failed_drive_scan_is_reported_instead_of_waited_on() {
    let mut f = fixture();
    f.update_mode();
    f.drive(SERIAL);
    f.fake().volume_error = Some(r"cannot read drive F:\: access denied".into());
    let status = f.engine.probe().unwrap();
    assert!(!status.update_drive);
    assert!(status.usb_error.unwrap().contains("F:"));
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
    let catalog = catalog_json(&[profile.clone()]);
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
    let catalog: Catalog =
        serde_json::from_str(include_str!("../../../config/firmware-profiles.json")).unwrap();
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
    let (mix, launcher) = patch_archive(&src, &dst, &[]).unwrap();
    assert_eq!((mix, launcher), (sum(b"mixer"), sum(b"launcher")));
    let mut i = 0;
    walk_archive(&dst, 1 << 20, false, |entry| {
        let mut b = Vec::new();
        entry.read_to_end(&mut b).unwrap();
        let h = entry.header();
        assert_eq!(b, data[i], "member {i}");
        assert_eq!(h.mode().unwrap(), 0o755);
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
    assert!(patch_archive(&src, &dir.path().join("bad.gz"), &[]).is_err());
    // A non-empty authorized_keys is refused.
    entries.pop();
    entries[0] = (
        header(&names[0], tar::EntryType::Regular, 3),
        b"key".to_vec(),
    );
    test_tar(&src, &entries);
    assert!(patch_archive(&src, &dir.path().join("bad.gz"), &[]).is_err());
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
    let mut catalog: Catalog =
        serde_json::from_str(include_str!("../../../config/firmware-profiles.json")).unwrap();
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
    // Stock firmware is checked against RØDE's manifest instead.
    for (channel, version) in [("stable", "1.7.8"), ("beta", "1.7.7")] {
        assert!(
            f.engine
                .prepare(channel, version, "", false)
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
        // Other RØDE hosts are not its update server.
        "https://rode.com/x",
        "https://cdn.rode.com/rcp2/1.7.7/os.tar.gz",
        "https://update.rode.com.attacker.test/x",
        "https://update.rode.com:8443/x",
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
    let patched =
        firmware::patch_archive_for_model(model, &source, &destination, &entries, None).unwrap();
    assert_eq!(patched.mixer, profile.mixer_sha);
    assert_eq!(patched.launcher, profile.launcher_sha);
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
        .map(|b| backup_settings(&work, b).unwrap());
    println!("settings: {} bytes", settings.as_ref().map_or(0, Vec::len));
    let bundle = runtime::bundle_files(&root).unwrap();
    let (entries, digest) = image_entries(&bundle, &profile, settings.as_deref()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("archive.tar.gz");
    let (mix, launcher) =
        patch_archive(&work.join("rode-original.tar.gz"), &out, &entries).unwrap();
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
    patch_archive(&src, &dst, &entries).unwrap();
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
    assert_eq!(found["opt/rc2fx/rc2fx-supervisor"].1, 0o700);
    assert_eq!(found["opt/rc2fx/firmware-profiles.json"].1, 0o600);
    // The installed release identity the device reports as its version.
    assert_eq!(found["opt/rc2fx/mixer-release.json"].1, 0o600);
    assert_eq!(
        found["opt/rc2fx/mixer-release.json"].2,
        include_bytes!("../../../config/mixer-release.json")
    );
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
    assert!(
        found[KEY_MEMBER].2.is_empty(),
        "authorized_keys stays as shipped"
    );
    assert_eq!(found["usr/bin/rc_audio_mixer"].2, b"mixer");

    // Without settings, no settings file is added.
    let (bare, same) = image_entries(&bundle, &sample_profile(), None).unwrap();
    assert_eq!(
        same, digest,
        "the runtime digest does not depend on settings"
    );
    assert!(!bare.iter().any(|e| e.path() == "opt/rc2fx/data/state.json"));

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
    assert!(patch_archive(&src, &dir.path().join("bad.gz"), &entries).is_err());
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
        backup_settings(&f.engine.work, f.saved().backup_for(SERIAL).unwrap()).unwrap(),
        body.as_bytes()
    );
    assert!(f.saved().backup_for("another-mixer").is_none());
    assert!(
        f.saved().backup_for("").is_none(),
        "an unknown mixer gets no settings"
    );
    // A changed file is refused.
    fs::write(&saved.file, b"{}").unwrap();
    assert!(backup_settings(&f.engine.work, &saved).is_err());
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

/// A minimal image that `patch_archive` accepts.
fn patchable_members() -> Vec<(tar::Header, Vec<u8>)> {
    let data: [(&str, &[u8]); 5] = [
        (KEY_MEMBER, b""),
        ("etc/hostname", b"rodecaster-pro-ii\n"),
        (
            "etc/ssh/sshd_config",
            b"PermitRootLogin yes\nAuthorizedKeysFile .ssh/authorized_keys\n",
        ),
        ("usr/bin/rc_audio_mixer", b"mixer"),
        ("usr/bin/audio-start.sh", b"launcher"),
    ];
    data.iter()
        .map(|(name, d)| (header(name, tar::EntryType::Regular, d.len()), d.to_vec()))
        .collect()
}

#[test]
fn patching_refuses_oversized_members_before_reading_them() {
    let dir = tempfile::tempdir().unwrap();
    let (src, dst) = (dir.path().join("src.tar.gz"), dir.path().join("dst.tar.gz"));
    test_tar(&src, &patchable_members());
    patch_archive(&src, &dst, &[]).unwrap();
    let small = firmware::MAX_SMALL_MEMBER as usize + 1;

    let launcher = |size| {
        let mut members = patchable_members();
        members[4] = (
            header("usr/bin/audio-start.sh", tar::EntryType::Regular, size),
            vec![b'x'; size],
        );
        members
    };
    test_tar(&src, &launcher(small - 1));
    patch_archive(&src, &dst, &[]).unwrap();
    test_tar(&src, &launcher(small));
    let error = patch_archive(&src, &dst, &[]).unwrap_err();
    assert!(error.0.contains("oversized"), "{}", error.0);

    let mut members = patchable_members();
    let size = firmware::MAX_MIXER_MEMBER as usize + 1;
    members[3] = (
        header("usr/bin/rc_audio_mixer", tar::EntryType::Regular, size),
        vec![0; size],
    );
    test_tar(&src, &members);
    let error = patch_archive(&src, &dst, &[]).unwrap_err();
    assert!(error.0.contains("oversized"), "{}", error.0);

    for kind in [tar::EntryType::GNULongName, tar::EntryType::XHeader] {
        let mut members = patchable_members();
        members.insert(0, (header("meta", kind, small), vec![b'x'; small]));
        test_tar(&src, &members);
        let error = patch_archive(&src, &dst, &[]).unwrap_err();
        assert!(error.0.contains("oversized"), "{kind:?}: {}", error.0);
    }
}

#[test]
fn patching_hashes_what_it_read_and_wrote_not_what_the_paths_hold() {
    let dir = tempfile::tempdir().unwrap();
    let (src, dst) = (dir.path().join("src.tar.gz"), dir.path().join("dst.tar.gz"));
    test_tar(&src, &patchable_members());
    let sha = file_hash(&src, Algorithm::Sha256).unwrap();
    let model = DeviceModel::ProIi;
    let patch = |want: &str| firmware::patch_archive_for_model(model, &src, &dst, &[], Some(want));

    let patched = patch(&sha).unwrap();
    assert_eq!(
        patched.archive_sha,
        file_hash(&dst, Algorithm::Sha256).unwrap()
    );
    assert_eq!(
        patched.archive_md5,
        file_hash(&dst, Algorithm::Md5).unwrap()
    );

    // The source was checked by path earlier; the bytes read differ now.
    fs::remove_file(&dst).unwrap();
    assert!(patch(&"0".repeat(64)).is_err());
    assert!(!dst.exists(), "a swapped source must not produce an image");
}
