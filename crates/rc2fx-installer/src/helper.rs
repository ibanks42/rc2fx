//! A long-lived installer process for systems where raw USB, mounting and
//! ejection need administrator rights the desktop app does not have.
//!
//! The desktop app starts `<app> --installer-helper <root> <work>` through
//! `pkexec` once. Requests and replies are JSON lines on its stdin/stdout. A
//! cancel line may arrive while a request runs; it stops a download the same
//! way [`Engine::cancel`] does in-process.

use crate::{Engine, Error, Progress, Reply, Request, Result};
use serde::{Deserialize, Serialize};
use std::{
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::{Arc, Mutex, atomic::Ordering, mpsc},
};

#[cfg(target_os = "linux")]
#[path = "helper_appimage.rs"]
mod appimage;

// Keep the staged AppDir alive until the helper has exited, including on the
// long-lived request path. Both privilege entry points use this same planner.
fn privileged_launch() -> Result<(Command, Option<tempfile::TempDir>)> {
    let exe = std::env::current_exe()?;
    #[cfg(target_os = "linux")]
    let launch = appimage::prepare(&exe, std::env::var_os("APPDIR").as_deref())?;
    #[cfg(target_os = "linux")]
    let (exe, guard) = (launch.executable, launch.guard);
    #[cfg(not(target_os = "linux"))]
    let guard = None;
    let mut command = Command::new("pkexec");
    command.arg(exe);
    Ok((command, guard))
}

pub const FLAG: &str = "--installer-helper";

/// The start of the error the app gets when Linux refuses it the mixer's
/// USB 1 control interface (no udev rule). The app offers to install
/// [`USB_RULE`] when it sees exactly this.
pub const USB_PERMISSION: &str = "USB permission required";

/// The one udev rule RC2FX needs on Linux, as shipped in config/udev. The
/// app shows these exact bytes before installing them, and installs nothing
/// else.
pub const USB_RULE: &str = include_str!("../../../config/udev/70-rc2fx.rules");

/// Where [`USB_RULE`] is installed.
pub const USB_RULE_PATH: &str = "/etc/udev/rules.d/70-rc2fx.rules";

/// `<app> --install-usb-rule`, started through `pkexec`: writes
/// [`USB_RULE`] to [`USB_RULE_PATH`], reloads udev and exits. It takes no
/// other input.
pub const USB_RULE_FLAG: &str = "--install-usb-rule";

/// The root argument that means "the payload embedded in this executable",
/// which is what the desktop app passes: it has no folder to name.
pub const EMBEDDED: &str = "-";
pub const DOWNLOAD: &str = "--download-payload";

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Message {
    Progress(Progress),
    Done(std::result::Result<Reply, String>),
}

/// What the desktop app writes to the helper.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Inbound {
    Request(Request),
    Cancel,
}

/// Serve requests on stdin until it closes.
pub fn serve(root: PathBuf, work: PathBuf) -> Result<()> {
    let stdout = Arc::new(Mutex::new(std::io::stdout()));
    let send = {
        let stdout = stdout.clone();
        move |message: &Message| -> Result<()> {
            let mut out = stdout.lock().map_err(|_| Error("stdout lock".into()))?;
            serde_json::to_writer(&mut *out, message)?;
            out.write_all(b"\n")?;
            out.flush()?;
            Ok(())
        }
    };
    let mut engine = engine_for(root, work);
    let progress = send.clone();
    engine.progress = Some(Box::new(move |p| {
        let _ = progress(&Message::Progress(p));
    }));
    // Read stdin on its own thread so a cancel reaches a running request.
    let cancel = engine.cancel.clone();
    let (requests, incoming) = mpsc::channel::<Result<Request>>();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let parsed = line
                .map_err(Error::from)
                .and_then(|l| serde_json::from_str::<Inbound>(&l).map_err(Error::from));
            let request = match parsed {
                Ok(Inbound::Cancel) => {
                    cancel.store(true, Ordering::Relaxed);
                    continue;
                }
                Ok(Inbound::Request(r)) => Ok(r),
                Err(e) => Err(e),
            };
            if requests.send(request).is_err() {
                break;
            }
        }
    });
    for request in incoming {
        engine.cancel.store(false, Ordering::Relaxed);
        let reply = engine.handle(request?).map_err(|e| e.0);
        send(&Message::Done(reply))?;
    }
    Ok(())
}

