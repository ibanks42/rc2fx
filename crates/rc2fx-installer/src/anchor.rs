//! The record that ties a prepared RC2FX image to the helper that built it.
//!
//! The cache folder belongs to the desktop user, so `installer.json` and
//! `archive.tar.gz` can be rewritten together while the root helper runs. The
//! privileged helper therefore builds the image itself, keeps its own copy of
//! what it prepared in a root-owned folder and refuses anything else. Stock images
//! the catalog pins are checked against those pins; other stock images RØDE
//! lists are recorded like RC2FX images. Without a privileged helper
//! there is no boundary to defend and nothing is recorded.

use crate::{Engine, Prepared, Result, bail, files};
use std::{fs, path::PathBuf};

impl Engine {
    /// Where the record for this cache folder lives; `None` when unprivileged.
    fn anchor_file(&self) -> Result<Option<PathBuf>> {
        #[cfg(test)]
        let dir = self.anchor_dir.clone();
        #[cfg(not(test))]
        let dir = crate::mixer_update::helper_directory("prepared")?;
        let name = files::sum(self.work.as_os_str().as_encoded_bytes());
        Ok(dir.map(|dir| dir.join(name)))
    }

    /// Record `prepared`, whose hashes this process computed while building it.
    pub(crate) fn anchor_prepared(&self, prepared: &Prepared) -> Result<()> {
        let Some(path) = self.anchor_file()? else {
            return Ok(());
        };
        // Not `files::atomic_file`: that gives the file to the desktop user.
        let part = path.with_extension("part");
        fs::write(&part, serde_json::to_vec(prepared)?)?;
        fs::rename(&part, &path)?;
        Ok(())
    }

    /// Refuse a prepared image that this helper did not record.
    pub(crate) fn check_anchor(&self, prepared: &Prepared) -> Result<()> {
        let Some(path) = self.anchor_file()? else {
            return Ok(());
        };
        let recorded = match fs::read(&path) {
            Ok(data) => serde_json::from_slice::<Prepared>(&data).ok(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };
        if recorded.as_ref() != Some(prepared) {
            bail!("this firmware was not prepared by the installer helper; prepare it again");
        }
        Ok(())
    }
}
