//! Signed in-place RC2FX updates over USB 1 without reflashing stock firmware.
//!
//! Verifies TUF role metadata and payload bytes against the compiled-in trust
//! root, swaps `/opt/rc2fx` in place with a rollback copy, and hands restart
//! and commit off to a detached `rc2fx.sh update-commit` process. After
//! reconnect, `GET api/v1/update/last` reports the outcome.
//!
//! First installs and stock firmware changes still require a full flash. Nothing
//! from an uploaded payload executes until signature, size, digest, firmware
//! profile, and version checks pass. `delta-1` uploads (`rc2fx-delta`) are
//! reconstructed into the signed envelope first, then verified identically.

pub mod install;
pub mod payload;
pub mod verify;

use install::{LastOutcome, Paths, Pending, StoredState};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{self, DirBuilder, File, OpenOptions},
    io::{Read, Write},
    os::unix::{
        fs::{DirBuilderExt, OpenOptionsExt},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

/// What `GET api/v1/version` reports as `inPlaceUpdates`. 2 adds the installed
/// file listing and delta uploads; the app treats any value above zero as
/// in-place capable.
pub const IN_PLACE_UPDATES: u32 = 2;
/// The delta encoding this mixer accepts, sent as `"encoding": "delta-1"`.
pub const DELTA_ENCODING: &str = "delta-1";
/// The largest payload this API accepts; the real one is about 11 MiB.
pub const MAX_PAYLOAD: u64 = 64 << 20;
/// The most raw bytes one `PUT` may carry. Base64 of this stays well inside
/// the 512 KiB USB request limit.
pub const MAX_CHUNK: usize = 256 << 10;
/// Free space kept in `/run` around a session.
const RUN_HEADROOM: u64 = 8 << 20;
/// Free space kept in `/opt` beside the staged bundle.
const OPT_HEADROOM: u64 = 16 << 20;
/// How long the detached driver waits for the new controller and audio.
const RESTART_SECONDS: u64 = 300;
/// Filename the desktop app writes to the temporary update drive (`rc2fx.sh update-drive`).
pub const DRIVE_FILE: &str = "RC2FX.UPD";
/// Volume label for the temporary update drive.
pub const DRIVE_LABEL: &str = "RC2FX";
/// Maximum payload size allowed on the update drive. The app only scans
/// removable disks smaller than 64 MiB so it doesn't confuse this drive with
/// the mixer's update-mode drive, leaving room for FAT overhead and slack.
const DRIVE_MAX_UPLOAD: u64 = 48 << 20;
/// Extra space reserved alongside the payload for the FAT filesystem and root directory.
const DRIVE_SLACK: u64 = 4 << 20;

/// Which installed copy a controller was started from, judged by the
/// directory that holds its release manifest (`rc2fx.sh launch` passes
/// `--release "$home/mixer-release.json"`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstalledBundle {
    /// `/opt/rc2fx`: the bundle in-place updates install into.
    Live,
    /// `/opt/.rc2fx-previous`: the rollback copy the shims start while an
    /// update has not committed. Its controller must still run `recover()`,
    /// or the unfinished update is never undone, but it offers no new update.
    Previous,
}

/// Where a release manifest places the controller: `None` for anything that
/// is not an installed bundle, such as a build run from RAM.
pub fn installed_bundle(paths: &Paths, release: &Path) -> Option<InstalledBundle> {
    let dir = release.parent()?;
    if dir == paths.bundle() {
        Some(InstalledBundle::Live)
    } else if dir == paths.previous() {
        Some(InstalledBundle::Previous)
    } else {
        None
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StartRequest {
    pub version: String,
    pub size: u64,
    pub sha256: String,
    pub metadata: verify::MetadataInput,
    /// Refused by default: releases only move forward.
    #[serde(default)]
    pub allow_downgrade: bool,
    /// `"delta-1"` when the upload is a delta rather than the signed envelope.
    #[serde(default)]
    pub encoding: Option<String>,
    /// The delta's length. Required with `encoding`, refused without it.
    #[serde(default)]
    pub upload_size: Option<u64>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Chunk {
    pub offset: u64,
    pub data: String,
}

/// A session's path: Accepting, then Complete once every upload byte is in,
/// then Installing, which ends in Restarting or Failed. A Failed session may
/// be installed again; an Accepting or Complete one may be replaced or
/// aborted.
#[derive(Clone, Debug, PartialEq, Eq)]
enum SessionState {
    Accepting,
    Complete,
    Installing,
    /// The install handed the restart on. `warning` says the swap failed
    /// part way and the previous version is being restored.
    Restarting {
        warning: Option<String>,
    },
    /// Verification or the install failed, for this reason.
    Failed(String),
}

impl SessionState {
    fn name(&self) -> &'static str {
        match self {
            Self::Accepting => "accepting",
            Self::Complete => "complete",
            Self::Installing => "installing",
            Self::Restarting { .. } => "restarting",
            Self::Failed(_) => "failed",
        }
    }
    /// What the progress reply carries as its error.
    fn error(&self) -> Option<&str> {
        match self {
            Self::Restarting { warning } => warning.as_deref(),
            Self::Failed(why) => Some(why),
            Self::Accepting | Self::Complete | Self::Installing => None,
        }
    }
}

/// What a session's upload holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Upload {
    /// The signed envelope itself.
    Envelope,
    /// A `delta-1` encoding of it. `decoded` says the envelope has been
    /// rebuilt from it once, so a retried install verifies that envelope
    /// instead of decoding again.
    Delta { decoded: bool },
}

impl Upload {
    fn encoding(self) -> Option<&'static str> {
        match self {
            Self::Envelope => None,
            Self::Delta { .. } => Some(DELTA_ENCODING),
        }
    }
}