/// The desktop side of a running helper.
pub struct Helper {
    child: Child,
    stdin: Arc<Mutex<Option<ChildStdin>>>,
    stdout: BufReader<ChildStdout>,
    _appdir: Option<tempfile::TempDir>,
}

/// Asks a running helper to stop its current download, from any thread.
#[derive(Clone)]
pub struct Canceller(Arc<Mutex<Option<ChildStdin>>>);

impl Canceller {
    pub fn cancel(&self) {
        if let Ok(mut stdin) = self.0.lock()
            && let Some(stdin) = stdin.as_mut()
        {
            let _ = write_line(stdin, &Inbound::Cancel);
        }
    }
}

fn write_line(stdin: &mut ChildStdin, message: &Inbound) -> Result<()> {
    serde_json::to_writer(&mut *stdin, message)?;
    stdin.write_all(b"\n")?;
    stdin.flush()?;
    Ok(())
}

/// The engine a helper serves: the payload embedded in this executable when
/// `root` is [`EMBEDDED`], otherwise the folder named.
fn engine_for(root: PathBuf, work: PathBuf) -> Engine {
    if root == Path::new(DOWNLOAD) {
        return Engine::download_only(work);
    }
    #[cfg(feature = "embedded")]
    if root == Path::new(EMBEDDED) {
        return Engine::embedded(work);
    }
    Engine::new(root, work)
}

impl Helper {
    pub fn start_download_only(work: &Path) -> Result<Self> {
        Self::spawn(Path::new(DOWNLOAD), work)
    }

    /// Start the helper for the payload embedded in this executable through
    /// `pkexec`, which asks for administrator permission once.
    #[cfg(feature = "embedded")]
    pub fn start_embedded(work: &Path) -> Result<Self> {
        Self::spawn(Path::new(EMBEDDED), work)
    }

    /// Start the helper for the installer folder `root` through `pkexec`.
    pub fn start(root: &Path, work: &Path) -> Result<Self> {
        Self::spawn(root, work)
    }

    fn spawn(root: &Path, work: &Path) -> Result<Self> {
        let (mut command, appdir) = privileged_launch()?;
        let mut child = command
            .arg(FLAG)
            .arg(root)
            .arg(work)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| {
                Error(format!(
                    "could not ask for administrator permission (pkexec): {e}"
                ))
            })?;
        let stdin = Arc::new(Mutex::new(child.stdin.take()));
        let stdout = BufReader::new(child.stdout.take().expect("piped stdout"));
        Ok(Helper {
            child,
            stdin,
            stdout,
            _appdir: appdir,
        })
    }

    /// Whether the helper process is still running. A request can fail
    /// because the mixer did; only a helper that has exited needs starting
    /// again.
    pub fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    pub fn canceller(&self) -> Canceller {
        Canceller(self.stdin.clone())
    }

    pub fn handle(
        &mut self,
        request: &Request,
        mut progress: impl FnMut(Progress),
    ) -> Result<Reply> {
        {
            let mut stdin = self
                .stdin
                .lock()
                .map_err(|_| Error("the installer helper stopped".into()))?;
            let stdin = stdin.as_mut().ok_or("the installer helper stopped")?;
            write_line(stdin, &Inbound::Request(request.clone()))?;
        }
        loop {
            let mut line = String::new();
            if self.stdout.read_line(&mut line)? == 0 {
                let status = self.child.try_wait().ok().flatten();
                return Err(Error(match status {
                    Some(s) if s.code() == Some(126) || s.code() == Some(127) => {
                        "administrator permission was not granted".into()
                    }
                    _ => "the installer helper stopped; reopen the installer".into(),
                }));
            }
            match serde_json::from_str(&line)? {
                Message::Progress(p) => progress(p),
                Message::Done(reply) => return reply.map_err(Error),
            }
        }
    }
}

impl Drop for Helper {
    fn drop(&mut self) {
        // Closing stdin ends the helper after any request it is serving.
        if let Ok(mut stdin) = self.stdin.lock() {
            stdin.take();
        }
        let _ = self.child.wait();
    }
}

