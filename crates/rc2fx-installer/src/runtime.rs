//! The bundled RC2FX runtime, the files it adds to the firmware image, and
//! The RC2FX runtime bundle, the files it adds to the image, settings backups
//! and readiness.

use crate::{
    BackupRecord, Catalog, Engine, Error, MAX_SETTINGS, Profile, Result, RuntimeProfile, Service,
    bail,
    embed::format::{self, BINARIES, SCRIPTS},
    files,
    firmware::supported,
    utc,
};
use std::{collections::HashMap, io::Read, path::Path};

/// The payload's files, in the order an embedded envelope carries them.
pub use crate::embed::format::PAYLOAD_NAMES;

#[cfg(test)]
mod recorder_contract_tests {
    #[test]
    fn passive_recorder_is_in_the_installed_and_checksummed_payload() {
        for name in ["librc2fx_jackerr.so", "rc2fx_jackerr_snapshot"] {
            assert!(
                crate::embed::format::PAYLOAD_NAMES.contains(&name),
                "missing payload {name}"
            );
            assert!(
                crate::embed::format::PINNED.contains(&name),
                "unchecksummed {name}"
            );
            assert!(
                crate::embed::format::BINARIES.contains(&name),
                "unchecked ELF {name}"
            );
        }
    }
}

pub type Bundle = HashMap<String, Vec<u8>>;

/// Read the runtime bundle from `runtime/` (a release folder) or
/// `target/mixer` (a source checkout), with scripts from `xtask/mixer`.
pub fn bundle_files(root: &Path) -> Result<Bundle> {
    format::read(root, false)
        .map_err(Error::from)
        .map(|files| files.into_iter().collect())
}

/// The same bundle for an engine whose source is a folder.
pub(crate) fn bundle_from_disk(root: &Path) -> Result<Bundle> {
    bundle_files(root)
}

/// The digest a prepared firmware records as its RC2FX runtime.
pub(crate) fn digest(files: &Bundle) -> String {
    let manifest: String = PAYLOAD_NAMES
        .iter()
        .map(|n| format!("{}  {n}\n", files::sum(data(files, n))))
        .collect();
    files::sum(manifest.as_bytes())
}

/// The payload's files in pinned order, for the shared checks.
pub(crate) fn view(files: &Bundle) -> Vec<(&str, &[u8])> {
    PAYLOAD_NAMES.iter().map(|n| (*n, data(files, n))).collect()
}

fn data<'a>(files: &'a Bundle, name: &str) -> &'a [u8] {
    files.get(name).map(Vec::as_slice).unwrap_or_default()
}

/// The catalog this payload supports, after the shared payload checks.
pub(crate) fn catalog_of(files: &Bundle) -> Result<Catalog> {
    format::validate(&view(files)).map_err(Error::from)?;
    Ok(serde_json::from_slice(data(
        files,
        "firmware-profiles.json",
    ))?)
}

/// The runtime profile the mixer's state is judged against.
pub(crate) fn profile_of(files: &Bundle) -> RuntimeProfile {
    serde_json::from_slice::<Catalog>(data(files, "firmware-profiles.json"))
        .ok()
        .and_then(|c| c.profiles.into_iter().find(supported))
        .and_then(|p| p.runtime)
        .unwrap_or_default()
}

/// Whether RC2FX's state says this payload's runtime is ready.
pub(crate) fn service(files: &Bundle, state: &[u8]) -> Service {
    match readiness_error(state, &profile_of(files)) {
        why if why.is_empty() => Service::Ready,
        why => Service::Starting { detail: why },
    }
}

/// A file or directory to add to the firmware image.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Entry {
    Dir {
        path: String,
        mode: u32,
    },
    File {
        path: String,
        mode: u32,
        data: Vec<u8>,
    },
}

impl Entry {
    pub fn path(&self) -> &str {
        match self {
            Entry::Dir { path, .. } | Entry::File { path, .. } => path,
        }
    }
}