struct Session {
    /// The id `start` handed out; every other call must name it.
    id: String,
    request: StartRequest,
    /// The number of upload bytes this session accepts and reports progress on.
    upload_size: u64,
    received: u64,
    state: SessionState,
    /// The envelope size is `request.size` either way.
    upload: Upload,
    /// Path to the temporary update drive image while it's attached for this session.
    drive: Option<PathBuf>,
}

/// Response for `POST api/v1/update/{id}/drive`, describing the temporary drive to copy into.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DriveOffer {
    pub id: String,
    pub label: &'static str,
    pub file: &'static str,
    pub size: u64,
}

/// The reply the upload endpoints return.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub id: String,
    pub state: &'static str,
    pub version: String,
    /// The signed envelope's size, even for a delta upload.
    pub size: u64,
    /// Upload bytes received; the envelope's size for a plain upload.
    pub received: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encoding: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upload_size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// `GET api/v1/update/installed`: the running release and the digest of each
/// payload file the mixer holds, which is what the app diffs against.
#[derive(Clone, Debug, Serialize)]
pub struct Installed {
    pub version: String,
    pub files: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UpdateError {
    /// A malformed request (400).
    Invalid(String),
    /// No such session, or the installed files cannot be described here (404).
    NotFound,
    /// The running release is unknown, so there is nothing to diff (404).
    Unavailable(String),
    /// Another update or a full-flash install is running (409).
    Busy(String),
    /// The request conflicts with this mixer's state or trust (409).
    Conflict(String),
    /// A chunk does not continue where the upload is; the client resumes at
    /// `expected` (409).
    Offset { expected: u64 },
    /// Not enough RAM or disk (507).
    Storage(String),
    /// The mixer's own failure (500).
    Failed(String),
}

impl UpdateError {
    pub fn status(&self) -> u16 {
        match self {
            Self::Invalid(_) => 400,
            Self::NotFound | Self::Unavailable(_) => 404,
            Self::Busy(_) | Self::Conflict(_) | Self::Offset { .. } => 409,
            Self::Storage(_) => 507,
            Self::Failed(_) => 500,
        }
    }
}

impl std::fmt::Display for UpdateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(why)
            | Self::Busy(why)
            | Self::Conflict(why)
            | Self::Storage(why)
            | Self::Failed(why)
            | Self::Unavailable(why) => f.write_str(why),
            Self::NotFound => f.write_str("No such update session"),
            Self::Offset { expected } => {
                write!(f, "The upload does not continue at offset {expected}")
            }
        }
    }
}

/// The shared handle the USB API uses. Only the USB 1 server is given one.
pub struct MixerUpdate {
    paths: Paths,
    profile: Option<String>,
    running: Option<String>,
    trusted_root: Vec<u8>,
    session: Mutex<Option<Session>>,
    /// False for a controller started from the rollback copy: it recovers and
    /// reports, but new updates wait for a start from the live bundle.
    offers_updates: bool,
}

impl MixerUpdate {
    /// `running` is the installed release's version, when the launcher could
    /// read it; `profile` is the firmware profile the launcher validated.
    pub fn new(paths: Paths, profile: Option<String>, running: Option<String>) -> Self {
        Self {
            paths,
            profile,
            running,
            trusted_root: verify::TRUSTED_ROOT.to_vec(),
            session: Mutex::new(None),
            offers_updates: true,
        }
    }

    /// A handle for a controller started from the rollback copy
    /// ([`InstalledBundle::Previous`]): `recover()` and the outcome endpoints
    /// work, and every new update is refused.
    pub fn without_new_updates(mut self) -> Self {
        self.offers_updates = false;
        self
    }

    /// Whether this controller accepts new updates (and so advertises
    /// `inPlaceUpdates`).
    pub fn offers_updates(&self) -> bool {
        self.offers_updates
    }

    fn session(&self) -> std::sync::MutexGuard<'_, Option<Session>> {
        self.session.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn payload_path(&self, id: &str) -> PathBuf {
        self.paths.run.join(id).join("payload")
    }

