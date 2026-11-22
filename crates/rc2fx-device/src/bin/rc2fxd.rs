//! Device entry point. No native work, storage mutation, or threads on --help.
use rc2fx_device::{
    api, controller::Controller, coordinator::Coordinator, native_backend::Runtime, store::Store,
};
use std::{
    collections::HashSet,
    ffi::OsString,
    io,
    path::PathBuf,
    sync::atomic::{AtomicBool, AtomicI32, Ordering},
    time::Duration,
};

fn main() -> std::process::ExitCode {
    match run(std::env::args_os().skip(1)) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("rc2fxd: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

const HELP: &str = "rc2fxd - RC2FX controller for the RODECaster Pro II native library
Usage: rc2fxd --data ABSOLUTE_DIRECTORY --native-socket ABSOLUTE_SOCKET
              --usb-socket ABSOLUTE_SOCKET [OPTIONS]
       rc2fxd plan --data ABSOLUTE_DIRECTORY --out ABSOLUTE_FILE

  plan                      Write the native library's launch plan from the
                            saved settings (creating default settings if none)

  --usb-socket SOCKET      Datagram socket the native library forwards the
                           desktop app's USB control reports to
  --ui-socket SOCKET       Optional owner-only local touchscreen control socket
  --stock-config FILE      Read-only saved RODE show for input discovery
  --release FILE           Installed mixer-release.json, reported as the
                           RC2FX mixer version
  --firmware-catalog FILE  Firmware catalog the launcher validated against
  --firmware-profile ID    Profile the launcher selected; with the catalog,
                           reported as the RODE firmware version
  --developer-ssh          Enable developer SSH status and key management
                           over USB 1 only
  --help                   Show help without starting or modifying anything
  --version                Print the build version

Connects to an independently launched native callback owner. It does not start
audio, load a library, change JACK routing, or install firmware. Global toggles,
per-path enable/disable and DSP replacement operate only on already-bound paths
without restarting the vendor audio engine. SIGINT/SIGTERM drain accepted saves;
native audio keeps running under its independent owner.";

#[derive(Debug)]
struct Arguments {
    data: PathBuf,
    socket: PathBuf,
    usb: PathBuf,
    ui: Option<PathBuf>,
    stock: Option<PathBuf>,
    release: Option<PathBuf>,
    firmware_catalog: Option<PathBuf>,
    firmware_profile: Option<String>,
    developer_ssh: bool,
}
#[derive(Debug)]
enum Command {
    Help,
    Version,
    Start(Arguments),
    Plan { data: PathBuf, out: PathBuf },
}
fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}
fn parse_plan(mut args: impl Iterator<Item = OsString>) -> io::Result<Command> {
    let (mut data, mut out) = (None, None);
    while let Some(key) = args.next() {
        let slot = match key.to_str() {
            Some("--data") if data.is_none() => &mut data,
            Some("--out") if out.is_none() => &mut out,
            _ => return Err(invalid("plan takes --data and --out once each")),
        };
        *slot = Some(PathBuf::from(
            args.next()
                .filter(|value| !value.is_empty())
                .ok_or_else(|| invalid("missing value"))?,
        ));
    }
    let (Some(data), Some(out)) = (data, out) else {
        return Err(invalid("plan requires --data and --out"));
    };
    if !data.is_absolute() || !out.is_absolute() {
        return Err(invalid("plan paths must be absolute"));
    }
    Ok(Command::Plan { data, out })
}
fn parse(args: impl Iterator<Item = OsString>) -> io::Result<Command> {
    let mut args = args.peekable();
    if args.peek().and_then(|first| first.to_str()) == Some("plan") {
        args.next();
        return parse_plan(args);
    }
    let mut seen = HashSet::new();
    let mut data = None;
    let mut socket = None;
    let mut stock = None;
    let mut usb = None;
    let mut ui = None;
    let mut release = None;
    let mut firmware_catalog = None;
    let mut firmware_profile = None;
    let mut developer_ssh = false;
    while let Some(key) = args.next() {
        let key = key.to_str().ok_or_else(|| invalid("option is not UTF-8"))?;
        if matches!(key, "--help" | "--version") {
            if !seen.is_empty() || args.peek().is_some() {
                return Err(invalid("help/version must be used alone"));
            }
            return Ok(if key == "--help" {
                Command::Help
            } else {
                Command::Version
            });
        }
        if !matches!(
            key,
            "--data"
                | "--native-socket"
                | "--usb-socket"
                | "--ui-socket"
                | "--stock-config"
                | "--release"
                | "--firmware-catalog"
                | "--firmware-profile"
                | "--developer-ssh"
        ) {
            return Err(invalid(format!("unknown option {key}; use --help")));
        }
        if !seen.insert(key.to_owned()) {
            return Err(invalid(format!("duplicate option {key}")));
        }
        if key == "--developer-ssh" {
            developer_ssh = true;
            continue;
        }
        let value = args
            .next()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| invalid(format!("missing value for {key}")))?;
        match key {
            "--data" => data = Some(PathBuf::from(value)),
            "--native-socket" => socket = Some(PathBuf::from(value)),
            "--stock-config" => stock = Some(PathBuf::from(value)),
            "--usb-socket" => usb = Some(PathBuf::from(value)),
            "--ui-socket" => ui = Some(PathBuf::from(value)),
            "--release" => release = Some(PathBuf::from(value)),
            "--firmware-catalog" => firmware_catalog = Some(PathBuf::from(value)),
            "--firmware-profile" => {
                firmware_profile = Some(
                    value
                        .into_string()
                        .map_err(|_| invalid("--firmware-profile is not UTF-8"))?,
                )
            }
            _ => unreachable!(),
        }
    }
    let data = data.ok_or_else(|| invalid("--data is required"))?;
    let socket = socket.ok_or_else(|| invalid("--native-socket is required"))?;
    let usb = usb.ok_or_else(|| invalid("--usb-socket is required"))?;
    if [&data, &socket, &usb]
        .into_iter()
        .chain(stock.iter())
        .chain(ui.iter())
        .chain(release.iter())
        .chain(firmware_catalog.iter())
        .any(|path| !path.is_absolute())
    {
        return Err(invalid("native controller paths must be absolute"));
    }
    if ui
        .as_ref()
        .is_some_and(|path| path == &usb || path == &socket)
    {
        return Err(invalid(
            "--ui-socket must differ from USB and native sockets",
        ));
    }
    if firmware_catalog.is_some() != firmware_profile.is_some() {
        return Err(invalid(
            "--firmware-catalog and --firmware-profile go together",
        ));
    }
    Ok(Command::Start(Arguments {
        developer_ssh,
        release,
        firmware_catalog,
        firmware_profile,
        data,
        socket,
        usb,
        ui,
        stock,
    }))
}
fn device_model() -> Result<rc2fx_firmware::Model, String> {
    let model = rc2fx_firmware::Model::read_device()?;
    model.require_runtime_with(rc2fx_firmware::experimental_duo_from_env())?;
    Ok(model)
}

