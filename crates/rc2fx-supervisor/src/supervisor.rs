//! Supervises a single RC2FX mixer process for its entire lifetime.
//!
//! Because this runs alongside live audio, the steady-state loop does one
//! `poll` and one `stat` per second with no forks (busy background cores slow
//! down the main mixer process). Process exits and signals still wake it
//! immediately.
//!
//! The 1-second poll interval is intentional. The controller times out after
//! two seconds if it can't connect, and the control socket is created before
//! the mixer finishes initializing, so starting the controller the instant the
//! socket appears risks failing back to stock audio. Watching the run directory
//! with inotify doesn't help either, since every report written there would
//! wake the supervisor.
//!
//! Teardown work stays in `rc2fx.sh`: `supervise-restart` tears down the USB
//! gadget and stops the processes, and `supervise-exit` cleans up any leftover
//! controller and writes the last-exit log.
//!
//! For controller-only updates, creating `reload` in the run directory stops
//! the controller and starts the new binary without interrupting mixer audio.
//! `supervisor.features` advertises `reload` so the updater can tell whether
//! the running supervisor supports it.
use std::{
    collections::HashSet,
    ffi::{OsStr, OsString},
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
        unix::{
            fs::{DirBuilderExt, FileTypeExt},
            process::{CommandExt, ExitStatusExt},
        },
    },
    panic::{self, AssertUnwindSafe},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::atomic::{AtomicBool, AtomicI32, Ordering},
};

/// Maximum time to wait for the mixer to open its control socket.
pub const READY_SECONDS: u32 = 60;
/// Mixer exits before this uptime count toward falling back to stock audio.
const EARLY_EXIT_SECONDS: i64 = 120;
/// Number of early exits in one boot before the launcher falls back to stock.
const EARLY_EXITS: usize = 2;
/// Delay before the first housekeeping pass so the mixer can spawn its threads.
const HOUSEKEEPING_FIRST: u32 = 10;
/// Interval for follow-up passes to catch threads that pin themselves to CPU 0 later.
const HOUSEKEEPING_EVERY: u32 = 5;
const OOM_SCORE: &str = "/proc/self/oom_score_adj";
/// Grace period for a controller to exit on reload before sending SIGKILL.
const RELOAD_STOP: std::time::Duration = std::time::Duration::from_secs(10);
const FEATURES: &str = "reload\n";

#[derive(Debug, PartialEq, Eq)]
pub struct Options {
    pub mixer: i32,
    /// Mixer start time in clock ticks from `/proc/PID/stat`, used to avoid
    /// acting on a reused PID.
    pub mixer_start: u64,
    /// Copy of `rc2fx.sh` in the run directory (not the bundle copy, which an
    /// install may replace or delete while we're still running).
    pub script: PathBuf,
    pub controller: PathBuf,
    pub controller_args: Vec<OsString>,
    pub run: PathBuf,
    pub boot: PathBuf,
    pub ready_seconds: u32,
}

/// Runs the supervision loop until the mixer exits or is stopped. Errors are
/// logged and still run the exit steps, because an unsupervised mixer has no
/// way to recover back to stock audio. The launcher handles the one case we
/// can't catch here: being killed by an uncatchable signal.
pub fn run(options: Options) {
    unsafe { libc::umask(0o077) };
    // Shield the supervisor so the OOM killer targets the mixer first; the
    // launcher can restart the mixer.
    if let Err(error) = fs::write(OOM_SCORE, "-1000") {
        say(&format!("Could not protect the supervisor: {error}"));
    }
    let signals = watch_signals();
    Supervisor::new(options, signals).supervise(Supervisor::watch);
}

struct Supervisor {
    options: Options,
    /// Kept open so teardown still works even if a newer launch deletes the run
    /// directory.
    script: Option<File>,
    /// Self-pipe reader used to wake `poll` immediately; `SIGNALED` tracks
    /// whether a signal actually arrived.
    signals: Option<OwnedFd>,
    /// Falls back to 1-second polling if the kernel doesn't support pidfd.
    mixer_fd: Option<Pidfd>,
    controller: Option<Controller>,
    /// Thread names we've already logged so recurring re-pins don't spam flash.
    reported: HashSet<String>,
}