    pub fn start(&self, request: StartRequest) -> Result<Progress, UpdateError> {
        if rc2fx_core::compare_versions(&request.version, &request.version).is_none() {
            return Err(UpdateError::Invalid(
                "version must be dotted numeric (for example 0.2.0)".into(),
            ));
        }
        if request.size == 0 || request.size > MAX_PAYLOAD {
            return Err(UpdateError::Invalid(format!(
                "size must be between 1 and {MAX_PAYLOAD} bytes"
            )));
        }
        if request.sha256.len() != 64
            || !request
                .sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(UpdateError::Invalid(
                "sha256 must be 64 lowercase hexadecimal characters".into(),
            ));
        }
        let encoding = match request.encoding.as_deref() {
            None => None,
            Some(value) if value == DELTA_ENCODING => Some(DELTA_ENCODING),
            Some(_) => {
                return Err(UpdateError::Invalid(
                    "encoding must be \"delta-1\" when it is present".into(),
                ));
            }
        };
        let upload_size = match (encoding, request.upload_size) {
            (Some(_), Some(upload)) if (1..=MAX_PAYLOAD).contains(&upload) => upload,
            (Some(_), Some(_)) => {
                return Err(UpdateError::Invalid(format!(
                    "uploadSize must be between 1 and {MAX_PAYLOAD} bytes"
                )));
            }
            (Some(_), None) => {
                return Err(UpdateError::Invalid(
                    "uploadSize is required with encoding delta-1".into(),
                ));
            }
            (None, Some(_)) => {
                return Err(UpdateError::Invalid(
                    "uploadSize is only accepted with encoding delta-1".into(),
                ));
            }
            (None, None) => request.size,
        };
        if !self.offers_updates {
            return Err(UpdateError::Conflict(
                "RC2FX is running its rollback copy; restart the mixer before updating".into(),
            ));
        }
        // Refused before the upload, not only after it: the record cannot
        // become readable while the upload runs.
        self.stored_state().map_err(UpdateError::Conflict)?;
        if self.paths.pending().exists() || self.paths.install_pending().exists() {
            return Err(UpdateError::Busy(
                "another install is pending; finish it or recover before updating".into(),
            ));
        }
        let mut slot = self.session();
        // Only USB 1's one client uploads, so a session it started and then
        // abandoned (the app closed mid-upload) must not block the next: a new
        // start replaces any session that isn't installing yet, and frees its
        // upload in RAM before the space check below.
        if let Some(session) = slot.as_mut() {
            if matches!(
                session.state,
                SessionState::Installing | SessionState::Restarting { .. }
            ) {
                return Err(UpdateError::Busy(
                    "an update is being installed; wait for it to finish".into(),
                ));
            }
            self.drop_drive(session);
            let _ = fs::remove_dir_all(self.paths.run.join(&session.id));
            *slot = None;
        }
        // A delta needs room for its upload and then for the envelope built
        // from it; a plain upload is the envelope already.
        let run_needed = request
            .size
            .saturating_add(if encoding.is_some() { upload_size } else { 0 })
            .saturating_add(RUN_HEADROOM);
        if let Some(free) = install::free_bytes(&self.paths.run)
            && free < run_needed
        {
            return Err(UpdateError::Storage(format!(
                "not enough RAM for the upload: {free} bytes free in {}, {run_needed} needed",
                self.paths.run.display(),
            )));
        }
        let opt_needed = request.size.saturating_add(OPT_HEADROOM);
        if let Some(free) = install::free_bytes(&self.paths.opt)
            && free < opt_needed
        {
            return Err(UpdateError::Storage(format!(
                "not enough space to install: {free} bytes free on {}, {opt_needed} needed",
                self.paths.opt.display(),
            )));
        }
        let id = new_id();
        let directory = self.paths.run.join(&id);
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&directory)
            .map_err(|e| UpdateError::Failed(format!("cannot create the upload session: {e}")))?;
        let payload = directory.join("payload");
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&payload)
            .map_err(|e| UpdateError::Failed(format!("cannot create the upload file: {e}")))?;
        *slot = Some(Session {
            id: id.clone(),
            request: request.clone(),
            upload_size,
            received: 0,
            state: SessionState::Accepting,
            upload: if encoding.is_some() {
                Upload::Delta { decoded: false }
            } else {
                Upload::Envelope
            },
            drive: None,
        });
        Ok(Progress {
            id,
            state: SessionState::Accepting.name(),
            version: request.version,
            size: request.size,
            received: 0,
            encoding,
            upload_size: encoding.map(|_| upload_size),
            error: None,
        })
    }

    pub fn append(&self, id: &str, chunk: Chunk) -> Result<Progress, UpdateError> {
        let data = decode_base64(&chunk.data)
            .ok_or_else(|| UpdateError::Invalid("data is not valid base64".into()))?;
        if data.is_empty() || data.len() > MAX_CHUNK {
            return Err(UpdateError::Invalid(format!(
                "each chunk carries 1 to {MAX_CHUNK} bytes"
            )));
        }
        let mut slot = self.session();
        let session = slot
            .as_mut()
            .filter(|s| s.id == id)
            .ok_or(UpdateError::NotFound)?;
        if session.drive.is_some() {
            return Err(UpdateError::Conflict(
                "the upload is coming through the update drive".into(),
            ));
        }
        let expected = session.received;
        // A replay of a chunk whose reply was lost is answered while the
        // upload is still accepting and also once it is complete: the last
        // chunk moves the session to Complete, and its replay must still get
        // the progress reply rather than a refusal.
        match session.state {
            SessionState::Accepting | SessionState::Complete => {}
            SessionState::Failed(_) => {
                return Err(UpdateError::Conflict(
                    "this session failed; start a new one".into(),
                ));
            }
            _ => {
                return Err(UpdateError::Conflict(
                    "the upload is already complete".into(),
                ));
            }
        }
        let path = self.payload_path(id);
        let offset = chunk.offset;
        if offset < expected {
            // A reply that was lost: the same bytes are accepted again.
            if offset + data.len() as u64 > expected {
                return Err(UpdateError::Offset { expected });
            }
            let mut stored = vec![0u8; data.len()];
            File::open(&path)
                .and_then(|mut file| {
                    use std::io::{Seek, SeekFrom};
                    file.seek(SeekFrom::Start(offset))?;
                    file.read_exact(&mut stored)
                })
                .map_err(|e| UpdateError::Failed(format!("cannot read the upload: {e}")))?;
            if stored != data {
                return Err(UpdateError::Offset { expected });
            }
            let progress = progress_of(id, session);
            return Ok(progress);
        }
        if matches!(session.state, SessionState::Complete) {
            return Err(UpdateError::Conflict(
                "the upload is already complete".into(),
            ));
        }
        if offset != expected {
            return Err(UpdateError::Offset { expected });
        }
        if expected + data.len() as u64 > session.upload_size {
            return Err(UpdateError::Invalid(
                "the chunk goes past the size the session declared".into(),
            ));
        }
        let written = OpenOptions::new()
            .append(true)
            .open(&path)
            .and_then(|mut file| file.write_all(&data));
        if let Err(error) = written {
            return Err(UpdateError::Failed(format!(
                "cannot write the upload: {error}"
            )));
        }
        session.received += data.len() as u64;
        if session.received == session.upload_size {
            session.state = SessionState::Complete;
        }
        Ok(progress_of(id, session))
    }

    pub fn progress(&self, id: &str) -> Result<Progress, UpdateError> {
        let slot = self.session();
        let session = slot
            .as_ref()
            .filter(|s| s.id == id)
            .ok_or(UpdateError::NotFound)?;
        Ok(progress_of(id, session))
    }

    pub fn abort(&self, id: &str) -> Result<(), UpdateError> {
        let mut slot = self.session();
        let Some(session) = slot.as_mut().filter(|s| s.id == id) else {
            return Err(UpdateError::NotFound);
        };
        if matches!(
            session.state,
            SessionState::Installing | SessionState::Restarting { .. }
        ) {
            return Err(UpdateError::Conflict(
                "the update is being installed and cannot be aborted".into(),
            ));
        }
        self.drop_drive(session);
        let _ = fs::remove_dir_all(self.paths.run.join(id));
        *slot = None;
        Ok(())
    }

    /// Attaches a temporary FAT disk image in RAM to RØDE's removable USB drive
    /// for a session that hasn't received any bytes yet. Copying the payload
    /// over mass storage is much faster than streaming it over USB 1 control.
    /// Fails if the drive is already in use or can't be created, in which case
    /// the app falls back to USB 1.
    pub fn attach_drive(&self, id: &str) -> Result<DriveOffer, UpdateError> {
        let mut slot = self.session();
        let session = slot
            .as_mut()
            .filter(|s| s.id == id)
            .ok_or(UpdateError::NotFound)?;
        if session.state != SessionState::Accepting || session.received != 0 {
            return Err(UpdateError::Conflict(
                "the update drive is only for an upload that has not started".into(),
            ));
        }
        if session.drive.is_some() {
            return Ok(drive_offer(session));
        }
        if session.upload_size > DRIVE_MAX_UPLOAD {
            return Err(UpdateError::Conflict(
                "the upload is too large for the update drive".into(),
            ));
        }
        let disk = (session.upload_size + DRIVE_SLACK).next_multiple_of(1 << 20);
        // Both the temporary disk image and the extracted payload live in RAM.
        if let Some(free) = install::free_bytes(&self.paths.run)
            && free < disk.saturating_add(RUN_HEADROOM)
        {
            return Err(UpdateError::Storage(format!(
                "not enough RAM for the update drive: {free} bytes free, {disk} needed",
            )));
        }
        let image = self.paths.run.join(id).join("drive.img");
        self.drive_script(&[
            "attach".as_ref(),
            image.as_os_str(),
            disk.to_string().as_ref(),
        ])
        .map_err(UpdateError::Conflict)?;
        session.drive = Some(image);
        Ok(drive_offer(session))
    }

    /// Detaches the temporary update drive and moves the uploaded file into the
    /// session as if every chunk had arrived over USB 1. If reading the file
    /// fails, the drive is released and the session falls back to accepting
    /// USB 1 chunks.
    pub fn take_drive(&self, id: &str) -> Result<Progress, UpdateError> {
        let mut slot = self.session();
        let session = slot
            .as_mut()
            .filter(|s| s.id == id)
            .ok_or(UpdateError::NotFound)?;
        let Some(image) = session.drive.take() else {
            return Err(UpdateError::Conflict(
                "this session has no update drive".into(),
            ));
        };
        let payload = self.payload_path(id);
        let taken = self.drive_script(&[
            "take".as_ref(),
            image.as_os_str(),
            payload.as_os_str(),
            session.upload_size.to_string().as_ref(),
        ]);
        if let Err(why) = taken {
            self.drive_script_quiet(&["drop".as_ref(), image.as_os_str()]);
            // Clear any partial file left by a failed copy so USB 1 fallback starts clean.
            let _ = OpenOptions::new().write(true).truncate(true).open(&payload);
            return Err(UpdateError::Conflict(why));
        }
        session.received = session.upload_size;
        session.state = SessionState::Complete;
        Ok(progress_of(id, session))
    }

    /// Detaches and removes the temporary update drive without taking any file from it.
    pub fn release_drive(&self, id: &str) -> Result<Progress, UpdateError> {
        let mut slot = self.session();
        let session = slot
            .as_mut()
            .filter(|s| s.id == id)
            .ok_or(UpdateError::NotFound)?;
        self.drop_drive(session);
        Ok(progress_of(id, session))
    }

    fn drop_drive(&self, session: &mut Session) {
        if let Some(image) = session.drive.take() {
            self.drive_script_quiet(&["drop".as_ref(), image.as_os_str()]);
        }
    }

    /// Runs `rc2fx.sh update-drive` from the installed bundle and returns the
    /// script's error output on failure.
    fn drive_script(&self, args: &[&std::ffi::OsStr]) -> Result<(), String> {
        let output = std::process::Command::new("/bin/bash")
            .arg(self.paths.bundle().join("rc2fx.sh"))
            .arg("update-drive")
            .args(args)
            .env_clear()
            .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|e| format!("cannot run the update drive script: {e}"))?;
        if output.status.success() {
            return Ok(());
        }
        let message = String::from_utf8_lossy(&output.stderr);
        let message = message.trim();
        Err(if message.is_empty() {
            "the update drive could not be used".into()
        } else {
            format!(
                "the update drive could not be used: {}",
                message.lines().last().unwrap_or(message)
            )
        })
    }

    fn drive_script_quiet(&self, args: &[&std::ffi::OsStr]) {
        if let Err(why) = self.drive_script(args) {
            eprintln!("RC2FX update: {why}");
        }
    }

    /// Verify the upload and start installing it. Verification is synchronous
    /// (so its refusal is the reply); the install and swap continue on a
    /// thread of their own and are reported by [`Self::progress`].
    pub fn install(self: Arc<Self>, id: &str) -> Result<Progress, UpdateError> {
        let (request, upload) = {
            let mut slot = self.session();
            let session = slot
                .as_mut()
                .filter(|s| s.id == id)
                .ok_or(UpdateError::NotFound)?;
            match session.state {
                SessionState::Complete | SessionState::Failed(_) => {}
                SessionState::Accepting => {
                    return Err(UpdateError::Conflict(
                        "the upload is not complete yet".into(),
                    ));
                }
                _ => return Ok(progress_of(id, session)),
            }
            session.state = SessionState::Installing;
            (session.request.clone(), session.upload)
        };
        // A delta is rebuilt into the signed envelope before verification, so
        // everything after this is exactly the plain-upload path. The envelope
        // is checked against the signed target there; nothing decoded here is
        // trusted until then.
        if matches!(upload, Upload::Delta { decoded: false }) {
            let envelope_needed = request.size.saturating_add(RUN_HEADROOM);
            if let Some(free) = install::free_bytes(&self.paths.run)
                && free < envelope_needed
            {
                let why = format!(
                    "not enough RAM to rebuild the release: {free} bytes free in {}, {envelope_needed} needed",
                    self.paths.run.display(),
                );
                self.fail(id, &why);
                return Err(UpdateError::Storage(why));
            }
            if let Err(why) = decode_delta(&self.paths, &self.payload_path(id), request.size) {
                self.fail(id, &why);
                return Err(UpdateError::Conflict(why));
            }
            let mut slot = self.session();
            if let Some(session) = slot.as_mut().filter(|s| s.id == id) {
                session.upload = Upload::Delta { decoded: true };
            }
        }
        let payload = self.payload_path(id);
        let prior = match self.stored_state() {
            Ok(prior) => prior,
            Err(why) => {
                self.fail(id, &why);
                return Err(UpdateError::Conflict(why));
            }
        };
        let facts = verify::ReleaseRequest {
            version: request.version.clone(),
            size: request.size,
            sha256: request.sha256.clone(),
            allow_downgrade: request.allow_downgrade,
        };
        let verified = match verify::verify(
            &self.trusted_root,
            &request.metadata,
            &facts,
            &payload,
            self.profile.as_deref(),
            self.running.as_deref(),
            &prior,
        ) {
            Ok(verified) => verified,
            Err(why) => {
                self.fail(id, &why);
                return Err(UpdateError::Conflict(why));
            }
        };
        // Advance the replay protection before anything else can fail. A
        // failed install must not let an older metadata set through later.
        if let Err(error) = self.save_state(&verified.metadata_versions) {
            self.fail(id, &error);
            return Err(UpdateError::Failed(error));
        }
        // Accepted: the previous outcome must not pass for this one's.
        if let Err(error) = install::clear_last(&self.paths) {
            let why = format!("cannot clear the previous update's outcome: {error}");
            self.fail(id, &why);
            return Err(UpdateError::Failed(why));
        }
        let this = Arc::clone(&self);
        let thread_id = id.to_owned();
        let request_id = id.to_owned();
        let spawned = std::thread::Builder::new()
            .name("rc2fx-update".into())
            .spawn(move || this.run_install(&thread_id, verified));
        if let Err(error) = spawned {
            let why = format!("cannot start the install: {error}");
            self.fail(&request_id, &why);
            return Err(UpdateError::Failed(why));
        }
        let slot = self.session();
        let session = slot
            .as_ref()
            .filter(|s| s.id == id)
            .ok_or(UpdateError::NotFound)?;
        Ok(progress_of(&request_id, session))
    }

    fn fail(&self, id: &str, why: &str) {
        let mut slot = self.session();
        if let Some(session) = slot.as_mut().filter(|s| s.id == id)
            && !matches!(session.state, SessionState::Restarting { .. })
        {
            session.state = SessionState::Failed(why.to_owned());
        }
    }

    /// Records how the install thread ended, if the session is still there.
    fn set_state(&self, id: &str, state: SessionState) {
        let mut slot = self.session();
        if let Some(session) = slot.as_mut().filter(|s| s.id == id) {
            session.state = state;
        }
    }

    /// The recorded replay and downgrade protection, empty before the first
    /// record. An unreadable record refuses the update instead of starting
    /// over, which would accept older signed metadata.
    fn stored_state(&self) -> Result<StoredState, String> {
        StoredState::load(&self.paths.state())
            .map(Option::unwrap_or_default)
            .map_err(|why| {
                eprintln!("RC2FX update: {why}");
                format!(
                    "the mixer's update record is unreadable, so no update can be checked against it ({why}); a full flash replaces it"
                )
            })
    }

    fn save_state(&self, state: &StoredState) -> Result<(), String> {
        let state = state.clone();
        if let Err(error) = DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(self.paths.data())
        {
            return Err(format!(
                "cannot create {}: {error}",
                self.paths.data().display()
            ));
        }
        state
            .save(&self.paths.state())
            .map_err(|e| format!("cannot record the update state: {e}"))
    }

    /// The install thread: read the payload, check its contents against the
    /// signed claims, stage it, swap it in, then hand the restart on.
    fn run_install(&self, id: &str, verified: verify::VerifiedRelease) {
        match self.install_inner(id, &verified) {
            Ok(()) => {
                self.set_state(id, SessionState::Restarting { warning: None });
            }
            Err(failure) => {
                if failure.swapped_in_doubt {
                    // Files may already be mixed. Start the previous bundle
                    // through the shim; its controller restores and records.
                    let _ = self.spawn_driver(id, &verified, true, &install::Swapped::default());
                    self.set_state(
                        id,
                        SessionState::Restarting {
                            warning: Some(format!(
                                "{}; the previous version is being restored",
                                failure.message
                            )),
                        },
                    );
                } else {
                    let _ = fs::remove_dir_all(self.paths.opt.join(format!(".rc2fx-stage.{id}")));
                    self.set_state(id, SessionState::Failed(failure.message));
                }
            }
        }
    }

    fn install_inner(
        &self,
        id: &str,
        verified: &verify::VerifiedRelease,
    ) -> Result<(), install::InstallFailure> {
        let payload_path = self.payload_path(id);
        let mut file = File::open(&payload_path).map_err(|e| install::InstallFailure {
            message: format!("cannot read the uploaded payload: {e}"),
            swapped_in_doubt: false,
        })?;
        let files = payload::read(&mut file).map_err(|e| install::InstallFailure {
            message: format!("the uploaded payload was refused: {e}"),
            swapped_in_doubt: false,
        })?;
        // The payload's own release identity and catalog must agree with the
        // signed claims, exactly as the desktop checks before flashing.
        let release = files
            .iter()
            .find(|(name, _)| name == "mixer-release.json")
            .map(|(_, data)| data)
            .ok_or_else(|| install::InstallFailure {
                message: "the payload carries no mixer-release.json".into(),
                swapped_in_doubt: false,
            })?;
        let release =
            rc2fx_core::MixerRelease::parse(release).map_err(|e| install::InstallFailure {
                message: format!("the payload's mixer-release.json is invalid: {e}"),
                swapped_in_doubt: false,
            })?;
        if release.version != verified.version
            || release.protocol_version != verified.metadata.protocol_version
        {
            return Err(install::InstallFailure {
                message: "the payload's release does not match the signed metadata".into(),
                swapped_in_doubt: false,
            });
        }
        let catalog = files
            .iter()
            .find(|(name, _)| name == "firmware-profiles.json")
            .map(|(_, data)| data)
            .ok_or_else(|| install::InstallFailure {
                message: "the payload carries no firmware catalog".into(),
                swapped_in_doubt: false,
            })?;
        let catalog = std::str::from_utf8(catalog)
            .map_err(|_| install::InstallFailure {
                message: "the payload's firmware catalog is not UTF-8".into(),
                swapped_in_doubt: false,
            })
            .and_then(|text| {
                rc2fx_firmware::Catalog::parse(text).map_err(|e| install::InstallFailure {
                    message: format!("the payload's firmware catalog is refused: {e}"),
                    swapped_in_doubt: false,
                })
            })?;
        let mut qualified: Vec<String> = catalog
            .profiles
            .iter()
            .filter(|profile| verify::qualified(profile))
            .map(|profile| profile.id.clone())
            .collect();
        let mut declared = verified.metadata.firmware_profiles.clone();
        qualified.sort();
        declared.sort();
        if qualified != declared {
            return Err(install::InstallFailure {
                message: "the signed firmware compatibility differs from the payload's catalog"
                    .into(),
                swapped_in_doubt: false,
            });
        }
        if let Some(profile) = self.profile.as_deref()
            && !catalog.profiles.iter().any(|p| p.id == profile)
        {
            return Err(install::InstallFailure {
                message: format!("the payload's catalog does not carry profile {profile}"),
                swapped_in_doubt: false,
            });
        }
        let staged = install::stage(&self.paths, id, &files)?;
        if let Some(profile) = self.profile.as_deref()
            && staged.profile != profile
        {
            return Err(install::InstallFailure {
                message: format!(
                    "the payload's firmware check selected {}, not this mixer's {profile}",
                    staged.profile
                ),
                swapped_in_doubt: false,
            });
        }
        let from = verified.from.clone().unwrap_or_default();
        let swapped = install::swap(&self.paths, &staged, &from, &verified.version)?;
        self.spawn_driver(id, verified, false, &swapped)
            .map_err(|e| install::InstallFailure {
                message: e,
                swapped_in_doubt: true,
            })
    }

    /// Copy the (new) `rc2fx.sh` into RAM and start it detached, so the
    /// restart, the readiness check and the commit outlive this controller.
    fn spawn_driver(
        &self,
        id: &str,
        verified: &verify::VerifiedRelease,
        rollback: bool,
        swapped: &install::Swapped,
    ) -> Result<(), String> {
        let script = self.paths.driver_script(id);
        fs::copy(self.paths.bundle().join("rc2fx.sh"), &script)
            .map_err(|e| format!("cannot stage the update driver: {e}"))?;
        let request = format!(
            "mode={}\ntarget={}\nfrom={}\ndeadline={}\ngui={}\nrestart={}\n",
            if rollback { "rollback" } else { "commit" },
            verified.version,
            verified.from.as_deref().unwrap_or(""),
            RESTART_SECONDS,
            if swapped.gui_shim_changed { "1" } else { "0" },
            if swapped.controller_only && !rollback {
                "controller"
            } else {
                "mixer"
            },
        );
        install::write_atomic(&self.paths.request(id), request.as_bytes(), 0o600)
            .map_err(|e| format!("cannot record the update request: {e}"))?;
        let _ = DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(self.paths.logs());
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(self.paths.logs().join("update.log"))
            .map_err(|e| format!("cannot open the update log: {e}"))?;
        let error_log = log
            .try_clone()
            .map_err(|e| format!("cannot open the update log: {e}"))?;
        let mut command = std::process::Command::new("/bin/bash");
        let home = if rollback {
            self.paths.previous()
        } else {
            self.paths.bundle()
        };
        command
            .arg(&script)
            .arg("update-commit")
            .arg(id)
            .env_clear()
            .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
            .env("RC2FX_HOME", home)
            .stdin(std::process::Stdio::null())
            .stdout(log)
            .stderr(error_log);
        // A new session, so stopping this controller for the restart does not
        // take the driver with it.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        command
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("cannot start the update driver: {e}"))
    }

    /// The last update's outcome, for the app after it reconnects.
    pub fn last(&self) -> Option<LastOutcome> {
        LastOutcome::load(&self.paths.last())
    }

    /// The running release and the digest of each payload file it installed.
    /// Missing or non-regular files are simply absent: the app then sends them
    /// whole. A running version the launcher could not read has nothing to
    /// diff, so the endpoint is unavailable rather than guessed.
    pub fn installed(&self) -> Result<Installed, UpdateError> {
        let version = self.running.clone().ok_or_else(|| {
            UpdateError::Unavailable("the running RC2FX version is unknown".into())
        })?;
        let bundle = self.paths.bundle();
        let mut files = BTreeMap::new();
        for name in install::PAYLOAD_NAMES {
            let path = bundle.join(name);
            if !fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_file()) {
                continue;
            }
            let digest = install::sum_file(&path).map_err(|e| {
                UpdateError::Failed(format!("cannot read the installed {name}: {e}"))
            })?;
            files.insert(name.to_owned(), digest);
        }
        Ok(Installed { version, files })
    }

    /// Finish or undo an update that did not commit, before serving. Runs at
    /// startup, when nothing can restart audio for us.
    pub fn recover(&self) {
        // Clean up any temporary update drive left attached if a previous
        // controller exited unexpectedly.
        if self.offers_updates {
            self.drive_script_quiet(&["drop-stale".as_ref()]);
        }
        if install::driver_alive(&self.paths) {
            // The restart this update asked for is still being driven.
            return;
        }
        install::remove_stages(&self.paths);
        self.clear_run_records();
        let pending = Pending::load(&self.paths.pending());
        if pending.is_some() {
            let outcome = if install::bundle_verifies(&self.paths.previous()) {
                match install::restore_previous(&self.paths) {
                    Ok(()) => Some(LastOutcome {
                        outcome: "rolledBack".into(),
                        from: pending.as_ref().map(|p| p.from.clone()),
                        to: pending.as_ref().map(|p| p.to.clone()),
                        reason: Some(
                            "the update did not finish; the previous version was restored".into(),
                        ),
                        gui_shim_needs_reboot: true,
                    }),
                    Err(why) => {
                        eprintln!(
                            "RC2FX update: the previous version could not be restored: {why}"
                        );
                        None
                    }
                }
            } else if install::bundle_verifies(&self.paths.bundle()) {
                Some(LastOutcome {
                    outcome: "rolledBack".into(),
                    from: pending.as_ref().map(|p| p.from.clone()),
                    to: pending.as_ref().map(|p| p.to.clone()),
                    reason: Some(
                        "the update did not finish; the installed files already pass their checks"
                            .into(),
                    ),
                    gui_shim_needs_reboot: true,
                })
            } else {
                eprintln!(
                    "RC2FX update: an unfinished update is pending and neither copy passes its checks"
                );
                None
            };
            if let Some(outcome) = outcome {
                install::clear_pending(&self.paths);
                if let Err(error) = outcome.save(&self.paths.last()) {
                    eprintln!("RC2FX update: cannot record the outcome: {error}");
                }
            }
        }
        // This start is not an unproven one: record what is running. An
        // unreadable record is left as it is: writing a fresh one would make
        // the loss of its protection permanent. Updates stay refused instead.
        let mut state = match StoredState::load(&self.paths.state()) {
            Ok(state) => state.unwrap_or_default(),
            Err(why) => {
                eprintln!("RC2FX update: {why}; left unchanged, and updates are refused");
                return;
            }
        };
        state.schema = 1;
        if let Some(running) = &self.running {
            state.installed_version = Some(running.clone());
        }
        if let Err(error) = self.save_state(&state) {
            eprintln!("RC2FX update: {error}");
        }
    }

    /// Record this controller's start for the update driver, which waits for
    /// the new one after the restart.
    pub fn note_started(&self) {
        let body = format!(
            "version={}\npid={}\n",
            self.running.as_deref().unwrap_or("unknown"),
            std::process::id()
        );
        if let Err(error) = DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.paths.run)
        {
            eprintln!("RC2FX update: cannot record the controller start: {error}");
            return;
        }
        if let Err(error) = install::write_atomic(&self.paths.started(), body.as_bytes(), 0o600) {
            eprintln!("RC2FX update: cannot record the controller start: {error}");
        }
    }

    /// Drop session and driver files from an earlier run of this controller.
    fn clear_run_records(&self) {
        let Ok(entries) = fs::read_dir(&self.paths.run) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name == "driver.lock" {
                continue;
            }
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                let _ = fs::remove_dir_all(entry.path());
            } else {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
}

