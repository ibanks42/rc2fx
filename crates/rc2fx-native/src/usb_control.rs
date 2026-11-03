//! A narrow read hook on the vendor-owned HID descriptor. No second reader,
//! blocking IPC, allocation, or control parsing on ordinary/vendor reads.
use libc::{c_int, c_void, size_t, ssize_t};
use std::{
    ffi::CStr,
    os::unix::net::UnixDatagram,
    path::Path,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

pub static READ: AtomicUsize = AtomicUsize::new(0);
pub static SYSTEM: AtomicUsize = AtomicUsize::new(0);
pub static OPEN: AtomicUsize = AtomicUsize::new(0);
static BRIDGE: OnceLock<(UnixDatagram, std::path::PathBuf)> = OnceLock::new();
static MARKED: AtomicUsize = AtomicUsize::new(0);
static GADGET: Mutex<Gadget> = Mutex::new(Gadget::new());
/// Mirrors `Gadget::keeping` for the open hook, which takes no lock.
static KEEPING: AtomicBool = AtomicBool::new(false);
/// Mirrors `Gadget::starting`: the startup gadget sequence has not completed.
static STARTING: AtomicBool = AtomicBool::new(true);
static LOADED: OnceLock<Instant> = OnceLock::new();

const GADGET_DIR: &str = "/sys/kernel/config/usb_gadget/g124";
const UDC_NAME: &str = "ff400000.usb";
const STAGED_DESCRIPTOR: &str = "/run/rc2fx/usb-report.bin";
const HID_NODE: &str = "/dev/hidg0";
const UNBIND: &[u8] = b"echo \"\" > /sys/kernel/config/usb_gadget/g124/UDC";
const STOP: &[u8] = b"/etc/init.d/uac2_gadget.sh stop ";
const START: &[u8] = b"/etc/init.d/uac2_gadget.sh start ";
/// How long after load the mixer's own startup gadget sequence may begin.
const STARTUP_WINDOW: Duration = Duration::from_secs(120);

/// The vendor's gadget control as this process has seen it. The inspected
/// 1.7.7 mixer unbinds the gadget (`echo "" > UDC`), runs the vendor script's
/// stop and start, writes the controller name into the UDC file and, when
/// that does not read back, resets the USB controller drivers through their
/// sysfs bind files; the same sequence serves startup, a USB mode change and
/// update mode. A restarted RC2FX mixer finds the gadget still bound with
/// RC2FX's descriptor, so its startup sequence is made a no-op: the host never
/// sees a disconnect, its audio streams and RODE's virtual devices survive,
/// and no teardown races live transfers. The next teardown request after
/// startup (a mode change) runs for real.
struct Gadget {
    /// The startup stop/start has not completed yet.
    starting: bool,
    /// This startup keeps the bound gadget; vendor rebuild steps are no-ops
    /// until the next teardown request.
    keeping: bool,
    stop_failed: bool,
}

impl Gadget {
    const fn new() -> Self {
        Self {
            starting: true,
            keeping: false,
            stop_failed: false,
        }
    }
}

/// The bound gadget, read when the vendor asks to tear it down.
#[derive(Debug, Default, PartialEq)]
struct Live {
    /// Bound to the controller with RC2FX's staged descriptor.
    ready: bool,
    /// Its idProduct as sysfs prints it.
    product: Option<String>,
}

/// `node` is the HID character device the kept gadget must still offer; a
/// retired name needs a rebuild to come back.
fn live(dir: &Path, staged: &Path, node: &Path) -> Live {
    use std::os::unix::fs::FileTypeExt;
    let read = |name: &str| std::fs::read(dir.join(name)).ok();
    let text = |bytes: Vec<u8>| String::from_utf8_lossy(&bytes).trim().to_owned();
    let udc = read("UDC").map(text);
    let descriptor = read("functions/hid.usb0/report_desc");
    let ready = udc.as_deref() == Some(UDC_NAME)
        && descriptor.is_some()
        && descriptor == std::fs::read(staged).ok()
        && std::fs::metadata(node).is_ok_and(|m| m.file_type().is_char_device());
    Live {
        ready,
        product: read("idProduct").map(text),
    }
}

fn mode_product_for(model: rc2fx_firmware::Model, mode: &[u8]) -> Option<&'static str> {
    use rc2fx_firmware::Model::{Duo, ProIi};
    match (model, mode) {
        (ProIi, b"stereo") => Some("0x0037"),
        (Duo, b"stereo") => Some("0x0050"),
        (ProIi, b"multi") => Some("0x0092"),
        (Duo, b"multi") => Some("0x0093"),
        (ProIi, b"extendstereo") => Some("0x0078"),
        (Duo, b"extendstereo") => Some("0x0079"),
        (ProIi, b"extendmulti") => Some("0x0094"),
        (Duo, b"extendmulti") => Some("0x0095"),
        _ => None,
    }
}