static STOP: AtomicBool = AtomicBool::new(false);
/// Eventfd written by the signal handler (-1 before init). Kept open until
/// process exit because the handler can fire at any time.
static STOP_FD: AtomicI32 = AtomicI32::new(-1);
extern "C" fn stop_signal(_: libc::c_int) {
    STOP.store(true, Ordering::Relaxed);
    let fd = STOP_FD.load(Ordering::Relaxed);
    if fd >= 0 {
        let one = 1u64.to_ne_bytes();
        // SAFETY: write(2) is async-signal-safe and the descriptor is never
        // closed. errno belongs to the code the signal interrupted.
        unsafe {
            let errno = *libc::__errno_location();
            libc::write(fd, one.as_ptr().cast(), one.len());
            *libc::__errno_location() = errno;
        }
    }
}
fn signals() -> io::Result<()> {
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = stop_signal as *const () as usize;
    action.sa_flags = libc::SA_RESTART;
    unsafe {
        libc::sigemptyset(&mut action.sa_mask);
    }
    for signal in [libc::SIGINT, libc::SIGTERM] {
        if unsafe { libc::sigaction(signal, &action, std::ptr::null_mut()) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}
/// Async wait handle for SIGINT/SIGTERM backed by an eventfd (no polling timer).
struct Stop(std::os::fd::RawFd);
impl Stop {
    /// Creates the eventfd before installing handlers so no signal fires without it.
    fn install() -> io::Result<Self> {
        let mut fd = STOP_FD.load(Ordering::Relaxed);
        if fd < 0 {
            // SAFETY: plain descriptor creation.
            fd = unsafe { libc::eventfd(0, libc::EFD_NONBLOCK | libc::EFD_CLOEXEC) };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            STOP_FD.store(fd, Ordering::Relaxed);
        }
        signals()?;
        Ok(Self(fd))
    }
    /// Resolves once a stop signal has arrived (even if it arrived before polling).
    async fn wait(&self) {
        use tokio::io::{Interest, unix::AsyncFd};
        if STOP.load(Ordering::Relaxed) {
            return;
        }
        // SAFETY: the descriptor is valid and stays open, as the same eventfd,
        // for the life of the process.
        let registered = unsafe { AsyncFd::register_with_interest(self.0, Interest::READABLE) };
        if let Ok(fd) = registered
            && fd.readable().await.is_ok()
        {
            return;
        }
        // Reactor rejected the fd; fall back to polling the atomic flag.
        while !STOP.load(Ordering::Relaxed) {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    /// Ends `wait` as a stop signal would, from an ordinary thread.
    fn wake(&self) {
        STOP.store(true, Ordering::Relaxed);
        let one = 1u64.to_ne_bytes();
        // SAFETY: the descriptor is valid and stays open for the life of the
        // process. A full counter still leaves the eventfd readable.
        unsafe { libc::write(self.0, one.as_ptr().cast(), one.len()) };
    }
}
/// Wakes `Stop` on drop if the holding thread panics, so a control-thread panic
/// brings down the daemon with a non-zero exit instead of serving stale state.
struct StopOnPanic<'stop>(&'stop Stop);
impl Drop for StopOnPanic<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.0.wake();
        }
    }
}
struct Close(rc2fx_device::coordinator::Handle);
impl Drop for Close {
    fn drop(&mut self) {
        self.0.close();
    }
}
pub fn run(args: impl Iterator<Item = OsString>) -> Result<(), Box<dyn std::error::Error>> {
    let args = match parse(args)? {
        Command::Help => {
            println!("{HELP}");
            return Ok(());
        }
        Command::Version => {
            println!("rc2fxd {} native", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Command::Plan { data, out } => {
            let model = device_model()?;
            let store = Store::open(&data, rc2fx_device::catalog::default_config(model))?;
            let plan = rc2fx_device::native_plan::plan_for_model(store.config(), model)?;
            // Write beside the target and rename, so a reader never sees half a plan.
            let partial = out.with_extension("partial");
            std::fs::write(&partial, serde_json::to_vec(&plan)?)?;
            std::fs::rename(&partial, &out)?;
            return Ok(());
        }
        Command::Start(args) => args,
    };
    let model = device_model()?;
    // Version probe failures are non-fatal; unreadable fields stay unknown and get logged.
    let (mut versions, problems) = rc2fx_device::versions::read(
        args.release.as_deref(),
        args.firmware_catalog.as_deref(),
        args.firmware_profile.as_deref(),
        model,
    );
    for problem in problems {
        eprintln!("{problem}");
    }
    // Only offer in-place updates when running from /opt/rc2fx (not a RAM build).
    // Running from the rollback copy still recovers and reports the failed update
    // but refuses new ones.
    let paths = rc2fx_device::mixer_update::install::Paths::mixer();
    let started_from = args
        .release
        .as_deref()
        .and_then(|release| rc2fx_device::mixer_update::installed_bundle(&paths, release));
    let image_check_dir = started_from.map(|_| paths.bundle());
    let mixer_update = started_from.map(|bundle| {
        let update = rc2fx_device::mixer_update::MixerUpdate::new(
            paths,
            args.firmware_profile.clone(),
            versions.runtime.clone(),
        );
        std::sync::Arc::new(match bundle {
            rc2fx_device::mixer_update::InstalledBundle::Live => update,
            rc2fx_device::mixer_update::InstalledBundle::Previous => update.without_new_updates(),
        })
    });
    if mixer_update
        .as_ref()
        .is_some_and(|update| update.offers_updates())
    {
        versions.in_place_updates = Some(rc2fx_device::mixer_update::IN_PLACE_UPDATES);
    }
    if let Some(update) = &mixer_update {
        // Recover any power-cut-interrupted update before serving requests.
        update.recover();
        update.note_started();
    }
    let executor = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    // Seed new storage with the same model defaults `plan` uses.
    let store = Store::open(&args.data, rc2fx_device::catalog::default_config(model))?;
    rc2fx_device::native_plan::plan_for_model(store.config(), model)?;
    let native = Runtime::connect(&args.socket, store.config(), Duration::from_secs(2))?;
    // The mixer process may still hold an older native library until restart;
    // report the protocol version it actually speaks.
    rc2fx_device::versions::note_native_protocol(&mut versions, native.protocol());
    let (handle, owner) = Coordinator::new_native(Controller::new(store), native)?;
    let stop = Stop::install()?;
    std::thread::scope(|scope| -> io::Result<()> {
        let _close = Close(handle.clone());
        let stop_ref = &stop;
        let controller = std::thread::Builder::new()
            .name("rc2fx-control".into())
            .spawn_scoped(scope, move || {
                let _stop = StopOnPanic(stop_ref);
                owner.run()
            })?;
        let outcome = executor.block_on(async {
            let options = api::Options {
                model,
                stock_config: args.stock,
                catalog: rc2fx_device::catalog::physical_catalog_for(model).to_vec(),
                versions,
                image_check_dir: None,
                developer_ssh: None,
                mixer_update: None,
            };
            // Only the USB 1 server can manage developer SSH, read the image
            // check, or install a signed release in place.
            let usb_options = api::Options {
                image_check_dir: image_check_dir.clone(),
                developer_ssh: args
                    .developer_ssh
                    .then(|| std::sync::Arc::new(rc2fx_device::developer_ssh::DeveloperSsh::new())),
                mixer_update: mixer_update.clone(),
                ..options.clone()
            };
            let show_watch = async {
                if let Some(path) = options.stock_config.clone() {
                    rc2fx_device::show_watch::run(
                        handle.clone(),
                        path,
                        options.catalog.clone(),
                        model,
                    )
                    .await;
                }
                std::future::pending::<()>().await
            };
            let usb = rc2fx_device::usb_control::serve(&args.usb, handle.clone(), usb_options);
            // An optional display endpoint must never take USB/audio down if
            // it cannot bind. Accepted mutations still use the same owner.
            let ui = async {
                if let Some(path) = &args.ui
                    && let Err(error) =
                        rc2fx_device::ui_control::serve(path, handle.clone(), options.clone()).await
                {
                    eprintln!("Touchscreen control unavailable: {error}");
                }
                std::future::pending::<()>().await
            };
            tokio::pin!(usb, ui, show_watch);
            let mut outcome = tokio::select! {
                result = &mut usb => result,
                () = &mut ui => unreachable!("optional UI endpoint never ends the controller"),
                () = &mut show_watch => unreachable!("show watcher never ends the controller"),
                () = stop.wait() => Ok(()),
            };
            // Close admission and drain already-accepted commands even if replies can't be sent.
            handle.close();
            if let Err(error) = handle.shutdown().await
                && outcome.is_ok()
            {
                outcome = Err(io::Error::other(error.to_string()));
            }
            outcome
        });
        handle.close();
        controller
            .join()
            .map_err(|_| io::Error::other("native controller thread panicked"))?;
        outcome
    })?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;

    /// The handlers and the eventfd are process-wide: one signal test at a time.
    static SIGNALS: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// A `Stop` with no signal pending.
    fn fresh_stop() -> Stop {
        let stop = Stop::install().unwrap();
        STOP.store(false, Ordering::Relaxed);
        let mut count = [0u8; 8];
        // Drain the eventfd; it is non-blocking, so an empty one just fails.
        unsafe { libc::read(stop.0, count.as_mut_ptr().cast(), count.len()) };
        stop
    }
    fn executor() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }
    /// Whether the wait ended within `limit`.
    fn stopped_within(stop: &Stop, limit: Duration) -> bool {
        executor().block_on(async { tokio::time::timeout(limit, stop.wait()).await.is_ok() })
    }
    const PROMPT: Duration = Duration::from_secs(5);

    #[test]
    fn a_signal_from_before_the_executor_started_still_stops_it() {
        let _one = SIGNALS.lock().unwrap_or_else(|e| e.into_inner());
        let stop = fresh_stop();
        // raise() returns after the handler has run on this thread.
        assert_eq!(unsafe { libc::raise(libc::SIGTERM) }, 0);
        assert!(STOP.load(Ordering::Relaxed));
        assert!(stopped_within(&stop, PROMPT), "an early SIGTERM was lost");
        // A signal between the flag check and the first poll leaves only the
        // eventfd to tell: with the flag cleared the wait still ends.
        STOP.store(false, Ordering::Relaxed);
        assert!(stopped_within(&stop, PROMPT), "the eventfd alone ends it");
    }

    #[test]
    fn sigint_and_sigterm_end_a_running_wait_without_a_polling_delay() {
        let _one = SIGNALS.lock().unwrap_or_else(|e| e.into_inner());
        for signal in [libc::SIGINT, libc::SIGTERM] {
            let stop = fresh_stop();
            // Nothing is pending: the wait does not end by itself.
            assert!(!stopped_within(&stop, Duration::from_millis(150)));
            let sender = std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(50));
                assert_eq!(unsafe { libc::kill(libc::getpid(), signal) }, 0);
            });
            let started = std::time::Instant::now();
            assert!(stopped_within(&stop, PROMPT), "signal {signal} was lost");
            // The signal itself ends the wait. The old flag was polled every
            // 100 ms; a generous bound still tells the two apart from a miss.
            assert!(started.elapsed() < PROMPT / 2, "{:?}", started.elapsed());
            sender.join().unwrap();
            assert!(STOP.load(Ordering::Relaxed));
        }
    }
    /// A control thread that panics ends the daemon's wait; one that
    /// returns does not, since the daemon's own stop decides that.
    #[test]
    fn a_panicking_control_thread_stops_the_daemon() {
        let _one = SIGNALS.lock().unwrap_or_else(|e| e.into_inner());
        let stop = fresh_stop();
        std::thread::scope(|scope| {
            let returned = scope.spawn(|| {
                let _stop = StopOnPanic(&stop);
            });
            assert!(returned.join().is_ok());
        });
        assert!(!stopped_within(&stop, Duration::from_millis(150)));
        std::thread::scope(|scope| {
            let panicked = scope.spawn(|| {
                let _stop = StopOnPanic(&stop);
                panic!("the control thread panicked");
            });
            assert!(panicked.join().is_err());
        });
        assert!(stopped_within(&stop, PROMPT), "the panic did not stop it");
        // The eventfd alone also ends it, as for a signal.
        STOP.store(false, Ordering::Relaxed);
        assert!(stopped_within(&stop, PROMPT), "the eventfd alone ends it");
    }

    #[test]
    fn start_needs_absolute_data_and_sockets_and_nothing_else() {
        let command = |values: &[&str]| parse(values.iter().map(OsString::from));
        assert!(matches!(command(&["--help"]).unwrap(), Command::Help));
        assert!(matches!(
            command(&[
                "--data",
                "/tmp/state",
                "--native-socket",
                "/tmp/s",
                "--usb-socket",
                "/tmp/u",
                "--ui-socket",
                "/tmp/ui"
            ]),
            Ok(Command::Start(_))
        ));
        assert!(matches!(
            command(&[
                "--data",
                "/tmp/state",
                "--native-socket",
                "/tmp/control.sock",
                "--usb-socket",
                "/tmp/usb.sock"
            ])
            .unwrap(),
            Command::Start(_)
        ));
        for values in [
            vec!["--data", "/tmp/state"],
            vec!["--data", "/tmp/state", "--native-socket", "/tmp/s"],
            vec![
                "--data",
                "/tmp/state",
                "--native-socket",
                "/tmp/s",
                "--usb-socket",
                "relative",
            ],
            vec!["--data", "/tmp/state", "--native-socket", "relative"],
            vec![
                "--data",
                "/tmp/state",
                "--native-socket",
                "/tmp/s",
                "--watcher",
                "/tmp/w",
            ],
            vec!["--help", "--data", "/tmp/state"],
            vec![
                "--data",
                "/tmp/state",
                "--native-socket",
                "/tmp/s",
                "--usb-socket",
                "/tmp/u",
                "--ui-socket",
                "relative",
            ],
        ] {
            assert!(command(&values).is_err());
        }
    }

    #[test]
    fn version_sources_are_optional_absolute_and_paired() {
        let command = |extra: &[&str]| {
            let mut values = vec![
                "--data",
                "/tmp/state",
                "--native-socket",
                "/tmp/s",
                "--usb-socket",
                "/tmp/u",
            ];
            values.extend_from_slice(extra);
            parse(values.into_iter().map(OsString::from))
        };
        let Ok(Command::Start(args)) = command(&[
            "--release",
            "/opt/rc2fx/mixer-release.json",
            "--firmware-catalog",
            "/opt/rc2fx/firmware-profiles.json",
            "--firmware-profile",
            "rodecaster-pro-ii-1.7.7",
        ]) else {
            panic!("all version sources are accepted");
        };
        assert_eq!(
            args.firmware_profile.as_deref(),
            Some("rodecaster-pro-ii-1.7.7")
        );
        assert!(
            matches!(command(&[]), Ok(Command::Start(a)) if a.release.is_none() && !a.developer_ssh)
        );
        assert!(matches!(
            command(&["--developer-ssh"]),
            Ok(Command::Start(a)) if a.developer_ssh
        ));
        for extra in [
            vec!["--release", "relative.json"],
            vec!["--firmware-profile", "rodecaster-pro-ii-1.7.7"],
            vec!["--firmware-catalog", "/opt/rc2fx/firmware-profiles.json"],
            vec!["--release", "/a", "--release", "/b"],
            vec!["--developer-ssh", "--developer-ssh"],
            vec!["--developer-ssh", "/opt/rc2fx/rc2fx.sh"],
        ] {
            assert!(command(&extra).is_err(), "{extra:?}");
        }
    }

    #[test]
    fn plan_subcommand_needs_absolute_data_and_output() {
        let command = |values: &[&str]| parse(values.iter().map(OsString::from));
        assert!(matches!(
            command(&["plan", "--data", "/tmp/state", "--out", "/tmp/plan.json"]).unwrap(),
            Command::Plan { .. }
        ));
        for values in [
            vec!["plan", "--data", "/tmp/state"],
            vec!["plan", "--data", "rel", "--out", "/tmp/plan.json"],
            vec!["plan", "--data", "/a", "--data", "/b", "--out", "/tmp/p"],
            vec!["plan", "--out", "/tmp/p", "--usb-socket", "/tmp/u"],
        ] {
            assert!(command(&values).is_err(), "{values:?}");
        }
    }
}
