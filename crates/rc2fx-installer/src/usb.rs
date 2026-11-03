//! The USB update: HID mode commands, copying firmware to the update drive,
//! safe ejection and the flash command.

use crate::{
    CommandRecord, DISK_BYTES, Engine, Hid, Prepared, Result, SECOND, UsbMixer, Volume, bail,
    files, now_unix, utc,
};
use std::{
    path::Path,
    time::{Duration, Instant},
};

#[cfg_attr(target_os = "linux", allow(dead_code))]
pub const VENDOR_ID: u16 = rc2fx_firmware::Model::VENDOR_ID;
#[cfg(test)]
pub const UPDATE_PID: u16 = rc2fx_firmware::Model::ProIi.update_pid();
/// Normal Pro II USB modes from the official `uac2_gadget.sh`. USB 2 (0x26)
/// is never selected.
#[cfg(test)]
pub const NORMAL_PIDS: [u16; 5] = *rc2fx_firmware::Model::ProIi.normal_pids();

/// Inventory is already restricted to RØDE by the platform. Recognition must
/// not broaden the list of devices to which we can send commands.
pub(crate) fn reject_experimental(items: &[Hid], serial: &str, allow: bool) -> Result<()> {
    for h in items
        .iter()
        .filter(|h| h.usage == 0xff00 && (serial.is_empty() || h.serial == serial))
    {
        if let Some(model) =
            rc2fx_firmware::Model::from_usb(rc2fx_firmware::Model::VENDOR_ID, h.pid)
        {
            model.require_runtime_with(allow)?;
        }
    }
    Ok(())
}

const FILES: [&str; 2] = ["archive.tar.gz", "archive.md5"];

fn control(h: &Hid) -> bool {
    h.usage == 0xff00
        && !h.serial.is_empty()
        && rc2fx_firmware::Model::from_usb(rc2fx_firmware::Model::VENDOR_ID, h.pid).is_some()
}

/// Pro II control interfaces, one per mixer.
pub(crate) fn mixers(items: &[Hid]) -> Vec<crate::UsbMixer> {
    items
        .iter()
        .filter(|h| control(h))
        .map(|h| UsbMixer {
            model: rc2fx_firmware::Model::from_usb(rc2fx_firmware::Model::VENDOR_ID, h.pid)
                .unwrap(),
            serial: h.serial.clone(),
            update_mode: is_update_pid(h.pid),
        })
        .collect()
}

/// The single control interface in one of `pids`, optionally for `serial`.
pub(crate) fn is_update_pid(pid: u16) -> bool {
    rc2fx_firmware::Model::ALL
        .iter()
        .any(|m| m.update_pid() == pid)
}

pub(crate) fn select_hid(
    items: &[Hid],
    model: rc2fx_firmware::Model,
    update: bool,
    serial: &str,
    allow: bool,
) -> Result<Hid> {
    reject_experimental(items, "", allow)?;
    model.require_runtime_with(allow)?;
    let devices = mixers(items);
    if devices.len() > 1 && (serial.is_empty() || devices.iter().any(|m| m.model != model)) {
        bail!("more than one RØDECaster is connected; connect only one model");
    }
    let update_pid = [model.update_pid()];
    let pids: &[u16] = if update {
        &update_pid
    } else {
        model.normal_pids()
    };
    let found: Vec<&Hid> = items
        .iter()
        .filter(|h| {
            control(h) && pids.contains(&h.pid) && (serial.is_empty() || serial == h.serial)
        })
        .collect();
    match found.as_slice() {
        [one] => Ok((*one).clone()),
        [] => bail!(
            "no matching {} {}is connected (device model and serial must match the prepared firmware)",
            model.label(),
            if update { "in USB update mode " } else { "" }
        ),
        _ => bail!("more than one RØDECaster is connected; connect only one"),
    }
}

