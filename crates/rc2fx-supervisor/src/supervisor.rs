//! Watches one RC2FX mixer from beside it, for as long as that mixer runs.
//!
//! Runs next to live audio, so its steady state is one `poll` a second and one
//! `stat`, with no forks: main slows down whenever the other CPUs are busy. A
//! process exit or a signal wakes it at once. The once-a-second pace is kept on
//! purpose. The controller gives up on the mixer after two seconds, and the
//! control socket exists before the mixer has finished starting, so starting
//! the controller the instant the socket appears could fail into stock audio.
//! A watch on the run directory is no use either: every report written there
//! would wake this process.
//!
//! The rare, delicate steps stay in `rc2fx.sh` and are called on the way out:
//! `supervise-restart` retires the USB node and stops the processes, and
//! `supervise-exit` stops a remaining controller and writes the last-exit log.
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

/// How long a mixer has to open its control socket.
pub const READY_SECONDS: u32 = 60;
/// A mixer that exits sooner than this counts towards falling back to stock.
const EARLY_EXIT_SECONDS: i64 = 120;
/// Early exits in one boot before the launcher's next restart is stock.
const EARLY_EXITS: usize = 2;
/// Let the mixer start its threads before the first housekeeping pass.
const HOUSEKEEPING_FIRST: u32 = 10;
/// Later passes catch a thread that pins itself to CPU 0 after starting.
const HOUSEKEEPING_EVERY: u32 = 5;
const OOM_SCORE: &str = "/proc/self/oom_score_adj";

#[derive(Debug, PartialEq, Eq)]
pub struct Options {
    pub mixer: i32,
    /// The mixer's start time in clock ticks, as in `/proc/PID/stat`. A PID
    /// alone could already belong to another process.
    pub mixer_start: u64,
    /// The run directory's copy of `rc2fx.sh`, not the bundle's: an install can
    /// remove or replace the bundle while this process still has to use it.
    pub script: PathBuf,
    pub controller: PathBuf,
    pub controller_args: Vec<OsString>,
    pub run: PathBuf,
    pub boot: PathBuf,
    pub ready_seconds: u32,
}

/// Supervises until the mixer is gone or has been handed back. Nothing here can
/// stop it watching: every problem is logged and ends in the exit steps, because
/// a mixer with nothing watching it has no way back to stock audio. Launch
/// covers what this cannot, a death by signal.
pub fn run(options: Options) {
    unsafe { libc::umask(0o077) };
    // The launcher restarts the mixer, so the OOM killer should pick it first.
    if let Err(error) = fs::write(OOM_SCORE, "-1000") {
        say(&format!("Could not protect the supervisor: {error}"));
    }
    let signals = watch_signals();
    Supervisor::new(options, signals).supervise(Supervisor::watch);
}

