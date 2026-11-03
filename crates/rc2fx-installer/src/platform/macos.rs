//! macOS: IOKit HID for the control interface; `ioreg`/`diskutil` for the
//! update disk. Not yet exercised on hardware.

use super::{
    Hid, Platform, Reports, Volume,
    macdisk::{disk_info, usb_disks, valid_disk_name},
};
use crate::{Error, Result, bail, firmware::Bounded, usb::VENDOR_ID, usb::update_size};
use std::{
    collections::{BTreeMap, VecDeque},
    ffi::{CStr, CString, c_char, c_void},
    path::Path,
    process::{Command, Stdio},
    sync::{
        Mutex,
        atomic::{AtomicI32, Ordering},
    },
    time::{Duration, Instant},
};

type CFRef = *const c_void;
const UTF8: u32 = 0x0800_0100;
const SINT64: isize = 4;

#[repr(C)]
struct DictionaryCallbacks([usize; 6]);

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFTypeDictionaryKeyCallBacks: DictionaryCallbacks;
    static kCFTypeDictionaryValueCallBacks: DictionaryCallbacks;
    fn CFRelease(cf: CFRef);
    fn CFRetain(cf: CFRef) -> CFRef;
    fn CFStringCreateWithCString(alloc: CFRef, s: *const c_char, encoding: u32) -> CFRef;
    fn CFStringGetCString(s: CFRef, buf: *mut c_char, size: isize, encoding: u32) -> u8;
    fn CFNumberCreate(alloc: CFRef, kind: isize, value: *const c_void) -> CFRef;
    fn CFNumberGetValue(number: CFRef, kind: isize, value: *mut c_void) -> u8;
    fn CFDictionaryCreate(
        alloc: CFRef,
        keys: *const CFRef,
        values: *const CFRef,
        count: isize,
        key_callbacks: *const DictionaryCallbacks,
        value_callbacks: *const DictionaryCallbacks,
    ) -> CFRef;
    fn CFSetGetCount(set: CFRef) -> isize;
    fn CFGetTypeID(cf: CFRef) -> usize;
    fn CFDataGetTypeID() -> usize;
    fn CFDataGetLength(data: CFRef) -> isize;
    fn CFDataGetBytePtr(data: CFRef) -> *const u8;
    fn CFSetGetValues(set: CFRef, values: *mut CFRef);
    fn CFRunLoopGetCurrent() -> CFRef;
    fn CFRunLoopRunInMode(mode: CFRef, seconds: f64, return_after_source: u8) -> i32;
}

type InputCallback = extern "C" fn(*mut c_void, i32, *mut c_void, u32, u32, *mut u8, isize);

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOHIDManagerCreate(alloc: CFRef, options: u32) -> CFRef;
    fn IOHIDManagerSetDeviceMatching(manager: CFRef, matching: CFRef);
    fn IOHIDManagerCopyDevices(manager: CFRef) -> CFRef;
    fn IOHIDDeviceGetProperty(device: CFRef, key: CFRef) -> CFRef;
    fn IOHIDDeviceGetService(device: CFRef) -> u32;
    fn IORegistryEntryGetRegistryEntryID(entry: u32, id: *mut u64) -> i32;
    fn IOHIDDeviceOpen(device: CFRef, options: u32) -> i32;
    fn IOHIDDeviceClose(device: CFRef, options: u32) -> i32;
    fn IOHIDDeviceRegisterInputReportCallback(
        device: CFRef,
        report: *mut u8,
        length: isize,
        callback: Option<InputCallback>,
        context: *mut c_void,
    );
    fn IOHIDDeviceScheduleWithRunLoop(device: CFRef, run_loop: CFRef, mode: CFRef);
    fn IOHIDDeviceUnscheduleFromRunLoop(device: CFRef, run_loop: CFRef, mode: CFRef);
    fn IOHIDDeviceSetReport(
        device: CFRef,
        kind: u32,
        id: isize,
        report: *const u8,
        length: isize,
    ) -> i32;
}

/// An owned CoreFoundation object.
struct Cf(CFRef);

impl Drop for Cf {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: owned reference from a Create/Copy function or CFRetain.
            unsafe { CFRelease(self.0) };
        }
    }
}

fn cf_string(s: &str) -> Cf {
    let c = CString::new(s).expect("no NUL in static keys");
    // SAFETY: NUL-terminated UTF-8.
    Cf(unsafe { CFStringCreateWithCString(std::ptr::null(), c.as_ptr(), UTF8) })
}

fn property(device: CFRef, name: &str) -> CFRef {
    let key = cf_string(name);
    // SAFETY: borrowed (Get rule) property of a live device.
    unsafe { IOHIDDeviceGetProperty(device, key.0) }
}

