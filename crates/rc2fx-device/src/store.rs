//! Saved effects and presets. The controller serializes mutations and owns all
//! engine admission/lifecycle work; Store never starts or controls audio.
use crate::persistence::{CommitError, Directory, random_id};
use rc2fx_core::{Config, ConfigUpdate, PresetSummary, SCHEMA_VERSION, Target};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::io;
use std::path::Path;

pub const MAX_STATE_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_PRESETS: usize = 128;
const MAX_STATE_NODES: usize = 4_000_000;
const MAX_REVISION: u64 = 9_007_199_254_740_991;

#[derive(Debug)]
pub enum StoreError {
    Invalid(String),
    Conflict,
    NotFound,
    Locked,
    Io(io::Error),
    DurabilityUncertain(String),
}
impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(message) => write!(f, "invalid request: {message}"),
            Self::Conflict => f.write_str("revision conflict"),
            Self::NotFound => f.write_str("not found"),
            Self::Locked => f.write_str("data directory is already locked"),
            Self::Io(error) => write!(f, "state I/O: {error}"),
            Self::DurabilityUncertain(message) => write!(
                f,
                "commit durability uncertain; restart and inspect state: {message}"
            ),
        }
    }
}
impl std::error::Error for StoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}
impl From<io::Error> for StoreError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}
type Result<T> = std::result::Result<T, StoreError>;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Preset {
    id: String,
    name: String,
    targets: Vec<Target>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DiskState {
    #[serde(rename = "engineGeneration")]
    engine_generation: u64,
    config: Config,
    presets: Vec<Preset>,
}

/// Holds an exclusive data-directory lock until dropped. Returned snapshots own
/// their complete target trees. Mutating methods require the controller's &mut
/// access and only acknowledge a candidate after a durable commit.
pub struct Store {
    directory: Directory,
    disk: DiskState,
    durability_fault: Option<String>,
}

/// A validated, isolated settings candidate. Exclusive borrowing prevents a
/// second store mutation between audio preparation and durable commit. Dropping
/// the reservation is an abort: no file or acknowledged configuration changes.
pub struct PreparedSave<'store> {
    store: &'store mut Store,
    candidate: DiskState,
    acknowledged: Config,
}
impl PreparedSave<'_> {
    pub fn candidate_config(&self) -> &Config {
        &self.candidate.config
    }

    /// The returned configuration is allocated during preparation. After a
    /// successful durable commit, only ownership moves/destruction remain here;
    /// the engine coordinator can immediately publish its reserved audio epoch.
    pub fn commit(self) -> Result<Config> {
        let Self {
            store,
            candidate,
            acknowledged,
        } = self;
        store.persist(&candidate)?;
        store.disk = candidate;
        Ok(acknowledged)
    }
}

impl Store {
    /// The saved state file's bytes, as stored.
    pub fn encoded_snapshot(&self) -> Result<Vec<u8>> {
        encode_disk(&self.disk)
    }

    pub fn open(path: impl AsRef<Path>, initial_config: Config) -> Result<Self> {
        let directory = Directory::open(path.as_ref()).map_err(|error| {
            if error.kind() == io::ErrorKind::WouldBlock {
                StoreError::Locked
            } else {
                StoreError::Io(error)
            }
        })?;
        let saved = directory.read(MAX_STATE_BYTES)?;
        let disk = if let Some(bytes) = &saved {
            decode_disk(bytes)?
        } else {
            DiskState {
                engine_generation: 0,
                config: initial_config,
                presets: Vec::new(),
            }
        };
        validate_disk(&disk)?;
        let mut store = Self {
            directory,
            disk,
            durability_fault: None,
        };
        if saved.is_none() {
            store.persist(&store.disk.clone())?;
        }
        Ok(store)
    }

    pub fn config(&self) -> &Config {
        &self.disk.config
    }

    pub fn generation(&self) -> u64 {
        self.disk.engine_generation
    }

