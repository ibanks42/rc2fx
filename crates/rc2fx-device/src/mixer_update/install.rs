//! Atomic in-place bundle installation and rollback under `/opt`.
//!
//! Layout:
//! - `/opt/rc2fx`: live bundle (`data/` and `logs/` are never moved or replaced)
//! - `/opt/.rc2fx-previous`: hard-linked snapshot of the last known-good files
//! - `/opt/.rc2fx-update-pending`: uncommitted update marker (distinct from
//!   `install.sh`'s `/opt/.rc2fx-install-pending`)
//!
//! Order of operations (safe across a power cut at any step):
//! 1. Stage under `/opt` and re-verify from disk.
//! 2. Hard-link live files into `.rc2fx-previous` (live bundle untouched).
//! 3. Write and sync `.rc2fx-update-pending`; launchers now prefer
//!    `.rc2fx-previous` unless an update driver is alive (`rc2fx-shim.sh`).
//! 4. Rename payload files over `/opt/rc2fx`, sync the dir, rewrite shims.
//! 5. Detached driver removes the pending marker only after the new controller
//!    and audio come up; otherwise the next boot rolls back before serving.

use sha2::{Digest, Sha256};
use std::{
    fs::{self, DirBuilder, File, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

/// The files a mixer payload carries, in its order. This and the three lists
/// below match `fixtures/payload/lists.json`, as `PayloadNames`, `Pinned`,
/// `Binaries` and `Scripts` in the desktop's
/// `desktop/installer/embed/format.go` do; the test
/// `the_payload_lists_match_the_shared_fixture` fails when they differ.
pub const PAYLOAD_NAMES: [&str; 14] = [
    "librc2fx_native.so",
    "rc2fxd",
    "rc2fx-supervisor",
    "rc2fx-firmware",
    "firmware-profiles.json",
    "librc2fx_gui.so",
    "librc2fx_jackerr.so",
    "rc2fx_jackerr_snapshot",
    "mixer-release.json",
    "SHA256SUMS",
    "rc2fx.sh",
    "rc2fx-shim.sh",
    "rc2fx-gui-shim.sh",
    "install.sh",
];
/// The order of the bundle's `SHA256SUMS` lines.
pub const PINNED: [&str; 9] = [
    "librc2fx_native.so",
    "rc2fxd",
    "rc2fx-supervisor",
    "rc2fx-firmware",
    "firmware-profiles.json",
    "librc2fx_gui.so",
    "librc2fx_jackerr.so",
    "rc2fx_jackerr_snapshot",
    "mixer-release.json",
];
/// Files that are ARM64 ELF executables or libraries.
pub const BINARIES: [&str; 7] = [
    "librc2fx_native.so",
    "rc2fxd",
    "rc2fx-supervisor",
    "rc2fx-firmware",
    "librc2fx_gui.so",
    "librc2fx_jackerr.so",
    "rc2fx_jackerr_snapshot",
];
/// Files that live in the mixer's shell.
pub const SCRIPTS: [&str; 4] = [
    "rc2fx.sh",
    "rc2fx-shim.sh",
    "rc2fx-gui-shim.sh",
    "install.sh",
];

/// Payload files the running mixer process does not hold. The controller runs
/// `rc2fxd`; the rest are read only by it, by an install or at the mixer's
/// next launch (the GUI shim at the next boot, as for any update). An update
/// that changes nothing else is applied by restarting the controller alone,
/// and audio keeps running.
pub const CONTROLLER_SIDE: [&str; 7] = [
    "rc2fxd",
    "rc2fx-firmware",
    "firmware-profiles.json",
    "mixer-release.json",
    "SHA256SUMS",
    "install.sh",
    "rc2fx-gui-shim.sh",
];

/// Longest accepted reason in the last-outcome record.
const MAX_REASON: usize = 512;

pub fn sum(data: &[u8]) -> String {
    let digest = Sha256::digest(data);
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// The SHA-256 of a file, read in bounded pieces. Used for an uploaded payload
/// too large to hold beside its own staging copy.
pub fn sum_file(path: &Path) -> io::Result<String> {
    let mut hasher = Sha256::new();
    let mut file = File::open(path)?;
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let digest = hasher.finalize();
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    Ok(out)
}

/// Every path the update code touches. Production uses the mixer's own
/// layout; the tests point it at a temporary directory.
#[derive(Clone, Debug)]
pub struct Paths {
    /// The directory that holds the bundle, the previous bundle and markers.
    pub opt: PathBuf,
    /// A directory in RAM for sessions and the driver's records.
    pub run: PathBuf,
    /// Where `rc_audio_mixer` and `rc_gui_juce` shims are installed.
    pub shims: PathBuf,
    /// RØDE's stock mixer executable.
    pub stock_mixer: PathBuf,
    /// RØDE's audio launcher script.
    pub audio_launcher: PathBuf,
}

impl Paths {
    pub fn mixer() -> Self {
        Self {
            opt: PathBuf::from("/opt"),
            run: PathBuf::from("/run/rc2fx-update"),
            shims: PathBuf::from("/usr/sbin"),
            stock_mixer: PathBuf::from("/usr/bin/rc_audio_mixer"),
            audio_launcher: PathBuf::from("/usr/bin/audio-start.sh"),
        }
    }

    pub fn bundle(&self) -> PathBuf {
        self.opt.join("rc2fx")
    }
    pub fn previous(&self) -> PathBuf {
        self.opt.join(".rc2fx-previous")
    }
    pub fn pending(&self) -> PathBuf {
        self.opt.join(".rc2fx-update-pending")
    }
    /// The developer install's own transaction marker (`install.sh`).
    pub fn install_pending(&self) -> PathBuf {
        self.opt.join(".rc2fx-install-pending")
    }
    pub fn data(&self) -> PathBuf {
        self.bundle().join("data")
    }
    pub fn logs(&self) -> PathBuf {
        self.bundle().join("logs")
    }
    pub fn state(&self) -> PathBuf {
        self.data().join("update-state.json")
    }
    pub fn last(&self) -> PathBuf {
        self.data().join("update-last")
    }
    pub fn driver(&self) -> PathBuf {
        self.run.join("driver")
    }
    pub fn started(&self) -> PathBuf {
        self.run.join("started")
    }
    pub fn request(&self, id: &str) -> PathBuf {
        self.run.join(format!("commit-{id}.request"))
    }
    pub fn driver_script(&self, id: &str) -> PathBuf {
        self.run.join(format!("commit-{id}.sh"))
    }
}

/// The persisted protection against replaying older signed metadata or older
/// releases. It sits beside the saved settings, not in the swapped files.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct StoredState {
    pub schema: u32,
    /// The release this mixer last committed, when known.
    pub installed_version: Option<String>,
    pub root_version: u64,
    pub targets_version: u64,
    pub snapshot_version: u64,
    pub timestamp_version: u64,
    /// A digest of the verified targets role as this build serializes it.
    /// Replaying different content under the same version is refused.
    pub targets_digest: String,
}

impl StoredState {
    /// The recorded state, or `None` when nothing has been recorded yet. A
    /// file that cannot be read or parsed is an error, never a fresh state:
    /// that would silently forget the replay and downgrade protection.
    pub fn load(path: &Path) -> Result<Option<Self>, String> {
        let file = match File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("cannot read {}: {error}", path.display())),
        };
        let mut text = String::new();
        file.take(16 * 1024)
            .read_to_string(&mut text)
            .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        serde_json::from_str(&text)
            .map(Some)
            .map_err(|error| format!("{} is damaged: {error}", path.display()))
    }

    /// Write atomically, so a power cut leaves the older record.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let bytes = serde_json::to_vec(self).map_err(io::Error::other)?;
        write_atomic(path, &bytes, 0o600)
    }
}

