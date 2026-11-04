//! Developer SSH access for the desktop app over USB 1: status, and adding or
//! removing RC2FX's one managed `authorized_keys` line, without reflashing.
//!
//! The behaviour lives in `rc2fx.sh developer-ssh`; this module validates the
//! request, runs the script off the async executor with bounded time, input
//! and output, serializes every call, and parses the script's report. It is
//! only reachable from the USB control API: the touchscreen socket is never
//! given a handle. Removing the key stops new logins; open sessions stay open.

use rc2fx_core::{Ed25519PublicKey, SshRequest, SshStatus};
use std::{
    io::{Read, Write},
    path::PathBuf,
    process::{Command, Stdio},
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

/// The longest a script call may take before it is killed.
const TIMEOUT: Duration = Duration::from_secs(10);
/// The most script output read; a status report is a few hundred bytes.
const MAX_OUTPUT: u64 = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SshError {
    /// The request or key is invalid (400).
    Invalid(String),
    /// Another SSH change is in progress (409).
    Busy,
    /// The first boot after an update is still running (409).
    FirstBoot(String),
    /// `~/.ssh` or `authorized_keys` is not safe to touch (409).
    Unsafe(String),
    /// The script failed or its report was unreadable (500).
    Failed(String),
}

impl SshError {
    pub fn status(&self) -> u16 {
        match self {
            SshError::Invalid(_) => 400,
            SshError::Busy | SshError::FirstBoot(_) | SshError::Unsafe(_) => 409,
            SshError::Failed(_) => 500,
        }
    }
}

impl std::fmt::Display for SshError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SshError::Invalid(why) => write!(f, "{why}"),
            SshError::Busy => f.write_str("Another SSH change is in progress"),
            SshError::FirstBoot(why) | SshError::Unsafe(why) | SshError::Failed(why) => {
                write!(f, "{why}")
            }
        }
    }
}

/// The handle the USB API uses. Cloned into every client; one permit, owned
/// by the blocking task for as long as the script runs, so a cancelled
/// request cannot let a second change start while the first is still going.
pub struct DeveloperSsh {
    script: PathBuf,
    env: Vec<(String, String)>,
    timeout: Duration,
    permit: Arc<tokio::sync::Semaphore>,
}

impl DeveloperSsh {
    /// `script` is the installed `rc2fx.sh`.
    pub fn new(script: PathBuf) -> Self {
        Self {
            script,
            env: Vec::new(),
            timeout: TIMEOUT,
            permit: Arc::new(tokio::sync::Semaphore::new(1)),
        }
    }

    /// Extra environment for the script; tests point its paths elsewhere.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn with_env(mut self, env: &[(&str, &str)]) -> Self {
        self.env = env
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        self
    }

    #[cfg(test)]
    pub(crate) fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub async fn status(&self) -> Result<SshStatus, SshError> {
        self.run(&["status"], String::new()).await
    }

    /// Validate `request` completely before anything runs, then enable or
    /// disable the managed key.
    pub async fn set(&self, request: SshRequest) -> Result<SshStatus, SshError> {
        let (action, input) = match (request.enabled, request.public_key) {
            (true, Some(key)) => {
                let key = Ed25519PublicKey::parse(&key).map_err(|e| SshError::Invalid(e.0))?;
                ("enable", format!("{}\n", key.as_str()))
            }
            (true, None) => {
                return Err(SshError::Invalid(
                    "enabling SSH needs the app's public key".into(),
                ));
            }
            (false, None) => ("disable", String::new()),
            (false, Some(_)) => {
                return Err(SshError::Invalid(
                    "disabling SSH takes no public key".into(),
                ));
            }
        };
        self.run(&[action], input).await
    }

    async fn run(&self, args: &[&str], input: String) -> Result<SshStatus, SshError> {
        let permit = self
            .permit
            .clone()
            .try_acquire_owned()
            .map_err(|_| SshError::Busy)?;
        let mut command = Command::new("/bin/bash");
        command
            .arg(&self.script)
            .arg("developer-ssh")
            .args(args)
            .env_clear()
            .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
            .envs(self.env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let timeout = self.timeout;
        let (code, stdout, stderr) = tokio::task::spawn_blocking(move || {
            // Held until the script and everything it started are gone.
            let _permit = permit;
            run_bounded(command, input, timeout)
        })
        .await
        .map_err(|e| SshError::Failed(format!("SSH helper task failed: {e}")))??;
        let message = || {
            let text = stderr.trim();
            if text.is_empty() {
                format!("rc2fx.sh developer-ssh exited with status {code:?}")
            } else {
                text.to_owned()
            }
        };
        match code {
            Some(0) => parse_status(&stdout),
            Some(2) => Err(SshError::Invalid(message())),
            Some(3) => Err(SshError::Unsafe(message())),
            Some(4) => Err(SshError::FirstBoot(message())),
            _ => Err(SshError::Failed(message())),
        }
    }
}