/// The privileged side of [`USB_RULE_FLAG`]: replace the rule file
/// atomically (0644, root-owned), then have udev reload its rules and apply
/// them to the hidraw devices already plugged in.
#[cfg(target_os = "linux")]
pub fn install_usb_rule() -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let path = Path::new(USB_RULE_PATH);
    let dir = path.parent().ok_or("no rules folder")?;
    let temporary = dir.join(".70-rc2fx.rules.tmp");
    {
        let mut file = std::fs::File::create(&temporary)?;
        file.write_all(USB_RULE.as_bytes())?;
        file.set_permissions(std::fs::Permissions::from_mode(0o644))?;
        file.sync_all()?;
    }
    if let Err(error) = std::fs::rename(&temporary, path) {
        let _ = std::fs::remove_file(&temporary);
        return Err(error.into());
    }
    for args in [
        &["control", "--reload-rules"][..],
        &["trigger", "--action=add", "--subsystem-match=hidraw"][..],
    ] {
        let status = clean_command(udevadm()?).args(args).status()?;
        if !status.success() {
            return Err(Error(format!(
                "udevadm {} failed ({status})",
                args.join(" ")
            )));
        }
    }
    Ok(())
}

/// Where distributions install udevadm. A root process never finds a tool
/// through `PATH`, which pkexec's caller may have influenced.
#[cfg(target_os = "linux")]
const UDEVADM: [&str; 3] = ["/usr/bin/udevadm", "/sbin/udevadm", "/bin/udevadm"];

#[cfg(target_os = "linux")]
fn udevadm() -> Result<&'static str> {
    UDEVADM
        .into_iter()
        .find(|path| Path::new(path).is_file())
        .ok_or_else(|| Error("udevadm was not found".into()))
}

/// A command that inherits nothing from the environment.
#[cfg(target_os = "linux")]
fn clean_command(program: &str) -> Command {
    let mut command = Command::new(program);
    command.env_clear();
    command
}

#[cfg(not(target_os = "linux"))]
pub fn install_usb_rule() -> Result<()> {
    Err("Only Linux needs a USB rule".into())
}

/// The desktop side: ask the system for permission (its own password
/// prompt) to run this app's [`USB_RULE_FLAG`] once, and wait for it.
pub fn request_usb_rule() -> Result<()> {
    let (mut command, _appdir) = privileged_launch()?;
    let status = command
        .arg(USB_RULE_FLAG)
        .stdin(Stdio::null())
        .status()
        .map_err(|e| {
            Error(format!(
                "could not ask for administrator permission (pkexec): {e}"
            ))
        })?;
    match status.code() {
        Some(0) => Ok(()),
        Some(126) | Some(127) => {
            Err("The password prompt was dismissed or the password wasn't accepted.".into())
        }
        _ => Err(Error(format!("Installing the USB rule failed ({status})."))),
    }
}

/// Whether device actions need the privileged helper: raw USB, mounting and
/// ejection on Linux require root.
pub fn needed() -> bool {
    #[cfg(target_os = "linux")]
    // SAFETY: geteuid has no preconditions.
    return unsafe { libc::geteuid() } != 0;
    #[cfg(not(target_os = "linux"))]
    false
}

#[cfg(test)]
mod tests {
    use super::Inbound;

    #[cfg(target_os = "linux")]
    #[test]
    fn udevadm_is_named_absolutely_and_inherits_no_environment() {
        assert!(super::UDEVADM.iter().all(|path| path.starts_with('/')));
        let output = super::clean_command("/usr/bin/env").output().unwrap();
        assert!(output.status.success());
        assert!(output.stdout.is_empty(), "the child saw an environment");
    }

    #[test]
    fn requests_and_cancel_share_one_wire_format() {
        let line = serde_json::to_string(&Inbound::Request(crate::Request::Releases)).unwrap();
        assert!(matches!(
            serde_json::from_str(&line).unwrap(),
            Inbound::Request(crate::Request::Releases)
        ));
        let line = serde_json::to_string(&Inbound::Cancel).unwrap();
        assert!(matches!(
            serde_json::from_str(&line).unwrap(),
            Inbound::Cancel
        ));
    }
}