fn gadget_command(command: &[u8]) -> Option<&'static CStr> {
    match command {
        b"/etc/init.d/uac2_gadget.sh stop stereo" => {
            Some(c"/bin/bash /run/rc2fx/usb-stop.sh stop stereo")
        }
        b"/etc/init.d/uac2_gadget.sh stop multi" => {
            Some(c"/bin/bash /run/rc2fx/usb-stop.sh stop multi")
        }
        b"/etc/init.d/uac2_gadget.sh stop extendstereo" => {
            Some(c"/bin/bash /run/rc2fx/usb-stop.sh stop extendstereo")
        }
        b"/etc/init.d/uac2_gadget.sh stop extendmulti" => {
            Some(c"/bin/bash /run/rc2fx/usb-stop.sh stop extendmulti")
        }
        b"/etc/init.d/uac2_gadget.sh stop syncmode" => {
            Some(c"/bin/bash /run/rc2fx/usb-stop.sh stop syncmode")
        }
        b"/etc/init.d/uac2_gadget.sh start stereo" => {
            Some(c"/bin/bash /run/rc2fx/usb-gadget.sh start stereo")
        }
        b"/etc/init.d/uac2_gadget.sh start multi" => {
            Some(c"/bin/bash /run/rc2fx/usb-gadget.sh start multi")
        }
        b"/etc/init.d/uac2_gadget.sh start extendstereo" => {
            Some(c"/bin/bash /run/rc2fx/usb-gadget.sh start extendstereo")
        }
        b"/etc/init.d/uac2_gadget.sh start extendmulti" => {
            Some(c"/bin/bash /run/rc2fx/usb-gadget.sh start extendmulti")
        }
        // These modes keep their stock layout, but must still respect a refused
        // preceding stop. The vendor caller does not inspect system's status.
        b"/etc/init.d/uac2_gadget.sh start syncmode" => {
            Some(c"/etc/init.d/uac2_gadget.sh start syncmode")
        }
        b"/etc/init.d/uac2_gadget.sh start update" => {
            Some(c"/etc/init.d/uac2_gadget.sh start update")
        }
        _ => None,
    }
}

/// The staged stop for `mode`; the vendor's stop tears down every function
/// whatever the mode, so an unlisted mode uses the stereo one.
fn stop_command(mode: &[u8]) -> &'static CStr {
    let mut command = STOP.to_vec();
    command.extend_from_slice(mode);
    gadget_command(&command).unwrap_or(c"/bin/bash /run/rc2fx/usb-stop.sh stop stereo")
}