fn drive_offer(session: &Session) -> DriveOffer {
    DriveOffer {
        id: session.id.clone(),
        label: DRIVE_LABEL,
        file: DRIVE_FILE,
        size: session.upload_size,
    }
}

fn progress_of(id: &str, session: &Session) -> Progress {
    Progress {
        id: id.to_owned(),
        state: session.state.name(),
        version: session.request.version.clone(),
        size: session.request.size,
        received: session.received,
        encoding: session.upload.encoding(),
        upload_size: session.upload.encoding().map(|_| session.upload_size),
        error: session.state.error().map(str::to_owned),
    }
}

/// Replace a delta upload with the envelope it encodes. The delta and the
/// rebuilt envelope share the session directory, so the peak use is what
/// `start` checked for.
fn decode_delta(paths: &Paths, payload: &Path, size: u64) -> Result<(), String> {
    let envelope = payload.with_file_name("envelope");
    let _ = fs::remove_file(&envelope);
    let bundle = paths.bundle();
    let mut installed = |name: &str| -> std::io::Result<Vec<u8>> {
        // The delta crate checks names too; this is the second guard on the
        // only path that joins a manifest name onto the installed bundle.
        if name.is_empty()
            || name.contains('/')
            || name.contains('\\')
            || name == "."
            || name == ".."
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "invalid payload file name",
            ));
        }
        fs::read(bundle.join(name))
    };
    let mut delta =
        File::open(payload).map_err(|e| format!("cannot read the delta upload: {e}"))?;
    let mut out = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&envelope)
        .map_err(|e| format!("cannot create the rebuilt envelope: {e}"))?;
    // The declared envelope size is also the decode bound, so a delta cannot
    // expand past the space `start` and `install` checked for. The size and
    // hash are checked against the signed target right after.
    let decoded = match rc2fx_delta::decode(&mut delta, &mut installed, &mut out, size) {
        Ok(decoded) => decoded,
        Err(why) => {
            let _ = fs::remove_file(&envelope);
            return Err(format!("the delta upload was refused: {why}"));
        }
    };
    if let Err(e) = out.sync_all() {
        let _ = fs::remove_file(&envelope);
        return Err(format!("cannot flush the rebuilt envelope: {e}"));
    }
    drop(out);
    if decoded != size {
        let _ = fs::remove_file(&envelope);
        return Err(format!(
            "the delta decoded to {decoded} bytes; the signed release is {size}"
        ));
    }
    // The upload now holds the envelope: verification and install read the
    // same file they do for a plain upload.
    fs::rename(&envelope, payload).map_err(|e| {
        let _ = fs::remove_file(&envelope);
        format!("cannot replace the delta upload with the envelope: {e}")
    })
}