/// Running controller child and its pidfd. Keeping them together ensures a
/// reaped controller's fd is dropped before the next `poll`.
struct Controller {
    child: Child,
    fd: Option<Pidfd>,
}

impl Supervisor {
    fn new(options: Options, signals: Option<OwnedFd>) -> Self {
        let script = File::open(&options.script);
        if let Err(error) = &script {
            say(&format!(
                "Could not open {}: {error}",
                options.script.display()
            ));
        }
        // Verify start time after opening the pidfd to close the PID-reuse
        // race. If it no longer matches, drop the fd and let the first loop
        // check treat the mixer as exited.
        let mixer_fd =
            Pidfd::open(options.mixer).filter(|_| process_is(options.mixer, options.mixer_start));
        Self {
            options,
            script: script.ok(),
            signals,
            mixer_fd,
            controller: None,
            reported: HashSet::new(),
        }
    }

    /// Ensures a panic inside `watch` still triggers stock fallback and cleanup.
    fn supervise(&mut self, watch: impl FnOnce(&mut Self)) {
        if panic::catch_unwind(AssertUnwindSafe(|| watch(self))).is_err() {
            self.end_mixer("Supervisor panicked; restoring stock audio", true);
        }
        self.finish();
    }

    fn watch(&mut self) {
        say(&format!("Supervising RC2FX mixer {}", self.options.mixer));
        let socket = self.options.run.join("control.sock");
        let stop = self.options.run.join("stop");
        let reload = self.options.run.join("reload");
        if let Err(error) = fs::write(self.options.run.join("supervisor.features"), FEATURES) {
            say(&format!(
                "Could not record the supervisor's features: {error}"
            ));
        }
        let mut tick = 0;
        loop {
            if !self.mixer_alive() {
                self.record_mixer_exit();
                return;
            }
            if SIGNALED.load(Ordering::Relaxed) {
                self.end_mixer("Supervisor signaled; restoring stock audio", true);
                return;
            }
            if stop.exists() {
                self.end_mixer("Stop requested", false);
                return;
            }
            if self.controller.is_none() {
                if is_socket(&socket) {
                    if !self.start_controller() {
                        return;
                    }
                } else if tick >= self.options.ready_seconds {
                    // Don't leave the device on a blank screen if the mixer never opens its socket.
                    let reason = format!(
                        "Mixer not ready after {} s; restoring stock audio",
                        self.options.ready_seconds
                    );
                    self.end_mixer(&reason, true);
                    return;
                }
            }
            if reload.exists() {
                let _ = fs::remove_file(&reload);
                // If the controller hasn't started yet, it will pick up the new binary anyway.
                if self.controller.is_some() && !self.reload_controller() {
                    return;
                }
            }
            if let Some(status) = self.controller_exit() {
                let reason =
                    format!("Controller exited with status {status}; restoring stock audio");
                self.end_mixer(&reason, true);
                return;
            }
            if housekeeping_due(tick) && self.mixer_alive() {
                self.relocate_housekeeping();
            }
            tick = tick.saturating_add(1);
            self.wait();
        }
    }

