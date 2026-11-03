//! The firmware catalog's shape: the profiles a payload supports and the
//! runtime contract each pins.
//!
//! This module is compiled twice — into the library, and into `build.rs` via
//! `#[path]` — so the payload's catalog is checked with the same typed
//! deserialization at build time that the installer uses at run time. A
//! catalog that a run-time reader would reject must never reach a binary.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct JobLifetime {
    pub abi: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RuntimeProfile {
    pub callback_abi: String,
    pub route_layout: String,
    pub source_enum: String,
    pub sample_rate: u32,
    pub block_frames: u32,
    pub job_lifetime: JobLifetime,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Profile {
    pub id: String,
    pub model: String,
    pub version: String,
    #[serde(rename = "mixerSha256")]
    pub mixer_sha: String,
    #[serde(rename = "launcherSha256")]
    pub launcher_sha: String,
    #[serde(rename = "archiveSha256")]
    pub archive_sha: String,
    #[serde(rename = "archiveMd5")]
    pub archive_md5: String,
    pub runtime: Option<RuntimeProfile>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Catalog {
    #[serde(rename = "schemaVersion")]
    pub schema: u32,
    pub profiles: Vec<Profile>,
}