/// Run `command` in its own process group with `input` on stdin. The whole
/// operation, including reading both streams to their end, has one
/// `timeout`; past it the process group is killed. Output over `MAX_OUTPUT`
/// is an error, never parsed. Blocking: call it from `spawn_blocking`.
fn run_bounded(
    mut command: Command,
    input: String,
    timeout: Duration,
) -> Result<(Option<i32>, String, String), SshError> {
    use std::os::unix::process::CommandExt;
    let failed = |e: std::io::Error| SshError::Failed(format!("cannot run rc2fx.sh: {e}"));
    let deadline = Instant::now() + timeout;
    let mut child = command.process_group(0).spawn().map_err(failed)?;
    let group = child.id() as libc::pid_t;
    let kill_group = || unsafe {
        libc::kill(-group, libc::SIGKILL);
    };
    if let Some(mut stdin) = child.stdin.take() {
        // A script that exits early closes the pipe; that is reported by its
        // exit status, not here.
        let _ = stdin.write_all(input.as_bytes());
    }
    let (sender, results) = mpsc::channel();
    for (index, stream) in [
        child
            .stdout
            .take()
            .map(|s| Box::new(s) as Box<dyn Read + Send>),
        child
            .stderr
            .take()
            .map(|s| Box::new(s) as Box<dyn Read + Send>),
    ]
    .into_iter()
    .enumerate()
    {
        let sender = sender.clone();
        std::thread::spawn(move || {
            let mut out = Vec::new();
            if let Some(stream) = stream {
                let _ = stream.take(MAX_OUTPUT + 1).read_to_end(&mut out);
            }
            let _ = sender.send((index, out));
        });
    }
    drop(sender);
    let mut streams = [None, None];
    let mut timed_out = false;
    while streams.iter().any(Option::is_none) {
        let left = deadline.saturating_duration_since(Instant::now());
        match results.recv_timeout(left) {
            Ok((index, out)) => streams[index] = Some(out),
            Err(_) if !timed_out => {
                // Something in the group still holds a pipe: end the group,
                // which closes the pipes and lets the readers finish.
                timed_out = true;
                kill_group();
                match results.recv_timeout(Duration::from_secs(1)) {
                    Ok((index, out)) => streams[index] = Some(out),
                    Err(_) => break,
                }
            }
            Err(_) => break,
        }
    }
    let status = loop {
        match child.try_wait().map_err(failed)? {
            Some(status) => break Some(status),
            None if Instant::now() >= deadline => {
                timed_out = true;
                kill_group();
                break child.wait().ok();
            }
            None => std::thread::sleep(Duration::from_millis(5)),
        }
    };
    // Anything the script left behind in its group goes with it.
    kill_group();
    if timed_out {
        return Err(SshError::Failed("rc2fx.sh developer-ssh timed out".into()));
    }
    let [Some(stdout), Some(stderr)] = streams else {
        return Err(SshError::Failed(
            "rc2fx.sh developer-ssh output was not read".into(),
        ));
    };
    if stdout.len() as u64 > MAX_OUTPUT || stderr.len() as u64 > MAX_OUTPUT {
        return Err(SshError::Failed(
            "rc2fx.sh developer-ssh output is too large".into(),
        ));
    }
    Ok((
        status.and_then(|s| s.code()),
        String::from_utf8_lossy(&stdout).into_owned(),
        String::from_utf8_lossy(&stderr).into_owned(),
    ))
}

