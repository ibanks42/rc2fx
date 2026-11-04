//! Put the mixer build on a RODECaster Pro II over SSH, or take it off again.
//! Uses the system `ssh` (bundled with Windows 10+, macOS and Linux). Remote
//! scripts go over stdin, so no quoting differs between host shells.
use crate::{Result, root, sha256};
use std::{
    fs,
    io::{BufRead, Read, Write},
    process::{Command, Stdio},
};

/// Runs on the mixer: the supervisor, and the launcher shim that starts it.
const RC2FX_SH: &str = include_str!("../mixer/rc2fx.sh");
const RC2FX_SHIM: &str = include_str!("../mixer/rc2fx-shim.sh");
/// The same for RØDE's display loop: `rc_gui_juce` by name, /usr/sbin first.
const RC2FX_GUI_SHIM: &str = include_str!("../mixer/rc2fx-gui-shim.sh");

/// Picks whichever copy of rc2fx.sh is on the mixer, as the launcher shim
/// does (a build in RAM wins); every copy shares the runtime state in
/// /run/rc2fx.
fn on_mixer(action: &str) -> String {
    format!(
        "for s in /run/rc2fx-dev/rc2fx.sh /opt/rc2fx/rc2fx.sh; do\n  \
         if [ -f $s ]; then exec /bin/bash $s {action}; fi\ndone\n\
         echo 'RC2FX is not on this mixer' >&2\nexit 1\n"
    )
}

/// A build run from RAM needs the launcher shim too. Without a bundle the
/// shim only execs the stock mixer; `mixer-uninstall` removes it.
const RUN: &str = "set -e
/run/rc2fx-upload/rc2fx-firmware check-device /run/rc2fx-upload/firmware-profiles.json /usr/bin/rc_audio_mixer /usr/bin/audio-start.sh > /dev/null
if ! test -x /usr/sbin/rc_audio_mixer; then
  cp /run/rc2fx-upload/rc2fx-shim.sh /usr/sbin/rc_audio_mixer
  chmod 755 /usr/sbin/rc_audio_mixer
  echo 'Added the launcher shim /usr/sbin/rc_audio_mixer; it does nothing without a bundle, and mixer-uninstall removes it'
fi
if ! test -x /usr/sbin/rc_gui_juce; then
  cp /run/rc2fx-upload/rc2fx-gui-shim.sh /usr/sbin/rc_gui_juce
  chmod 755 /usr/sbin/rc_gui_juce
  echo 'Added the display shim /usr/sbin/rc_gui_juce; it runs the stock GUI without a bundle, and mixer-uninstall removes it; it takes effect at the next boot'
fi
rm -rf /run/rc2fx-dev
mv /run/rc2fx-upload /run/rc2fx-dev
status=0
/bin/bash /run/rc2fx-dev/rc2fx.sh restart --wait 240 || status=$?
if test \"$status\" -eq 3; then echo 'After the power cycle, run this again: a build in RAM does not survive it'; fi
exit \"$status\"
";

const INSTALL: &str = include_str!("../mixer/install.sh");
/// After an install: commit once RC2FX is processing audio (its controller
/// runs and the mixer's callbacks advance in the native report), otherwise
/// roll back to the previous runtime.
const COMMIT: &str = r#"set -e
test -f /opt/.rc2fx-install-pending || { echo 'Install already committed'; exit 0; }
callbacks() { grep -o '"callbacks":[0-9]*' /run/rc2fx/audit.json 2>/dev/null | head -n 1 | tr -dc 0-9; }
for n in $(seq 1 60); do
 first=$(callbacks); sleep 2; second=$(callbacks)
 if test -n "$first" && test -n "$second" && test "$second" -gt "$first" && pgrep -x rc2fxd > /dev/null; then
  /bin/sh /opt/rc2fx/install.sh commit
  echo 'RC2FX is processing audio; install committed'
  exit 0
 fi
done
echo 'RC2FX did not start processing audio; rolling back' >&2
/bin/sh /opt/rc2fx/install.sh rollback
exit 1
"#;

const UNINSTALL: &str = r#"set -e
for s in /run/rc2fx-dev/rc2fx.sh /opt/rc2fx/rc2fx.sh; do
  if [ -f $s ]; then /bin/bash $s stop; break; fi
done
rm -f /usr/sbin/rc_audio_mixer /usr/sbin/rc_gui_juce /etc/rc5.d/S99rc2fx /etc/init.d/rc2fx
rm -rf /run/rc2fx-dev /run/rc2fx /run/rc2fx-boot
if [ "$PURGE" = 1 ]; then
  rm -rf /opt/rc2fx
  echo "RC2FX and its settings removed"
