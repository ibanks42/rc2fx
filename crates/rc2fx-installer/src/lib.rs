//! RC2FX installation for a RØDECaster Pro II.
//!
//! RC2FX is installed by flashing official RØDE firmware that already
//! contains it: the runtime, its autostart and the mixer's saved settings are
//! added to the root filesystem image on this computer. The mixer is only
//! touched through its own USB update mechanism. The desktop app's USB 1
//! connection to RC2FX supplies the settings saved beforehand and confirms it
//! is running afterwards.
//!
//! The work folder is a cache: the prepared firmware, settings backups and a
//! log of USB commands sent. Device commands are only sent when explicitly
//! requested, and an unacknowledged command is never repeated automatically.

mod anchor;
pub mod app_release;
pub mod app_update;
pub mod control;
pub mod embed;
mod files;
mod firmware;
pub mod helper;
pub mod inspection;
pub mod mixer_update;
pub mod platform;
mod runtime;
pub use runtime::service_from_state;
mod ssh;
mod usb;

pub use platform::{Hid, Platform, Reports, Volume};
pub use rc2fx_firmware::Model as DeviceModel;

pub const DUO_WARNING: &str = "RØDECaster Duo support is experimental and has NOT been tested on hardware. Flashing, recovery and audio routing may fail, cause loud or incorrect audio, or leave the mixer unusable. Back up your RØDE show, disconnect speakers and headphones before testing, and proceed entirely at your own risk. Only Duo firmware is allowed; never flash Pro II firmware onto a Duo.";

/// Retained build identity used by host package checks without launching a GUI.
/// Call once from the application entrypoint so the linker retains the marker.
pub fn payload_mode() -> &'static str {
    #[cfg(feature = "embedded")]
    let mode = "RC2FX-PAYLOAD-MODE:embedded:1";
    #[cfg(not(feature = "embedded"))]
    let mode = "RC2FX-PAYLOAD-MODE:download:1";
    std::hint::black_box(mode)
}
pub use embed::{
    Catalog, Envelope, Identity, JobLifetime, Profile, RuntimeProfile, bundled_release,
    newest_supported_firmware,
};
pub use firmware::rode_releases;
pub use rc2fx_core::{DeviceVersions, MixerRelease, compare_versions};
pub use runtime::{Bundle, PAYLOAD_NAMES, bundle_files};
pub use ssh::{PinnedHost, mixer_host_alias, pin_mixer_host_keys, ssh_key_path, ssh_public_key};

use serde::{Deserialize, Serialize};
use std::{
    fmt,
    path::{Path, PathBuf},
    time::Duration,
};

pub(crate) const SAVED_NAME: &str = "installer.json";
pub(crate) const MAX_DOWNLOAD: u64 = 600 << 20;
pub(crate) const MAX_SETTINGS: u64 = 16 << 20;
pub(crate) const DISK_BYTES: u64 = 560_000 * 512;
pub(crate) const KEY_MEMBER: &str = "home/root/.ssh/authorized_keys";
pub(crate) const SECOND: Duration = Duration::from_secs(1);

#[derive(Debug)]
pub struct Error(pub String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

macro_rules! from_error {
    ($($t:ty),*) => {$(
        impl From<$t> for Error {
            fn from(error: $t) -> Self {
                Error(error.to_string())
            }
        }
    )*};
}
from_error!(
    std::io::Error,
    serde_json::Error,
    reqwest::Error,
    String,
    &str
);

pub type Result<T> = std::result::Result<T, Error>;

macro_rules! bail {
    ($($t:tt)*) => {
        return Err($crate::Error(format!($($t)*)))
    };
}
pub(crate) use bail;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Release {
    pub minimum_app_version: String,
    /// Qualified firmware catalog ID used for signed mixer compatibility.
    pub profile: String,
    pub model: String,
    pub channel: String,
    pub version: String,
    pub url: String,
    pub digest_url: String,
    #[serde(rename = "rc2fx_supported")]
    pub supported: bool,
}

/// Firmware with RC2FX built in, ready to copy to the mixer.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prepared {
    /// Absent only for legacy Pro II caches; never inferred from the mixer hash.
    pub model: Option<rc2fx_firmware::Model>,
    pub version: String,
    pub channel: String,
    pub profile: String,
    pub archive_sha: String,
    pub archive_md5: String,
    /// An SSH key an older installer built in; empty in anything built now. A
    /// cached image with one is never reused or flashed.
    pub key: String,
    /// SHA-256 of that key's authorized_keys line.
    pub key_sha: String,
    /// SHA-256 of the SSH host key line built in with that key.
    pub host_key: String,
    /// SHA-256 of the RC2FX runtime built in; empty if none.
    pub runtime: String,
    /// SHA-256 of the settings built in; empty if none.
    pub settings: String,
    /// Unchanged RØDE firmware, without RC2FX.
    pub stock: bool,
}