/// The script's report: `managed KEY`, `unmanaged N`, `host LINE`...,
/// `server running|stopped|unknown`. `unmanaged` is required.
pub(crate) fn parse_status(text: &str) -> Result<SshStatus, SshError> {
    let bad = |why: String| Err(SshError::Failed(format!("unreadable SSH status: {why}")));
    let mut status = SshStatus::default();
    let mut unmanaged = None;
    for line in text.lines() {
        let (kind, value) = line.split_once(' ').unwrap_or((line, ""));
        match kind {
            "managed" => {
                let key = Ed25519PublicKey::parse(value).map_err(|e| SshError::Failed(e.0))?;
                status.managed = true;
                status.managed_fingerprint = Some(key.fingerprint());
            }
            "unmanaged" => match value.parse::<u32>() {
                Ok(n) => unmanaged = Some(n),
                Err(_) => return bad(format!("{line:?}")),
            },
            "host" => status.host_keys.push(value.to_owned()),
            "server" => {
                status.server_running = match value {
                    "running" => Some(true),
                    "stopped" => Some(false),
                    _ => None,
                }
            }
            _ => return bad(format!("{line:?}")),
        }
    }
    let Some(unmanaged) = unmanaged else {
        return bad("no key count".into());
    };
    status.unmanaged_keys = unmanaged;
    Ok(status)
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIEPzD7gW9u5+SC5iq8gzujWEOTBQO1CbVCLO8fAOGkip";
    #[cfg(target_os = "linux")]
    const MARK: &str = rc2fx_core::SSH_MANAGED_MARKER;

    #[cfg(target_os = "linux")]
    struct Fixture {
        _dir: tempfile::TempDir,
        ssh: PathBuf,
        first_boot: PathBuf,
        handle: DeveloperSsh,
    }

    #[cfg(target_os = "linux")]
    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let ssh = dir.path().join("home/root/.ssh");
        std::fs::create_dir_all(&ssh).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o700)).unwrap();
        let hosts = dir.path().join("etc/ssh");
        std::fs::create_dir_all(&hosts).unwrap();
        std::fs::write(
            hosts.join("ssh_host_ed25519_key.pub"),
            format!("{KEY} root@mixer\n"),
        )
        .unwrap();
        let first_boot = dir.path().join("first-boot");
        let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../xtask/mixer/rc2fx.sh");
        let handle = DeveloperSsh::new(script).with_env(&[
            ("RC2FX_SSH_DIR", ssh.to_str().unwrap()),
            ("RC2FX_SSH_HOST_DIR", hosts.to_str().unwrap()),
            ("RC2FX_FIRST_BOOT", first_boot.to_str().unwrap()),
            ("RC2FX_DATA", dir.path().join("data").to_str().unwrap()),
            ("RC2FX_LOGS", dir.path().join("logs").to_str().unwrap()),
        ]);
        Fixture {
            _dir: dir,
            ssh,
            first_boot,
            handle,
        }
    }

    fn enable(key: &str) -> SshRequest {
        SshRequest {
            enabled: true,
            public_key: Some(key.into()),
        }
    }

    // The real script uses GNU stat: Linux only.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn enable_status_and_disable_round_trip_through_the_real_script() {
        let f = fixture();
        let keys = f.ssh.join("authorized_keys");
        std::fs::write(
            &keys,
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOTHERkeyOTHERkeyOTHERkeyOTHERkeyOTHERkeyOT\n",
        )
        .unwrap();
        let status = f.handle.status().await.unwrap();
        assert!(!status.managed);
        assert_eq!(status.unmanaged_keys, 1);
        assert_eq!(status.host_keys, [KEY]);
        let on = f
            .handle
            .set(enable(&format!("{KEY} me@laptop")))
            .await
            .unwrap();
        assert!(on.managed);
        assert_eq!(
            on.managed_fingerprint.as_deref(),
            Some("SHA256:QbAgxtEX0z3ZCUzJ86NO6a0PYcnvrkRmVh0+cMm05TY")
        );
        // Managed access does not hide the key RC2FX did not add.
        assert_eq!(on.unmanaged_keys, 1);
        assert!(
            std::fs::read_to_string(&keys)
                .unwrap()
                .contains(&format!("{KEY} {MARK}"))
        );
        let off = f
            .handle
            .set(SshRequest {
                enabled: false,
                public_key: None,
            })
            .await
            .unwrap();
        assert!(!off.managed);
        assert_eq!(off.unmanaged_keys, 1);
    }

    #[tokio::test]
    async fn invalid_requests_are_refused_before_anything_runs() {
        // A missing script would fail with 500 if anything ran.
        let missing = DeveloperSsh::new(PathBuf::from("/nonexistent/rc2fx.sh"));
        for request in [
            enable("ssh-rsa AAAAB3NzaC1yc2E"),
            enable(&format!("command=\"sh\" {KEY}")),
            enable(&format!("{KEY}\n{KEY}")),
            SshRequest {
                enabled: true,
                public_key: None,
            },
            SshRequest {
                enabled: false,
                public_key: Some(KEY.into()),
            },
        ] {
            let error = missing.set(request.clone()).await.unwrap_err();
            assert_eq!(error.status(), 400, "{request:?}");
        }
    }

    // The real script uses GNU stat: Linux only.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn first_boot_and_unsafe_paths_are_conflicts() {
        let f = fixture();
        std::fs::write(&f.first_boot, "").unwrap();
        let error = f.handle.set(enable(KEY)).await.unwrap_err();
        assert!(matches!(error, SshError::FirstBoot(_)), "{error:?}");
        assert_eq!(error.status(), 409);
        std::fs::remove_file(&f.first_boot).unwrap();
        std::os::unix::fs::symlink("/etc/passwd", f.ssh.join("authorized_keys")).unwrap();
        let error = f.handle.set(enable(KEY)).await.unwrap_err();
        assert!(matches!(error, SshError::Unsafe(_)), "{error:?}");
    }

    /// A stand-in rc2fx.sh that sleeps, holding its pipes, then reports.
    fn slow_script(dir: &std::path::Path, seconds: &str) -> PathBuf {
        let path = dir.join("slow.sh");
        std::fs::write(
            &path,
            format!(
                "sleep {seconds} & echo $! > {}/child.pid\nwait\necho 'unmanaged 0'\n",
                dir.display()
            ),
        )
        .unwrap();
        path
    }

    #[tokio::test]
    async fn a_cancelled_request_keeps_the_permit_until_the_script_ends() {
        let dir = tempfile::tempdir().unwrap();
        let handle = Arc::new(DeveloperSsh::new(slow_script(dir.path(), "1")));
        // Cancel the first call while its script is still running.
        let first = tokio::time::timeout(Duration::from_millis(200), handle.status()).await;
        assert!(first.is_err(), "the first call was cancelled");
        assert_eq!(handle.status().await.unwrap_err(), SshError::Busy);
        assert_eq!(handle.set(enable(KEY)).await.unwrap_err(), SshError::Busy);
        // Once the script has ended, the permit is free again.
        tokio::time::sleep(Duration::from_millis(1500)).await;
        assert_eq!(handle.status().await.unwrap().unmanaged_keys, 0);
    }

    #[tokio::test]
    async fn the_deadline_covers_the_whole_group_and_cleans_it_up() {
        let dir = tempfile::tempdir().unwrap();
        let handle = DeveloperSsh::new(slow_script(dir.path(), "30"))
            .with_timeout(Duration::from_millis(300));
        let start = Instant::now();
        let error = handle.status().await.unwrap_err();
        assert!(
            matches!(error, SshError::Failed(ref why) if why.contains("timed out")),
            "{error:?}"
        );
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "{:?}",
            start.elapsed()
        );
        // The background sleep that held the pipes is gone too.
        let pid: i32 = std::fs::read_to_string(dir.path().join("child.pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        std::thread::sleep(Duration::from_millis(100));
        assert_ne!(unsafe { libc::kill(pid, 0) }, 0, "child {pid} survived");
    }

    #[test]
    fn oversized_output_is_an_error_not_a_prefix() {
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "head -c 70000 /dev/zero; exit 0"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let error = run_bounded(command, String::new(), TIMEOUT).unwrap_err();
        assert!(matches!(error, SshError::Failed(ref why) if why.contains("too large")));
    }

    #[test]
    fn reports_are_parsed_strictly() {
        let ok = parse_status(&format!(
            "managed {KEY}\nunmanaged 2\nhost {KEY}\nserver stopped\n"
        ))
        .unwrap();
        assert!(ok.managed && ok.unmanaged_keys == 2 && ok.server_running == Some(false));
        for bad in [
            "",
            "managed nope\nunmanaged 0\n",
            "unmanaged x\n",
            "unmanaged 0\nsurprise\n",
            "host x\n",
        ] {
            assert!(parse_status(bad).is_err(), "{bad:?}");
        }
    }
}