/// What RC2FX adds to the root filesystem: the same layout `install.sh`
/// creates on a running mixer, with autostart, and optional saved settings.
/// Returns the entries and the runtime digest (recorded as bundle.sha256).
pub(crate) fn image_entries(
    files: &Bundle,
    profile: &Profile,
    settings: Option<&[u8]>,
) -> Result<(Vec<Entry>, String)> {
    let c = catalog_of(files)?;
    if let Some(settings) = settings {
        let value: serde_json::Value = serde_json::from_slice(settings)?;
        let config = value.get("config").unwrap_or(&value);
        let targets = config
            .get("targets")
            .and_then(serde_json::Value::as_array)
            .ok_or("settings have no target identities")?;
        let duo = profile.model == rc2fx_firmware::Model::Duo.id();
        if targets.iter().any(|t| {
            t.get("id")
                .and_then(serde_json::Value::as_str)
                .is_none_or(|id| id.starts_with("duo-") != duo)
        }) {
            bail!("saved settings belong to a different mixer model; refusing to move audio paths");
        }
    }
    if !c.profiles.iter().any(|p| {
        p.id == profile.id
            && p.model == profile.model
            && p.runtime == profile.runtime
            && p.mixer_sha == profile.mixer_sha
            && p.launcher_sha == profile.launcher_sha
            && supported(p)
    }) {
        bail!(
            "the bundled runtime does not support firmware {}",
            profile.version
        );
    }
    let digest = digest(files);
    let mut entries = vec![
        Entry::Dir {
            path: "opt/rc2fx".into(),
            mode: 0o700,
        },
        Entry::Dir {
            path: "opt/rc2fx/data".into(),
            mode: 0o700,
        },
        Entry::Dir {
            path: "opt/rc2fx/logs".into(),
            mode: 0o700,
        },
    ];
    for n in PAYLOAD_NAMES {
        let executable = BINARIES.contains(&n) || SCRIPTS.contains(&n);
        entries.push(Entry::File {
            path: format!("opt/rc2fx/{n}"),
            mode: if executable { 0o700 } else { 0o600 },
            data: files[n].clone(),
        });
    }
    if profile.model == rc2fx_firmware::Model::Duo.id() {
        entries.push(Entry::File {
            path: "opt/rc2fx/experimental-duo".into(),
            mode: 0o600,
            data: b"1\n".to_vec(),
        });
    }
    entries.push(Entry::File {
        path: "opt/rc2fx/bundle.sha256".into(),
        mode: 0o600,
        data: format!("{digest}\n").into_bytes(),
    });
    if let Some(settings) = settings {
        entries.push(Entry::File {
            path: "opt/rc2fx/data/state.json".into(),
            mode: 0o600,
            data: settings.to_vec(),
        });
    }
    // RØDE's launcher starts the mixer by name with /usr/sbin ahead of
    // /usr/bin, so this runs in its place and execs the stock executable with
    // RC2FX injected from the first mixer start of every boot.
    entries.push(Entry::File {
        path: "usr/sbin/rc_audio_mixer".into(),
        mode: 0o755,
        data: files["rc2fx-shim.sh"].clone(),
    });
    // The same for RØDE's display loop, which starts rc_gui_juce by name with
    // /usr/sbin first: the GUI shim hands over to `rc2fx.sh gui-launch`,
    // which always execs the stock GUI, with the partial-conversion module
    // only when every check passes.
    entries.push(Entry::File {
        path: "usr/sbin/rc_gui_juce".into(),
        mode: 0o755,
        data: files["rc2fx-gui-shim.sh"].clone(),
    });
    Ok((entries, digest))
}

/// The settings file a backup saved, unchanged since. The record is the
/// desktop user's to edit, so its path must name a file directly in `work`.
pub(crate) fn backup_settings(work: &Path, b: &BackupRecord) -> Result<Vec<u8>> {
    let path = Path::new(&b.file);
    if path.parent() != Some(work) || path.file_name().is_none() {
        bail!("the settings backup is not in the installer's folder");
    }
    let mut data = Vec::new();
    files::open_work_file(path)?
        .take(MAX_SETTINGS + 1)
        .read_to_end(&mut data)?;
    if files::sum(&data) != b.sha {
        bail!("the settings backup changed since it was saved");
    }
    check_settings(&data)?;
    Ok(data)
}