    /// Waits up to 1 second, waking early on process exit or signal.
    fn wait(&mut self) {
        let fd = |pidfd: &Option<Pidfd>| pidfd.as_ref().map_or(-1, |pidfd| pidfd.0.as_raw_fd());
        // `poll` ignores negative fds.
        let mut fds = [
            self.signals.as_ref().map_or(-1, AsRawFd::as_raw_fd),
            fd(&self.mixer_fd),
            self.controller
                .as_ref()
                .map_or(-1, |controller| fd(&controller.fd)),
        ]
        .map(|fd| libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        });
        unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, 1000) };
    }

    fn mixer_alive(&self) -> bool {
        match &self.mixer_fd {
            Some(pidfd) => !pidfd.exited(),
            None => process_is(self.options.mixer, self.options.mixer_start),
        }
    }

    fn record_mixer_exit(&self) {
        let age = uptime_seconds() - self.options.mixer_start as i64 / clock_ticks();
        say(&format!(
            "Mixer exited after {age} s (its launch shell logs the status)"
        ));
        if age >= EARLY_EXIT_SECONDS {
            return;
        }
        let exits = self.boot_file("exits");
        let appended = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&exits)
            .and_then(|mut file| writeln!(file, "{}", uptime_seconds()));
        if let Err(error) = appended {
            say(&format!("Could not record the early exit: {error}"));
        }
        let count = fs::read_to_string(&exits).map_or(0, |text| text.lines().count());
        if count >= EARLY_EXITS {
            self.request_stock(&format!("mixer exited early {count} times this boot"));
            say("Falling back to stock audio for the rest of this boot");
        }
    }

    fn request_stock(&self, reason: &str) {
        if let Err(error) = fs::write(self.boot_file("stock"), format!("{reason}\n")) {
            say(&format!("Could not record the stock request: {error}"));
        }
    }

    /// `launch` creates the boot directory, so we recreate it best-effort here
    /// and let the actual file write surface any failure.
    fn boot_file(&self, name: &str) -> PathBuf {
        let _ = fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.options.boot);
        self.options.boot.join(name)
    }

    /// Returns `false` if the controller failed to spawn and the mixer was ended.
    fn start_controller(&mut self) -> bool {
        let spawned = File::create(self.options.run.join("controller.log")).and_then(|log| {
            let mut command = Command::new(&self.options.controller);
            command
                .args(&self.options.controller_args)
                .stdin(Stdio::null())
                .stdout(log.try_clone()?)
                .stderr(log);
            // Don't inherit the supervisor's OOM immunity into the controller.
            unsafe { command.pre_exec(reset_oom_score) };
            command.spawn()
        });
        match spawned {
            Ok(child) => {
                say(&format!("Controller started (PID {})", child.id()));
                let fd = Pidfd::open(child.id() as i32);
                self.controller = Some(Controller { child, fd });
                true
            }
            Err(error) => {
                say(&format!("Could not start the controller: {error}"));
                self.end_mixer(
                    "Controller exited with status 127; restoring stock audio",
                    true,
                );
                false
            }
        }
    }

    /// Restarts the controller in place while leaving the mixer running.
    /// Sends SIGTERM, waits up to `RELOAD_STOP` before SIGKILL, removes stale
    /// controller sockets, and spawns the new binary. Returns `false` if the
    /// new controller failed to start and the mixer was ended.
    fn reload_controller(&mut self) -> bool {
        say("Reloading the controller for an update; the mixer keeps running");
        if let Some(Controller {
            child: mut controller,
            ..
        }) = self.controller.take()
        {
            // Safe to signal by PID since we haven't reaped this child yet.
            unsafe { libc::kill(controller.id() as i32, libc::SIGTERM) };
            let deadline = std::time::Instant::now() + RELOAD_STOP;
            loop {
                match controller.try_wait() {
                    Ok(Some(status)) => {
                        say(&format!(
                            "Controller stopped for the reload with status {}",
                            shell_status(status)
                        ));
                        break;
                    }
                    Ok(None) if std::time::Instant::now() < deadline => {
                        std::thread::sleep(std::time::Duration::from_millis(50));
                    }
                    _ => {
                        say("Controller did not stop for the reload; killing it");
                        let _ = controller.kill();
                        let _ = controller.wait();
                        break;
                    }
                }
            }
        }
        for name in ["usb.sock", "ui.sock"] {
            let path = self.options.run.join(name);
            if fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_socket()) {
                let _ = fs::remove_file(path);
            }
        }
        self.start_controller()
    }

    /// Reaps an exited controller and returns its shell-style exit status.
    fn controller_exit(&mut self) -> Option<i32> {
        let status = self.controller.as_mut()?.child.try_wait().ok()??;
        self.controller = None;
        Some(shell_status(status))
    }

    /// Stops the mixer so the launcher can restart it (into stock audio if `stock` is true).
    fn end_mixer(&mut self, reason: &str, stock: bool) {
        say(reason);
        if stock {
            self.request_stock(reason);
        }
        if !self.mixer_alive() {
            return;
        }
        // Only preserve the USB gadget when restarting into RC2FX; stock rebuilds it.
        let keep_usb = !stock && self.options.run.join("keep-usb").exists();
        let mut args = vec![
            OsString::from("supervise-restart"),
            OsString::from(if keep_usb { "1" } else { "0" }),
            OsString::from(self.options.mixer.to_string()),
            OsString::from(self.options.mixer_start.to_string()),
        ];
        args.extend(self.controller_pid());
        if self.script(&args) {
            // The script already stopped the controller; just reap it here.
            if let Some(mut controller) = self.controller.take() {
                let _ = controller.child.try_wait();
            }
        } else {
            say("Leaving the mixer running without RC2FX control; rc2fx.sh start recovers it");
        }
    }

    fn finish(&mut self) {
        let mut args = vec![OsString::from("supervise-exit")];
        args.extend(self.controller_pid());
        let finished = self.script(&args);
        if let Some(Controller { child, .. }) = self.controller.as_mut() {
            if !finished {
                let _ = child.kill();
            }
            let _ = child.try_wait();
        }
        // Only remove supervisor.pid if it still points to us (a newer launch may have overwritten it).
        let record = self.options.run.join("supervisor.pid");
        let own = fs::read_to_string(&record)
            .is_ok_and(|text| text.trim() == std::process::id().to_string());
        if own {
            let _ = fs::remove_file(record);
        }
        unsafe { libc::sync() };
    }

    fn controller_pid(&self) -> Option<OsString> {
        let pid = self.controller.as_ref()?.child.id();
        Some(pid.to_string().into())
    }

    fn script(&self, args: &[OsString]) -> bool {
        let mut command = Command::new("/bin/bash");
        match &self.script {
            Some(file) => {
                let fd = file.as_raw_fd();
                command.arg(format!("/proc/self/fd/{fd}"));
                // Clear CLOEXEC only for this bash child so other children don't inherit the script fd.
                unsafe { command.pre_exec(move || keep_open(fd)) };
            }
            None => {
                command.arg(&self.options.script);
            }
        }
        let status = command.args(args).stdin(Stdio::null()).status();
        match status {
            Ok(status) => status.success(),
            Err(error) => {
                say(&format!(
                    "Could not run {} {}: {error}",
                    self.options.script.display(),
                    args.first()
                        .map_or(OsStr::new(""), OsString::as_os_str)
                        .display()
                ));
                false
            }
        }
    }

    fn relocate_housekeeping(&mut self) {
        let moved = relocate(&MixerThreads(self.options.mixer));
        if worth_reporting(&mut self.reported, &moved) {
            say(&format!(
                "Moved {} RØDE housekeeping threads off CPU 0 (CPU 0 is the suppression worker's): {}",
                moved.len(),
                moved.join(";")
            ));
        }
    }
}

