//! Linux: hidraw for the control interface; sysfs, mountinfo, mount(2) and
//! SCSI commands for the update disk. Requires root (the privileged helper).

use super::{Hid, Platform, Reports, Volume};
use crate::{
    Error, Result, bail,
    files::exists,
    usb::{is_update_pid, update_size, vendor_usage},
};
use std::{
    ffi::CString,
    fs, io,
    os::{fd::RawFd, unix::ffi::OsStrExt},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

pub(super) struct Host;

fn read_trim(path: &Path) -> String {
    fs::read_to_string(path)
        .unwrap_or_default()
        .trim()
        .to_owned()
}

fn root() -> bool {
    // SAFETY: geteuid has no preconditions.
    unsafe { libc::geteuid() == 0 }
}

/// Serial and product ID of the RØDE USB device above a sysfs node.
fn usb_ancestor(start: &Path) -> Result<(String, u16)> {
    let mut current = fs::canonicalize(start)?;
    loop {
        if read_trim(&current.join("idVendor")) == "19f7" {
            let pid = u16::from_str_radix(&read_trim(&current.join("idProduct")), 16)
                .map_err(|e| Error(e.to_string()))?;
            return Ok((read_trim(&current.join("serial")), pid));
        }
        if !current.pop() || current.as_os_str() == "/" {
            bail!("device is not attached to a RØDE USB parent");
        }
    }
}

fn cstring(path: &Path) -> Result<CString> {
    CString::new(path.as_os_str().as_bytes()).map_err(|e| Error(e.to_string()))
}

struct Fd(RawFd);

impl Drop for Fd {
    fn drop(&mut self) {
        // SAFETY: the descriptor is owned and closed once.
        unsafe { libc::close(self.0) };
    }
}

fn open_fd(path: &Path, flags: i32) -> io::Result<Fd> {
    let path = CString::new(path.as_os_str().as_bytes())?;
    // SAFETY: NUL-terminated path.
    let fd = unsafe { libc::open(path.as_ptr(), flags | libc::O_CLOEXEC) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(Fd(fd))
}

fn wait_fd(fd: RawFd, event: i16, deadline: Instant) -> Result<()> {
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            bail!("USB transfer timed out");
        }
        let mut poll = libc::pollfd {
            fd,
            events: event,
            revents: 0,
        };
        let ms = remaining.as_millis().clamp(1, 100) as i32;
        // SAFETY: one valid pollfd.
        let n = unsafe { libc::poll(&mut poll, 1, ms) };
        if n < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(e.into());
        }
        if n > 0 {
            if poll.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
                bail!("USB disconnected or reported an I/O error");
            }
            if poll.revents & event != 0 {
                return Ok(());
            }
        }
    }
}

struct LinuxReports(Fd);

impl Reports for LinuxReports {
    fn write(&mut self, report: &[u8], timeout: Duration) -> Result<usize> {
        wait_fd(self.0.0, libc::POLLOUT, Instant::now() + timeout)?;
        // SAFETY: valid buffer and length.
        let n = unsafe { libc::write(self.0.0, report.as_ptr().cast(), report.len()) };
        if n < 0 {
            return Err(io::Error::last_os_error().into());
        }
        Ok(n as usize)
    }