/// Runs a vendor gadget command, or keeps the bound gadget instead. Returns
/// the wait status and whether the command was made a no-op; `None` for a
/// command that is not gadget control. `startup_window` is whether this
/// process is young enough for the command to be its startup sequence.
fn run_gadget_for_model(
    state: &mut Gadget,
    startup_window: bool,
    command: &CStr,
    live: impl Fn() -> Live,
    original: impl Fn(&CStr) -> c_int,
    model: Option<rc2fx_firmware::Model>,
) -> Option<(c_int, bool)> {
    let bytes = command.to_bytes();
    let starting = state.starting && startup_window;
    if bytes == UNBIND {
        if starting && live().ready {
            state.keeping = true;
            return Some((0, true));
        }
        state.keeping = false;
        return Some((original(command), false));
    }
    let replacement = gadget_command(bytes)?;
    if bytes.starts_with(STOP) {
        if starting && (state.keeping || live().ready) {
            state.keeping = true;
            return Some((0, true));
        }
        state.keeping = false;
        let status = original(replacement);
        state.stop_failed = status != 0;
        return Some((status, false));
    }
    let mode = &bytes[START.len()..];
    state.starting = false;
    if state.keeping {
        let expected = model.and_then(|m| mode_product_for(m, mode));
        if expected.is_some() && live().product.as_deref() == expected {
            return Some((0, true));
        }
        // The saved USB mode changed between mixers: rebuild after all.
        state.keeping = false;
        let status = original(stop_command(mode));
        state.stop_failed = status != 0;
        if status != 0 {
            return Some((status, false));
        }
    }
    if state.stop_failed {
        // system() returns a wait status, not an exit code. No vendor script is
        // called here: a rebuild must not follow a refused quiescence check.
        return Some((1 << 8, false));
    }
    Some((original(replacement), false))
}

#[cfg(test)]
fn run_gadget(
    state: &mut Gadget,
    startup_window: bool,
    command: &CStr,
    live: impl Fn() -> Live,
    original: impl Fn(&CStr) -> c_int,
) -> Option<(c_int, bool)> {
    run_gadget_for_model(
        state,
        startup_window,
        command,
        live,
        original,
        Some(rc2fx_firmware::Model::ProIi),
    )
}

pub unsafe extern "C" fn system(command: *const libc::c_char) -> c_int {
    let original: unsafe extern "C" fn(*const libc::c_char) -> c_int =
        unsafe { std::mem::transmute(SYSTEM.load(Ordering::Acquire)) };
    if !command.is_null()
        && BRIDGE.get().is_some()
        && std::path::Path::new("/run/rc2fx/usb-gadget.sh").is_file()
        && std::path::Path::new("/run/rc2fx/usb-stop.sh").is_file()
    {
        let command = unsafe { CStr::from_ptr(command) };
        let bytes = command.to_bytes();
        if bytes == UNBIND || gadget_command(bytes).is_some() {
            // Serialize stop/start execution, including the failure latch. This
            // is a vendor control path; the audio callback never takes this
            // mutex.
            let mut state = GADGET.lock().unwrap_or_else(|e| e.into_inner());
            let startup_window = LOADED.get().is_some_and(|t| t.elapsed() < STARTUP_WINDOW);
            let result = run_gadget_for_model(
                &mut state,
                startup_window,
                command,
                || {
                    live(
                        Path::new(GADGET_DIR),
                        Path::new(STAGED_DESCRIPTOR),
                        Path::new(HID_NODE),
                    )
                },
                |cmd| unsafe { original(cmd.as_ptr()) },
                rc2fx_firmware::Model::read_device().ok(),
            );
            KEEPING.store(state.keeping, Ordering::Release);
            STARTING.store(state.starting, Ordering::Release);
            if let Some((status, kept)) = result {
                let text = String::from_utf8_lossy(bytes);
                if kept {
                    eprintln!("RC2FX USB gadget kept: `{text}` made a no-op");
                } else {
                    super::probe::record(super::probe::Kind::UsbGadget, 0);
                    if status != 0 {
                        eprintln!(
                            "RC2FX USB gadget command failed or was refused: status={status}"
                        );
                    }
                }
                return status;
            }
        }
    }
    unsafe { original(command) }
}

/// Sysfs files the vendor writes to bind, reset or re-role the USB controller.
fn diverted(path: &[u8]) -> bool {
    path == b"/sys/kernel/config/usb_gadget/g124/UDC"
        || path == b"/sys/class/usb_role/ffe09080.usb-role-switch/role"
        || (path.starts_with(b"/sys/bus/platform/drivers/")
            && (path.ends_with(b"/bind") || path.ends_with(b"/unbind")))
}

