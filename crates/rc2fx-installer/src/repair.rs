//! Repairing a mixer from a computer, for when it can't repair itself.
//!
//! Holding a RØDECaster's power button for about 15 seconds at power-on makes
//! its bootloader show the whole eMMC to the computer as a USB disk (`ums mmc
//! 0`, Amlogic's gadget `1b8e:fada`). Four partitions are on it: two ext4
//! system roots (RØDE's updater alternates between them), the FAT settings
//! partition and the FAT data partition.
//!
//! A flash once left one of RØDE's start-up scripts zero-filled on the new
//! root: the right size, none of the contents. That script mounts the
//! settings partition, so the mixer lost its setup at every start. The
//! repair compares each root's files with the list the installer put in the
//! image (`image_check`), or with the image saved on this computer when it
//! is the one that was flashed, and replaces a damaged file's contents with
//! a byte-identical copy: from the other root, or from that image. It only
//! ever rewrites a regular file in place with the same number of bytes, and
//! all ext4 work is libext2fs's (`rc2fx-ext2fs`). Before writing, it copies
//! the settings partition to this computer.
use crate::{
    DamagedFile, Engine, RepairFindings, RepairResult, RepairTarget, Result, RootFindings, bail,
    files, firmware, image_check,
};
use rc2fx_ext2fs::{Entry, Filesystem};
use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// The USB identity of the bootloader's mass-storage gadget.
pub(crate) const RECOVERY_VENDOR: &str = "1b8e";
pub(crate) const RECOVERY_PRODUCT: &str = "fada";

const SECTOR: u64 = 512;
const HOSTNAMES: [&str; 2] = ["rodecaster-pro-ii", "rodecaster-pro-ii-mini"];
const SUMS: &str = "opt/rc2fx/SHA256SUMS";
/// The largest file read whole; RØDE's largest is its mixer executable.
const MAX_FILE: u64 = 256 << 20;

/// One primary partition, in bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Part {
    start: u64,
    len: u64,
}

/// The four primary partitions of an MBR, refusing anything that isn't four
/// ordered, non-overlapping partitions inside the disk.
fn partitions(sector0: &[u8; 512], disk_len: u64) -> Result<[Part; 4]> {
    if sector0[510..] != [0x55, 0xAA] {
        bail!("the disk has no partition table");
    }
    let mut parts = [Part { start: 0, len: 0 }; 4];
    let mut end = SECTOR;
    for (i, part) in parts.iter_mut().enumerate() {
        let entry = &sector0[446 + 16 * i..462 + 16 * i];
        let kind = entry[4];
        let lba = u64::from(u32::from_le_bytes(entry[8..12].try_into().unwrap()));
        let sectors = u64::from(u32::from_le_bytes(entry[12..16].try_into().unwrap()));
        let (start, len) = (lba * SECTOR, sectors * SECTOR);
        if kind == 0 || sectors == 0 || start < end || start + len > disk_len {
            bail!("the disk's partitions are not a RØDECaster's");
        }
        end = start + len;
        *part = Part { start, len };
    }
    Ok(parts)
}

/// RØDE's layout on this disk: four partitions, ext4 roots in the first
/// two with a RØDECaster's hostname, and whether the settings partition's
/// FAT boot sector looks sound.
struct Layout {
    device: PathBuf,
    parts: [Part; 4],
    hostname: String,
    settings_ok: bool,
}

fn layout(device: &Path, disk_len: u64) -> Result<Layout> {
    let mut disk = std::fs::File::open(device)?;
    let mut sector0 = [0u8; 512];
    disk.read_exact(&mut sector0)?;
    let parts = partitions(&sector0, disk_len)?;
    let mut hostname = None;
    for part in &parts[..2] {
        let fs = Filesystem::open(device, part.start, false).map_err(|e| {
            crate::Error(format!(
                "a system partition is not ext4 ({e}); this is not a RØDECaster's storage"
            ))
        })?;
        if let Some((_, data)) = read(&fs, "etc/hostname")? {
            let name = String::from_utf8_lossy(&data).trim().to_owned();
            if HOSTNAMES.contains(&name.as_str()) {
                hostname = Some(name);
            }
        }
    }
    let Some(hostname) = hostname else {
        bail!("neither system partition belongs to a RØDECaster; nothing was changed");
    };
    let mut boot = [0u8; 512];
    disk.seek(SeekFrom::Start(parts[2].start))?;
    disk.read_exact(&mut boot)?;
    let settings_ok =
        boot[510..] == [0x55, 0xAA] && (&boot[82..87] == b"FAT32" || &boot[54..58] == b"FAT1");
    Ok(Layout {
        device: device.to_owned(),
        parts,
        hostname,
        settings_ok,
    })
}