/// The marker written before any live file changes. Its presence means the
/// update was not committed; its absence is the completion marker.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pending {
    pub schema: u32,
    pub from: String,
    pub to: String,
}

impl Pending {
    pub fn load(path: &Path) -> Option<Self> {
        let text = fs::read_to_string(path).ok()?;
        serde_json::from_str(&text).ok()
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        let bytes = serde_json::to_vec(self).map_err(io::Error::other)?;
        write_atomic(path, &bytes, 0o600)
    }
}

/// What the last update did, as `GET api/v1/update/last` reports it and as the
/// driver and this module record it (`key=value` lines in `update-last`).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LastOutcome {
    pub outcome: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The GUI shim changes only at the next boot; its audio sibling applies
    /// at the restart the update makes.
    pub gui_shim_needs_reboot: bool,
}

impl LastOutcome {
    pub fn load(path: &Path) -> Option<Self> {
        let file = File::open(path).ok()?;
        let mut text = String::new();
        file.take(4 * 1024).read_to_string(&mut text).ok()?;
        let mut outcome = None;
        let (mut from, mut to, mut reason, mut gui) = (None, None, None, false);
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim_end();
            match key {
                "outcome" if matches!(value, "committed" | "rolledBack") => {
                    outcome = Some(value.to_owned());
                }
                "from" if !value.is_empty() => from = Some(value.to_owned()),
                "to" if !value.is_empty() => to = Some(value.to_owned()),
                "reason" if !value.is_empty() => reason = Some(value.to_owned()),
                "gui" => gui = value == "1",
                _ => {}
            }
        }
        Some(Self {
            outcome: outcome?,
            from,
            to,
            reason: reason.map(|r| r.chars().take(MAX_REASON).collect()),
            gui_shim_needs_reboot: gui,
        })
    }

    fn to_record(&self) -> String {
        format!(
            "schema=1\noutcome={}\nfrom={}\nto={}\nreason={}\ngui={}\n",
            self.outcome,
            self.from.as_deref().unwrap_or(""),
            self.to.as_deref().unwrap_or(""),
            self.reason.as_deref().unwrap_or(""),
            if self.gui_shim_needs_reboot { "1" } else { "0" },
        )
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        write_atomic(path, self.to_record().as_bytes(), 0o600)
    }
}