fn housekeeping_due(tick: u32) -> bool {
    tick >= HOUSEKEEPING_FIRST && (tick - HOUSEKEEPING_FIRST).is_multiple_of(HOUSEKEEPING_EVERY)
}

/// Returns `true` if `moved` contains at least one thread name we haven't logged yet.
fn worth_reporting(reported: &mut HashSet<String>, moved: &[String]) -> bool {
    let mut fresh = false;
    for name in moved {
        fresh |= reported.insert(name.clone());
    }
    fresh
}

/// Thread inspection/affinity operations for housekeeping. Threads can exit at
/// any point, and `list` can miss a thread during concurrent exits, which the
/// next pass will pick up.
trait Threads {
    fn list(&self) -> Vec<i32>;
    fn pinned_to_cpu0(&self, tid: i32) -> bool;
    fn name(&self, tid: i32) -> Option<String>;
    fn is_normal_priority(&self, tid: i32) -> bool;
    fn move_off_cpu0(&self, tid: i32) -> bool;
}

/// RØDE pins ~15 `SCHED_OTHER` mixer threads to CPU 0 (main thread, HIDThread,
/// UartRunThread, JUCE Timer, SysMonThread, etc.). CPU 0 also runs the RR5
/// suppression worker, and the audio path contends on non-PI locks held by
/// those threads; moving them to CPUs 1 and 3 eliminated fader-triggered
/// callback stalls (device test, 2026-11-01).
///
/// Only non-`rc2fx`, `SCHED_OTHER` threads pinned exclusively to CPU 0 are
/// moved. We check affinity first because it's a single syscall and filters out
/// almost every thread after the first pass.
fn relocate(threads: &impl Threads) -> Vec<String> {
    let mut moved = Vec::new();
    for tid in threads.list() {
        if !threads.pinned_to_cpu0(tid) {
            continue;
        }
        let Some(name) = threads.name(tid) else {
            continue;
        };
        if !name.starts_with("rc2fx")
            && threads.is_normal_priority(tid)
            && threads.move_off_cpu0(tid)
        {
            moved.push(name);
        }
    }
    moved
}