    /// Reserve before exposing a boot or lifecycle acknowledgement. Generations
    /// are independent of settings revisions and never reused across reopen.
    pub fn reserve_generation(&mut self) -> Result<u64> {
        self.ensure_writable()?;
        let generation = self
            .disk
            .engine_generation
            .checked_add(1)
            .filter(|value| *value <= MAX_REVISION)
            .ok_or_else(|| StoreError::Invalid("engine generation exhausted".into()))?;
        let mut candidate = self.disk.clone();
        candidate.engine_generation = generation;
        self.persist(&candidate)?;
        self.disk = candidate;
        Ok(generation)
    }

    /// Build a selection candidate without modifying the complete effects
    /// catalog. Physical admission and active-audio proof belong to the owner.
    pub fn prepare_selection(
        &mut self,
        expected_revision: u64,
        ids: &[String],
    ) -> Result<PreparedSave<'_>> {
        self.ensure_writable()?;
        self.check_revision(expected_revision)?;
        let selected = self.disk.config.canonical_selection(ids).map_err(invalid)?;
        let revision = expected_revision
            .checked_add(1)
            .filter(|value| *value <= MAX_REVISION)
            .ok_or_else(|| StoreError::Invalid("revision exhausted".into()))?;
        let mut candidate = self.disk.clone();
        candidate.config.revision = revision;
        candidate.config.selected_targets = selected;
        candidate.config.validate().map_err(invalid)?;
        let acknowledged = candidate.config.clone();
        Ok(PreparedSave {
            store: self,
            candidate,
            acknowledged,
        })
    }

    /// Some means a rename succeeded but durability was not acknowledged. Disk
    /// may already contain a new snapshot; this instance refuses every mutation.
    pub fn durability_fault(&self) -> Option<&str> {
        self.durability_fault.as_deref()
    }

    pub fn apply(&mut self, update: ConfigUpdate) -> Result<Config> {
        self.prepare_apply(update)?.commit()
    }

    /// Prepare the full saved catalog before reserving the selected audio
    /// projection. No persistence occurs until the returned token is committed.
    pub fn prepare_apply(&mut self, update: ConfigUpdate) -> Result<PreparedSave<'_>> {
        self.ensure_writable()?;
        self.check_revision(update.expected_revision)?;
        self.disk
            .config
            .validate_update(&update.targets)
            .map_err(invalid)?;
        let revision = update
            .expected_revision
            .checked_add(1)
            .filter(|revision| *revision <= MAX_REVISION)
            .ok_or_else(|| StoreError::Invalid("revision exhausted".into()))?;
        let mut candidate = self.disk.clone();
        candidate.config.revision = revision;
        candidate.config.targets = update.targets;
        let acknowledged = candidate.config.clone();
        Ok(PreparedSave {
            store: self,
            candidate,
            acknowledged,
        })
    }

    pub fn presets(&self) -> Vec<PresetSummary> {
        self.disk
            .presets
            .iter()
            .map(|preset| PresetSummary {
                id: preset.id.clone(),
                name: preset.name.clone(),
            })
            .collect()
    }

    pub fn create_preset(&mut self, name: &str, expected_revision: u64) -> Result<PresetSummary> {
        self.ensure_writable()?;
        self.check_revision(expected_revision)?;
        if !valid_name(name) {
            return Err(StoreError::Invalid(
                "preset name must contain 1 to 128 UTF-8 bytes".into(),
            ));
        }
        if self.disk.presets.len() >= MAX_PRESETS {
            return Err(StoreError::Invalid("too many presets".into()));
        }
        let id = loop {
            let id = random_id()?;
            if !self.disk.presets.iter().any(|preset| preset.id == id) {
                break id;
            }
        };
        let summary = PresetSummary {
            id: id.clone(),
            name: name.into(),
        };
        let mut candidate = self.disk.clone();
        candidate.presets.push(Preset {
            id,
            name: name.into(),
            targets: self.disk.config.targets.clone(),
        });
        self.persist(&candidate)?;
        self.disk = candidate;
        Ok(summary)
    }

    pub fn apply_preset(&mut self, id: &str, expected_revision: u64) -> Result<Config> {
        self.prepare_preset(id, expected_revision)?.commit()
    }

    /// Presets use the same preflight/save path as effects updates. The selected
    /// topology remains the current committed selection, never preset content.
    pub fn prepare_preset(&mut self, id: &str, expected_revision: u64) -> Result<PreparedSave<'_>> {
        self.ensure_writable()?;
        self.check_revision(expected_revision)?;
        let targets = self
            .disk
            .presets
            .iter()
            .find(|preset| preset.id == id)
            .ok_or(StoreError::NotFound)?
            .targets
            .clone();
        self.prepare_apply(ConfigUpdate {
            expected_revision,
            targets,
        })
    }

    pub fn delete_preset(&mut self, id: &str) -> Result<()> {
        self.ensure_writable()?;
        let index = self
            .disk
            .presets
            .iter()
            .position(|preset| preset.id == id)
            .ok_or(StoreError::NotFound)?;
        let mut candidate = self.disk.clone();
        candidate.presets.remove(index);
        self.persist(&candidate)?;
        self.disk = candidate;
        Ok(())
    }

    fn ensure_writable(&self) -> Result<()> {
        if let Some(fault) = &self.durability_fault {
            Err(StoreError::DurabilityUncertain(fault.clone()))
        } else {
            Ok(())
        }
    }
    fn check_revision(&self, revision: u64) -> Result<()> {
        if revision == self.disk.config.revision {
            Ok(())
        } else {
            Err(StoreError::Conflict)
        }
    }
    fn persist(&mut self, candidate: &DiskState) -> Result<()> {
        self.ensure_writable()?;
        let output = encode_disk(candidate)?;
        match self.directory.commit(&output) {
            Ok(()) => Ok(()),
            Err(CommitError::BeforeRename(error)) => Err(StoreError::Io(error)),
            #[cfg(unix)]
            Err(CommitError::AfterRename(error)) => {
                let message = error.to_string();
                self.durability_fault = Some(message.clone());
                Err(StoreError::DurabilityUncertain(message))
            }
        }
    }
}