fn new_id() -> String {
    let mut bytes = [0u8; 8];
    if File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .is_ok()
    {
        let mut out = String::with_capacity(16);
        for byte in bytes {
            out.push_str(&format!("{byte:02x}"));
        }
        return out;
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mixed = (nanos as u64) ^ (u64::from(std::process::id()) << 32) ^ uptime_nanos();
    format!("{mixed:016x}")
}

fn uptime_nanos() -> u64 {
    std::fs::read_to_string("/proc/uptime")
        .ok()
        .and_then(|text| text.split_whitespace().next().map(str::to_owned))
        .and_then(|seconds| seconds.parse::<f64>().ok())
        .map(|seconds| (seconds * 1e6) as u64)
        .unwrap_or(0)
}

/// Strict standard base64, padding required. Returns `None` for anything else.
fn decode_base64(text: &str) -> Option<Vec<u8>> {
    rc2fx_core::ssh::decode_base64(text)
}

/// The decoder's inverse, for the tests only.
#[cfg(test)]
pub(crate) fn encode_base64_for_test(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for block in bytes.chunks(3) {
        let mut combined = 0u32;
        for (index, byte) in block.iter().enumerate() {
            combined |= u32::from(*byte) << (16 - index * 8);
        }
        for index in 0..4 {
            if index <= block.len() {
                out.push(ALPHABET[((combined >> (18 - index * 6)) & 0x3f) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests;