/// One installed bundle, after the files were staged and checked.
pub struct Staged {
    pub dir: PathBuf,
    /// The firmware profile the payload's own check selected for this mixer.
    pub profile: String,
}

/// What an install changed outside the payload files.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Swapped {
    pub gui_shim_changed: bool,
    /// Every file outside [`CONTROLLER_SIDE`] is byte for byte the one that
    /// was installed before.
    pub controller_only: bool,
}

/// A failure after the swap began: `swapped_in_doubt` is true once the pending
/// marker exists, so the caller must not simply delete the stage.
#[derive(Debug)]
pub struct InstallFailure {
    pub message: String,
    pub swapped_in_doubt: bool,
}

impl std::fmt::Display for InstallFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

fn fail(message: impl Into<String>, swapped_in_doubt: bool) -> InstallFailure {
    InstallFailure {
        message: message.into(),
        swapped_in_doubt,
    }
}

fn io_failure(what: &str, error: io::Error, swapped_in_doubt: bool) -> InstallFailure {
    fail(format!("{what}: {error}"), swapped_in_doubt)
}

pub fn write_atomic(path: &Path, data: &[u8], mode: u32) -> io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::other("path has no parent"))?;
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::other("path has no file name"))?;
    let temporary = dir.join(format!(".{}.new", name.to_string_lossy()));
    {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(mode)
            .open(&temporary)?;
        file.write_all(data)?;
        file.sync_all()?;
    }
    fs::set_permissions(&temporary, fs::Permissions::from_mode(mode))?;
    fs::rename(&temporary, path)?;
    fsync_dir(dir)
}

fn fsync_dir(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

/// Stage `files` under `/opt` (the filesystem the bundle lives on, so the
/// swap is a rename) and check the staged copy from disk.
pub fn stage(
    paths: &Paths,
    id: &str,
    files: &[(String, Vec<u8>)],
) -> Result<Staged, InstallFailure> {
    validate_names(files).map_err(|e| fail(e, false))?;
    fs::create_dir_all(&paths.opt).map_err(|e| io_failure("create /opt", e, false))?;
    let dir = paths.opt.join(format!(".rc2fx-stage.{id}"));
    let _ = fs::remove_dir_all(&dir);
    let mut builder = DirBuilder::new();
    builder.mode(0o700);
    builder
        .create(&dir)
        .map_err(|e| io_failure("create the staging directory", e, false))?;
    for (name, data) in files {
        let path = dir.join(name);
        let mode = if BINARIES.contains(&name.as_str()) || SCRIPTS.contains(&name.as_str()) {
            0o700
        } else {
            0o600
        };
        let written = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&path)
            .and_then(|mut file| {
                file.write_all(data)?;
                file.sync_all()
            });
        if let Err(error) = written {
            let _ = fs::remove_dir_all(&dir);
            return Err(io_failure(&format!("write staged {name}"), error, false));
        }
        if sum(data) != sum_of(&path) {
            let _ = fs::remove_dir_all(&dir);
            return Err(fail(
                format!("staged {name} does not match its bytes after writing"),
                false,
            ));
        }
    }
    fsync_dir(&dir).map_err(|e| io_failure("flush the staging directory", e, false))?;
    check_manifest(&dir, files).map_err(|e| {
        let _ = fs::remove_dir_all(&dir);
        fail(e, false)
    })?;
    let profile = check_device(paths, &dir).map_err(|e| {
        let _ = fs::remove_dir_all(&dir);
        fail(e, false)
    })?;
    Ok(Staged { dir, profile })
}