fn decode_disk(bytes: &[u8]) -> Result<DiskState> {
    let text = std::str::from_utf8(bytes)
        .map_err(|error| StoreError::Invalid(format!("state is not UTF-8: {error}")))?;
    let corrupt =
        |error: rc2fx_core::CoreError| StoreError::Invalid(format!("corrupt state: {error}"));
    rc2fx_core::from_json_limited(text, MAX_STATE_BYTES, MAX_STATE_NODES).map_err(corrupt)
}

fn encode_disk(disk: &DiskState) -> Result<Vec<u8>> {
    let mut output = LimitedOutput(Vec::new());
    serde_json::to_writer(&mut output, disk)
        .map_err(|error| StoreError::Invalid(format!("cannot encode bounded state: {error}")))?;
    Ok(output.0)
}

struct LimitedOutput(Vec<u8>);
impl io::Write for LimitedOutput {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_STATE_BYTES - self.0.len() {
            return Err(io::Error::other(
                "saved state would exceed 16 MiB; delete presets or reduce EQ bands",
            ));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn invalid(error: rc2fx_core::CoreError) -> StoreError {
    StoreError::Invalid(error.to_string())
}
fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 128
}
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 48
        && id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}
fn validate_disk(disk: &DiskState) -> Result<()> {
    if disk.engine_generation > MAX_REVISION {
        return Err(StoreError::Invalid(
            "engine generation exceeds exact wire integer range".into(),
        ));
    }
    disk.config.validate().map_err(invalid)?;
    if disk.config.revision > MAX_REVISION {
        return Err(StoreError::Invalid(
            "revision exceeds exact wire integer range".into(),
        ));
    }
    if disk.presets.len() > MAX_PRESETS {
        return Err(StoreError::Invalid("too many presets".into()));
    }
    let mut seen = HashSet::new();
    for preset in &disk.presets {
        if !valid_id(&preset.id) || !seen.insert(&preset.id) || !valid_name(&preset.name) {
            return Err(StoreError::Invalid("invalid or duplicate preset".into()));
        }
        Config {
            schema_version: SCHEMA_VERSION,
            revision: 1,
            selected_targets: Vec::new(),
            targets: preset.targets.clone(),
        }
        .validate()
        .map_err(invalid)?;
    }
    Ok(())
}

#[cfg(all(test, unix))]
#[path = "../tests/support/store_faults.rs"]
mod faults;