else
  for f in /opt/rc2fx/* /opt/rc2fx/.[!.]*; do
    [ -e "$f" ] || continue
    case "${f##*/}" in data) ;; *) rm -rf "$f" ;; esac
  done
  echo "RC2FX removed; settings kept in /opt/rc2fx/data (uninstall --purge deletes them)"
fi
sync
"#;

// Every flag change ends in `sync`: the mixer is power-cycled through an
// outlet, and a recent ext4 write that was never flushed can be lost
// (measured on the device 2026-11-01).
const ENABLE: &str = r#"test -x /usr/sbin/rc_audio_mixer || { echo "RC2FX is not installed"; exit 1; }
rm -f /opt/rc2fx/disabled
sync
echo "RC2FX will start at boot"
"#;

const DISABLE: &str = r#"test -x /usr/sbin/rc_audio_mixer || { echo "RC2FX is not installed"; exit 1; }
touch /opt/rc2fx/disabled
sync
echo "RC2FX will not start at boot (stop it now with: just mixer-stop)"
"#;

pub fn run(action: &str, rest: &[&str]) -> Result {
    if action == "api-session" && rest.is_empty() {
        return api_session();
    }
    let mixer = Mixer::load()?;
    match (action, rest) {
        ("run", []) => {
            mixer.upload()?;
            mixer.script(RUN)
        }
        ("install", []) => {
            mixer.upload()?;
            mixer.script(INSTALL)?;
            mixer.script(COMMIT)
        }
        ("uninstall", []) => mixer.script(UNINSTALL),
        ("uninstall", ["--purge"]) => mixer.script(&format!("PURGE=1\n{UNINSTALL}")),
        ("stop" | "status" | "logs", []) => mixer.script(&on_mixer(action)),
        ("api", [method, path]) => api(method, path, None),
        ("api", [method, path, body]) => api(method, path, Some(body)),
        ("enable", []) => mixer.script(ENABLE),
        ("xruns", args) => crate::xruns::run(&mixer, args),
        ("disable", []) => mixer.script(DISABLE),
        _ => Err(format!(
            "unknown mixer action `{action}`; see `cargo xtask help`"
        )),
    }
}

pub struct Mixer {
    host: String,
    options: Vec<String>,
}

impl Mixer {
    /// Settings from the environment win over .rc2fx-mixer.conf.
    pub fn load() -> Result<Self> {
        let file = fs::read_to_string(root().join(".rc2fx-mixer.conf")).unwrap_or_default();
        let setting = |key: &str| {
            std::env::var(format!("RC2FX_MIXER_{}", key.to_uppercase()))
                .ok()
                .or_else(|| {
                    file.lines()
                        .filter_map(|line| line.split_once('='))
                        .find(|(name, _)| name.trim() == key)
                        .map(|(_, value)| value.trim().trim_matches('"').to_owned())
                })
                .filter(|value| !value.is_empty())
        };
        let ip = setting("ip").ok_or(
            "no mixer address: copy .rc2fx-mixer.conf.example to .rc2fx-mixer.conf and set ip",
        )?;
        let user = setting("user").unwrap_or_else(|| "root".into());
        let mut options: Vec<String> = ["-o", "BatchMode=yes", "-o", "ConnectTimeout=10"]
            .map(String::from)
            .into();
        if let Some(key) = setting("key") {
            options.extend(["-i".into(), key, "-o".into(), "IdentitiesOnly=yes".into()]);
        }
        if let Some(known) = setting("known_hosts") {
            options.extend(["-o".into(), format!("UserKnownHostsFile={known}")]);
        }
        // The app pins the mixer's host key under `rc2fx-<id>`, not its
        // address, so the key stays trusted when the address changes.
        if let Some(alias) = setting("host_key_alias") {
            if !alias
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            {
                return Err(format!("host_key_alias {alias:?} is not an RC2FX alias"));
            }
            options.extend(["-o".into(), format!("HostKeyAlias={alias}")]);
        }
        Ok(Self {
            host: format!("{user}@{ip}"),
            options,
        })
    }

    /// Run `remote` on the mixer with `input` on its stdin.
    pub fn ssh(&self, remote: &str, input: &[u8]) -> Result {
        let mut child = Command::new("ssh")
            .args(&self.options)
            .arg(&self.host)
            .arg(remote)
            .stdin(Stdio::piped())
            .spawn()
            .map_err(|e| format!("could not start ssh: {e}"))?;
        let written = child.stdin.take().unwrap().write_all(input);
        let status = child.wait().map_err(|e| e.to_string())?;
        if !status.success() {
            return Err(format!("command on the mixer failed ({status})"));
        }
        written.map_err(|e| format!("sending to the mixer: {e}"))
    }