struct MixerThreads(i32);

impl Threads for MixerThreads {
    fn list(&self) -> Vec<i32> {
        let Ok(tasks) = fs::read_dir(format!("/proc/{}/task", self.0)) else {
            return Vec::new();
        };
        tasks
            .flatten()
            .filter_map(|task| task.file_name().to_str()?.parse().ok())
            .collect()
    }

    fn pinned_to_cpu0(&self, tid: i32) -> bool {
        let mut set: libc::cpu_set_t = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::cpu_set_t>();
        if unsafe { libc::sched_getaffinity(tid, size, &mut set) } != 0 {
            return false;
        }
        unsafe { libc::CPU_COUNT(&set) == 1 && libc::CPU_ISSET(0, &set) }
    }

    fn name(&self, tid: i32) -> Option<String> {
        let name = fs::read_to_string(format!("/proc/{}/task/{tid}/comm", self.0)).ok()?;
        Some(name.trim_end_matches('\n').to_owned())
    }

    fn is_normal_priority(&self, tid: i32) -> bool {
        // Mask out SCHED_RESET_ON_FORK, which can be OR'd into the returned policy.
        let policy = unsafe { libc::sched_getscheduler(tid) };
        policy >= 0 && policy & !libc::SCHED_RESET_ON_FORK == libc::SCHED_OTHER
    }

    fn move_off_cpu0(&self, tid: i32) -> bool {
        let mut set: libc::cpu_set_t = unsafe { std::mem::zeroed() };
        unsafe {
            libc::CPU_SET(1, &mut set);
            libc::CPU_SET(3, &mut set);
            libc::sched_setaffinity(tid, std::mem::size_of::<libc::cpu_set_t>(), &set) == 0
        }
    }
}

/// PID-reuse-safe process handle that becomes readable in `poll` on exit.
struct Pidfd(OwnedFd);

impl Pidfd {
    fn open(pid: i32) -> Option<Self> {
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
        (fd >= 0).then(|| Self(unsafe { OwnedFd::from_raw_fd(fd as RawFd) }))
    }

    fn exited(&self) -> bool {
        let mut fd = libc::pollfd {
            fd: self.0.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        unsafe { libc::poll(&mut fd, 1, 0) != 0 }
    }
}

/// Parses state and start time from `/proc/PID/stat`. Uses `rsplit_once(") ")`
/// because the comm field can contain spaces and parentheses.
fn parse_stat(line: &str) -> Option<(char, u64)> {
    let (_, rest) = line.rsplit_once(") ")?;
    let mut fields = rest.split_ascii_whitespace();
    let state = fields.next()?.chars().next()?;
    Some((state, fields.nth(18)?.parse().ok()?))
}

fn process_is(pid: i32, start: u64) -> bool {
    let Ok(mut file) = File::open(format!("/proc/{pid}/stat")) else {
        return false;
    };
    let mut line = String::new();
    file.read_to_string(&mut line).is_ok()
        && parse_stat(&line).is_some_and(|(state, started)| state != 'Z' && started == start)
}

fn is_socket(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|metadata| metadata.file_type().is_socket())
}

fn shell_status(status: ExitStatus) -> i32 {
    status
        .code()
        .or(status.signal().map(|signal| 128 + signal))
        .unwrap_or(0)
}

fn uptime_seconds() -> i64 {
    fs::read_to_string("/proc/uptime")
        .ok()
        .and_then(|text| text.split('.').next()?.parse().ok())
        .unwrap_or(0)
}

fn clock_ticks() -> i64 {
    unsafe { libc::sysconf(libc::_SC_CLK_TCK) }.max(1)
}

/// Runs between `fork` and `exec`; must be async-signal-safe (no allocations).
fn reset_oom_score() -> io::Result<()> {
    // Use a literal C string here instead of allocating from `OOM_SCORE`.
    let fd = unsafe { libc::open(c"/proc/self/oom_score_adj".as_ptr(), libc::O_WRONLY) };
    if fd >= 0 {
        unsafe {
            libc::write(fd, c"0".as_ptr().cast(), 1);
            libc::close(fd);
        }
    }
    Ok(())
}