fn integer(device: CFRef, name: &str) -> i64 {
    let p = property(device, name);
    let mut n = 0i64;
    if !p.is_null() {
        // SAFETY: CFNumber converted into an i64 out value.
        unsafe { CFNumberGetValue(p, SINT64, (&mut n as *mut i64).cast()) };
    }
    n
}

fn text(device: CFRef, name: &str) -> String {
    let p = property(device, name);
    if p.is_null() {
        return String::new();
    }
    let mut buf = [0 as c_char; 1024];
    // SAFETY: the buffer size is passed.
    if unsafe { CFStringGetCString(p, buf.as_mut_ptr(), buf.len() as isize, UTF8) } == 0 {
        return String::new();
    }
    // SAFETY: CFStringGetCString NUL-terminates on success.
    unsafe { CStr::from_ptr(buf.as_ptr()) }
        .to_string_lossy()
        .into_owned()
}

/// A data property's bytes; empty when absent or not data.
fn bytes(device: CFRef, name: &str) -> Vec<u8> {
    let p = property(device, name);
    // SAFETY: type check before treating the borrowed property as CFData.
    if p.is_null() || unsafe { CFGetTypeID(p) != CFDataGetTypeID() } {
        return Vec::new();
    }
    // SAFETY: CFData owns `length` bytes at its pointer while the device lives.
    unsafe {
        let length = CFDataGetLength(p).max(0) as usize;
        let start = CFDataGetBytePtr(p);
        if start.is_null() {
            return Vec::new();
        }
        std::slice::from_raw_parts(start, length).to_vec()
    }
}

/// Matching RØDE vendor-page HID devices. The manager owns the device set.
struct Devices {
    _manager: Cf,
    _set: Cf,
    items: Vec<(CFRef, Hid)>,
}

fn enumerate() -> Result<Devices> {
    // SAFETY: default allocator, no options.
    let manager = Cf(unsafe { IOHIDManagerCreate(std::ptr::null(), 0) });
    if manager.0.is_null() {
        bail!("cannot initialize macOS USB discovery");
    }
    let key = cf_string("VendorID");
    let vendor = i64::from(VENDOR_ID);
    // SAFETY: i64 value for kCFNumberSInt64Type.
    let number =
        Cf(unsafe { CFNumberCreate(std::ptr::null(), SINT64, (&vendor as *const i64).cast()) });
    // SAFETY: one key/value pair with the standard CFType callbacks.
    let matching = Cf(unsafe {
        CFDictionaryCreate(
            std::ptr::null(),
            &key.0,
            &number.0,
            1,
            &kCFTypeDictionaryKeyCallBacks,
            &kCFTypeDictionaryValueCallBacks,
        )
    });
    if matching.0.is_null() {
        bail!("cannot create USB matching dictionary");
    }
    // SAFETY: valid manager and dictionary; the set is owned (Copy rule).
    let set = unsafe {
        IOHIDManagerSetDeviceMatching(manager.0, matching.0);
        Cf(IOHIDManagerCopyDevices(manager.0))
    };
    let mut items = Vec::new();
    if !set.0.is_null() {
        // SAFETY: valid CFSet.
        let count = unsafe { CFSetGetCount(set.0) };
        if !(0..=64).contains(&count) {
            bail!("unexpected USB device count");
        }
        let mut devices = vec![std::ptr::null(); count as usize];
        if count > 0 {
            // SAFETY: the buffer holds `count` values.
            unsafe { CFSetGetValues(set.0, devices.as_mut_ptr()) };
        }
        for d in devices {
            if integer(d, "VendorID") != i64::from(VENDOR_ID)
                || integer(d, "PrimaryUsagePage") != 0xff00
            {
                continue;
            }
            let mut id = 0u64;
            // SAFETY: the service belongs to a live device.
            if unsafe { IORegistryEntryGetRegistryEntryID(IOHIDDeviceGetService(d), &mut id) } != 0
            {
                bail!("cannot identify HID registry entry");
            }
            items.push((
                d,
                Hid {
                    path: id.to_string(),
                    serial: text(d, "SerialNumber"),
                    pid: integer(d, "ProductID") as u16,
                    usage: 0xff00,
                    input: 4096,
                    output: 64,
                    control: rc2fx_usb::declares_control(&bytes(d, "ReportDescriptor")),
                },
            ));
        }
    }
    Ok(Devices {
        _manager: manager,
        _set: set,
        items,
    })
}

struct Inbox {
    responses: Mutex<VecDeque<Vec<u8>>>,
    error: AtomicI32,
}