/// A regular file's entry and contents, or `None` when it doesn't exist.
fn read(fs: &Filesystem, path: &str) -> Result<Option<(Entry, Vec<u8>)>> {
    let Some(entry) = fs.lookup(path).map_err(ext2)? else {
        return Ok(None);
    };
    if !entry.regular || entry.size > MAX_FILE {
        return Ok(Some((entry, Vec::new())));
    }
    let data = fs.read(&entry).map_err(ext2)?;
    Ok(Some((entry, data)))
}

fn ext2(e: rc2fx_ext2fs::Error) -> crate::Error {
    crate::Error(format!("libext2fs: {e}"))
}

/// `sha256  path` lines, as `image_check` writes them.
fn parse_manifest(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let (sha, path) = line.split_once("  ")?;
            (sha.len() == 64 && sha.bytes().all(|b| b.is_ascii_hexdigit()) && !path.is_empty())
                .then(|| (sha.to_owned(), path.to_owned()))
        })
        .collect()
}

/// The firmware image saved on this computer: its RC2FX checksums, to tell
/// whether it is what a root was flashed with, and its file list.
struct SavedImage {
    path: PathBuf,
    sums: Option<Vec<u8>>,
    manifest: Vec<(String, String)>,
}

fn saved_image(path: &Path) -> Result<SavedImage> {
    let mut sums = None;
    let mut manifest = Vec::new();
    firmware::walk_archive(path, 4 << 30, false, |entry| {
        if entry.header().entry_type() != tar::EntryType::Regular {
            return Ok(());
        }
        let name = firmware::normalized(&firmware::entry_name(entry)).to_owned();
        if name == SUMS {
            let mut data = Vec::new();
            entry.read_to_end(&mut data)?;
            sums = Some(data);
        } else if image_check::listed(&name) {
            let mut hashing = files::Hashing::new(entry);
            std::io::copy(&mut hashing, &mut std::io::sink())?;
            manifest.push((hashing.finish().1, name));
        }
        Ok(())
    })?;
    Ok(SavedImage {
        path: path.to_owned(),
        sums,
        manifest,
    })
}

/// The members of the saved image named in `wanted`.
fn saved_members(path: &Path, wanted: &HashSet<String>) -> Result<HashMap<String, Vec<u8>>> {
    let mut found = HashMap::new();
    if wanted.is_empty() {
        return Ok(found);
    }
    firmware::walk_archive(path, 4 << 30, false, |entry| {
        let name = firmware::normalized(&firmware::entry_name(entry)).to_owned();
        if wanted.contains(&name) && entry.size() <= MAX_FILE {
            let mut data = Vec::new();
            entry.read_to_end(&mut data)?;
            found.insert(name, data);
        }
        Ok(())
    })?;
    Ok(found)
}

/// A damaged file and the identical bytes that restore it.
struct Fix {
    path: String,
    want: String,
    data: Vec<u8>,
}

/// Where a root's expected hashes come from.
fn manifest_of(
    fs: &Filesystem,
    saved: Option<&SavedImage>,
) -> Result<(String, Vec<(String, String)>)> {
    // The image's own list, or the saved image when it is the one this root
    // was flashed with.
    Ok(match read(fs, image_check::MANIFEST_MEMBER)? {
        Some((_, data)) if !data.is_empty() => (
            "the image's file list".into(),
            parse_manifest(&String::from_utf8_lossy(&data)),
        ),
        _ => match (saved, read(fs, SUMS)?) {
            (Some(saved), Some((_, sums))) if saved.sums.as_deref() == Some(&sums[..]) => (
                "the firmware image saved on this computer".into(),
                saved.manifest.clone(),
            ),
            _ => ("nothing: no file list for this system".into(), Vec::new()),
        },
    })
}

/// For files whose contents don't match: the identical bytes that restore
/// each, or why there are none. Sources are the other root's copy, then the
/// saved image; a copy counts only if its SHA-256 is the expected one and
/// its size is the damaged file's.
fn sources(
    other: Option<&Filesystem>,
    saved: Option<&SavedImage>,
    unclean: bool,
    suspects: Vec<(String, String, u64)>,
) -> Result<(Vec<Fix>, Vec<DamagedFile>)> {
    let mut found: HashMap<String, Vec<u8>> = HashMap::new();
    let mut missing = HashSet::new();
    for (path, want, _) in &suspects {
        match other.map(|fs| read(fs, path)).transpose()?.flatten() {
            Some((_, data)) if files::sum(&data) == *want => {
                found.insert(path.clone(), data);
            }
            _ => {
                missing.insert(path.clone());
            }
        }
    }
    if let Some(saved) = saved {
        found.extend(saved_members(&saved.path, &missing)?);
    }
    let (mut fixes, mut damaged) = (Vec::new(), Vec::new());
    for (path, want, size) in suspects {
        let why = match found.remove(&path) {
            _ if unclean => Some(
                "the mixer wasn't shut down cleanly, so nothing is written to this system; start the mixer normally once, then check again".into(),
            ),
            Some(data) if files::sum(&data) != want => {
                Some("no identical copy to restore it from".into())
            }
            Some(data) if data.len() as u64 != size => {
                Some("its size changed; a repair only replaces contents of the same size".into())
            }
            Some(data) => {
                fixes.push(Fix {
                    path: path.clone(),
                    want,
                    data,
                });
                None
            }
            None => Some("no identical copy to restore it from".into()),
        };
        damaged.push(DamagedFile { path, why });
    }
    Ok((fixes, damaged))
}

