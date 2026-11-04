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
    sync::atomic::{AtomicBool, Ordering},
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
extern "C" fn stop_signal(_: libc::c_int) {
    STOP.store(true, Ordering::Relaxed);
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
    signals()?;
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
            let mut outcome = loop {
                tokio::select! {
                    result = &mut usb => break result,
                    () = &mut ui => unreachable!("optional UI endpoint never ends the controller"),
                    () = &mut show_watch => unreachable!("show watcher never ends the controller"),
                    _ = tokio::time::sleep(Duration::from_millis(100)) => {
                        if STOP.load(Ordering::Relaxed) {
                            break Ok(());
                        }
                    }
                }
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
