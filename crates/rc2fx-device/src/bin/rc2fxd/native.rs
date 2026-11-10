//! Explicit native-controller build profile. No JACK registrations, worker
//! construction, preload environment, vendor process or firmware changes here.
use rc2fx_core::Config;
use rc2fx_device::{
    api, controller::Controller, coordinator::Coordinator, native_backend::Runtime, store::Store,
};
use std::{
    collections::HashSet,
    ffi::OsString,
    io::{self, Read},
    path::PathBuf,
    sync::atomic::{AtomicBool, AtomicI32, Ordering},
    time::Duration,
};

const HELP: &str = "rc2fxd - RC2FX controller for the RODECaster Pro II native library
Usage: rc2fxd --data ABSOLUTE_DIRECTORY --native-socket ABSOLUTE_SOCKET
              --usb-socket ABSOLUTE_SOCKET [OPTIONS]
       rc2fxd plan --data ABSOLUTE_DIRECTORY --out ABSOLUTE_FILE

  plan                      Write the native library's launch plan from the
                            saved settings (creating default settings if none)

  --usb-socket SOCKET      Datagram socket the native library forwards the
                           desktop app's USB control reports to
  --ui-socket SOCKET       Optional owner-only local touchscreen control socket
  --initial-config FILE    Seed new storage; existing saved settings win
  --stock-config FILE      Read-only saved RODE show for input discovery
  --release FILE           Installed mixer-release.json, reported as the
                           RC2FX mixer version
  --firmware-catalog FILE  Firmware catalog the launcher validated against
  --firmware-profile ID    Profile the launcher selected; with the catalog,
                           reported as the RODE firmware version
  --developer-ssh SCRIPT   Installed rc2fx.sh; enables developer SSH status
                           and key management over USB 1 only
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
    initial: Option<PathBuf>,
    stock: Option<PathBuf>,
    release: Option<PathBuf>,
    firmware_catalog: Option<PathBuf>,
    firmware_profile: Option<String>,
    developer_ssh: Option<PathBuf>,
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
    let mut initial = None;
    let mut stock = None;
    let mut usb = None;
    let mut ui = None;
    let mut release = None;
    let mut firmware_catalog = None;
    let mut firmware_profile = None;
    let mut developer_ssh = None;
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
                | "--initial-config"
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
        let value = args
            .next()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| invalid(format!("missing value for {key}")))?;
        match key {
            "--data" => data = Some(PathBuf::from(value)),
            "--native-socket" => socket = Some(PathBuf::from(value)),
            "--initial-config" => initial = Some(PathBuf::from(value)),
            "--stock-config" => stock = Some(PathBuf::from(value)),
            "--usb-socket" => usb = Some(PathBuf::from(value)),
            "--ui-socket" => ui = Some(PathBuf::from(value)),
            "--release" => release = Some(PathBuf::from(value)),
            "--firmware-catalog" => firmware_catalog = Some(PathBuf::from(value)),
            "--developer-ssh" => developer_ssh = Some(PathBuf::from(value)),
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
        .chain(initial.iter())
        .chain(stock.iter())
        .chain(ui.iter())
        .chain(release.iter())
        .chain(firmware_catalog.iter())
        .chain(developer_ssh.iter())
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
        initial,
        stock,
    }))
}
fn device_model() -> Result<rc2fx_firmware::Model, String> {
    let model = rc2fx_firmware::Model::read_device()?;
    model.require_runtime_with(rc2fx_firmware::experimental_duo_from_env())?;
    Ok(model)
}

static STOP: AtomicBool = AtomicBool::new(false);
/// The eventfd the signal handler writes to, or -1 before it exists. It stays
/// open until the process exits, because the handler can run at any time.
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
/// SIGINT and SIGTERM, as something the executor can wait for. The handler
/// writes to an eventfd, so the executor sleeps until a signal arrives and has
/// no timer that polls a flag.
struct Stop(std::os::fd::RawFd);
impl Stop {
    /// Creates the eventfd and then installs the handlers, in that order, so
    /// no signal finds a handler without it.
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
    /// Returns once a stop signal has arrived. The eventfd stays readable
    /// after a signal, so one that arrived before this is first polled, or
    /// before the executor existed, is still seen.
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
        // The reactor refused the descriptor: poll the flag as before.
        while !STOP.load(Ordering::Relaxed) {
            tokio::time::sleep(Duration::from_millis(100)).await;
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
    // Reported versions never stop the controller: what cannot be read stays
    // unknown, and the log says why.
    let (versions, problems) = rc2fx_device::versions::read(
        args.release.as_deref(),
        args.firmware_catalog.as_deref(),
        args.firmware_profile.as_deref(),
        model,
    );
    for problem in problems {
        eprintln!("{problem}");
    }
    let initial = if let Some(path) = args.initial {
        let mut text = String::new();
        std::fs::File::open(path)?
            .take(512 * 1024 + 1)
            .read_to_string(&mut text)?;
        Config::from_json(&text)?
    } else {
        rc2fx_device::catalog::default_config(model)
    };
    initial.validate()?;
    let executor = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let store = Store::open(&args.data, initial)?;
    rc2fx_device::native_plan::plan_for_model(store.config(), model)?;
    let native = Runtime::connect(&args.socket, store.config(), Duration::from_secs(2))?;
    let (handle, owner) = Coordinator::new_native(Controller::new(store), native)?;
    let stop = Stop::install()?;
    std::thread::scope(|scope| -> io::Result<()> {
        let _close = Close(handle.clone());
        let controller = std::thread::Builder::new()
            .name("rc2fx-control".into())
            .spawn_scoped(scope, move || owner.run())?;
        let outcome = executor.block_on(async {
            let options = api::Options {
                stock_config: args.stock,
                catalog: rc2fx_device::catalog::physical_catalog_for(model).to_vec(),
                versions,
                developer_ssh: None,
            };
            // Only the USB 1 server can manage developer SSH.
            let usb_options = api::Options {
                developer_ssh: args.developer_ssh.clone().map(|script| {
                    std::sync::Arc::new(rc2fx_device::developer_ssh::DeveloperSsh::new(script))
                }),
                ..options.clone()
            };
            let show_watch = async {
                if let Some(path) = options.stock_config.clone() {
                    rc2fx_device::show_watch::run(handle.clone(), path, options.catalog.clone())
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
            // Close admission, then drain: commands already accepted still
            // complete even though their replies can no longer be sent.
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
        assert!(matches!(command(&[]), Ok(Command::Start(a)) if a.release.is_none()));
        for extra in [
            vec!["--release", "relative.json"],
            vec!["--firmware-profile", "rodecaster-pro-ii-1.7.7"],
            vec!["--firmware-catalog", "/opt/rc2fx/firmware-profiles.json"],
            vec!["--release", "/a", "--release", "/b"],
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