fn sum_of(path: &Path) -> String {
    match fs::read(path) {
        Ok(bytes) => sum(&bytes),
        Err(_) => String::new(),
    }
}

fn validate_names(files: &[(String, Vec<u8>)]) -> Result<(), String> {
    if files.len() != PAYLOAD_NAMES.len() {
        return Err("the payload does not carry every runtime file".into());
    }
    for (want, (name, _)) in PAYLOAD_NAMES.iter().zip(files) {
        if name.as_str() != *want {
            return Err(format!("the payload has {name} where {want} belongs"));
        }
    }
    Ok(())
}

/// The `sha256sum -c SHA256SUMS` step: the staged manifest must be exactly the
/// pinned manifest of the staged files.
fn check_manifest(dir: &Path, files: &[(String, Vec<u8>)]) -> Result<(), String> {
    let mut manifest = String::new();
    for name in PINNED {
        let data = files
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, d)| d.as_slice())
            .ok_or_else(|| format!("the payload is missing {name}"))?;
        manifest.push_str(&format!("{}  {name}\n", sum(data)));
    }
    let stored = fs::read(dir.join("SHA256SUMS")).map_err(|e| format!("SHA256SUMS: {e}"))?;
    if stored != manifest.as_bytes() {
        return Err("the payload's SHA256SUMS does not match its files".into());
    }
    Ok(())
}

/// `rc2fx-firmware check-device` from the payload itself, so a bundle for
/// another firmware is refused before anything live changes.
fn check_device(paths: &Paths, dir: &Path) -> Result<String, String> {
    let mut command = std::process::Command::new(dir.join("rc2fx-firmware"));
    command
        .arg("check-device")
        .arg(dir.join("firmware-profiles.json"))
        .arg(&paths.stock_mixer)
        .arg(&paths.audio_launcher)
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .stdin(std::process::Stdio::null());
    // A Duo is admitted only with the launcher's opt-in, which this daemon
    // inherited; the check must see the same answer the launcher will.
    if let Ok(value) = std::env::var("RC2FX_EXPERIMENTAL_DUO") {
        command.env("RC2FX_EXPERIMENTAL_DUO", value);
    }
    let output = command
        .output()
        .map_err(|e| format!("cannot run the payload's rc2fx-firmware: {e}"))?;
    if !output.status.success() {
        let said = String::from_utf8_lossy(&output.stderr);
        let said = said.trim();
        return Err(if said.is_empty() {
            "the payload does not support this firmware".into()
        } else {
            format!("the payload does not support this firmware: {said}")
        });
    }
    let reported = String::from_utf8_lossy(&output.stdout);
    let profile = reported.split_whitespace().next().unwrap_or_default();
    if profile.is_empty() {
        return Err("the payload's firmware check reported no profile".into());
    }
    Ok(profile.to_owned())
}

/// Build `.rc2fx-previous` from the live files and flush it. The live bundle
/// is untouched when this fails.
fn snapshot_previous(paths: &Paths) -> Result<(), InstallFailure> {
    let live = paths.bundle();
    let previous = paths.previous();
    remove_previous(paths).map_err(|e| io_failure("remove the older rollback copy", e, false))?;
    let mut builder = DirBuilder::new();
    builder.mode(0o700);
    builder
        .create(&previous)
        .map_err(|e| io_failure("create the rollback copy", e, false))?;
    for name in PAYLOAD_NAMES {
        let source = live.join(name);
        match fs::symlink_metadata(&source) {
            Ok(metadata) if metadata.file_type().is_file() => {}
            Ok(_) => {
                return Err(fail(
                    format!("the installed {name} is not a regular file; refusing to update"),
                    false,
                ));
            }
            Err(error) => {
                return Err(io_failure(
                    &format!("the installed bundle is missing {name}"),
                    error,
                    false,
                ));
            }
        }
        if let Err(error) = fs::hard_link(&source, previous.join(name)) {
            return Err(io_failure(
                &format!("copy {name} into the rollback copy"),
                error,
                false,
            ));
        }
    }
    // The full-flash installer records whether RC2FX started at boot; keep
    // that with the rollback copy as it does.
    let marker = live.join(".installer-autostart");
    if marker.is_file() {
        let _ = fs::hard_link(&marker, previous.join(".installer-autostart"));
    }
    fsync_dir(&previous).map_err(|e| io_failure("flush the rollback copy", e, false))
}