    /// Run `remote` on the mixer and return what it writes to stdout.
    pub fn capture(&self, remote: &str) -> Result<Vec<u8>> {
        let output = Command::new("ssh")
            .args(&self.options)
            .arg(&self.host)
            .arg(remote)
            .stdin(Stdio::null())
            .stderr(Stdio::inherit())
            .output()
            .map_err(|e| format!("could not start ssh: {e}"))?;
        if !output.status.success() {
            return Err(format!("command on the mixer failed ({})", output.status));
        }
        Ok(output.stdout)
    }

    /// Run `script` with the mixer's shell and return what it writes to
    /// stdout; its stderr is passed through.
    pub fn capture_script(&self, script: &str) -> Result<Vec<u8>> {
        let mut child = Command::new("ssh")
            .args(&self.options)
            .arg(&self.host)
            .arg("/bin/sh -s")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| format!("could not start ssh: {e}"))?;
        let written = child.stdin.take().unwrap().write_all(script.as_bytes());
        let output = child.wait_with_output().map_err(|e| e.to_string())?;
        if !output.status.success() {
            return Err(format!("command on the mixer failed ({})", output.status));
        }
        written.map_err(|e| format!("sending to the mixer: {e}"))?;
        Ok(output.stdout)
    }

    fn script(&self, script: &str) -> Result {
        self.ssh("/bin/sh -s", script.as_bytes())
    }

    /// Stage the build in RAM at /run/rc2fx-upload, verified on arrival.
    fn upload(&self) -> Result {
        let build = root().join("target/mixer");
        let read = |name: &str| {
            fs::read(build.join(name))
                .map_err(|_| format!("target/mixer/{name} is missing; run `just mixer-build`"))
        };
        let library = read("librc2fx_native.so")?;
        let controller = read("rc2fxd")?;
        let profile_checker = read("rc2fx-firmware")?;
        let gui_module = read(crate::setup::GUI_MODULE)?;
        let recorder = read(crate::setup::JACKERR_MODULE)?;
        let recorder_snapshot = read(crate::setup::JACKERR_SNAPSHOT)?;
        let profiles = read("firmware-profiles.json")?;
        rc2fx_firmware::Catalog::parse(std::str::from_utf8(&profiles).map_err(|e| e.to_string())?)?;
        let release = read("mixer-release.json")?;
        rc2fx_installer::MixerRelease::parse(&release).map_err(|e| e.0)?;
        let source = fs::read_to_string(root().join("config/mixer-release.json"))
            .map_err(|e| format!("config/mixer-release.json: {e}"))?
            .replace("\r\n", "\n");
        if release != source.as_bytes() {
            return Err(
                "target/mixer/mixer-release.json differs from config/mixer-release.json; \
                 run `just mixer-build`"
                    .into(),
            );
        }
        let sums = read("SHA256SUMS")?;
        let expected = format!(
            "{}  librc2fx_native.so\n{}  rc2fxd\n{}  rc2fx-firmware\n{}  firmware-profiles.json\n\
             {}  {}\n{}  {}\n{}  {}\n{}  mixer-release.json\n",
            sha256(&library),
            sha256(&controller),
            sha256(&profile_checker),
            sha256(&profiles),
            sha256(&gui_module),
            crate::setup::GUI_MODULE,
            sha256(&recorder),
            crate::setup::JACKERR_MODULE,
            sha256(&recorder_snapshot),
            crate::setup::JACKERR_SNAPSHOT,
            sha256(&release)
        );
        if sums != expected.as_bytes() {
            return Err("target/mixer does not match its SHA256SUMS; rebuild it".into());
        }
        let unix = |text: &str| text.replace("\r\n", "\n").into_bytes();
        let mut archive = tar::Builder::new(Vec::new());
        for (name, bytes, mode) in [
            ("librc2fx_native.so", library, 0o600),
            ("rc2fxd", controller, 0o700),
            ("rc2fx-firmware", profile_checker, 0o700),
            ("firmware-profiles.json", profiles, 0o600),
            (crate::setup::GUI_MODULE, gui_module, 0o600),
            (crate::setup::JACKERR_MODULE, recorder, 0o600),
            (crate::setup::JACKERR_SNAPSHOT, recorder_snapshot, 0o700),
            ("mixer-release.json", release, 0o600),
            ("SHA256SUMS", sums, 0o600),
            ("rc2fx.sh", unix(RC2FX_SH), 0o700),
            ("rc2fx-shim.sh", unix(RC2FX_SHIM), 0o755),
            ("rc2fx-gui-shim.sh", unix(RC2FX_GUI_SHIM), 0o755),
            ("install.sh", unix(INSTALL), 0o700),
        ] {
            let mut header = tar::Header::new_ustar();
            header.set_size(bytes.len() as u64);
            header.set_mode(mode);
            header.set_mtime(0);
            header.set_cksum();
            archive
                .append_data(&mut header, name, bytes.as_slice())
                .map_err(|e| e.to_string())?;
        }
        let archive = archive.into_inner().map_err(|e| e.to_string())?;
        println!("Uploading to {}", self.host);
        self.ssh(
            "set -e; rm -rf /run/rc2fx-upload; mkdir -m 700 /run/rc2fx-upload; \
             tar -xf - -C /run/rc2fx-upload; cd /run/rc2fx-upload; sha256sum -c SHA256SUMS > /dev/null",
            &archive,
        )
    }
}