/// While the bound gadget is kept, the vendor's controller bind, reset and
/// role writes go to /dev/null; its reads (the UDC read-back) see the real
/// files. `open` is variadic in libc; on AArch64 Linux the optional mode
/// arrives in the third argument register either way.
pub unsafe extern "C" fn open(
    path: *const libc::c_char,
    flags: c_int,
    mode: libc::c_uint,
) -> c_int {
    let original: unsafe extern "C" fn(*const libc::c_char, c_int, libc::c_uint) -> c_int =
        unsafe { std::mem::transmute(OPEN.load(Ordering::Acquire)) };
    if path.is_null() {
        return unsafe { original(path, flags, mode) };
    }
    // At boot the HID node does not exist until the mixer has built its
    // gadget, so the vendor's HID thread first opens it afterwards. A mixer
    // restarted over a kept gadget finds the node at once, and a mixer that
    // receives host reports before it has finished starting crashes (signal
    // 11, observed 2026-10-26). Refuse the early open as boot would.
    if unsafe { CStr::from_ptr(path) }.to_bytes() == HID_NODE.as_bytes()
        && STARTING.load(Ordering::Acquire)
        && LOADED.get().is_some_and(|t| t.elapsed() < STARTUP_WINDOW)
        && live(
            Path::new(GADGET_DIR),
            Path::new(STAGED_DESCRIPTOR),
            Path::new(HID_NODE),
        )
        .ready
    {
        eprintln!("RC2FX USB gadget kept: HID node open deferred until the mixer has started");
        unsafe { *libc::__errno_location() = libc::ENOENT };
        return -1;
    }
    if KEEPING.load(Ordering::Acquire) && flags & libc::O_ACCMODE != libc::O_RDONLY {
        let bytes = unsafe { CStr::from_ptr(path) }.to_bytes();
        if diverted(bytes) {
            eprintln!(
                "RC2FX USB gadget kept: vendor write to {} ignored",
                String::from_utf8_lossy(bytes)
            );
            return unsafe { original(c"/dev/null".as_ptr(), flags, mode) };
        }
    }
    unsafe { original(path, flags, mode) }
}

pub fn initialize() {
    // Only a hash-qualified mixer process may claim reports. Inherited helper
    // environments and unsupported firmware must retain stock behavior.
    if super::FIRMWARE_PROFILE.get().is_none() {
        return;
    }
    let _ = LOADED.set(Instant::now());
    let Some(path) = std::env::var_os("RC2FX_USB_SOCKET") else {
        return;
    };
    let path = std::path::PathBuf::from(path);
    if !path.is_absolute() {
        return;
    }
    if let Ok(socket) = UnixDatagram::unbound()
        && socket.set_nonblocking(true).is_ok()
    {
        let _ = BRIDGE.set((socket, path));
        eprintln!("RC2FX USB read bridge enabled");
    }
}