fn check_settings(data: &[u8]) -> Result<()> {
    if data.len() as u64 > MAX_SETTINGS {
        bail!("the settings file exceeds 16 MiB");
    }
    let value: serde_json::Value = serde_json::from_slice(data)
        .map_err(|e| Error(format!("the settings file is not valid JSON: {e}")))?;
    if !value.is_object() {
        bail!("the settings file is not an RC2FX settings object");
    }
    Ok(())
}

/// What RC2FX's state (`GET api/v1/state`, as the app's USB connection reads
/// it) says about readiness, judged against the installer folder's runtime
/// profile.
pub fn service_from_state(root: &Path, state: &[u8]) -> Service {
    crate::embed::Source::folder(root).service(state)
}

impl Engine {
    /// Keep RC2FX's settings file, read over USB by the app, for the mixer
    /// `serial`.
    pub(crate) fn save_settings(&mut self, serial: &str, settings: &str) -> Result<BackupRecord> {
        if serial.is_empty() {
            bail!("connect the mixer so its settings can be matched to it");
        }
        self.step("backup", "Saving the mixer's RC2FX settings");
        let data = settings.as_bytes();
        if data.len() > MAX_SETTINGS as usize {
            bail!("RC2FX's settings exceed {MAX_SETTINGS} bytes");
        }
        check_settings(data)?;
        let stamp: String = utc().chars().filter(char::is_ascii_alphanumeric).collect();
        let name = format!(
            "settings-{}-{stamp}.json",
            &files::sum(serial.as_bytes())[..12]
        );
        let dest = self.work.join(name);
        files::atomic_file(&dest, data)?;
        let record = BackupRecord {
            serial: serial.into(),
            file: dest.to_string_lossy().into_owned(),
            sha: files::sum(data),
            created: utc(),
        };
        let mut saved = self.load()?;
        saved.backups.push(record.clone());
        self.save(&saved)?;
        Ok(record)
    }
}

pub(crate) fn readiness_error(body: &[u8], r: &RuntimeProfile) -> String {
    #[derive(serde::Deserialize, Default)]
    #[serde(default, rename_all = "camelCase")]
    struct EngineView {
        mode: String,
        status: String,
        audio_advancing: bool,
        applied_revision: Option<u64>,
        sample_rate: u32,
        block_size: u32,
        message: String,
    }
    #[derive(serde::Deserialize, Default)]
    #[serde(default)]
    struct Config {
        revision: Option<u64>,
    }
    #[derive(serde::Deserialize, Default)]
    #[serde(default)]
    struct StateView {
        engine: EngineView,
        config: Config,
    }
    let s: StateView = match serde_json::from_slice(body) {
        Ok(s) => s,
        Err(e) => return format!("invalid state JSON: {e}"),
    };
    let a = &s.engine;
    if a.mode == "native"
        && a.status == "running"
        && a.audio_advancing
        && a.applied_revision.is_some()
        && a.applied_revision == s.config.revision
        && a.sample_rate == r.sample_rate
        && a.block_size == r.block_frames
    {
        return String::new();
    }
    // While starting, the engine's own message says what it waits for.
    if a.status != "running" && !a.message.is_empty() {
        return a.message.clone();
    }
    let revision = |v: Option<u64>| v.map_or("missing".to_owned(), |v| v.to_string());
    format!(
        "engine mode={:?} status={:?} advancing={} appliedRevision={} configRevision={} sampleRate={} blockSize={} (expected {}/{})",
        a.mode,
        a.status,
        a.audio_advancing,
        revision(a.applied_revision),
        revision(s.config.revision),
        a.sample_rate,
        a.block_size,
        r.sample_rate,
        r.block_frames
    )
}