extern "C" fn input_callback(
    context: *mut c_void,
    result: i32,
    _sender: *mut c_void,
    _kind: u32,
    _id: u32,
    report: *mut u8,
    length: isize,
) {
    // SAFETY: `context` is the Inbox kept alive by MacReports until the
    // callback is unregistered.
    let inbox = unsafe { &*(context as *const Inbox) };
    if result != 0 {
        inbox.error.store(result, Ordering::SeqCst);
        return;
    }
    if !(0..=4096).contains(&length) || report.is_null() {
        return;
    }
    // SAFETY: IOKit provides `length` valid bytes at `report`.
    let data = unsafe { std::slice::from_raw_parts(report, length as usize) }.to_vec();
    if let Ok(mut queue) = inbox.responses.lock()
        && queue.len() < 8
    {
        queue.push_back(data);
    }
}

/// An open device scheduled on this thread's run loop in a private mode.
/// Every call, including drop, must happen on the opening thread.
struct MacReports {
    device: Cf,
    run_loop: CFRef,
    mode: Cf,
    buffer: Box<[u8; 4096]>,
    inbox: Box<Inbox>,
}

impl Reports for MacReports {
    fn write(&mut self, report: &[u8], _: Duration) -> Result<usize> {
        // SAFETY: output report from a valid buffer.
        let status = unsafe {
            IOHIDDeviceSetReport(
                self.device.0,
                1,
                isize::from(report[0]),
                report.as_ptr(),
                report.len() as isize,
            )
        };
        if status != 0 {
            bail!("macOS HID write failed: 0x{:08x}", status as u32);
        }
        Ok(report.len())
    }

    fn read(&mut self, buf: &mut [u8], timeout: Duration) -> Result<usize> {
        let deadline = Instant::now() + timeout;
        loop {
            let status = self.inbox.error.load(Ordering::SeqCst);
            if status != 0 {
                bail!("macOS HID input failed: 0x{:08x}", status as u32);
            }
            if let Some(report) = self
                .inbox
                .responses
                .lock()
                .ok()
                .and_then(|mut q| q.pop_front())
            {
                let n = report.len().min(buf.len());
                buf[..n].copy_from_slice(&report[..n]);
                return Ok(n);
            }
            if Instant::now() > deadline {
                bail!("USB transfer timed out");
            }
            // SAFETY: runs only our private mode on this thread's run loop.
            unsafe { CFRunLoopRunInMode(self.mode.0, 0.02, 1) };
        }
    }
}

impl Drop for MacReports {
    fn drop(&mut self) {
        // SAFETY: unregister before the buffer and inbox are freed.
        unsafe {
            IOHIDDeviceRegisterInputReportCallback(
                self.device.0,
                self.buffer.as_mut_ptr(),
                self.buffer.len() as isize,
                None,
                std::ptr::null_mut(),
            );
            IOHIDDeviceUnscheduleFromRunLoop(self.device.0, self.run_loop, self.mode.0);
            IOHIDDeviceClose(self.device.0, 0);
        }
    }
}

fn run(program: &str, args: &[&str]) -> Result<Vec<u8>> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| Error(format!("{program}: {e}")))?;
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut stdout = child.stdout.take().expect("piped");
    let reader = std::thread::spawn(move || {
        let mut out = Bounded {
            inner: Vec::new(),
            remaining: 8 << 20,
        };
        std::io::copy(&mut stdout, &mut out).map(|_| out.inner)
    });
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            bail!("{program} timed out");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let mut stderr = String::new();
    if let Some(mut e) = child.stderr.take() {
        use std::io::Read;
        let _ = e.read_to_string(&mut stderr);
    }
    let out = reader
        .join()
        .map_err(|_| Error(format!("{program} output failed")))??;
    if !status.success() {
        let name = Path::new(program)
            .file_name()
            .unwrap_or_default()
            .to_string_lossy();
        bail!("{name}: {} ({status})", stderr.trim());
    }
    Ok(out)
}

fn plist_of(program: &str, args: &[&str]) -> Result<plist::Value> {
    plist::Value::from_reader_xml(run(program, args)?.as_slice())
        .map_err(|e| Error(format!("{program}: {e}")))
}

fn disk_map() -> Result<BTreeMap<String, String>> {
    Ok(usb_disks(&plist_of(
        "/usr/sbin/ioreg",
        &["-a", "-r", "-c", "IOUSBHostDevice"],
    )?))
}

fn statfs(path: &str) -> Result<(bool, u64, u64)> {
    let c = CString::new(path).map_err(|e| Error(e.to_string()))?;
    // SAFETY: zeroed statfs is a valid out value; NUL-terminated path.
    let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(c.as_ptr(), &mut stat) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: f_fstypename is NUL-terminated.
    let name = unsafe { CStr::from_ptr(stat.f_fstypename.as_ptr()) };
    Ok((
        name.to_bytes() == b"msdos",
        stat.f_blocks * u64::from(stat.f_bsize),
        stat.f_bavail * u64::from(stat.f_bsize),
    ))
}