/// A saved copy of a mixer's RC2FX settings (`state.json`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BackupRecord {
    pub serial: String,
    pub file: String,
    pub sha: String,
    pub created: String,
}

/// A USB command sent to a mixer. `acknowledged` is false while the result
/// is unknown; such a command is never sent again automatically.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CommandRecord {
    pub command: String,
    pub serial: String,
    pub at: String,
    /// Seconds since the Unix epoch, for "how long ago" checks.
    pub at_unix: u64,
    pub acknowledged: bool,
    pub error: String,
}

/// The work folder's cache.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Saved {
    pub prepared: Option<Prepared>,
    pub backups: Vec<BackupRecord>,
    pub commands: Vec<CommandRecord>,
}

impl Saved {
    /// The newest backup for a mixer.
    pub fn backup_for(&self, serial: &str) -> Option<&BackupRecord> {
        self.backups
            .iter()
            .rev()
            .find(|b| !serial.is_empty() && b.serial == serial)
    }

    pub fn last_command(&self) -> Option<&CommandRecord> {
        self.commands.last()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UsbMixer {
    pub model: DeviceModel,
    pub serial: String,
    pub update_mode: bool,
}

/// What RC2FX reports over USB 1, as the desktop app's connection sees it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Service {
    /// Running with advancing native audio and its settings applied.
    Ready,
    /// Answering, but its audio is not ready yet; the detail says why.
    Starting { detail: String },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Status {
    pub usb: Vec<UsbMixer>,
    pub usb_error: Option<String>,
    pub saved: Saved,
    /// Whether the prepared firmware's files are present (not re-hashed).
    pub prepared_present: bool,
    /// Whether the update drive of a mixer in USB update mode is visible.
    pub update_drive: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Request {
    /// Session policy, sent explicitly by the desktop's saved preference.
    AllowExperimentalDuo(bool),
    Probe,
    Releases,
    /// The stock firmware RØDE publishes for the connected model.
    StockReleases,
    /// Discover authenticated mixer releases. Does not select or install one.
    MixerReleases,
    /// Independently authenticate the exact offer reviewed by the user.
    SelectMixer {
        offer: mixer_update::MixerOffer,
        firmware_profile: String,
    },
    /// Explicitly select the optional compiled-in payload. No network fallback.
    SelectBundledMixer,
    /// Keep `settings` (RC2FX's settings file, read over USB by the app) for
    /// the mixer `serial`.
    SaveSettings {
        serial: String,
        settings: String,
    },
    /// Build firmware with RC2FX and the newest settings saved for `serial`,
    /// or with `stock`, stage the release unchanged.
    Prepare {
        channel: String,
        version: String,
        serial: String,
        stock: bool,
    },
    EnterUsbMode {
        serial: String,
    },
    /// Check the prepared firmware in the work folder against this engine's
    /// payload and its checksum files, without touching the mixer.
    Verify,
    Flash {
        serial: String,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Reply {
    Status(Box<Status>),
    Releases(Vec<Release>),
    StockReleases(Vec<Release>),
    MixerReleases(Vec<mixer_update::MixerOffer>),
    MixerSelected(Identity),
    Prepared(Prepared),
    Backup(BackupRecord),
    Done,
}

/// Progress reported synchronously on the action thread. `total` is zero
/// when unknown.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Progress {
    pub phase: String,
    pub completed: u64,
    pub total: u64,
    pub detail: String,
}

pub struct Engine {
    /// The payload and catalog this engine installs from: the app's own
    /// embedded copy, or a folder for developer tools and tests.
    pub(crate) source: embed::Source,
    pub work: PathBuf,
    pub platform: Box<dyn Platform>,
    pub progress: Option<Box<dyn FnMut(Progress) + Send>>,
    /// Set from another thread to stop a download; the request then fails
    /// with [`CANCELLED`]. Cleared when the next request starts.
    pub cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    http: Option<reqwest::blocking::Client>,
    model: DeviceModel,
    allow_experimental_duo: bool,
    /// Stands in for the helper's root-owned folder.
    #[cfg(test)]
    pub(crate) anchor_dir: Option<PathBuf>,
}

/// The error of a request stopped through [`Engine::cancel`].
pub const CANCELLED: &str = "Cancelled.";

impl Engine {
    /// Download-only desktop installer; no checkout or bundled runtime fallback.
    pub fn download_only(work: impl Into<PathBuf>) -> Self {
        Self::with_source(embed::Source::Download, work)
    }

    /// Prepare firmware from authenticated, owned download bytes. The flashing
    /// mechanism is identical to the embedded payload path.
    pub fn verified(payload: mixer_update::VerifiedMixerPayload, work: impl Into<PathBuf>) -> Self {
        Self::with_source(embed::Source::Verified(payload), work)
    }

    /// An engine that installs from the payload embedded in this executable,
    /// caching in `work`. This is what the desktop app uses: nothing has to sit
    /// beside it.
    #[cfg(feature = "embedded")]
    pub fn embedded(work: impl Into<PathBuf>) -> Self {
        Self::with_source(embed::Source::Embedded, work)
    }

    /// An engine for the installer folder `root`, caching in `work`.
    pub fn new(root: impl Into<PathBuf>, work: impl Into<PathBuf>) -> Self {
        Self::with_source(embed::Source::folder(root), work)
    }

    pub fn with_platform(
        root: impl Into<PathBuf>,
        work: impl Into<PathBuf>,
        platform: Box<dyn Platform>,
    ) -> Self {
        Self::with_source_platform(embed::Source::folder(root), work, platform)
    }

    pub(crate) fn with_source(source: embed::Source, work: impl Into<PathBuf>) -> Self {
        Self::with_source_platform(source, work, platform::host())
    }

    pub(crate) fn with_source_platform(
        source: embed::Source,
        work: impl Into<PathBuf>,
        platform: Box<dyn Platform>,
    ) -> Self {
        Self {
            source,
            work: work.into(),
            platform,
            progress: None,
            cancel: Default::default(),
            http: None,
            model: DeviceModel::ProIi,
            allow_experimental_duo: false,
            #[cfg(test)]
            anchor_dir: None,
        }
    }

    pub(crate) fn cancelled(&self) -> bool {
        self.cancel.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub(crate) fn emit(&mut self, event: Progress) {
        if let Some(progress) = self.progress.as_mut() {
            progress(event);
        }
    }

    /// A byte-progress callback for one phase, reporting about every MiB.
    pub(crate) fn reporter<'a>(
        &'a mut self,
        phase: &'a str,
        detail: &'a str,
    ) -> impl FnMut(u64, u64) + 'a {
        let mut last: Option<u64> = None;
        move |completed, total| {
            if last.is_none_or(|l| completed >= l + (1 << 20) || completed == total) {
                last = Some(completed);
                self.emit(Progress {
                    phase: phase.into(),
                    completed,
                    total,
                    detail: detail.into(),
                });
            }
        }
    }

    pub(crate) fn step(&mut self, phase: &str, detail: &str) {
        self.emit(Progress {
            phase: phase.into(),
            detail: detail.into(),
            ..Default::default()
        });
    }

    pub fn handle(&mut self, request: Request) -> Result<Reply> {
        files::private_dirs(&self.work)?;
        // Two installers must not interleave USB commands or cache writes.
        let lock = files::open_local_file(&self.work.join("installer.lock"), false)?;
        let _guard = files::lock_action(&lock).map_err(|_| {
            Error("another RC2FX installer is working with this mixer folder".into())
        })?;
        if matches!(self.source, embed::Source::Download)
            && matches!(
                request,
                Request::Releases
                    | Request::Prepare { .. }
                    | Request::Verify
                    | Request::EnterUsbMode { .. }
                    | Request::Flash { .. }
            )
        {
            let work = self.work.clone();
            self.step("firmware-check", "Checking signed firmware profiles");
            self.mixer_operation(async {
                let repository = mixer_update::official(&work).await?;
                mixer_update::refresh_catalog(&repository).await
            })?;
        }
        match request {
            Request::AllowExperimentalDuo(allow) => {
                self.allow_experimental_duo = allow;
                Ok(Reply::Status(Box::new(self.probe()?)))
            }
            Request::Probe => Ok(Reply::Status(Box::new(self.probe()?))),
            Request::Releases => Ok(Reply::Releases(self.releases()?)),
            Request::StockReleases => {
                self.step("stock-check", "Checking RØDE's firmware list");
                Ok(Reply::StockReleases(self.stock_releases()?))
            }
            Request::MixerReleases => {
                self.step("mixer-check", "Checking signed mixer releases");
                let work = self.work.clone();
                let offers = self.mixer_operation(async {
                    let repository = mixer_update::official(&work).await?;
                    mixer_update::refresh_catalog(&repository).await?;
                    repository.offers()
                })?;
                Ok(Reply::MixerReleases(offers))
            }
            Request::SelectMixer {
                offer,
                firmware_profile,
            } => {
                self.step("mixer-download", "Downloading and verifying mixer software");
                // An unsuccessful selection cannot leave an old payload silently selected.
                self.source = embed::Source::Download;
                let work = self.work.clone();
                let payload = self.mixer_operation(async {
                    mixer_update::official(&work)
                        .await?
                        .accept_cached(
                            &offer,
                            env!("CARGO_PKG_VERSION"),
                            rc2fx_core::MIXER_PROTOCOL_VERSION,
                            &firmware_profile,
                            &mixer_update::metadata_directory(&work)?.join("payloads"),
                        )
                        .await
                })?;
                let identity = embed::identity_of("download", &payload.bundle).map_err(Error)?;
                self.source = embed::Source::Verified(payload);
                Ok(Reply::MixerSelected(identity))
            }
            Request::SelectBundledMixer => {
                // Match downloaded selection: failure must discard the old choice.
                self.source = embed::Source::Download;
                let identity = embed::identity()?;
                self.source = embed::Source::Embedded;
                Ok(Reply::MixerSelected(identity))
            }
            Request::SaveSettings { serial, settings } => {
                Ok(Reply::Backup(self.save_settings(&serial, &settings)?))
            }
            Request::Prepare {
                channel,
                version,
                serial,
                stock,
            } => {
                // The helper is otherwise probed only when it starts, and the
                // image is built for the mixer connected now.
                self.probe()?;
                Ok(Reply::Prepared(
                    self.prepare(&channel, &version, &serial, stock)?,
                ))
            }
            Request::Verify => {
                let prepared = self
                    .load()?
                    .prepared
                    .clone()
                    .ok_or("there is no prepared firmware to check")?;
                self.verify_prepared(&prepared)?;
                Ok(Reply::Prepared(prepared))
            }
            Request::EnterUsbMode { serial } => {
                self.enter_usb_mode(&serial)?;
                Ok(Reply::Done)
            }
            Request::Flash { serial } => {
                self.flash(&serial)?;
                Ok(Reply::Done)
            }
        }
    }

    pub(crate) fn load(&self) -> Result<Saved> {
        load_saved(&self.work)
    }

    pub(crate) fn save(&self, saved: &Saved) -> Result<()> {
        let mut data = serde_json::to_vec_pretty(saved)?;
        data.push(b'\n');
        files::atomic_file(&self.work.join(SAVED_NAME), &data)
    }

    pub(crate) fn probe(&mut self) -> Result<Status> {
        let saved = self.load()?;
        let (usb, usb_error) = match self.platform.inventory() {
            Ok(items) => {
                let mixers = usb::mixers(&items);
                if let [one] = mixers.as_slice() {
                    self.model = one.model;
                }
                let error = if mixers.len() > 1 {
                    Some("More than one RØDECaster is connected; connect only one.".into())
                } else {
                    usb::reject_experimental(&items, "", self.allow_experimental_duo)
                        .err()
                        .map(|e| e.0)
                };
                (mixers, error)
            }
            Err(e) => (Vec::new(), Some(e.0)),
        };
        let (update_drive, drive_error) = if usb.iter().any(|m| m.update_mode) {
            match self.platform.volumes() {
                Ok(volumes) => (
                    volumes.iter().any(|v| {
                        usb::update_size(v.total)
                            && usb.iter().any(|m| m.update_mode && m.serial == v.serial)
                    }),
                    None,
                ),
                Err(e) => (false, Some(e.0)),
            }
        } else {
            (false, None)
        };
        let prepared_present = saved.prepared.is_some()
            && files::exists(&self.work.join("archive.tar.gz"))
            && files::exists(&self.work.join("archive.md5"));
        Ok(Status {
            usb,
            usb_error: usb_error.or(drive_error),
            saved,
            prepared_present,
            update_drive,
        })
    }
}

/// Read the cache; empty if there is none yet.
pub fn load_saved(work: &Path) -> Result<Saved> {
    match files::open_work_file(&work.join(SAVED_NAME)) {
        Ok(file) => Ok(serde_json::from_reader(std::io::BufReader::new(file))?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Saved::default()),
        Err(e) => Err(e.into()),
    }
}

pub(crate) fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub(crate) fn utc() -> String {
    let secs = now_unix();
    let (days, rem) = (secs / 86_400, secs % 86_400);
    // Civil-from-days (Howard Hinnant), valid for the Unix era.
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

pub fn default_key() -> PathBuf {
    files::home_dir()
        .unwrap_or_default()
        .join(".ssh")
        .join("rc2fx_rodecaster_pro_ii")
}

/// The installer's cache: progress, downloads and backups. It lives in the
/// user's data folder, so it survives moving or replacing the app.
pub fn default_work() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())?;
    #[cfg(target_os = "macos")]
    let base = files::home_dir()?.join("Library/Application Support");
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| files::home_dir().map(|h| h.join(".local/share")))?;
    Some(base.join("RC2FX").join("installer"))
}

#[cfg(test)]
mod tests;