/// Runs between `fork` and `exec` to clear `FD_CLOEXEC` on one descriptor.
fn keep_open(fd: RawFd) -> io::Result<()> {
    match unsafe { libc::fcntl(fd, libc::F_SETFD, 0) } {
        0 => Ok(()),
        _ => Err(io::Error::last_os_error()),
    }
}

static SIGNALED: AtomicBool = AtomicBool::new(false);
static SIGNAL_WRITER: AtomicI32 = AtomicI32::new(-1);

extern "C" fn on_signal(_: libc::c_int) {
    SIGNALED.store(true, Ordering::Relaxed);
    let byte = 1u8;
    let writer = SIGNAL_WRITER.load(Ordering::Relaxed);
    unsafe { libc::write(writer, (&raw const byte).cast(), 1) };
}

/// Installs handlers for SIGINT, SIGTERM, and SIGHUP and returns the read end
/// of a self-pipe so `poll` wakes immediately. If pipe creation fails, we still
/// install the handlers and fall back to noticing `SIGNALED` on the next tick.
/// Both ends are `O_CLOEXEC`, so children start clean.
fn watch_signals() -> Option<OwnedFd> {
    let mut ends = [-1; 2];
    let reader =
        if unsafe { libc::pipe2(ends.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) } == 0 {
            SIGNAL_WRITER.store(ends[1], Ordering::Relaxed);
            Some(unsafe { OwnedFd::from_raw_fd(ends[0]) })
        } else {
            say(&format!(
                "Could not open the signal pipe: {}",
                io::Error::last_os_error()
            ));
            None
        };
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = on_signal as *const () as usize;
    action.sa_flags = libc::SA_RESTART;
    unsafe { libc::sigemptyset(&mut action.sa_mask) };
    for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        if unsafe { libc::sigaction(signal, &action, std::ptr::null_mut()) } != 0 {
            say(&format!(
                "Could not catch signal {signal}: {}",
                io::Error::last_os_error()
            ));
        }
    }
    reader
}

/// Writes a timestamped line to `rc2fx.log` matching `rc2fx.sh`'s `say`.
/// Uses `writeln!` instead of `println!` so a full disk doesn't panic the
/// supervisor.
fn say(message: &str) {
    let _ = writeln!(
        io::stdout(),
        "{} {message}",
        stamp(unsafe { libc::time(std::ptr::null_mut()) })
    );
}