fn volume(device: &str, serial: &str) -> Result<Volume> {
    let name = Path::new(device)
        .file_name()
        .unwrap_or_default()
        .to_string_lossy();
    if !valid_disk_name(&name) || Path::new(device).parent() != Some(Path::new("/dev")) {
        bail!("invalid macOS disk identifier");
    }
    let info = disk_info(&plist_of(
        "/usr/sbin/diskutil",
        &["info", "-plist", device],
    )?)
    .ok_or("invalid disk information")?;
    let mut v = Volume {
        device: device.into(),
        serial: serial.into(),
        path: info.mount_point,
        fs: if info.fat {
            "FAT".into()
        } else {
            String::new()
        },
        total: info.total,
        free: info.free,
    };
    if !v.path.is_empty() {
        let (fat, total, free) = statfs(&v.path)?;
        if !fat {
            bail!("update filesystem is not FAT");
        }
        v.fs = "FAT".into();
        v.total = total;
        v.free = free;
    }
    Ok(v)
}

pub(super) struct Host;

impl Platform for Host {
    fn inventory(&self) -> Result<Vec<Hid>> {
        Ok(enumerate()?.items.into_iter().map(|(_, h)| h).collect())
    }

    fn open(&self, h: &Hid) -> Result<Box<dyn Reports>> {
        let devices = enumerate()?;
        let device = devices
            .items
            .iter()
            .find(|(_, item)| item.path == h.path && item.serial == h.serial && item.pid == h.pid)
            // SAFETY: retain keeps the device alive after the set is freed.
            .map(|(d, _)| Cf(unsafe { CFRetain(*d) }))
            .ok_or("macOS USB identity changed")?;
        drop(devices);
        // SAFETY: valid retained device.
        let status = unsafe { IOHIDDeviceOpen(device.0, 0) };
        if status != 0 {
            bail!("macOS USB access failed: 0x{:08x}", status as u32);
        }
        let mut reports = MacReports {
            device,
            // SAFETY: the current thread's run loop (Get rule).
            run_loop: unsafe { CFRunLoopGetCurrent() },
            mode: cf_string("RC2FXUSB"),
            buffer: Box::new([0; 4096]),
            inbox: Box::new(Inbox {
                responses: Mutex::new(VecDeque::new()),
                error: AtomicI32::new(0),
            }),
        };
        let context = (&*reports.inbox as *const Inbox).cast_mut().cast();
        // SAFETY: buffer and inbox are boxed and outlive the registration,
        // which Drop removes first.
        unsafe {
            IOHIDDeviceRegisterInputReportCallback(
                reports.device.0,
                reports.buffer.as_mut_ptr(),
                reports.buffer.len() as isize,
                Some(input_callback),
                context,
            );
            IOHIDDeviceScheduleWithRunLoop(reports.device.0, reports.run_loop, reports.mode.0);
        }
        Ok(Box::new(reports))
    }

    fn volumes(&self) -> Result<Vec<Volume>> {
        let devices = disk_map()?;
        let mut result = Vec::new();
        for (device, serial) in &devices {
            let v = volume(device, serial)?;
            if !update_size(v.total) {
                continue;
            }
            let partitioned = devices
                .keys()
                .any(|other| other != device && other.starts_with(&format!("{device}s")));
            if !partitioned {
                result.push(v);
            }
        }
        Ok(result)
    }

    fn mount(&self, v: Volume) -> Result<Volume> {
        if disk_map()?.get(&v.device) != Some(&v.serial) {
            bail!("macOS disk ownership changed");
        }
        let mut current = volume(&v.device, &v.serial)?;
        if current.path.is_empty() {
            run("/usr/sbin/diskutil", &["mount", &v.device])?;
            current = volume(&v.device, &v.serial)?;
        }
        Ok(current)
    }

    fn media_present(&self, v: &Volume) -> Result<bool> {
        let Some(serial) = disk_map()?.get(&v.device).cloned() else {
            return Ok(false);
        };
        if serial != v.serial {
            bail!("macOS disk identity changed");
        }
        Ok(volume(&v.device, &serial)?.total > 0)
    }

    fn eject(&self, v: &Volume) -> Result<()> {
        if disk_map()?.get(&v.device) != Some(&v.serial) {
            bail!("macOS disk ownership changed before eject");
        }
        run("/usr/sbin/diskutil", &["eject", &v.device])?;
        Ok(())
    }
}
