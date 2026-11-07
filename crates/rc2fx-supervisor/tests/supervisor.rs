//! Runs the real `rc2fx-supervisor` against real child processes, with a stub
//! `rc2fx.sh` and controller. No mixer, USB device or system file is touched.
#![cfg(target_os = "linux")]
use std::{
    fs,
    os::unix::{fs::PermissionsExt, net::UnixDatagram},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread::sleep,
    time::{Duration, Instant},
};

/// Records each call, then behaves like the real script: a restart ends the
/// mixer and controller unless `restart-fails` exists.
const SCRIPT: &str = r#"#!/bin/bash
root=ROOT
echo "$*" >> "$root/calls"
case "$1" in
  supervise-restart)
    test ! -e "$root/restart-fails" || exit 1
    kill -TERM "$3" ${5:+"$5"} 2>/dev/null ;;
  supervise-exit) test -z "${2:-}" || kill -TERM "$2" 2>/dev/null ;;
esac
exit 0
"#;
const CONTROLLER: &str = r#"#!/bin/bash
root=$(dirname "$0")
echo "$$ $*" > "$root/controller-started"
echo 'controller output'
test ! -e "$root/controller-exits" || exit "$(cat "$root/controller-exits")"
exec sleep 60
"#;

struct Rig {
    _dir: tempfile::TempDir,
    root: PathBuf,
    mixer: Child,
    supervisor: Option<Child>,
}

fn executable(path: &Path, text: &str) {
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn start_ticks(pid: u32) -> String {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    let rest = stat.rsplit_once(") ").unwrap().1;
    rest.split_whitespace().nth(19).unwrap().to_owned()
}

fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        sleep(Duration::from_millis(20));
    }
}