fn remove_previous(paths: &Paths) -> io::Result<()> {
    let previous = paths.previous();
    match fs::symlink_metadata(&previous) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            // A rollback copy that holds saved settings is kept, as the
            // full-flash installer keeps one; only its name moves aside.
            if previous.join("data").exists() || previous.join("logs").exists() {
                for attempt in 0..8 {
                    let aside = paths
                        .opt
                        .join(format!(".rc2fx-retained.{}.{attempt}", std::process::id()));
                    if !aside.exists() {
                        return fs::rename(&previous, aside);
                    }
                }
                return Err(io::Error::other(
                    "the rollback copy holds saved settings and could not be moved aside",
                ));
            }
            fs::remove_dir_all(&previous)
        }
        Ok(_) => Err(io::Error::other(format!(
            "{} is not a directory",
            previous.display()
        ))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// The swap itself: pending marker, file renames, shims. Errors after the
/// marker exists report `swapped_in_doubt`.
pub fn swap(
    paths: &Paths,
    staged: &Staged,
    from: &str,
    to: &str,
) -> Result<Swapped, InstallFailure> {
    let live = paths.bundle();
    let controller_only = PAYLOAD_NAMES
        .iter()
        .filter(|name| !CONTROLLER_SIDE.contains(name))
        .all(|name| same_bytes(&staged.dir.join(name), &live.join(name)));
    snapshot_previous(paths)?;
    Pending {
        schema: 1,
        from: from.to_owned(),
        to: to.to_owned(),
    }
    .save(&paths.pending())
    .map_err(|e| io_failure("record the pending update", e, false))?;
    for name in PAYLOAD_NAMES {
        fs::rename(staged.dir.join(name), live.join(name))
            .map_err(|e| io_failure(&format!("replace {name}"), e, true))?;
    }
    fsync_dir(&live).map_err(|e| io_failure("flush the installed bundle", e, true))?;
    fsync_dir(&paths.opt).map_err(|e| io_failure("flush /opt", e, true))?;
    let digest = bundle_digest_from_disk(&live)?;
    write_atomic(
        &live.join("bundle.sha256"),
        format!("{digest}\n").as_bytes(),
        0o600,
    )
    .map_err(|e| io_failure("record the bundle digest", e, true))?;
    let mut swapped =
        write_shims(paths, &live).map_err(|e| io_failure("install the launcher shims", e, true))?;
    swapped.controller_only = controller_only;
    fsync_dir(&paths.shims).map_err(|e| io_failure("flush /usr/sbin", e, true))?;
    let _ = fs::remove_dir(&staged.dir);
    Ok(swapped)
}

/// Whether two files can both be read and hold the same bytes.
fn same_bytes(a: &Path, b: &Path) -> bool {
    matches!((fs::read(a), fs::read(b)), (Ok(a), Ok(b)) if a == b)
}

fn bundle_digest_from_disk(dir: &Path) -> Result<String, InstallFailure> {
    let mut manifest = String::new();
    for name in PAYLOAD_NAMES {
        let bytes = fs::read(dir.join(name))
            .map_err(|e| io_failure(&format!("read installed {name}"), e, true))?;
        manifest.push_str(&format!("{}  {name}\n", sum(&bytes)));
    }
    Ok(sum(manifest.as_bytes()))
}

/// Install the audio and GUI shims from the live bundle. Returns whether the
/// GUI shim's bytes changed, which only takes effect at the next boot.
fn write_shims(paths: &Paths, live: &Path) -> io::Result<Swapped> {
    let audio = fs::read(live.join("rc2fx-shim.sh"))?;
    let gui = fs::read(live.join("rc2fx-gui-shim.sh"))?;
    let gui_before = fs::read(paths.shims.join("rc_gui_juce")).ok();
    write_atomic(&paths.shims.join("rc_audio_mixer"), &audio, 0o755)?;
    write_atomic(&paths.shims.join("rc_gui_juce"), &gui, 0o755)?;
    Ok(Swapped {
        gui_shim_changed: gui_before.as_deref() != Some(gui.as_slice()),
        controller_only: false,
    })
}

/// Is an update driver working right now? The shim and this module both use
/// this rule: while one is alive the new bundle is the one to start; after a
/// power cut the record is gone with `/run`.
pub fn driver_alive(paths: &Paths) -> bool {
    let Ok(text) = fs::read_to_string(paths.driver()) else {
        return false;
    };
    let mut fields = text.split_whitespace();
    let (Some(pid), Some(started)) = (fields.next(), fields.next()) else {
        return false;
    };
    let Ok(pid) = pid.parse::<u32>() else {
        return false;
    };
    let command = match fs::read(format!("/proc/{pid}/cmdline")) {
        Ok(command) => command,
        Err(_) => return false,
    };
    if !command
        .windows(b"update-commit".len())
        .any(|w| w == b"update-commit")
    {
        return false;
    }
    crate::proc_stat::process_start(pid).as_deref() == Some(started)
}

/// Every payload file in `dir` is present, regular and matches the manifest.
pub fn bundle_verifies(dir: &Path) -> bool {
    let Ok(manifest) = fs::read(dir.join("SHA256SUMS")) else {
        return false;
    };
    let mut expected = String::new();
    for name in PINNED {
        let path = dir.join(name);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_file() => {}
            _ => return false,
        }
        let Ok(bytes) = fs::read(&path) else {
            return false;
        };
        expected.push_str(&format!("{}  {name}\n", sum(&bytes)));
    }
    if manifest != expected.as_bytes() {
        return false;
    }
    PAYLOAD_NAMES.iter().all(|name| dir.join(name).is_file())
}