/// One root's manifest compared with its files: the missing or irregular
/// ones, and the ones whose contents differ (path, expected hash, size).
type Comparison = (Vec<DamagedFile>, Vec<(String, String, u64)>);

impl Engine {
    fn compare(
        &mut self,
        fs: &Filesystem,
        index: u8,
        manifest: &[(String, String)],
    ) -> Result<Comparison> {
        let detail = format!("Checking system partition {index}");
        let (mut broken, mut suspects) = (Vec::new(), Vec::new());
        let total = manifest.len() as u64;
        for (done, (want, path)) in manifest.iter().enumerate() {
            if self.cancelled() {
                bail!("{}", crate::CANCELLED);
            }
            if done % 64 == 0 {
                self.reporter("repair-check", &detail)(done as u64, total);
            }
            match read(fs, path)? {
                None => broken.push(DamagedFile {
                    path: path.clone(),
                    why: Some("it is missing; a repair only restores files that are there".into()),
                }),
                Some((entry, _)) if !entry.regular => broken.push(DamagedFile {
                    path: path.clone(),
                    why: Some("it is no longer a regular file".into()),
                }),
                Some((entry, data)) if files::sum(&data) != *want => {
                    suspects.push((path.clone(), want.clone(), entry.size))
                }
                Some(_) => {}
            }
        }
        Ok((broken, suspects))
    }

    /// The saved image, read only when a root needs it.
    fn saved_image(&mut self) -> Option<SavedImage> {
        let archive = self.work.join("archive.tar.gz");
        if !files::exists(&archive) {
            return None;
        }
        self.step(
            "repair-check",
            "Reading the firmware image saved on this computer",
        );
        saved_image(&archive).ok()
    }

    /// Look the mixer's storage over. Reads only.
    pub(crate) fn repair_inspect(&mut self) -> Result<RepairFindings> {
        let device = self.platform.recovery_device()?;
        self.step("repair-check", "Reading the mixer's partitions");
        let layout = layout(&device.path, device.len)?;
        let saved = self.saved_image();
        let mut roots = Vec::new();
        for index in 0..2 {
            let part = layout.parts[index];
            let fs = Filesystem::open(&layout.device, part.start, false).map_err(ext2)?;
            let other = Filesystem::open(&layout.device, layout.parts[1 - index].start, false).ok();
            let unclean = fs.needs_recovery();
            let (reference, manifest) = manifest_of(&fs, saved.as_ref())?;
            let (mut damaged, suspects) = self.compare(&fs, index as u8 + 1, &manifest)?;
            let (_, more) = sources(other.as_ref(), saved.as_ref(), unclean, suspects)?;
            damaged.extend(more);
            damaged.sort_by(|a, b| a.path.cmp(&b.path));
            roots.push(RootFindings {
                index: index as u8 + 1,
                reference,
                unclean,
                damaged,
            });
        }
        Ok(RepairFindings {
            refused: None,
            hostname: layout.hostname,
            settings_ok: layout.settings_ok,
            roots,
        })
    }

