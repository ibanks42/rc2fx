//! Arm, check, then run, against a stand-in for ssh and the mixer: every
//! command that changes or runs something on the mixer arms power recovery
//! and verifies it before it sends anything else, and sends nothing else when
//! that fails. `Err` is what `main` turns into a non-zero exit.
use super::*;

/// Logs each remote command and each use of the flag to `events`. It runs the
/// rc2fx.sh it is sent, for arm-power-recovery only, against the file `flag`,
/// and nothing else it is given. Files beside it make it fail.
const FAKE_SSH: &str = r#"fake=$(dirname "$0"); remote=${!#}
echo "ssh $remote" >> "$fake/events"
test ! -e "$fake/unreachable" || { echo 'ssh: connection timed out' >&2; exit 255; }
stubs="fw_printenv() { echo \"fw_printenv \$*\" >> '$fake/events'; cat '$fake/flag'; }
fw_setenv() { echo \"fw_setenv \$*\" >> '$fake/events'; test ! -e '$fake/setenv-fails' && echo \"\$2\" > '$fake/flag'; }"
case "$remote" in
  '/bin/bash -s -- arm-power-recovery')
    { echo "$stubs"; sed "s|/usr/bin/first-boot|$fake/first-boot|"; } | bash -s -- arm-power-recovery ;;
  'fw_printenv -n reboot_pwr_button_bypass') cat "$fake/readback" 2>/dev/null || cat "$fake/flag" ;;
  *) cat > /dev/null ;;
esac
"#;

/// A successful arming from a flag of 0: the script reads, writes and
/// verifies, then the host reads the flag back.
const ARMED: [&str; 5] = [
    "ssh /bin/bash -s -- arm-power-recovery",
    "fw_printenv -n reboot_pwr_button_bypass",
    "fw_setenv reboot_pwr_button_bypass 1",
    "fw_printenv -n reboot_pwr_button_bypass",
    "ssh fw_printenv -n reboot_pwr_button_bypass",
];

/// Every arming command, and the first thing it sends once armed.
const COMMANDS: [(&[&str], &str); 9] = [
    (&["mixer", "run"], "ssh set -e; rm -rf /run/rc2fx-upload;"),
    (
        &["mixer", "install"],
        "ssh set -e; rm -rf /run/rc2fx-upload;",
    ),
    (&["mixer", "uninstall"], "ssh /bin/sh -s"),
    (&["mixer", "uninstall", "--purge"], "ssh /bin/sh -s"),
    (&["mixer", "stop"], "ssh /bin/sh -s"),
    (&["mixer", "enable"], "ssh /bin/sh -s"),
    (&["mixer", "disable"], "ssh /bin/sh -s"),
    (&["mixer", "xruns", "0"], "ssh /bin/sh -s"),
    (&["bench"], "ssh set -e; rm -rf /run/rc2fx-bench;"),
];

/// A stand-in mixer with its flag at 0, in a directory that also serves as
/// target/mixer and target/bench.
struct Fake(tempfile::TempDir);

impl Fake {
    /// `marker` is a file the stand-in looks for, holding `content`.
    fn new(marker: &str, content: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let write = |name: &str, bytes: &[u8]| fs::write(dir.path().join(name), bytes).unwrap();
        let mut sums = String::new();
        for name in [
            "librc2fx_native.so",
            "rc2fxd",
            "rc2fx-supervisor",
            "rc2fx-firmware",
            "firmware-profiles.json",
            crate::setup::GUI_MODULE,
            crate::setup::JACKERR_MODULE,
            crate::setup::JACKERR_SNAPSHOT,
            "mixer-release.json",
        ] {
            let bytes = match name {
                "firmware-profiles.json" => rc2fx_firmware::DEFAULT_CATALOG.into(),
                "mixer-release.json" => {
                    fs::read_to_string(root().join("config/mixer-release.json")).unwrap()
                }
                _ => name.to_string(),
            };
            sums += &format!("{}  {name}\n", sha256(bytes.as_bytes()));
            write(name, bytes.as_bytes());
        }
        write("SHA256SUMS", sums.as_bytes());
        write("rc2fx-bench", b"placeholder");
        // An empty 48 kHz recording.
        write(
            "speech.wav",
            b"RIFF\0\0\0\0WAVEfmt \x10\0\0\0\x01\0\x01\0\x80\xbb\0\0\0\x77\x01\0\x02\0\x10\0data\0\0\0\0",
        );
        write("ssh", FAKE_SSH.as_bytes());
        write("flag", b"0\n");
        write(marker, content.as_bytes());
        Self(dir)
    }

    /// Run a command as `cargo xtask` would. The stand-in goes through bash,
    /// never as a program just written while another test thread forks.
    fn run(&self, command: &[&str]) -> Result {
        let dir = self.0.path();
        let mixer = Mixer {
            host: "root@fake".into(),
            options: vec![dir.join("ssh").to_str().unwrap().into()],
            program: "bash",
        };
        match command {
            ["bench"] => {
                let recording = dir.join("speech.wav");
                crate::bench::run_in(dir, || Ok(mixer), recording.to_str().unwrap(), &[])
            }
            _ => act(&mixer, dir, command[1], &command[2..]),
        }
    }

    fn events(&self) -> Vec<String> {
        let events = fs::read_to_string(self.0.path().join("events")).unwrap();
        events.lines().map(String::from).collect()
    }
}

#[test]
fn every_arming_command_arms_and_verifies_before_it_sends_anything_else() {
    for (command, first) in COMMANDS {
        let fake = Fake::new("no-marker", "");
        // xruns and bench fail afterwards: the stand-in returns no results.
        let _ = fake.run(command);
        let events = fake.events();
        assert!(
            events.len() > 5 && events[..5] == ARMED && events[5].starts_with(first),
            "{command:?}: {events:?}"
        );
    }
}

#[test]
fn when_arming_fails_the_command_is_refused_and_nothing_else_is_sent() {
    // What fails, the reason given, and how much of the arming happened.
    for (marker, content, reason, sent) in [
        ("setenv-fails", "", "(exit status: 1)", 3),
        ("readback", "0\n", "reads back as \"0\", not 1", 5),
        ("unreachable", "", "(exit status: 255) ssh: connection", 1),
        (
            "first-boot",
            "",
            "(exit status: 4) Refusing recovery arming",
            1,
        ),
    ] {
        for (command, _) in COMMANDS {
            let fake = Fake::new(marker, content);
            let error = fake.run(command).unwrap_err();
            let name = command[..command.len().min(2)].join(" ");
            let refusal = format!("`{name}` was not started: power recovery could not be armed (");
            assert!(
                error.starts_with(&refusal)
                    && error.contains(reason)
                    && error.ends_with("would stay off until its button is pressed."),
                "{marker}: {error}"
            );
            assert_eq!(fake.events(), ARMED[..sent], "{name}, {marker}");
        }
    }
}

#[test]
fn status_and_logs_only_read_and_do_not_arm() {
    for action in ["status", "logs"] {
        let fake = Fake::new("no-marker", "");
        fake.run(&["mixer", action]).unwrap();
        assert_eq!(fake.events(), ["ssh /bin/sh -s"], "{action}");
    }
}