/// Check `archive.tar.gz` and `archive.md5` in `dir` against the prepared
/// firmware, reading the archive once. `progress` receives bytes read.
pub(crate) fn archive_matches(
    dir: &Path,
    p: &Prepared,
    progress: impl FnMut(u64, u64),
) -> Result<()> {
    let archive = dir.join("archive.tar.gz");
    let (sha, md5) = files::hash_both(&archive, progress)?;
    if sha != p.archive_sha || md5 != p.archive_md5 {
        bail!("{} does not match the prepared firmware", archive.display());
    }
    let md = std::fs::read(dir.join("archive.md5"))?;
    if md.trim_ascii() != p.archive_md5.as_bytes() {
        bail!("archive.md5 does not match the prepared firmware");
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn verify_archive(dir: &Path, p: &Prepared) -> Result<()> {
    archive_matches(dir, p, |_, _| {})
}

pub(crate) fn update_size(size: u64) -> bool {
    (DISK_BYTES - (8 << 20)..=DISK_BYTES).contains(&size)
}

fn valid_volume(v: &Volume) -> Result<()> {
    if v.path.is_empty()
        || !["FAT", "FAT32", "vfat"].contains(&v.fs.as_str())
        || !update_size(v.total)
    {
        bail!("the drive is not the mixer's FAT update drive");
    }
    Ok(())
}

/// Copy `src` over `dst` through a temporary file on the same drive. Only
/// our two file names are ever written; the caller reads the copy back.
fn stage_file(src: &Path, dst: &Path, progress: impl FnMut(u64, u64)) -> Result<()> {
    let name = dst.file_name().unwrap_or_default().to_string_lossy();
    let temp = dst.with_file_name(format!(".rc2fx-{name}.part"));
    for path in [dst, temp.as_path()] {
        match std::fs::symlink_metadata(path) {
            Ok(info) if info.is_file() => std::fs::remove_file(path)?,
            Ok(_) => bail!("{} on the update drive is not a file", path.display()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    files::copy_with_progress(src, &temp, progress)?;
    std::fs::rename(&temp, dst)?;
    Ok(())
}

impl Engine {
    fn record_command(&self, command: &str, serial: &str) -> Result<usize> {
        let mut saved = self.load()?;
        saved.commands.push(CommandRecord {
            command: command.into(),
            serial: serial.into(),
            at: utc(),
            at_unix: now_unix(),
            acknowledged: false,
            error: String::new(),
        });
        let excess = saved.commands.len().saturating_sub(50);
        saved.commands.drain(..excess);
        self.save(&saved)?;
        Ok(saved.commands.len() - 1)
    }

    fn finish_command(&self, index: usize, result: &Result<()>) -> Result<()> {
        let mut saved = self.load()?;
        if let Some(c) = saved.commands.get_mut(index) {
            match result {
                Ok(()) => c.acknowledged = true,
                Err(e) => c.error = e.0.clone(),
            }
        }
        self.save(&saved)
    }

    /// Send one HID command letter, logging it first. A missing ACK leaves
    /// the result unknown; nothing is retried.
    fn command(&mut self, h: &Hid, letter: u8, name: &str) -> Result<()> {
        let mut d = self.platform.open(h)?;
        let length = h.output;
        if !(64..=4096).contains(&length) {
            bail!("unexpected HID output report size");
        }
        let input = h.input.max(256);
        if input > 4096 {
            bail!("unexpected HID input report size");
        }
        let mut report = vec![0u8; length];
        report[0] = 1;
        report[1] = letter;
        let index = self.record_command(name, &h.serial)?;
        let result = (|| {
            let n = d.write(&report, 5 * SECOND)?;
            // Windows may pad an output report and return the padded byte
            // count. Only a count shorter than the report is a short write.
            if n < report.len() {
                bail!("HID short write: {n}/{}", report.len());
            }
            let mut buf = vec![0u8; input];
            let n = d.read(&mut buf, 5 * SECOND).map_err(|e| {
                crate::Error(format!(
                    "the mixer did not acknowledge the command, which it may still have received: {e}"
                ))
            })?;
            if n < 2 || buf[0] != 2 || buf[1] != b'A' {
                bail!("the mixer's reply was not the expected acknowledgement");
            }
            Ok(())
        })();
        self.finish_command(index, &result)?;
        result
    }

    /// Switch a normal-mode mixer to USB update mode (HID `M`). Its audio
    /// stops and it shows "Updating via USB"; nothing is written yet.
    pub(crate) fn enter_usb_mode(&mut self, serial: &str) -> Result<()> {
        let prepared = self
            .load()?
            .prepared
            .ok_or("prepare firmware before switching the mixer to update mode")?;
        self.verify_prepared(&prepared)?;
        let all = self.platform.inventory()?;
        reject_experimental(&all, "", self.allow_experimental_duo)?;
        if mixers(&all).iter().any(|m| m.update_mode) {
            bail!("a mixer is already in USB update mode");
        }
        let h = select_hid(
            &all,
            prepared.model.unwrap_or(rc2fx_firmware::Model::ProIi),
            false,
            serial,
            self.allow_experimental_duo,
        )?;
        self.step("usb-mode", "Switching the mixer to USB update mode");
        self.command(&h, b'M', "usb-mode")
    }

    /// Copy the prepared firmware to the mixer's update drive, read it back,
    /// eject the drive safely and send the flash command (HID `U`).
    pub(crate) fn flash(&mut self, serial: &str) -> Result<()> {
        let prepared = self
            .load()?
            .prepared
            .ok_or("prepare firmware before flashing")?;
        self.verify_prepared(&prepared)?;
        let model = prepared.model.unwrap_or(rc2fx_firmware::Model::ProIi);
        let h = select_hid(
            &self.platform.inventory()?,
            model,
            true,
            serial,
            self.allow_experimental_duo,
        )?;
        let owned: Vec<Volume> = self
            .platform
            .volumes()?
            .into_iter()
            .filter(|v| v.serial == h.serial && update_size(v.total))
            .collect();
        let volume = match owned.as_slice() {
            [volume] => volume,
            [] => bail!(
                "the mixer's update drive is not visible. If it was ejected, disconnect the mixer's power for 30 seconds, turn it on and start again; nothing was flashed"
            ),
            _ => bail!("found several update drives for this mixer; connect only one mixer"),
        };
        let v = self.platform.mount(volume.clone())?;
        valid_volume(&v)?;
        let drive = Path::new(&v.path);
        for name in FILES {
            let src = self.work.join(name);
            stage_file(&src, &drive.join(name), self.reporter("stage", name))?;
        }
        let check = self.reporter("verify", "Reading the firmware back from the mixer");
        archive_matches(drive, &prepared, check)?;
        for name in FILES {
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(drive.join(name))?
                .sync_all()?;
        }
        self.step("eject", "Safely ejecting the update drive");
        self.platform.eject(&v)?;
        let deadline = Instant::now() + Duration::from_secs(20);
        while self.platform.media_present(&v)? {
            if Instant::now() > deadline {
                bail!(
                    "the computer has not released the update drive; close anything using it and try again. Nothing was flashed"
                );
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        let h = select_hid(
            &self.platform.inventory()?,
            model,
            true,
            &h.serial,
            self.allow_experimental_duo,
        )?;
        self.step("flash", "Starting the firmware update");
        self.command(&h, b'U', "flash")
    }
}

/// Whether a HID report descriptor declares a vendor-page (0xff00)
/// application collection. Parsing never sends data to the device.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn vendor_usage(descriptor: &[u8]) -> bool {
    let mut page = 0u32;
    let mut stack = Vec::new();
    let mut i = 0;
    while i < descriptor.len() {
        let prefix = descriptor[i];
        i += 1;
        if prefix == 0xfe {
            if i + 2 > descriptor.len() {
                return false;
            }
            i += 2 + descriptor[i] as usize;
            if i > descriptor.len() {
                return false;
            }
            continue;
        }
        let n = match prefix & 3 {
            3 => 4,
            n => n as usize,
        };
        if i + n > descriptor.len() {
            return false;
        }
        let v = descriptor[i..i + n]
            .iter()
            .enumerate()
            .fold(0u32, |v, (j, b)| v | (u32::from(*b) << (8 * j)));
        i += n;
        match prefix & 0xfc {
            0x04 => page = v,
            0xa4 => stack.push(page),
            0xb4 => match stack.pop() {
                Some(p) => page = p,
                None => return false,
            },
            0xa0 if page == 0xff00 => return true,
            _ => {}
        }
    }
    false
}
