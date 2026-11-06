//! Windows: SetupAPI/HID for the control interface, Win32 volumes and the
//! Shell eject verb.

use super::{Hid, Platform, Reports, Volume};
use crate::{
    Error, Result, bail,
    usb::{VENDOR_ID, update_size},
};
use rc2fx_usb::{INPUT_ID, OUTPUT_ID};
use std::{
    ffi::c_void,
    io, mem, ptr,
    time::{Duration, Instant},
};
use windows_sys::{
    Win32::{
        Devices::{
            DeviceAndDriverInstallation::{
                DIGCF_DEVICEINTERFACE, DIGCF_PRESENT, SP_DEVICE_INTERFACE_DATA,
                SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInterfaces, SetupDiGetClassDevsW,
                SetupDiGetDeviceInterfaceDetailW,
            },
            HumanInterfaceDevice::{
                HIDD_ATTRIBUTES, HIDP_CAPS, HIDP_REPORT_TYPE, HIDP_STATUS_SUCCESS, HIDP_VALUE_CAPS,
                HidD_FreePreparsedData, HidD_GetAttributes, HidD_GetHidGuid, HidD_GetPreparsedData,
                HidD_GetSerialNumberString, HidD_SetNumInputBuffers, HidP_GetCaps,
                HidP_GetValueCaps, HidP_Input, HidP_Output,
            },
        },
        Foundation::{
            CloseHandle, ERROR_IO_PENDING, ERROR_NO_MORE_ITEMS, GENERIC_READ, GENERIC_WRITE,
            HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
        },
        Storage::FileSystem::{
            CreateFileW, FILE_FLAG_OVERLAPPED, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ,
            FILE_SHARE_WRITE, FlushFileBuffers, GetDiskFreeSpaceExW, GetDriveTypeW,
            GetLogicalDriveStringsW, GetVolumeInformationW, OPEN_EXISTING, ReadFile, WriteFile,
        },
        System::{
            Diagnostics::Debug::{
                SEM_FAILCRITICALERRORS, SEM_NOOPENFILEERRORBOX, SetThreadErrorMode,
            },
            IO::{CancelIoEx, DeviceIoControl, GetOverlappedResult, OVERLAPPED},
            Ioctl::{
                FSCTL_DISMOUNT_VOLUME, FSCTL_LOCK_VOLUME, IOCTL_STORAGE_EJECT_MEDIA,
                IOCTL_STORAGE_MEDIA_REMOVAL, IOCTL_STORAGE_QUERY_PROPERTY,
            },
            Threading::{CreateEventW, WaitForSingleObject},
        },
    },
    core::GUID,
};

const DRIVE_REMOVABLE: u32 = 2;

pub(super) struct Host;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

fn from_wide(s: &[u16]) -> String {
    let end = s.iter().position(|&c| c == 0).unwrap_or(s.len());
    String::from_utf16_lossy(&s[..end])
}

/// A handle closed on drop.
struct Owned(HANDLE);

impl Drop for Owned {
    fn drop(&mut self) {
        // SAFETY: the handle was returned open by CreateFileW/CreateEventW.
        unsafe { CloseHandle(self.0) };
    }
}