/// Remove staging directories left by an interrupted update. Only names this
/// module creates are touched, and only when a driver is not using them.
pub fn remove_stages(paths: &Paths) {
    let Ok(entries) = fs::read_dir(&paths.opt) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with(".rc2fx-stage.") {
            continue;
        }
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

/// Restore the previous files over the live bundle. Used at startup after an
/// interrupted update, when nothing can restart audio for us.
pub fn restore_previous(paths: &Paths) -> Result<(), String> {
    let previous = paths.previous();
    if !bundle_verifies(&previous) {
        return Err("the rollback copy is incomplete".into());
    }
    let live = paths.bundle();
    for name in PAYLOAD_NAMES {
        let bytes = fs::read(previous.join(name))
            .map_err(|e| format!("read the rollback copy's {name}: {e}"))?;
        let mode = if BINARIES.contains(&name) || SCRIPTS.contains(&name) {
            0o700
        } else {
            0o600
        };
        write_atomic(&live.join(name), &bytes, mode).map_err(|e| format!("restore {name}: {e}"))?;
    }
    let digest = bundle_digest_from_disk(&live).map_err(|e| e.message)?;
    write_atomic(
        &live.join("bundle.sha256"),
        format!("{digest}\n").as_bytes(),
        0o600,
    )
    .map_err(|e| format!("record the restored bundle digest: {e}"))?;
    write_shims(paths, &live).map_err(|e| format!("restore the launchers: {e}"))?;
    fsync_dir(&live).map_err(|e| format!("flush the restored bundle: {e}"))?;
    fsync_dir(&paths.shims).map_err(|e| format!("flush /usr/sbin: {e}"))?;
    Ok(())
}

/// Clear the marker and the driver's RAM records after a commit.
pub fn clear_pending(paths: &Paths) {
    let _ = fs::remove_file(paths.pending());
    let _ = fsync_dir(&paths.opt);
}

/// Forget the previous update's outcome once a new install is accepted, so
/// `GET api/v1/update/last` never reports it as this install's outcome (both
/// can name the same target version). Until the new outcome is recorded the
/// endpoint answers as if no update had run.
pub fn clear_last(paths: &Paths) -> io::Result<()> {
    match fs::remove_file(paths.last()) {
        Ok(()) => fsync_dir(&paths.data()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// Free bytes on the filesystem that holds `path`, when it can be read.
pub fn free_bytes(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut stats: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: the path is a valid C string and stats points at writable memory.
    if unsafe { libc::statvfs(path.as_ptr(), &mut stats) } != 0 {
        return None;
    }
    // The field types differ between libc targets; widen both explicitly.
    #[allow(clippy::useless_conversion)]
    let (blocks, size) = (u64::from(stats.f_bavail), u64::from(stats.f_frsize));
    Some(blocks.saturating_mul(size))
}