/// One controller API request over USB 1 (docs/reference/api.md), the body JSON text or
/// `@file`; prints the status and the reply body.
fn api(method: &str, path: &str, body: Option<&str>) -> Result {
    use rc2fx_installer::control::{Client, Request};
    let body = match body {
        None => None,
        Some(text) => {
            let text = match text.strip_prefix('@') {
                Some(file) => fs::read_to_string(file).map_err(|e| format!("{file}: {e}"))?,
                None => text.to_string(),
            };
            Some(serde_json::from_str(&text).map_err(|e| format!("body: {e}"))?)
        }
    };
    let request = Request {
        method: method.to_uppercase(),
        path: path.trim_start_matches('/').to_string(),
        body,
    };
    let reply = Client::open()?
        .request(&request)
        .map_err(|e| e.message().to_string())?;
    println!("{}", reply.status);
    println!(
        "{}",
        serde_json::to_string_pretty(&reply.body).map_err(|e| e.to_string())?
    );
    if (200..300).contains(&reply.status) {
        Ok(())
    } else {
        Err(format!("the controller answered {}", reply.status))
    }
}

/// JSON-lines requests/replies over ONE USB client/nonce. Separate one-shot
/// invocations otherwise contend with their own previous five-second lease.
fn api_session() -> Result {
    let mut client = rc2fx_installer::control::Client::open()?;
    api_stream(
        std::io::stdin().lock(),
        std::io::stdout().lock(),
        |request| {
            client
                .request(&request)
                .map_err(|error| error.message().to_string())
        },
    )
}

fn api_stream(
    mut input: impl BufRead,
    mut output: impl Write,
    mut request: impl FnMut(
        rc2fx_installer::control::Request,
    ) -> std::result::Result<rc2fx_installer::control::Reply, String>,
) -> Result {
    const LIMIT: u64 = 1024 * 1024;
    let mut line = Vec::new();
    loop {
        line.clear();
        let n = (&mut input)
            .take(LIMIT + 1)
            .read_until(b'\n', &mut line)
            .map_err(|e| e.to_string())?;
        if n == 0 {
            return Ok(());
        }
        if n as u64 > LIMIT {
            return Err("USB request line exceeds 1 MiB".into());
        }
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let reply = request(
            serde_json::from_slice(&line).map_err(|e| format!("Invalid request JSON: {e}"))?,
        )?;
        serde_json::to_writer(&mut output, &reply).map_err(|e| e.to_string())?;
        writeln!(output).map_err(|e| e.to_string())?;
        output.flush().map_err(|e| e.to_string())?;
    }
}

#[cfg(test)]
mod tests {
    use super::RC2FX_SH;

    #[test]
    fn every_flag_change_is_synced_for_a_power_cut() {
        for (name, script) in [("enable", super::ENABLE), ("disable", super::DISABLE)] {
            assert!(
                script.contains("\nsync\n"),
                "{name} must sync its flag change"
            );
        }
    }

    #[test]
    fn missing_launcher_is_not_reported_as_success() {
        let script = super::on_mixer("status");
        assert!(script.contains("/run/rc2fx-dev/rc2fx.sh /opt/rc2fx/rc2fx.sh"));
        assert!(script.ends_with("exit 1\n"));
    }