fn open_path(name: &str, access: u32, flags: FILE_FLAGS_AND_ATTRIBUTES) -> io::Result<Owned> {
    let name = wide(name);
    // SAFETY: `name` is NUL-terminated and outlives the call.
    let handle = unsafe {
        CreateFileW(
            name.as_ptr(),
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            ptr::null(),
            OPEN_EXISTING,
            flags,
            ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    Ok(Owned(handle))
}

fn inspect_hid(device: HANDLE, name: &str) -> Result<Option<Hid>> {
    // SAFETY: zeroed is a valid HIDD_ATTRIBUTES; Size is set before the call.
    let mut a: HIDD_ATTRIBUTES = unsafe { mem::zeroed() };
    a.Size = mem::size_of::<HIDD_ATTRIBUTES>() as u32;
    // SAFETY: `device` is an open handle and `a` is a valid out pointer.
    if !unsafe { HidD_GetAttributes(device, &mut a) } {
        return Err(io::Error::last_os_error().into());
    }
    if a.VendorID != VENDOR_ID {
        return Ok(None);
    }
    let mut preparsed = 0;
    // SAFETY: valid handle and out pointer.
    if !unsafe { HidD_GetPreparsedData(device, &mut preparsed) } {
        return Err(io::Error::last_os_error().into());
    }
    // SAFETY: zeroed HIDP_CAPS is valid; preparsed data is freed below.
    let mut caps: HIDP_CAPS = unsafe { mem::zeroed() };
    let status = unsafe { HidP_GetCaps(preparsed, &mut caps) };
    // RC2FX's reports sit in RØDE's collection; their presence is its layout.
    let control = status == HIDP_STATUS_SUCCESS
        && declares(
            preparsed,
            HidP_Output,
            caps.NumberOutputValueCaps,
            OUTPUT_ID,
        )
        && declares(preparsed, HidP_Input, caps.NumberInputValueCaps, INPUT_ID);
    // SAFETY: obtained from HidD_GetPreparsedData above.
    unsafe { HidD_FreePreparsedData(preparsed) };
    if status != HIDP_STATUS_SUCCESS {
        bail!("cannot read HID report capabilities");
    }
    let mut serial = [0u16; 256];
    // SAFETY: the buffer length is given in bytes.
    let ok = unsafe {
        HidD_GetSerialNumberString(
            device,
            serial.as_mut_ptr().cast::<c_void>(),
            mem::size_of_val(&serial) as u32,
        )
    };
    if !ok && caps.UsagePage == 0xff00 && a.ProductID != 0x26 {
        bail!("RØDE USB control interface has no readable serial");
    }
    Ok(Some(Hid {
        path: name.to_owned(),
        serial: from_wide(&serial),
        pid: a.ProductID,
        usage: caps.UsagePage,
        input: caps.InputReportByteLength as usize,
        output: caps.OutputReportByteLength as usize,
        control,
    }))
}

/// Whether the collection declares report `id` of `kind`.
fn declares(preparsed: isize, kind: HIDP_REPORT_TYPE, count: u16, id: u8) -> bool {
    // SAFETY: zeroed HIDP_VALUE_CAPS is valid plain data.
    let mut caps = vec![unsafe { mem::zeroed::<HIDP_VALUE_CAPS>() }; usize::from(count)];
    let mut n = count;
    // SAFETY: `caps` holds `n` entries; preparsed data is live in the caller.
    let status = unsafe { HidP_GetValueCaps(kind, caps.as_mut_ptr(), &mut n, preparsed) };
    status == HIDP_STATUS_SUCCESS
        && caps[..usize::from(n.min(count))]
            .iter()
            .any(|c| c.ReportID == id)
}

struct WinReports(Owned);

impl WinReports {
    fn transfer(
        &mut self,
        buf: *mut u8,
        len: usize,
        write: bool,
        timeout: Duration,
    ) -> Result<usize> {
        // SAFETY: manual-reset, initially unsignalled, unnamed event.
        let event = unsafe { CreateEventW(ptr::null(), 1, 0, ptr::null()) };
        if event.is_null() {
            return Err(io::Error::last_os_error().into());
        }
        let event = Owned(event);
        // SAFETY: zeroed OVERLAPPED is valid; the event is set below.
        let mut over: OVERLAPPED = unsafe { mem::zeroed() };
        over.hEvent = event.0;
        let mut count = 0u32;
        // SAFETY: `buf` stays valid until the operation completes or is
        // cancelled and waited for below.
        let ok = unsafe {
            if write {
                WriteFile(self.0.0, buf, len as u32, &mut count, &mut over)
            } else {
                ReadFile(self.0.0, buf, len as u32, &mut count, &mut over)
            }
        };
        if ok == 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(ERROR_IO_PENDING as i32) {
                return Err(error.into());
            }
            let ms = timeout.as_millis().clamp(1, u32::MAX as u128) as u32;
            // SAFETY: valid event handle.
            let status = unsafe { WaitForSingleObject(event.0, ms) };
            if status != WAIT_OBJECT_0 {
                // SAFETY: cancel and wait so the kernel no longer uses `buf`.
                unsafe {
                    CancelIoEx(self.0.0, &over);
                    GetOverlappedResult(self.0.0, &over, &mut count, 1);
                }
                bail!("USB transfer timed out");
            }
            // SAFETY: the operation completed; read its byte count.
            if unsafe { GetOverlappedResult(self.0.0, &over, &mut count, 0) } == 0 {
                return Err(io::Error::last_os_error().into());
            }
        }
        Ok(count as usize)
    }
}

impl Reports for WinReports {
    fn write(&mut self, report: &[u8], timeout: Duration) -> Result<usize> {
        let mut data = report.to_vec();
        self.transfer(data.as_mut_ptr(), data.len(), true, timeout)
    }
    fn read(&mut self, buf: &mut [u8], timeout: Duration) -> Result<usize> {
        self.transfer(buf.as_mut_ptr(), buf.len(), false, timeout)
    }
}

fn drive_root(p: &str) -> Result<String> {
    let mut p = p.to_owned();
    if p.len() == 2 {
        p.push('\\');
    }
    let b = p.as_bytes();
    if b.len() != 3
        || b[1] != b':'
        || (b[2] != b'\\' && b[2] != b'/')
        || !b[0].is_ascii_alphabetic()
    {
        bail!("expected a Windows drive root");
    }
    Ok(format!("{}:\\", (b[0] as char).to_ascii_uppercase()))
}

/// Filesystem name and capacity of a mounted volume. Never shows an Insert
/// Disk dialog for an empty slot.
fn volume_info(root: &str) -> io::Result<(String, u64, u64)> {
    let path = wide(root);
    let mut previous = 0;
    // SAFETY: thread-local error mode; restored before returning.
    if unsafe {
        SetThreadErrorMode(
            SEM_FAILCRITICALERRORS | SEM_NOOPENFILEERRORBOX,
            &mut previous,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let result = (|| {
        let mut fs = [0u16; 32];
        // SAFETY: the buffer and its length match.
        let ok = unsafe {
            GetVolumeInformationW(
                path.as_ptr(),
                ptr::null_mut(),
                0,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                fs.as_mut_ptr(),
                fs.len() as u32,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        let (mut free, mut total) = (0u64, 0u64);
        // SAFETY: valid out pointers.
        if unsafe { GetDiskFreeSpaceExW(path.as_ptr(), &mut free, &mut total, ptr::null_mut()) }
            == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok((from_wide(&fs), total, free))
    })();
    // SAFETY: restores the previous thread error mode.
    unsafe { SetThreadErrorMode(previous, ptr::null_mut()) };
    result
}

/// The device path of the volume at a drive root, such as `\\.\E:`.
fn volume_path(root: &str) -> String {
    format!(r"\\.\{}", &root[..2])
}

/// USB storage serial and whether the volume is on a removable USB bus.
fn storage_identity(root: &str) -> Result<(String, bool)> {
    let device = open_path(&volume_path(root), 0, 0)?;
    let query = [0u8; 12]; // StorageDeviceProperty, PropertyStandardQuery
    let mut buf = vec![0u8; 4096];
    let mut n = 0u32;
    // SAFETY: input/output buffers and their sizes match.
    let ok = unsafe {
        DeviceIoControl(
            device.0,
            IOCTL_STORAGE_QUERY_PROPERTY,
            query.as_ptr().cast(),
            query.len() as u32,
            buf.as_mut_ptr().cast(),
            buf.len() as u32,
            &mut n,
            ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error().into());
    }
    let n = n as usize;
    if n < 36 {
        bail!("short storage descriptor");
    }
    let word =
        |at: usize| u32::from_le_bytes(buf[at..at + 4].try_into().expect("4 bytes")) as usize;
    let (offset, bus) = (word(24), word(28));
    if offset < 36 || offset >= n {
        bail!("USB storage serial is missing");
    }
    let serial = &buf[offset..n];
    let end = serial.iter().position(|&b| b == 0).unwrap_or(serial.len());
    let serial = String::from_utf8_lossy(&serial[..end]).trim().to_owned();
    // BusTypeUsb is 7; byte 10 is RemovableMedia.
    Ok((serial, bus == 7 && buf[10] != 0))
}

fn no_media(e: &io::Error) -> bool {
    matches!(e.raw_os_error(), Some(2 | 3 | 15 | 21 | 55 | 1112 | 1167))
}

impl Platform for Host {
    fn inventory(&self) -> Result<Vec<Hid>> {
        // SAFETY: zeroed GUID is a valid out value.
        let mut guid: GUID = unsafe { mem::zeroed() };
        unsafe { HidD_GetHidGuid(&mut guid) };
        // SAFETY: valid GUID pointer; destroyed below.
        let set = unsafe {
            SetupDiGetClassDevsW(
                &guid,
                ptr::null(),
                ptr::null_mut(),
                DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
            )
        };
        if set == -1 {
            return Err(io::Error::last_os_error().into());
        }
        struct Set(isize);
        impl Drop for Set {
            fn drop(&mut self) {
                // SAFETY: obtained from SetupDiGetClassDevsW.
                unsafe { SetupDiDestroyDeviceInfoList(self.0) };
            }
        }
        let set = Set(set);
        let mut all = Vec::new();
        for i in 0.. {
            // SAFETY: zeroed interface data with cbSize set.
            let mut data: SP_DEVICE_INTERFACE_DATA = unsafe { mem::zeroed() };
            data.cbSize = mem::size_of::<SP_DEVICE_INTERFACE_DATA>() as u32;
            if unsafe { SetupDiEnumDeviceInterfaces(set.0, ptr::null(), &guid, i, &mut data) } == 0
            {
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(ERROR_NO_MORE_ITEMS as i32) {
                    break;
                }
                return Err(error.into());
            }
            let mut needed = 0u32;
            // SAFETY: size query with a null buffer.
            unsafe {
                SetupDiGetDeviceInterfaceDetailW(
                    set.0,
                    &data,
                    ptr::null_mut(),
                    0,
                    &mut needed,
                    ptr::null_mut(),
                )
            };
            if !(6..=65536).contains(&needed) {
                bail!("invalid HID interface path size");
            }
            // u32-aligned storage for SP_DEVICE_INTERFACE_DETAIL_DATA_W.
            let mut buf = vec![0u32; (needed as usize).div_ceil(4)];
            buf[0] = if cfg!(target_pointer_width = "64") {
                8
            } else {
                6
            };
            // SAFETY: the buffer holds `needed` bytes with cbSize set.
            if unsafe {
                SetupDiGetDeviceInterfaceDetailW(
                    set.0,
                    &data,
                    buf.as_mut_ptr().cast(),
                    needed,
                    ptr::null_mut(),
                    ptr::null_mut(),
                )
            } == 0
            {
                return Err(io::Error::last_os_error().into());
            }
            // SAFETY: DevicePath is a NUL-terminated UTF-16 string at offset 4.
            let path = unsafe {
                std::slice::from_raw_parts(
                    buf.as_ptr().cast::<u8>().add(4).cast::<u16>(),
                    (needed as usize - 4) / 2,
                )
            };
            let name = from_wide(path);
            if !name.to_ascii_lowercase().contains("vid_19f7") {
                continue;
            }
            let device = open_path(&name, 0, 0)
                .map_err(|e| Error(format!("inspect RØDE USB interface: {e}")))?;
            if let Some(info) = inspect_hid(device.0, &name)?
                && info.pid != 0
            {
                all.push(info);
            }
        }
        Ok(all)
    }

    fn open(&self, h: &Hid) -> Result<Box<dyn Reports>> {
        let handle = open_path(&h.path, GENERIC_READ | GENERIC_WRITE, FILE_FLAG_OVERLAPPED)?;
        let actual = inspect_hid(handle.0, &h.path).ok().flatten();
        if actual.as_ref().is_none_or(|a| {
            a.serial != h.serial || a.pid != h.pid || a.usage != h.usage || a.output != h.output
        }) {
            bail!("HID identity or capabilities changed before opening");
        }
        // RØDE's own input reports share this collection; a deep queue keeps
        // a burst of them from displacing RC2FX's replies (512 is the most).
        // SAFETY: open HID handle.
        if !unsafe { HidD_SetNumInputBuffers(handle.0, 512) } {
            return Err(io::Error::last_os_error().into());
        }
        Ok(Box::new(WinReports(handle)))
    }

    fn volumes(&self) -> Result<Vec<Volume>> {
        let mut roots = [0u16; 512];
        // SAFETY: the buffer length is given in characters.
        let n = unsafe { GetLogicalDriveStringsW(roots.len() as u32, roots.as_mut_ptr()) } as usize;
        if n == 0 {
            return Err(io::Error::last_os_error().into());
        }
        if n >= roots.len() {
            bail!("drive inventory too large");
        }
        let mut result = Vec::new();
        let mut failure = None;
        for root in roots[..n].split(|&c| c == 0).map(String::from_utf16_lossy) {
            if root.is_empty() {
                continue;
            }
            // SAFETY: NUL-terminated path.
            if unsafe { GetDriveTypeW(wide(&root).as_ptr()) } != DRIVE_REMOVABLE {
                continue;
            }
            let (fs, total, free) = match volume_info(&root) {
                Ok(info) => info,
                Err(e) if no_media(&e) => continue,
                Err(e) => {
                    failure.get_or_insert(Error(format!("cannot read drive {root}: {e}")));
                    continue;
                }
            };
            if !update_size(total) {
                continue;
            }
            let (serial, usb) = match storage_identity(&root) {
                Ok(identity) => identity,
                Err(e) => {
                    failure.get_or_insert(Error(format!("cannot identify drive {root}: {e}")));
                    continue;
                }
            };
            if usb {
                result.push(Volume {
                    path: root.clone(),
                    device: root,
                    serial,
                    fs,
                    total,
                    free,
                });
            }
        }
        // A locked or unformatted drive elsewhere must not hide the mixer's.
        match failure {
            Some(e) if result.is_empty() => Err(e),
            _ => Ok(result),
        }
    }

    fn mount(&self, mut v: Volume) -> Result<Volume> {
        let (fs, total, free) = volume_info(&v.path)?;
        v.fs = fs;
        v.total = total;
        v.free = free;
        Ok(v)
    }

    fn media_present(&self, v: &Volume) -> Result<bool> {
        let root = drive_root(&v.path)?;
        match volume_info(&root) {
            Ok(_) => Ok(true),
            Err(e) if no_media(&e) => Ok(false),
            Err(e) => bail!("cannot inspect update media: {e}"),
        }
    }

    /// Eject through the volume itself, as Microsoft documents for removable
    /// media: lock (retrying while something briefly holds a file open, such
    /// as a virus scan of the new firmware), dismount, then eject the media.
    /// Unlike the Shell's eject verb this never shows a dialog, and it never
    /// removes the USB parent device.
    fn eject(&self, v: &Volume) -> Result<()> {
        let root = drive_root(&v.path)?;
        let (serial, usb) = storage_identity(&root)?;
        if !usb || serial != v.serial {
            bail!("volume identity changed before ejection");
        }
        let device = open_path(&volume_path(&root), GENERIC_READ | GENERIC_WRITE, 0)
            .map_err(|e| Error(format!("cannot open the update drive to eject it: {e}")))?;
        let control = |code: u32, input: &[u8]| -> io::Result<()> {
            let mut returned = 0u32;
            // SAFETY: the input buffer and its size match; no output buffer.
            let ok = unsafe {
                DeviceIoControl(
                    device.0,
                    code,
                    if input.is_empty() {
                        ptr::null()
                    } else {
                        input.as_ptr().cast()
                    },
                    input.len() as u32,
                    ptr::null_mut(),
                    0,
                    &mut returned,
                    ptr::null_mut(),
                )
            };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        };
        let deadline = Instant::now() + Duration::from_secs(20);
        while let Err(e) = control(FSCTL_LOCK_VOLUME, &[]) {
            if Instant::now() > deadline {
                bail!(
                    "something on this computer is still using the update drive, such as an Explorer window or a virus scan. Close it and try again; nothing was flashed ({e})"
                );
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        // SAFETY: valid volume handle.
        unsafe { FlushFileBuffers(device.0) };
        control(FSCTL_DISMOUNT_VOLUME, &[])
            .map_err(|e| Error(format!("could not dismount the update drive: {e}")))?;
        // PREVENT_MEDIA_REMOVAL { PreventMediaRemoval: FALSE }; not every
        // device supports it, and ejection decides.
        let _ = control(IOCTL_STORAGE_MEDIA_REMOVAL, &[0]);
        control(IOCTL_STORAGE_EJECT_MEDIA, &[])
            .map_err(|e| Error(format!("could not eject the update drive: {e}")))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn volume_paths() {
        let root = super::drive_root("e:").unwrap();
        assert_eq!(root, r"E:\");
        assert_eq!(super::volume_path(&root), r"\\.\E:");
        assert!(super::drive_root(r"E:\x").is_err());
    }
}