fn stamp(time: libc::time_t) -> String {
    let mut utc: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::gmtime_r(&time, &mut utc) };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        utc.tm_year + 1900,
        utc.tm_mon + 1,
        utc.tm_mday,
        utc.tm_hour,
        utc.tm_min,
        utc.tm_sec
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, collections::HashMap};

    struct Thread {
        name: &'static str,
        cpu0_only: bool,
        normal: bool,
        moves: bool,
    }

    const PINNED: Thread = Thread {
        name: "HIDThread",
        cpu0_only: true,
        normal: true,
        moves: true,
    };

    #[derive(Default)]
    struct Fake {
        threads: HashMap<i32, Thread>,
        /// Returned by `list`, then gone before any follow-up query.
        vanished: Vec<i32>,
        moved: RefCell<Vec<i32>>,
        named: RefCell<Vec<i32>>,
    }

    impl Threads for Fake {
        fn list(&self) -> Vec<i32> {
            let mut tids: Vec<_> = self.threads.keys().chain(&self.vanished).copied().collect();
            tids.sort();
            tids
        }
        fn pinned_to_cpu0(&self, tid: i32) -> bool {
            self.threads
                .get(&tid)
                .is_some_and(|thread| thread.cpu0_only)
        }
        fn name(&self, tid: i32) -> Option<String> {
            self.named.borrow_mut().push(tid);
            Some(self.threads.get(&tid)?.name.to_owned())
        }
        fn is_normal_priority(&self, tid: i32) -> bool {
            self.threads.get(&tid).is_some_and(|thread| thread.normal)
        }
        fn move_off_cpu0(&self, tid: i32) -> bool {
            let moves = self.threads.get(&tid).is_some_and(|thread| thread.moves);
            if moves {
                self.moved.borrow_mut().push(tid);
            }
            moves
        }
    }

    fn fake(threads: impl IntoIterator<Item = (i32, Thread)>) -> Fake {
        Fake {
            threads: threads.into_iter().collect(),
            ..Fake::default()
        }
    }

    #[test]
    fn moves_only_the_mixers_normal_priority_threads_pinned_to_cpu0() {
        let threads = fake([
            (
                101,
                Thread {
                    name: "rc_audio_mixer",
                    ..PINNED
                },
            ),
            (102, PINNED),
            (
                103,
                Thread {
                    name: "JUCEJack",
                    normal: false,
                    ..PINNED
                },
            ),
            (
                104,
                Thread {
                    name: "Pool",
                    cpu0_only: false,
                    ..PINNED
                },
            ),
        ]);
        assert_eq!(relocate(&threads), ["rc_audio_mixer", "HIDThread"]);
        assert_eq!(*threads.moved.borrow(), [101, 102]);
    }

    #[test]
    fn leaves_rc2fx_threads_alone() {
        let threads = fake([
            (
                101,
                Thread {
                    name: "rc2fx-dsp",
                    ..PINNED
                },
            ),
            (
                102,
                Thread {
                    name: "rc2fx",
                    ..PINNED
                },
            ),
        ]);
        assert!(relocate(&threads).is_empty());
        assert!(threads.moved.borrow().is_empty());
    }

    #[test]
    fn a_thread_that_exits_mid_pass_ends_nothing() {
        let mut threads = fake([(101, PINNED), (103, PINNED)]);
        threads.vanished.push(102);
        assert_eq!(relocate(&threads), ["HIDThread", "HIDThread"]);
    }

    #[test]
    fn a_refused_move_is_not_reported_as_moved() {
        let threads = fake([
            (
                101,
                Thread {
                    moves: false,
                    ..PINNED
                },
            ),
            (102, PINNED),
        ]);
        assert_eq!(relocate(&threads), ["HIDThread"]);
        assert_eq!(*threads.moved.borrow(), [102]);
    }

    #[test]
    fn asks_nothing_more_about_a_thread_that_is_not_pinned() {
        let threads = fake([(
            101,
            Thread {
                cpu0_only: false,
                ..PINNED
            },
        )]);
        relocate(&threads);
        assert!(threads.named.borrow().is_empty());
    }

    #[test]
    fn only_a_thread_name_not_yet_in_the_log_is_worth_a_line() {
        let names = |names: &[&str]| {
            names
                .iter()
                .map(|&name| name.to_owned())
                .collect::<Vec<_>>()
        };
        let mut reported = HashSet::new();
        assert!(!worth_reporting(&mut reported, &[]));
        assert!(worth_reporting(&mut reported, &names(&["Pool", "Pool"])));
        assert!(!worth_reporting(&mut reported, &names(&["Pool"])));
        assert!(worth_reporting(
            &mut reported,
            &names(&["Pool", "HIDThread"])
        ));
        assert!(!worth_reporting(&mut reported, &names(&["HIDThread"])));
    }

    #[test]
    fn a_panic_while_watching_ends_the_mixer_into_stock() {
        let dir = tempfile::tempdir().unwrap();
        let calls = dir.path().join("calls");
        let script = dir.path().join("rc2fx.sh");
        fs::write(&script, format!("echo \"$*\" >> {}\n", calls.display())).unwrap();
        let mut mixer = Command::new("sleep").arg("60").spawn().unwrap();
        let pid = mixer.id() as i32;
        let line = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
        let (_, start) = parse_stat(&line).unwrap();
        let options = Options {
            mixer: pid,
            mixer_start: start,
            script,
            controller: dir.path().join("controller"),
            controller_args: Vec::new(),
            run: dir.path().to_owned(),
            boot: dir.path().join("boot"),
            ready_seconds: READY_SECONDS,
        };
        Supervisor::new(options, None).supervise(|_| panic!("forced"));
        mixer.kill().unwrap();
        mixer.wait().unwrap();
        assert_eq!(
            fs::read_to_string(dir.path().join("boot/stock")).unwrap(),
            "Supervisor panicked; restoring stock audio\n"
        );
        assert_eq!(
            fs::read_to_string(calls).unwrap(),
            format!("supervise-restart 0 {pid} {start}\nsupervise-exit\n")
        );
    }

    #[test]
    fn a_reaped_controller_takes_its_exit_handle_with_it() {
        let dir = tempfile::tempdir().unwrap();
        // Run `/bin/sh` directly instead of a freshly written script to avoid
        // ETXTBSY if another test thread forks while the file is open for writing.
        let line = fs::read_to_string("/proc/self/stat").unwrap();
        let (_, start) = parse_stat(&line).unwrap();
        let options = Options {
            mixer: std::process::id() as i32,
            mixer_start: start,
            script: dir.path().join("rc2fx.sh"),
            controller: PathBuf::from("/bin/sh"),
            controller_args: vec!["-c".into(), "exit 3".into()],
            run: dir.path().to_owned(),
            boot: dir.path().join("boot"),
            ready_seconds: READY_SECONDS,
        };
        let mut supervisor = Supervisor::new(options, None);
        assert_eq!(supervisor.controller_exit(), None);
        assert!(supervisor.start_controller());
        assert!(supervisor.controller_pid().is_some());
        assert!(supervisor.controller.as_ref().unwrap().fd.is_some());
        let status = loop {
            if let Some(status) = supervisor.controller_exit() {
                break status;
            }
            supervisor.wait();
        };
        assert_eq!(status, 3);
        assert!(supervisor.controller.is_none());
        assert_eq!(supervisor.controller_pid(), None);
    }

    #[test]
    fn housekeeping_runs_from_tick_10_every_5_ticks() {
        let due: Vec<_> = (0..26).filter(|tick| housekeeping_due(*tick)).collect();
        assert_eq!(due, [10, 15, 20, 25]);
    }

    #[test]
    fn a_stat_line_keeps_its_fields_whatever_the_name_holds() {
        let tail = "1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 987654 20 21";
        for name in ["plain", "with space", "a) b", ") ", "(("] {
            let line = format!("42 ({name}) S {tail}\n");
            assert_eq!(parse_stat(&line), Some(('S', 987654)), "{name}");
        }
        assert_eq!(parse_stat("42 (short) S 1 2 3\n"), None);
        assert_eq!(parse_stat(""), None);
    }

    #[test]
    fn a_process_is_matched_by_its_start_time_not_only_its_pid() {
        let pid = std::process::id() as i32;
        let line = fs::read_to_string("/proc/self/stat").unwrap();
        let (_, start) = parse_stat(&line).unwrap();
        assert!(process_is(pid, start));
        assert!(!process_is(pid, start + 1));
    }

    #[test]
    fn a_zombie_is_gone_by_either_check() {
        let mut child = Command::new("true").spawn().unwrap();
        let pid = child.id() as i32;
        let pidfd = Pidfd::open(pid).unwrap();
        let line = |pid| fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
        let (_, start) = parse_stat(&line(pid)).unwrap();
        while !pidfd.exited() {
            std::thread::yield_now();
        }
        assert_eq!(parse_stat(&line(pid)).unwrap().0, 'Z');
        assert!(!process_is(pid, start));
        child.wait().unwrap();
    }

    #[test]
    fn the_real_thread_queries_see_this_process() {
        let this = MixerThreads(std::process::id() as i32);
        // Use the raw syscall because glibc 2.28 (our CI/check target) lacks gettid().
        let tid = unsafe { libc::syscall(libc::SYS_gettid) } as libc::pid_t;
        // Reading /proc/self/task can transiently miss a live thread while
        // sibling test threads exit; retry a few times.
        assert!((0..1000).any(|_| this.list().contains(&tid)));
        assert!(this.name(tid).is_some());
        assert!(this.is_normal_priority(tid));
    }

    #[test]
    fn a_stamp_matches_the_scripts_utc_form() {
        assert_eq!(stamp(0), "1970-01-01T00:00:00Z");
        assert_eq!(stamp(1_793_913_220), "2026-11-05T21:13:40Z");
    }

    #[test]
    fn a_killed_process_reports_a_shell_style_status() {
        assert_eq!(shell_status(ExitStatus::from_raw(9)), 137);
        assert_eq!(shell_status(ExitStatus::from_raw(3 << 8)), 3);
    }
}