struct Supervisor {
    options: Options,
    /// Held open so that the exit steps need no file: a new launch deletes the
    /// run directory once this process has had a few seconds to leave it.
    script: Option<File>,
    /// Only wakes `poll` early; `SIGNALLED` is what says a signal arrived.
    signals: Option<OwnedFd>,
    /// Without one (no kernel support) exits are seen on the next second.
    mixer_fd: Option<Pidfd>,
    controller: Option<Child>,
    controller_fd: Option<Pidfd>,
    /// Thread names already in the log. A thread that keeps pinning itself must
    /// not write a line to flash every pass.
    reported: HashSet<String>,
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
        // Checked after opening, so that the handle is known to be the mixer's.
        // The mixer may already be gone and its PID someone else's; with no
        // handle, the first look finds that and treats it as the mixer's exit.
        let mixer_fd =
            Pidfd::open(options.mixer).filter(|_| process_is(options.mixer, options.mixer_start));
        Self {
            options,
            script: script.ok(),
            signals,
            mixer_fd,
            controller: None,
            controller_fd: None,
            reported: HashSet::new(),
        }
    }

    /// A panic in `watch` must still leave through the exit steps.
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
        let mut tick = 0;
        loop {
            if !self.mixer_alive() {
                self.record_mixer_exit();
                return;
            }
            if SIGNALLED.load(Ordering::Relaxed) {
                self.end_mixer("Supervisor signalled; restoring stock audio", true);
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
                    // A mixer that never opens its control socket would leave
                    // a blank screen.
                    let reason = format!(
                        "Mixer not ready after {} s; restoring stock audio",
                        self.options.ready_seconds
                    );
                    self.end_mixer(&reason, true);
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

    /// One second, or less when a process exits or a signal arrives.
    fn wait(&mut self) {
        let fd = |pidfd: &Option<Pidfd>| pidfd.as_ref().map_or(-1, |pidfd| pidfd.0.as_raw_fd());
        // `poll` skips a negative descriptor.
        let mut fds = [
            self.signals.as_ref().map_or(-1, AsRawFd::as_raw_fd),
            fd(&self.mixer_fd),
            fd(&self.controller_fd),
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
        // Two early exits in one boot: the launcher's next restart is stock.
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

    /// Launch has made the directory. Only a write that needs it finds out if
    /// it has gone, and that write reports the failure.
    fn boot_file(&self, name: &str) -> PathBuf {
        let _ = fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.options.boot);
        self.options.boot.join(name)
    }

    /// False when the controller could not start and the mixer has been ended.
    fn start_controller(&mut self) -> bool {
        let spawned = File::create(self.options.run.join("controller.log")).and_then(|log| {
            let mut command = Command::new(&self.options.controller);
            command
                .args(&self.options.controller_args)
                .stdin(Stdio::null())
                .stdout(log.try_clone()?)
                .stderr(log);
            // Only this process is shielded from the OOM killer.
            unsafe { command.pre_exec(reset_oom_score) };
            command.spawn()
        });
        match spawned {
            Ok(child) => {
                say(&format!("Controller started (PID {})", child.id()));
                self.controller_fd = Pidfd::open(child.id() as i32);
                self.controller = Some(child);
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

    /// Reaps a controller that has exited and returns its shell-style status.
    fn controller_exit(&mut self) -> Option<i32> {
        let status = self.controller.as_mut()?.try_wait().ok()??;
        self.controller = None;
        self.controller_fd = None;
        Some(shell_status(status))
    }

    /// Ends the mixer for the launcher to restart, stock when `stock` is set.
    fn end_mixer(&mut self, reason: &str, stock: bool) {
        say(reason);
        if stock {
            self.request_stock(reason);
        }
        if !self.mixer_alive() {
            return;
        }
        // Only a restart into RC2FX asks to keep the gadget; stock rebuilds it.
        let keep_usb = !stock && self.options.run.join("keep-usb").exists();
        let mut args = vec![
            OsString::from("supervise-restart"),
            OsString::from(if keep_usb { "1" } else { "0" }),
            OsString::from(self.options.mixer.to_string()),
            OsString::from(self.options.mixer_start.to_string()),
        ];
        args.extend(self.controller_pid());
        if self.script(&args) {
            // Stopped by the script; anything left here would be stopped twice.
            if let Some(mut controller) = self.controller.take() {
                let _ = controller.try_wait();
            }
            self.controller_fd = None;
        } else {
            say("Leaving the mixer running without RC2FX control; rc2fx.sh start recovers it");
        }
    }

    fn finish(&mut self) {
        let mut args = vec![OsString::from("supervise-exit")];
        args.extend(self.controller_pid());
        let finished = self.script(&args);
        if let Some(controller) = self.controller.as_mut() {
            if !finished {
                let _ = controller.kill();
            }
            let _ = controller.try_wait();
        }
        // Only this supervisor's own record: the next launch may have written its.
        let record = self.options.run.join("supervisor.pid");
        let own = fs::read_to_string(&record)
            .is_ok_and(|text| text.trim() == std::process::id().to_string());
        if own {
            let _ = fs::remove_file(record);
        }
        unsafe { libc::sync() };
    }

    fn controller_pid(&self) -> Option<OsString> {
        let pid = self.controller.as_ref()?.id();
        Some(pid.to_string().into())
    }

    fn script(&self, args: &[OsString]) -> bool {
        let mut command = Command::new("/bin/bash");
        match &self.script {
            Some(file) => {
                let fd = file.as_raw_fd();
                command.arg(format!("/proc/self/fd/{fd}"));
                // Bash alone gets it: the file closes on exec everywhere else.
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

/// True when a pass moved a thread name the log has not shown yet.
fn worth_reporting(reported: &mut HashSet<String>, moved: &[String]) -> bool {
    let mut fresh = false;
    for name in moved {
        fresh |= reported.insert(name.clone());
    }
    fresh
}

/// What housekeeping needs to know about, and do to, the mixer's threads. A
/// thread can exit at any moment, so each answer may be that it is gone, and
/// `list` can miss a live thread while another exits: the next pass finds it.
trait Threads {
    fn list(&self) -> Vec<i32>;
    fn pinned_to_cpu0(&self, tid: i32) -> bool;
    fn name(&self, tid: i32) -> Option<String>;
    fn is_normal_priority(&self, tid: i32) -> bool;
    fn move_off_cpu0(&self, tid: i32) -> bool;
}

/// RØDE pins about fifteen SCHED_OTHER threads of the mixer process to CPU 0
/// only (its main thread, HIDThread, UartRunThread, JUCE Timer, SysMonThread
/// and others). CPU 0 is where the RR5 suppression worker runs, and the audio
/// path waits on non-PI locks those threads hold: moving them to CPUs 1 and 3
/// removed the fader-triggered callback stalls (device test, 2026-11-01).
/// Only threads the mixer owns are touched, only SCHED_OTHER ones, and only
/// those whose affinity is exactly CPU 0; RT threads and any other affinity
/// are left alone. The affinity test comes first: it is one system call, and
/// it rules out almost every thread on every pass after the first.
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
        // The reset-on-fork flag rides along in the result.
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

/// A process handle that cannot come to mean another process, and that `poll`
/// reports as readable when the process exits.
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

/// The state and start time from a `/proc/PID/stat` line. The name is skipped
/// up to the last `) `, because it can hold spaces and brackets of its own.
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

/// Runs between `fork` and `exec`, so it allocates nothing.
fn reset_oom_score() -> io::Result<()> {
    // Spelled out because this cannot allocate the C string for `OOM_SCORE`.
    let fd = unsafe { libc::open(c"/proc/self/oom_score_adj".as_ptr(), libc::O_WRONLY) };
    if fd >= 0 {
        unsafe {
            libc::write(fd, c"0".as_ptr().cast(), 1);
            libc::close(fd);
        }
    }
    Ok(())
}

/// Runs between `fork` and `exec`: lets one descriptor through the exec.
fn keep_open(fd: RawFd) -> io::Result<()> {
    match unsafe { libc::fcntl(fd, libc::F_SETFD, 0) } {
        0 => Ok(()),
        _ => Err(io::Error::last_os_error()),
    }
}

static SIGNALLED: AtomicBool = AtomicBool::new(false);
static SIGNAL_WRITER: AtomicI32 = AtomicI32::new(-1);

extern "C" fn on_signal(_: libc::c_int) {
    SIGNALLED.store(true, Ordering::Relaxed);
    let byte = 1u8;
    let writer = SIGNAL_WRITER.load(Ordering::Relaxed);
    unsafe { libc::write(writer, (&raw const byte).cast(), 1) };
}

/// Catches SIGINT, SIGTERM and SIGHUP. The pipe it returns turns them into
/// something `poll` sees, so none can land just before the wait and go unnoticed
/// for a second; without a pipe that second is all that is lost. Both ends close
/// on exec, and handlers do not survive it, so children start clean.
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

/// One line of `rc2fx.log`, in the same form as the script's `say`. Never
/// `println!`: it panics on a failed write, and a full disk must not end the
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
        /// Listed, then gone by the time anything asks about them.
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
        // The raw call: glibc only has gettid() from 2.30, and the checks build
        // on 2.28.
        let tid = unsafe { libc::syscall(libc::SYS_gettid) } as libc::pid_t;
        // Listing can skip a live thread while another exits, and the other
        // tests' threads are exiting. On the mixer the next pass finds it.
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