    #[test]
    fn api_session_preserves_one_requester_and_flushes_each_reply() {
        #[derive(Default)]
        struct Output {
            bytes: Vec<u8>,
            flushes: usize,
        }
        impl std::io::Write for Output {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.bytes.extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                self.flushes += 1;
                Ok(())
            }
        }
        let mut output = Output::default();
        let mut calls = 0;
        super::api_stream(&b"{\"method\":\"GET\",\"path\":\"api/v1/state\"}\n{\"method\":\"GET\",\"path\":\"api/v1/state\"}\n"[..], &mut output, |request| {
            calls += 1;
            assert_eq!(request.path, "api/v1/state");
            Ok(rc2fx_installer::control::Reply { status: 200, body: serde_json::json!({"call": calls}) })
        }).unwrap();
        assert_eq!(calls, 2);
        assert_eq!(output.flushes, 2);
        assert_eq!(String::from_utf8(output.bytes).unwrap().lines().count(), 2);
    }

    #[test]
    fn api_session_does_not_replay_transport_failures() {
        let mut calls = 0;
        let result = super::api_stream(
            &b"{\"method\":\"PUT\",\"path\":\"api/v1/config\"}\n"[..],
            Vec::new(),
            |_| {
                calls += 1;
                Err("outcome unknown".into())
            },
        );
        assert_eq!(result, Err("outcome unknown".into()));
        assert_eq!(calls, 1);
    }

    /// RØDE's Pro II 1.7.6/1.7.7 HID report descriptor.
    const STOCK: [u8; 46] = [
        0x06, 0x00, 0xff, 0x09, 0x01, 0xa1, 0x01, 0x75, 0x08, 0x15, 0x00, 0x25, 0xff, 0x09, 0x02,
        0x85, 0x01, 0x95, 0x3f, 0x91, 0x02, 0x09, 0x03, 0x85, 0x02, 0x95, 0x3f, 0x81, 0x02, 0x09,
        0x04, 0x85, 0x03, 0x95, 0xff, 0x91, 0x02, 0x09, 0x05, 0x85, 0x04, 0x95, 0xff, 0x81, 0x02,
        0xc0,
    ];

    fn prepare_usb() -> &'static str {
        let start = RC2FX_SH
            .find("prepare_usb() {")
            .expect("prepare_usb in rc2fx.sh");
        let body = &RC2FX_SH[start..];
        &body[..body.find("\n}\n").expect("end of prepare_usb")]
    }

    /// The descriptor `prepare_usb` stages from the stock one: its `head -c`
    /// prefix followed by its `printf` bytes.
    fn staged() -> Vec<u8> {
        let body = prepare_usb();
        let head: usize = body
            .split("head -c ")
            .nth(1)
            .expect("head -c")
            .split_whitespace()
            .next()
            .unwrap()
            .parse()
            .unwrap();
        let printf = body
            .lines()
            .find(|l| l.trim_start().starts_with(r"printf '\x"))
            .expect("descriptor printf");
        let escapes = printf.split('\'').nth(1).unwrap();
        let mut bytes = STOCK[..head].to_vec();
        bytes.extend(
            escapes
                .split(r"\x")
                .skip(1)
                .map(|h| u8::from_str_radix(h, 16).unwrap()),
        );
        bytes
    }

    /// Short-item prefixes, in order.
    fn items(descriptor: &[u8]) -> Vec<u8> {
        let (mut i, mut prefixes) = (0, Vec::new());
        while i < descriptor.len() {
            let prefix = descriptor[i];
            prefixes.push(prefix);
            i += 1 + [0, 1, 2, 4][usize::from(prefix & 3)];
        }
        assert_eq!(i, descriptor.len(), "descriptor ends mid-item");
        prefixes
    }

    #[test]
    fn staged_descriptor_keeps_rodes_items_in_one_collection() {
        let staged = staged();
        assert_eq!(staged.len(), 62);
        assert_eq!(staged[..STOCK.len() - 1], STOCK[..STOCK.len() - 1]);
        let items = items(&staged);
        // One application collection, closed once at the very end: Windows
        // lists RØDE's HID interface as a single device.
        assert_eq!(items.iter().filter(|p| **p & 0xfc == 0xa0).count(), 1);
        assert_eq!(items.iter().filter(|p| **p & 0xfc == 0xc0).count(), 1);
        assert_eq!(items.last(), Some(&0xc0));
        assert!(rc2fx_usb::declares_control(&staged));
        assert!(!rc2fx_usb::declares_control(&STOCK));
    }

    #[test]
    fn staging_only_accepts_the_stock_descriptor_it_was_built_from() {
        let body = prepare_usb();
        assert!(body.contains("sha256sum -c"));
        assert!(body.contains(&crate::sha256(&STOCK)));
        assert!(body.contains(r#"> "$run/usb-report.bin""#));
    }
}