pub unsafe extern "C" fn read(fd: c_int, buffer: *mut c_void, count: size_t) -> ssize_t {
    let original: unsafe extern "C" fn(c_int, *mut c_void, size_t) -> ssize_t =
        unsafe { std::mem::transmute(READ.load(Ordering::Acquire)) };
    let n = unsafe { original(fd, buffer, count) };
    let Some((socket, path)) = BRIDGE.get() else {
        return n;
    };
    if n < 8 {
        return n;
    }
    let bytes = unsafe { std::slice::from_raw_parts(buffer.cast::<u8>(), n as usize) };
    if !rc2fx_usb::marked(bytes, rc2fx_usb::OUTPUT_ID) {
        return n;
    }
    let first = MARKED.fetch_add(1, Ordering::Relaxed) < 3;
    // Resolve the actual descriptor each time a marked packet arrives. No stale
    // cached fd can swallow a later file/socket read after close/reopen.
    let fd_path = format!("/proc/self/fd/{fd}\0");
    let mut target = [0u8; 32];
    let length = unsafe {
        libc::readlink(
            CStr::from_bytes_with_nul_unchecked(fd_path.as_bytes()).as_ptr(),
            target.as_mut_ptr().cast(),
            target.len(),
        )
    };
    let live = length == 10 && &target[..10] == b"/dev/hidg0";
    let retired = length == 20 && &target[..20] == b"/dev/hidg0 (deleted)";
    if !live && !retired {
        if first {
            eprintln!(
                "RC2FX USB marker on another descriptor: fd={fd}, bytes={n}, path length={length}"
            );
        }
        return n;
    }
    // The launcher retires the old devtmpfs name before a mixer restart. Keep
    // consuming our marked traffic during that handoff, but do not deliver a
    // request whose reply would need a different/new gadget descriptor.
    if retired {
        return 0;
    }
    super::probe::usb_report();
    if first {
        eprintln!("RC2FX USB received report: bytes={n}");
    }
    if let Some(bytes) = rc2fx_usb::canonical_output(bytes) {
        // Full queue or absent daemon means host timeout, never stock parsing.
        let result = socket.send_to(bytes, path);
        if first {
            eprintln!(
                "RC2FX USB local delivery: {result:?}; valid frame={}",
                rc2fx_usb::Frame::decode(bytes, rc2fx_usb::OUTPUT_ID).is_some()
            );
        }
    } else if first {
        eprintln!(
            "RC2FX USB rejected report length/padding: bytes={n}, last={}",
            bytes[bytes.len() - 1]
        );
    }
    // The inspected vendor loop returns to select() for read <= 0. Returning
    // zero consumes exactly this report without fabricating errno in its libc.
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stock() -> Live {
        Live::default()
    }

    fn bound(product: &str) -> impl Fn() -> Live {
        let product = product.to_owned();
        move || Live {
            ready: true,
            product: Some(product.clone()),
        }
    }

    fn unbind() -> &'static CStr {
        c"echo \"\" > /sys/kernel/config/usb_gadget/g124/UDC"
    }

    #[test]
    fn failed_stop_blocks_every_rebuild_until_successful_stop() {
        for failure in [-1, 1 << 8, 9] {
            let mut state = Gadget::new();
            assert_eq!(
                run_gadget(
                    &mut state,
                    false,
                    c"/etc/init.d/uac2_gadget.sh stop multi",
                    stock,
                    |_| failure
                ),
                Some((failure, false))
            );
            for start in [
                c"/etc/init.d/uac2_gadget.sh start stereo",
                c"/etc/init.d/uac2_gadget.sh start multi",
                c"/etc/init.d/uac2_gadget.sh start extendstereo",
                c"/etc/init.d/uac2_gadget.sh start extendmulti",
                c"/etc/init.d/uac2_gadget.sh start syncmode",
                c"/etc/init.d/uac2_gadget.sh start update",
            ] {
                assert_eq!(
                    run_gadget(&mut state, false, start, stock, |_| panic!(
                        "unsafe rebuild"
                    )),
                    Some((256, false))
                );
            }
            assert_eq!(
                run_gadget(
                    &mut state,
                    false,
                    c"/etc/init.d/uac2_gadget.sh stop multi",
                    stock,
                    |cmd| {
                        assert_eq!(cmd, c"/bin/bash /run/rc2fx/usb-stop.sh stop multi");
                        0
                    }
                ),
                Some((0, false))
            );
            assert_eq!(
                run_gadget(
                    &mut state,
                    false,
                    c"/etc/init.d/uac2_gadget.sh start multi",
                    stock,
                    |cmd| {
                        assert_eq!(cmd, c"/bin/bash /run/rc2fx/usb-gadget.sh start multi");
                        0
                    }
                ),
                Some((0, false))
            );
        }
    }

    #[test]
    fn unrelated_commands_are_not_claimed_and_special_modes_keep_stock_layout() {
        let mut state = Gadget::new();
        state.stop_failed = true;
        assert_eq!(
            run_gadget(&mut state, false, c"sync", stock, |_| panic!(
                "not a gadget command"
            )),
            None
        );
        assert!(state.stop_failed);
        state.stop_failed = false;
        for command in [
            c"/etc/init.d/uac2_gadget.sh start syncmode",
            c"/etc/init.d/uac2_gadget.sh start update",
        ] {
            assert_eq!(
                run_gadget(&mut state, false, command, stock, |actual| {
                    assert_eq!(actual, command);
                    0
                }),
                Some((0, false))
            );
        }
    }

    #[test]
    fn a_bound_gadget_with_rc2fx_descriptor_is_kept_through_startup() {
        let mut state = Gadget::new();
        let live = bound("0x0094");
        let no_run = |_: &CStr| -> c_int { panic!("the gadget must not be touched") };
        assert_eq!(
            run_gadget(&mut state, true, unbind(), &live, no_run),
            Some((0, true))
        );
        assert!(state.keeping);
        assert_eq!(
            run_gadget(
                &mut state,
                true,
                c"/etc/init.d/uac2_gadget.sh stop extendmulti",
                &live,
                no_run
            ),
            Some((0, true))
        );
        assert_eq!(
            run_gadget(
                &mut state,
                true,
                c"/etc/init.d/uac2_gadget.sh start extendmulti",
                &live,
                no_run
            ),
            Some((0, true))
        );
        // Kept through the vendor's bind and read-back that follow the start.
        assert!(state.keeping);
        assert!(!state.starting);
        // The next teardown, a mode change, runs for real.
        assert_eq!(
            run_gadget(&mut state, true, unbind(), &live, |cmd| {
                assert_eq!(cmd, unbind());
                0
            }),
            Some((0, false))
        );
        assert!(!state.keeping);
        assert_eq!(
            run_gadget(
                &mut state,
                true,
                c"/etc/init.d/uac2_gadget.sh stop extendmulti",
                &live,
                |cmd| {
                    assert_eq!(cmd, c"/bin/bash /run/rc2fx/usb-stop.sh stop extendmulti");
                    0
                }
            ),
            Some((0, false))
        );
        assert_eq!(
            run_gadget(
                &mut state,
                true,
                c"/etc/init.d/uac2_gadget.sh start stereo",
                &live,
                |cmd| {
                    assert_eq!(cmd, c"/bin/bash /run/rc2fx/usb-gadget.sh start stereo");
                    0
                }
            ),
            Some((0, false))
        );
    }

    #[test]
    fn duo_usb_modes_retain_only_the_same_model_and_mode() {
        use rc2fx_firmware::Model;
        for (mode, product) in [
            (b"stereo".as_slice(), "0x0050"),
            (b"multi", "0x0093"),
            (b"extendstereo", "0x0079"),
            (b"extendmulti", "0x0095"),
        ] {
            assert_eq!(mode_product_for(Model::Duo, mode), Some(product));
            assert_ne!(mode_product_for(Model::ProIi, mode), Some(product));
            let command = std::ffi::CString::new(format!(
                "/etc/init.d/uac2_gadget.sh start {}",
                std::str::from_utf8(mode).unwrap()
            ))
            .unwrap();
            let live = bound(product);
            let mut state = Gadget::new();
            state.keeping = true;
            assert_eq!(
                run_gadget_for_model(
                    &mut state,
                    true,
                    &command,
                    &live,
                    |_| panic!("same Duo mode must be kept"),
                    Some(Model::Duo)
                ),
                Some((0, true))
            );
        }
    }

    #[test]
    fn a_changed_mode_rebuilds_after_all() {
        let mut state = Gadget::new();
        let live = bound("0x0037");
        let ran = std::cell::RefCell::new(Vec::new());
        assert_eq!(
            run_gadget(
                &mut state,
                true,
                c"/etc/init.d/uac2_gadget.sh stop extendmulti",
                &live,
                |_| panic!()
            ),
            Some((0, true))
        );
        assert_eq!(
            run_gadget(
                &mut state,
                true,
                c"/etc/init.d/uac2_gadget.sh start extendmulti",
                &live,
                |cmd| {
                    ran.borrow_mut().push(cmd.to_owned());
                    0
                }
            ),
            Some((0, false))
        );
        assert_eq!(
            ran.into_inner(),
            [
                c"/bin/bash /run/rc2fx/usb-stop.sh stop extendmulti".to_owned(),
                c"/bin/bash /run/rc2fx/usb-gadget.sh start extendmulti".to_owned(),
            ]
        );
        assert!(!state.keeping);
        // A refused stop in that rebuild latches as it would have directly.
        let mut state = Gadget::new();
        assert_eq!(
            run_gadget(
                &mut state,
                true,
                c"/etc/init.d/uac2_gadget.sh stop stereo",
                &live,
                |_| panic!()
            ),
            Some((0, true))
        );
        assert_eq!(
            run_gadget(
                &mut state,
                true,
                c"/etc/init.d/uac2_gadget.sh start update",
                &live,
                |cmd| {
                    assert_eq!(cmd, c"/bin/bash /run/rc2fx/usb-stop.sh stop stereo");
                    9
                }
            ),
            Some((9, false))
        );
        assert!(state.stop_failed);
    }

    #[test]
    fn stock_descriptor_or_late_teardown_is_not_kept() {
        let cases: [(bool, Box<dyn Fn() -> Live>); 2] =
            [(true, Box::new(stock)), (false, Box::new(bound("0x0094")))];
        for (startup_window, live) in cases {
            let mut state = Gadget::new();
            assert_eq!(
                run_gadget(&mut state, startup_window, unbind(), &live, |cmd| {
                    assert_eq!(cmd, unbind());
                    0
                }),
                Some((0, false))
            );
            assert_eq!(
                run_gadget(
                    &mut state,
                    startup_window,
                    c"/etc/init.d/uac2_gadget.sh stop stereo",
                    &live,
                    |cmd| {
                        assert_eq!(cmd, c"/bin/bash /run/rc2fx/usb-stop.sh stop stereo");
                        0
                    }
                ),
                Some((0, false))
            );
            assert!(!state.keeping);
        }
    }

    #[test]
    fn only_controller_control_files_are_diverted() {
        for path in [
            b"/sys/kernel/config/usb_gadget/g124/UDC".as_slice(),
            b"/sys/class/usb_role/ffe09080.usb-role-switch/role",
            b"/sys/bus/platform/drivers/dwc2/unbind",
            b"/sys/bus/platform/drivers/dwc3-meson-g12a/bind",
            b"/sys/bus/platform/drivers/phy-meson-gxl-usb2/unbind",
        ] {
            assert!(diverted(path), "{}", String::from_utf8_lossy(path));
        }
        for path in [
            b"/sys/kernel/config/usb_gadget/g124/functions/mass_storage.usb0/lun.0/file".as_slice(),
            b"/sys/class/gpio/gpio387/value",
            b"/sys/bus/platform/drivers/dwc2/uevent",
            b"/dev/hidg0",
        ] {
            assert!(!diverted(path), "{}", String::from_utf8_lossy(path));
        }
    }

    #[test]
    fn live_gadget_is_ready_only_when_bound_with_the_staged_descriptor() {
        let dir = std::env::temp_dir().join(format!("rc2fx-gadget-{}", std::process::id()));
        let functions = dir.join("functions/hid.usb0");
        std::fs::create_dir_all(&functions).unwrap();
        let staged = dir.join("usb-report.bin");
        std::fs::write(&staged, b"\x06\x00\xff\xc0").unwrap();
        std::fs::write(dir.join("idProduct"), "0x0094\n").unwrap();
        std::fs::write(dir.join("UDC"), "\n").unwrap();
        std::fs::write(functions.join("report_desc"), b"\x06\x00\xff\xc0").unwrap();
        let node = Path::new("/dev/null");
        assert_eq!(
            live(&dir, &staged, node),
            Live {
                ready: false,
                product: Some("0x0094".into())
            }
        );
        std::fs::write(dir.join("UDC"), "ff400000.usb\n").unwrap();
        assert!(live(&dir, &staged, node).ready);
        // A retired or missing HID node needs a rebuild to come back.
        assert!(!live(&dir, &staged, &dir.join("hidg0")).ready);
        assert!(!live(&dir, &staged, &staged).ready);
        std::fs::write(functions.join("report_desc"), b"\x06\x00\xff\x09\xc0").unwrap();
        assert!(!live(&dir, &staged, node).ready);
        std::fs::remove_file(&staged).unwrap();
        std::fs::write(functions.join("report_desc"), b"\x06\x00\xff\xc0").unwrap();
        assert!(
            !live(&dir, &staged, node).ready,
            "no staged descriptor means no RC2FX layout"
        );
        assert_eq!(
            live(Path::new("/nonexistent/gadget"), &staged, node),
            Live::default()
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