    fn read(&mut self, buf: &mut [u8], timeout: Duration) -> Result<usize> {
        let deadline = Instant::now() + timeout;
        loop {
            wait_fd(self.0.0, libc::POLLIN, deadline)?;
            // SAFETY: valid buffer and length.
            let n = unsafe { libc::read(self.0.0, buf.as_mut_ptr().cast(), buf.len()) };
            if n >= 0 {
                return Ok(n as usize);
            }
            let e = io::Error::last_os_error();
            if !matches!(e.raw_os_error(), Some(libc::EAGAIN | libc::EINTR)) {
                return Err(e.into());
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Mount {
    pub device: String,
    pub path: String,
    pub fs: String,
}

fn decode_mount(s: &str) -> String {
    s.replace(r"\040", " ")
        .replace(r"\011", "\t")
        .replace(r"\012", "\n")
        .replace(r"\134", "\\")
}

pub(crate) fn parse_mounts(data: &str) -> Result<Vec<Mount>> {
    let mut result = Vec::new();
    for line in data.lines().filter(|l| !l.trim().is_empty()) {
        let Some((left, right)) = line.split_once(" - ") else {
            bail!("invalid mount inventory");
        };
        let left: Vec<_> = left.split_whitespace().collect();
        let right: Vec<_> = right.split_whitespace().collect();
        if left.len() < 6 || right.len() < 3 {
            bail!("invalid mount inventory");
        }
        result.push(Mount {
            device: decode_mount(right[1]),
            path: decode_mount(left[4]),
            fs: right[0].to_owned(),
        });
    }
    Ok(result)
}

fn mounts() -> Result<Vec<Mount>> {
    parse_mounts(&fs::read_to_string("/proc/self/mountinfo")?)
}

/// USB serial and whole-disk node of a Pro II update block device.
fn block_identity(device: &str) -> Result<(String, String)> {
    let path = Path::new(device);
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    if path.parent() != Some(Path::new("/dev")) || name.contains("..") || name.is_empty() {
        bail!("invalid block device");
    }
    let sys = Path::new("/sys/class/block").join(&*name);
    match usb_ancestor(&sys) {
        Ok((serial, pid)) if is_update_pid(pid) && !serial.is_empty() => {
            let mut whole = device.to_owned();
            if exists(&sys.join("partition")) {
                let resolved = fs::canonicalize(&sys)?;
                let parent = resolved
                    .parent()
                    .and_then(Path::file_name)
                    .unwrap_or_default();
                whole = format!("/dev/{}", parent.to_string_lossy());
            }
            Ok((serial, whole))
        }
        _ => bail!("block device is not a RØDECaster update disk"),
    }
}

fn capacity(mut v: Volume) -> Result<Volume> {
    let path = cstring(Path::new(&v.path))?;
    // SAFETY: zeroed statfs is a valid out value; NUL-terminated path.
    let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(path.as_ptr(), &mut stat) } != 0 {
        return Err(io::Error::last_os_error().into());
    }
    if stat.f_type as i64 != 0x4d44 {
        bail!("update filesystem is not FAT");
    }
    v.total = stat.f_blocks as u64 * stat.f_bsize as u64;
    v.free = stat.f_bavail as u64 * stat.f_bsize as u64;
    v.fs = "vfat".into();
    Ok(v)
}

/// Linux sg_io_hdr_t from <scsi/sg.h>. No data transfer is requested.
#[repr(C)]
#[derive(Default)]
pub(crate) struct SgHeader {
    pub interface: i32,
    pub direction: i32,
    pub cmd_len: u8,
    pub sense_len: u8,
    pub iovec_count: u16,
    pub transfer_len: u32,
    pub transfer: usize,
    pub command: usize,
    pub sense: usize,
    pub timeout: u32,
    pub flags: u32,
    pub pack_id: i32,
    pub user: usize,
    pub status: u8,
    pub masked_status: u8,
    pub message: u8,
    pub sense_written: u8,
    pub host_status: u16,
    pub driver_status: u16,
    pub residual: i32,
    pub duration: u32,
    pub info: u32,
}

#[derive(Debug, PartialEq)]
pub(crate) enum Scsi {
    Ok,
    NoMedium,
    UnitAttention,
    Failed(String),
}

pub(crate) fn scsi_status(h: &SgHeader, sense: &[u8]) -> Scsi {
    if h.info & 1 == 0 && h.status == 0 && h.host_status == 0 && h.driver_status == 0 {
        return Scsi::Ok;
    }
    if h.status == 2 && h.host_status == 0 && (h.driver_status == 0 || h.driver_status == 8) {
        let parsed = if sense.len() >= 14 && matches!(sense[0] & 0x7f, 0x70 | 0x71) && sense[7] >= 6
        {
            Some((sense[2] & 0x0f, sense[12]))
        } else if sense.len() >= 8 && matches!(sense[0] & 0x7f, 0x72 | 0x73) {
            Some((sense[1] & 0x0f, sense[2]))
        } else {
            None
        };
        match parsed {
            Some((2, 0x3a)) => return Scsi::NoMedium,
            Some((6, _)) => return Scsi::UnitAttention,
            _ => {}
        }
    }
    let hex: String = sense.iter().map(|b| format!("{b:02x}")).collect();
    Scsi::Failed(format!(
        "SCSI media operation failed (status {:02x}, sense {hex})",
        h.status
    ))
}

fn scsi_no_data(fd: RawFd, command: &[u8]) -> Result<Scsi> {
    let mut sense = [0u8; 64];
    let mut header = SgHeader {
        interface: b'S' as i32,
        direction: -1, // SG_DXFER_NONE
        cmd_len: command.len() as u8,
        sense_len: sense.len() as u8,
        command: command.as_ptr() as usize,
        sense: sense.as_mut_ptr() as usize,
        timeout: 10_000,
        ..Default::default()
    };
    // SAFETY: SG_IO with no data buffer; command and sense outlive the call.
    if unsafe { libc::ioctl(fd, 0x2285, &mut header) } != 0 {
        let e = io::Error::last_os_error();
        if e.raw_os_error() == Some(libc::ENOMEDIUM) {
            return Ok(Scsi::NoMedium);
        }
        return Err(e.into());
    }
    let written = (header.sense_written as usize).min(sense.len());
    Ok(scsi_status(&header, &sense[..written]))
}

impl Platform for Host {
    fn inventory(&self) -> Result<Vec<Hid>> {
        let mut result = Vec::new();
        let Ok(entries) = fs::read_dir("/sys/class/hidraw") else {
            return Ok(result);
        };
        for entry in entries {
            let p = entry?.path();
            let Ok((serial, pid)) = usb_ancestor(&p.join("device")) else {
                continue;
            };
            let descriptor = fs::read(p.join("device").join("report_descriptor"))
                .map_err(|e| Error(format!("read USB report descriptor: {e}")))?;
            if vendor_usage(&descriptor) {
                result.push(Hid {
                    path: format!(
                        "/dev/{}",
                        p.file_name().unwrap_or_default().to_string_lossy()
                    ),
                    serial,
                    pid,
                    usage: 0xff00,
                    input: 4096,
                    output: 64,
                    control: rc2fx_usb::declares_control(&descriptor),
                });
            }
        }
        Ok(result)
    }

    fn open(&self, h: &Hid) -> Result<Box<dyn Reports>> {
        // Fail before M even if a udev rule grants hidraw access: staging and
        // safe media ejection still require the privileged helper.
        if !root() {
            bail!("USB installation requires administrator permission");
        }
        self.open_control(h)
    }

    fn open_control(&self, h: &Hid) -> Result<Box<dyn Reports>> {
        let name = Path::new(&h.path).file_name().unwrap_or_default();
        match usb_ancestor(&Path::new("/sys/class/hidraw").join(name).join("device")) {
            Ok((serial, pid)) if serial == h.serial && pid == h.pid => {}
            _ => bail!("USB identity changed"),
        }
        let fd = open_fd(Path::new(&h.path), libc::O_RDWR | libc::O_NONBLOCK).map_err(|e| {
            // Only a refusal is a permission problem; the app offers to fix
            // that one (helper::USB_RULE).
            if e.kind() == io::ErrorKind::PermissionDenied {
                Error(format!("{}: {e}", crate::helper::USB_PERMISSION))
            } else {
                Error(format!("Couldn't open the mixer's USB control: {e}"))
            }
        })?;
        Ok(Box::new(LinuxReports(fd)))
    }

    fn volumes(&self) -> Result<Vec<Volume>> {
        let mounted = mounts()?;
        let mut result = Vec::new();
        for entry in fs::read_dir("/sys/class/block")? {
            let name = entry?.file_name().to_string_lossy().into_owned();
            let device = format!("/dev/{name}");
            let Ok((serial, _)) = block_identity(&device) else {
                continue;
            };
            let sys = PathBuf::from("/sys/class/block").join(&name);
            let Ok(sectors) = read_trim(&sys.join("size")).parse::<u64>() else {
                continue;
            };
            if !update_size(sectors * 512) {
                continue;
            }
            if !exists(&sys.join("partition")) {
                // A whole disk with partitions is represented by its partition.
                let has_children = fs::read_dir(&sys)?.flatten().any(|child| {
                    child.file_name().to_string_lossy().starts_with(&name)
                        && exists(&child.path().join("partition"))
                });
                if has_children {
                    continue;
                }
            }
            let mut v = Volume {
                device: device.clone(),
                serial,
                total: sectors * 512,
                ..Default::default()
            };
            for m in mounted.iter().filter(|m| m.device == device) {
                if !v.path.is_empty() {
                    bail!("update disk has multiple mounts; close extra mounts before continuing");
                }
                v.path = m.path.clone();
                v.fs = m.fs.clone();
            }
            if !v.path.is_empty() {
                v = capacity(v)?;
            }
            result.push(v);
        }
        Ok(result)
    }

    fn mount(&self, mut v: Volume) -> Result<Volume> {
        match block_identity(&v.device) {
            Ok((serial, _)) if serial == v.serial => {}
            _ => bail!("USB disk identity changed"),
        }
        let paths: Vec<String> = mounts()?
            .into_iter()
            .filter(|m| m.device == v.device)
            .map(|m| m.path)
            .collect();
        if paths.len() > 1 {
            bail!("update volume has multiple mounts");
        }
        if let Some(path) = paths.into_iter().next() {
            v.path = path;
            return capacity(v);
        }
        if !root() {
            bail!("mounting requires administrator permission");
        }
        let mut nonce = [0u8; 6];
        getrandom::fill(&mut nonce).map_err(|e| Error(e.to_string()))?;
        let dir = PathBuf::from(format!("/run/rc2fx-media-{}", crate::files::hex(&nonce)));
        std::os::unix::fs::DirBuilderExt::mode(&mut fs::DirBuilder::new(), 0o700).create(&dir)?;
        let (source, target) = (cstring(Path::new(&v.device))?, cstring(&dir)?);
        let (fstype, options) = (
            CString::new("vfat").expect("static"),
            CString::new("umask=077").expect("static"),
        );
        // SAFETY: NUL-terminated arguments that outlive the call.
        let ok = unsafe {
            libc::mount(
                source.as_ptr(),
                target.as_ptr(),
                fstype.as_ptr(),
                libc::MS_NODEV | libc::MS_NOSUID | libc::MS_NOEXEC,
                options.as_ptr().cast(),
            )
        };
        if ok != 0 {
            let e = io::Error::last_os_error();
            let _ = fs::remove_dir(&dir);
            bail!("mount update volume: {e}");
        }
        v.path = dir.to_string_lossy().into_owned();
        capacity(v)
    }

    fn media_present(&self, v: &Volume) -> Result<bool> {
        if v.device.is_empty() {
            bail!("Linux progress lacks the update block device");
        }
        let (serial, _) = block_identity(&v.device)?;
        if serial != v.serial {
            bail!("update disk identity changed");
        }
        let fd = match open_fd(Path::new(&v.device), libc::O_RDONLY | libc::O_NONBLOCK) {
            Ok(fd) => fd,
            Err(e) if matches!(e.raw_os_error(), Some(libc::ENOMEDIUM | libc::ENXIO)) => {
                return Ok(false);
            }
            Err(e) => return Err(e.into()),
        };
        // BLKGETSIZE64 can retain cached capacity after media removal. Ask
        // the identified LUN directly; only an explicit no-medium response
        // counts as absence. Unit attention is not proof of ejection.
        match scsi_no_data(fd.0, &[0, 0, 0, 0, 0, 0])? {
            Scsi::Ok | Scsi::UnitAttention => Ok(true),
            Scsi::NoMedium => Ok(false),
            Scsi::Failed(e) => Err(Error(e)),
        }
    }

    fn eject(&self, v: &Volume) -> Result<()> {
        let whole = match block_identity(&v.device) {
            Ok((serial, whole)) if serial == v.serial => whole,
            _ => bail!("update volume identity changed before ejection"),
        };
        for m in mounts()?.into_iter().filter(|m| m.device == v.device) {
            let path = cstring(Path::new(&m.path))?;
            // SAFETY: NUL-terminated path.
            if unsafe { libc::umount(path.as_ptr()) } != 0 {
                bail!(
                    "update files are still in use; close them and retry: {}",
                    io::Error::last_os_error()
                );
            }
            let mounted = Path::new(&m.path);
            if mounted.parent() == Some(Path::new("/run"))
                && mounted
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with("rc2fx-media-"))
            {
                let _ = fs::remove_dir(mounted);
            }
        }
        // O_EXCL refuses a disk with another mounted partition. No forced
        // unmount, power-off, USB unbind or removal of the HID parent.
        let fd = open_fd(
            Path::new(&whole),
            libc::O_RDONLY | libc::O_NONBLOCK | libc::O_EXCL,
        )
        .map_err(|e| Error(format!("cannot exclusively access update media: {e}")))?;
        for command in [[0x1e, 0, 0, 0, 0, 0], [0x1b, 0, 0, 0, 2, 0]] {
            match scsi_no_data(fd.0, &command)? {
                Scsi::Ok => {}
                Scsi::NoMedium => bail!("update media disappeared during ejection"),
                Scsi::UnitAttention => bail!("SCSI unit attention during ejection; retry"),
                Scsi::Failed(e) => return Err(Error(e)),
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mount_escapes_and_scsi_header() {
        let mounts =
            parse_mounts("36 25 8:17 / /run/media/a\\040b rw,nosuid - vfat /dev/sdb1 rw\n")
                .unwrap();
        assert_eq!(
            mounts,
            [Mount {
                device: "/dev/sdb1".into(),
                path: "/run/media/a b".into(),
                fs: "vfat".into()
            }]
        );
        assert!(parse_mounts("garbage\n").is_err());
        // sg_io_hdr_t is 88 bytes on 64-bit Linux.
        #[cfg(target_pointer_width = "64")]
        assert_eq!(std::mem::size_of::<SgHeader>(), 88);
    }

    #[test]
    fn media_absence_requires_explicit_sense() {
        let check = SgHeader {
            status: 2,
            info: 1,
            ..Default::default()
        };
        let mut fixed = [0u8; 18];
        fixed[0] = 0x70;
        fixed[2] = 2;
        fixed[7] = 10;
        fixed[12] = 0x3a;
        assert_eq!(scsi_status(&check, &fixed), Scsi::NoMedium);
        fixed[2] = 6;
        assert_eq!(scsi_status(&check, &fixed), Scsi::UnitAttention);
        assert!(matches!(scsi_status(&check, &[]), Scsi::Failed(_)));
        assert_eq!(scsi_status(&SgHeader::default(), &[]), Scsi::Ok);
    }
}