    /// Back up the settings partition, then restore the files the user
    /// approved from a check. Nothing from the app is trusted: the layout,
    /// each file's expected hash, its damage and its source are all checked
    /// again here, for those files only, and nothing else is written.
    pub(crate) fn repair(&mut self, targets: &[RepairTarget]) -> Result<RepairResult> {
        let device = self.platform.recovery_device()?;
        self.step("repair-check", "Reading the mixer's partitions");
        let layout = layout(&device.path, device.len)?;
        let mut saved = None;
        let mut plans = Vec::new();
        let mut result = RepairResult::default();
        for index in 0..2usize {
            let wanted: Vec<&str> = targets
                .iter()
                .filter(|t| usize::from(t.root) == index + 1)
                .map(|t| t.path.as_str())
                .collect();
            if wanted.is_empty() {
                continue;
            }
            let part = layout.parts[index];
            let fs = Filesystem::open(&layout.device, part.start, false).map_err(ext2)?;
            let other = Filesystem::open(&layout.device, layout.parts[1 - index].start, false).ok();
            if read(&fs, image_check::MANIFEST_MEMBER)?.is_none() && saved.is_none() {
                saved = self.saved_image();
            }
            let (_, manifest) = manifest_of(&fs, saved.as_ref())?;
            let expected: HashMap<&str, &str> = manifest
                .iter()
                .map(|(sha, path)| (path.as_str(), sha.as_str()))
                .collect();
            let mut listed = Vec::new();
            for path in wanted {
                match expected.get(path) {
                    Some(want) => listed.push((want.to_string(), path.to_owned())),
                    None => result.not_repaired.push((
                        path.to_owned(),
                        "it is not in this system's file list".into(),
                    )),
                }
            }
            let (broken, suspects) = self.compare(&fs, index as u8 + 1, &listed)?;
            for file in broken {
                result
                    .not_repaired
                    .push((file.path, file.why.unwrap_or_default()));
            }
            // Asked for, but intact now: nothing to do.
            for (_, path) in &listed {
                if !suspects.iter().any(|(p, _, _)| p == path)
                    && !result.not_repaired.iter().any(|(p, _)| p == path)
                {
                    result.repaired.push(path.clone());
                }
            }
            let (fixes, damaged) = sources(
                other.as_ref(),
                saved.as_ref(),
                fs.needs_recovery(),
                suspects,
            )?;
            for file in damaged {
                if let Some(why) = file.why {
                    result.not_repaired.push((file.path, why));
                }
            }
            plans.push((index, fixes));
        }
        if plans.iter().all(|(_, fixes)| fixes.is_empty()) {
            return Ok(result);
        }
        result.backup = self
            .backup_settings(&layout)?
            .to_string_lossy()
            .into_owned();
        for (index, fixes) in plans {
            if fixes.is_empty() {
                continue;
            }
            let part = layout.parts[index];
            self.step(
                "repair-write",
                &format!("Restoring files on system partition {}", index + 1),
            );
            let mut fs = Filesystem::open(&layout.device, part.start, true).map_err(ext2)?;
            if fs.needs_recovery() {
                bail!("the mixer's system was not shut down cleanly; nothing was written");
            }
            for fix in &fixes {
                let Some(entry) = fs.lookup(&fix.path).map_err(ext2)? else {
                    continue;
                };
                fs.overwrite(&entry, &fix.data).map_err(ext2)?;
            }
            fs.close().map_err(ext2)?;
            // Read everything back from the disk.
            let fs = Filesystem::open(&layout.device, part.start, false).map_err(ext2)?;
            for fix in fixes {
                match read(&fs, &fix.path)? {
                    Some((_, data)) if files::sum(&data) == fix.want => {
                        result.repaired.push(fix.path)
                    }
                    _ => result.not_repaired.push((
                        fix.path,
                        "the restored copy didn't read back identical".into(),
                    )),
                }
            }
        }
        drop(device);
        Ok(result)
    }

    /// A copy of the settings partition (the mixer's setup, shows, Wi-Fi and
    /// CallMe pairing, serial) in the work folder: private to the user who
    /// started the installer, even when the helper writes it as root.
    fn backup_settings(&mut self, layout: &Layout) -> Result<PathBuf> {
        let part = layout.parts[2];
        let dir = self.work.join("repair");
        files::private_dirs(&dir)?;
        let name = format!("settings-partition-{}.img", crate::now_unix());
        let path = dir.join(&name);
        let mut disk = std::fs::File::open(&layout.device)?;
        disk.seek(SeekFrom::Start(part.start))?;
        let mut out = files::Hashing::new(files::Temp::new(&dir, ".settings-")?);
        let mut buf = vec![0u8; 1 << 20];
        let mut done = 0;
        while done < part.len {
            if self.cancelled() {
                bail!("{}", crate::CANCELLED);
            }
            let n = buf.len().min((part.len - done) as usize);
            disk.read_exact(&mut buf[..n])?;
            out.write_all(&buf[..n])?;
            done += n as u64;
            self.reporter(
                "repair-backup",
                "Copying the settings partition to this computer",
            )(done, part.len);
        }
        let (temp, sha, _) = out.finish();
        temp.persist(&path)?;
        let mut sum = files::Temp::new(&dir, ".settings-")?;
        sum.write_all(format!("{sha}  {name}\n").as_bytes())?;
        sum.persist(&path.with_extension("img.sha256"))?;
        Ok(path)
    }
}

#[cfg(test)]
mod tests;