impl Rig {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_owned();
        fs::create_dir(root.join("run")).unwrap();
        let script = SCRIPT.replace("ROOT", &root.display().to_string());
        executable(&root.join("rc2fx.sh"), &script);
        executable(&root.join("controller"), CONTROLLER);
        let mixer = Command::new("sleep").arg("60").spawn().unwrap();
        Self {
            _dir: dir,
            root,
            mixer,
            supervisor: None,
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    fn start(&mut self, ready_seconds: u32, mixer_start: Option<&str>) {
        let script = self.path("rc2fx.sh");
        self.spawn(ready_seconds, mixer_start, &script);
    }

    fn start_with_script(&mut self, ready_seconds: u32, script: &Path) {
        self.spawn(ready_seconds, None, script);
    }

    fn spawn(&mut self, ready_seconds: u32, mixer_start: Option<&str>, script: &Path) {
        let start = mixer_start.map_or_else(|| start_ticks(self.mixer.id()), str::to_owned);
        let log = fs::File::create(self.path("rc2fx.log")).unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_rc2fx-supervisor"))
            .args([
                "--mixer",
                &self.mixer.id().to_string(),
                "--mixer-start",
                &start,
            ])
            .args(["--ready-seconds", &ready_seconds.to_string()])
            .arg("--script")
            .arg(script)
            .arg("--controller")
            .arg(self.path("controller"))
            .arg("--run")
            .arg(self.path("run"))
            .arg("--boot")
            .arg(self.path("boot"))
            .args(["--", "--data", "/x y", "--flag"])
            .stdin(Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .unwrap();
        fs::write(self.path("run/supervisor.pid"), format!("{}\n", child.id())).unwrap();
        self.supervisor = Some(child);
    }

    fn open_socket(&self) -> UnixDatagram {
        UnixDatagram::bind(self.path("run/control.sock")).unwrap()
    }

    fn wait_for_controller(&self) -> u32 {
        let file = self.path("controller-started");
        wait_for("the controller", || {
            fs::read_to_string(&file).is_ok_and(|text| text.ends_with('\n'))
        });
        let text = fs::read_to_string(file).unwrap();
        text.split_whitespace().next().unwrap().parse().unwrap()
    }

    fn finish(&mut self) {
        let child = self.supervisor.as_mut().unwrap();
        wait_for("the supervisor to exit", || {
            child.try_wait().unwrap().is_some()
        });
    }

    fn log(&self) -> String {
        fs::read_to_string(self.path("rc2fx.log")).unwrap()
    }

    fn calls(&self) -> Vec<String> {
        let text = fs::read_to_string(self.path("calls")).unwrap_or_default();
        text.lines().map(str::to_owned).collect()
    }

    fn stock(&self) -> Option<String> {
        fs::read_to_string(self.path("boot/stock")).ok()
    }

    fn restart_call(&self, keep: u32) -> String {
        let mixer = self.mixer.id();
        format!("supervise-restart {keep} {mixer} {}", start_ticks(mixer))
    }

    fn wait_until_supervising(&self) {
        wait_for("the supervisor to settle", || {
            self.log().contains("Supervising")
        });
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = self.mixer.kill();
        let _ = self.mixer.wait();
        if let Some(child) = &mut self.supervisor {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[test]
fn starts_the_controller_and_stops_on_request_without_asking_for_stock() {
    let mut rig = Rig::new();
    rig.start(60, None);
    let _socket = rig.open_socket();
    let controller = rig.wait_for_controller();
    assert_eq!(
        fs::read_to_string(rig.path("controller-started")).unwrap(),
        format!("{controller} --data /x y --flag\n")
    );
    wait_for("controller output", || {
        fs::read_to_string(rig.path("run/controller.log"))
            .is_ok_and(|text| text.contains("controller output"))
    });
    fs::write(rig.path("run/stop"), "").unwrap();
    rig.finish();
    assert_eq!(
        rig.calls(),
        [
            format!("{} {controller}", rig.restart_call(0)),
            "supervise-exit".to_owned()
        ]
    );
    assert_eq!(rig.stock(), None);
    assert!(!rig.path("run/supervisor.pid").exists());
    assert!(
        rig.log()
            .contains(&format!("Controller started (PID {controller})"))
    );
}

#[test]
fn a_kept_gadget_is_passed_on_for_a_restart_into_rc2fx() {
    let mut rig = Rig::new();
    fs::write(rig.path("run/keep-usb"), "").unwrap();
    rig.start(60, None);
    fs::write(rig.path("run/stop"), "").unwrap();
    rig.finish();
    assert_eq!(rig.calls()[0], rig.restart_call(1));
}

#[test]
fn a_controller_exit_restores_stock_and_never_keeps_the_gadget() {
    let mut rig = Rig::new();
    fs::write(rig.path("run/keep-usb"), "").unwrap();
    fs::write(rig.path("controller-exits"), "3").unwrap();
    rig.start(60, None);
    let _socket = rig.open_socket();
    rig.finish();
    let reason = "Controller exited with status 3; restoring stock audio";
    assert_eq!(rig.stock().unwrap(), format!("{reason}\n"));
    assert!(rig.log().contains(reason));
    assert_eq!(
        rig.calls(),
        [rig.restart_call(0), "supervise-exit".to_owned()]
    );
}

#[test]
fn a_controller_that_cannot_start_restores_stock() {
    let mut rig = Rig::new();
    fs::remove_file(rig.path("controller")).unwrap();
    rig.start(60, None);
    let _socket = rig.open_socket();
    rig.finish();
    assert!(rig.stock().unwrap().contains("status 127"));
}

#[test]
fn a_mixer_that_never_opens_its_socket_restores_stock() {
    let mut rig = Rig::new();
    rig.start(1, None);
    rig.finish();
    assert_eq!(
        rig.stock().unwrap(),
        "Mixer not ready after 1 s; restoring stock audio\n"
    );
    assert!(rig.calls()[0].starts_with("supervise-restart 0"));
}

#[test]
fn the_controller_waits_for_the_next_second_after_the_socket_appears() {
    // The controller's two-second connect starts from here, so it must not
    // race a socket that has only just been bound.
    let mut rig = Rig::new();
    rig.start(60, None);
    rig.wait_until_supervising();
    sleep(Duration::from_millis(100));
    let _socket = rig.open_socket();
    sleep(Duration::from_millis(300));
    assert!(!rig.path("controller-started").exists());
    rig.wait_for_controller();
}

#[test]
fn a_mixer_exit_is_seen_at_once_and_two_early_exits_ask_for_stock() {
    let mut rig = Rig::new();
    fs::create_dir(rig.path("boot")).unwrap();
    rig.start(60, None);
    rig.wait_until_supervising();
    sleep(Duration::from_millis(100));
    let began = Instant::now();
    rig.mixer.kill().unwrap();
    rig.finish();
    assert!(
        began.elapsed() < Duration::from_millis(800),
        "{:?}",
        began.elapsed()
    );
    assert_eq!(rig.stock(), None);
    assert_eq!(rig.calls(), ["supervise-exit"]);

    let mut second = Rig::new();
    fs::create_dir(second.path("boot")).unwrap();
    fs::copy(rig.path("boot/exits"), second.path("boot/exits")).unwrap();
    second.start(60, None);
    second.wait_until_supervising();
    second.mixer.kill().unwrap();
    second.finish();
    assert_eq!(
        second.stock().unwrap(),
        "mixer exited early 2 times this boot\n"
    );
}

#[test]
fn a_reused_pid_is_not_taken_for_the_mixer_when_the_process_is_gone() {
    let mut rig = Rig::new();
    rig.mixer.kill().unwrap();
    rig.mixer.wait().unwrap();
    rig.start(60, Some("1"));
    rig.finish();
    assert!(rig.log().contains("Mixer exited after"));
    assert_eq!(rig.calls(), ["supervise-exit"]);
}

#[test]
fn a_live_process_on_a_reused_pid_is_left_alone() {
    let mut rig = Rig::new();
    rig.start(60, Some("1"));
    rig.finish();
    assert_eq!(rig.calls(), ["supervise-exit"]);
    assert!(rig.mixer.try_wait().unwrap().is_none());
}

#[test]
fn a_boot_directory_it_cannot_write_does_not_stop_it_watching() {
    let mut rig = Rig::new();
    fs::write(rig.path("boot"), "").unwrap();
    rig.start(1, None);
    rig.finish();
    assert!(rig.log().contains("Could not record the stock request"));
    assert_eq!(
        rig.calls(),
        [rig.restart_call(0), "supervise-exit".to_owned()]
    );
}

#[test]
fn the_exit_steps_still_run_once_the_run_directory_has_gone() {
    // The next launch deletes it after five seconds, whatever is still here.
    let mut rig = Rig::new();
    fs::rename(rig.path("rc2fx.sh"), rig.path("run/rc2fx.sh")).unwrap();
    rig.start_with_script(1, &rig.path("run/rc2fx.sh"));
    rig.wait_until_supervising();
    fs::remove_file(rig.path("run/rc2fx.sh")).unwrap();
    rig.finish();
    assert_eq!(
        rig.calls(),
        [rig.restart_call(0), "supervise-exit".to_owned()]
    );
}

#[test]
fn only_bash_is_handed_the_script() {
    let mut rig = Rig::new();
    executable(
        &rig.path("controller"),
        "#!/bin/bash\nls -l /proc/self/fd > \"$(dirname \"$0\")/controller-fds\"\nexec sleep 60\n",
    );
    rig.start(60, None);
    let _socket = rig.open_socket();
    wait_for("the controller", || rig.path("controller-fds").exists());
    wait_for("the listing", || {
        fs::read_to_string(rig.path("controller-fds")).is_ok_and(|text| !text.is_empty())
    });
    let fds = fs::read_to_string(rig.path("controller-fds")).unwrap();
    assert!(!fds.contains("rc2fx.sh"), "{fds}");
}

#[test]
fn a_signal_ends_the_mixer_into_stock_at_once() {
    let mut rig = Rig::new();
    rig.start(60, None);
    let _socket = rig.open_socket();
    let controller = rig.wait_for_controller();
    let pid = rig.supervisor.as_ref().unwrap().id() as i32;
    assert_eq!(unsafe { libc::kill(pid, libc::SIGTERM) }, 0);
    rig.finish();
    assert!(rig.stock().unwrap().contains("Supervisor signalled"));
    assert_eq!(
        rig.calls()[0],
        format!("{} {controller}", rig.restart_call(0))
    );
}

#[test]
fn a_failed_restart_leaves_the_mixer_and_hands_the_controller_to_the_exit_step() {
    let mut rig = Rig::new();
    fs::write(rig.path("restart-fails"), "").unwrap();
    rig.start(60, None);
    let _socket = rig.open_socket();
    let controller = rig.wait_for_controller();
    fs::write(rig.path("run/stop"), "").unwrap();
    rig.finish();
    assert!(
        rig.log()
            .contains("Leaving the mixer running without RC2FX control")
    );
    assert_eq!(rig.calls()[1], format!("supervise-exit {controller}"));
    assert!(rig.mixer.try_wait().unwrap().is_none());
}

#[test]
fn the_record_of_a_newer_supervisor_is_left_alone() {
    let mut rig = Rig::new();
    rig.start(60, None);
    fs::write(rig.path("run/supervisor.pid"), "1\n").unwrap();
    fs::write(rig.path("run/stop"), "").unwrap();
    rig.finish();
    assert_eq!(
        fs::read_to_string(rig.path("run/supervisor.pid")).unwrap(),
        "1\n"
    );
}

#[test]
fn it_never_answers_to_the_controllers_process_name() {
    // `pgrep -x rc2fxd` decides that an install worked: it must find only the
    // controller.
    let mut rig = Rig::new();
    rig.start(60, None);
    rig.wait_until_supervising();
    let pid = rig.supervisor.as_ref().unwrap().id();
    let name = fs::read_to_string(format!("/proc/{pid}/comm")).unwrap();
    assert_ne!(name.trim_end(), "rc2fxd");
}

#[test]
fn version_is_how_launch_proves_the_binary_runs() {
    let output = Command::new(env!("CARGO_BIN_EXE_rc2fx-supervisor"))
        .arg("--version")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).starts_with("rc2fx-supervisor "));
}
