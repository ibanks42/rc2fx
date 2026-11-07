//! The developer SSH key and pinned host keys. Neither goes into firmware: the
//! app adds the public key over USB 1. The installer itself never connects over
//! SSH; the key lets `cargo xtask mixer` reach the mixer.

use crate::{
    Error, Result, bail,
    files::{self, exists},
};
use ssh_key::{
    LineEnding, PrivateKey, PublicKey,
    private::{Ed25519Keypair, KeypairData},
};
use std::path::{Path, PathBuf};

/// The canonical `authorized_keys` line for a key: algorithm and base64, no
/// comment. This is the exact text written into the patched firmware.
fn authorized_line(key: &PublicKey) -> Result<Vec<u8>> {
    let bare = PublicKey::new(key.key_data().clone(), "");
    Ok(format!("{}\n", bare.to_openssh().map_err(|e| e.to_string())?).into_bytes())
}

/// Parse text holding exactly one plain public key line (no options).
fn single_key(text: &[u8]) -> Option<PublicKey> {
    if text.len() > 20000 {
        return None;
    }
    let text = std::str::from_utf8(text).ok()?;
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
    let line = lines.next()?;
    lines.next().is_none().then_some(())?;
    PublicKey::from_openssh(line).ok()
}

fn expand_home(value: &str) -> Result<PathBuf> {
    if let Some(rest) = value.strip_prefix("~/") {
        let home = files::home_dir().ok_or("cannot find the home directory")?;
        return Ok(home.join(rest));
    }
    Ok(PathBuf::from(value))
}

/// Return the private key path and its canonical public line, creating an
/// Ed25519 pair when neither half exists. Existing keys are only validated.
pub(crate) fn ensure_key(value: &str) -> Result<(PathBuf, Vec<u8>)> {
    if value.is_empty() {
        bail!("missing SSH key path");
    }
    let private = std::path::absolute(expand_home(value)?)?;
    let public = PathBuf::from(format!("{}.pub", private.display()));
    if exists(&private) != exists(&public) {
        bail!("only one half of the SSH key pair exists; preserve it and inspect the key path");
    }
    if !exists(&private) {
        files::private_dirs(private.parent().unwrap_or(Path::new(".")))?;
        let mut seed = [0u8; 32];
        getrandom::fill(&mut seed).map_err(|e| Error(e.to_string()))?;
        let key = PrivateKey::new(
            KeypairData::Ed25519(Ed25519Keypair::from_seed(&seed)),
            "rc2fx-rodecaster-pro-ii",
        )
        .map_err(|e| e.to_string())?;
        let pem = key.to_openssh(LineEnding::LF).map_err(|e| e.to_string())?;
        // Exclusive creation prevents concurrent installers replacing a key.
        files::exclusive_file(&private, pem.as_bytes())?;
        files::exclusive_file(&public, &authorized_line(key.public_key())?)?;
    }
    let text = std::fs::read_to_string(&private)?;
    let signer = PrivateKey::from_openssh(&text)
        .map_err(|e| Error(format!("read SSH key (OpenSSH format required): {e}")))?;
    let parsed = single_key(&std::fs::read(&public)?).ok_or("expected one plain SSH public key")?;
    if parsed.key_data() != signer.public_key().key_data() {
        bail!("SSH public and private keys do not match");
    }
    Ok((private, authorized_line(&parsed)?))
}

/// Where the desktop app's developer SSH key lives.
pub fn ssh_key_path() -> Result<PathBuf> {
    let path = crate::default_key();
    if !path.is_absolute() {
        bail!("cannot find the home directory for the SSH key");
    }
    Ok(path)
}

/// The desktop app's developer SSH public key as one `authorized_keys` line
/// (`ssh-ed25519 BASE64`), creating the Ed25519 key pair at
/// [`ssh_key_path`] if neither half exists. The private key never leaves
/// this computer.
pub fn ssh_public_key() -> Result<String> {
    let (_, line) = ensure_key(&ssh_key_path()?.to_string_lossy())?;
    let line = String::from_utf8(line).map_err(|e| e.to_string())?;
    rc2fx_core::Ed25519PublicKey::parse(&line)
        .map(|key| key.as_str().to_owned())
        .map_err(|e| Error(e.0))
}

/// Where a mixer's SSH host keys are pinned, for
/// `ssh -o UserKnownHostsFile=<known_hosts> -o HostKeyAlias=<alias>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PinnedHost {
    /// `rc2fx-` and twelve hex digits from the mixer's serial: stable across
    /// network addresses.
    pub alias: String,
    pub known_hosts: PathBuf,
}

/// The `HostKeyAlias` for the mixer with this USB serial.
pub fn mixer_host_alias(serial: &str) -> Result<String> {
    if serial.trim().is_empty() {
        bail!("connect the mixer so its SSH identity can be matched to it");
    }
    Ok(format!("rc2fx-{}", &files::sum(serial.as_bytes())[..12]))
}

/// Pin `host_keys` (public host key lines the mixer reported over USB 1) for
/// the mixer `serial` in the installer cache's `known_hosts`. Only that
/// mixer's alias lines are replaced; every other line is kept.
pub fn pin_mixer_host_keys(serial: &str, host_keys: &[String]) -> Result<PinnedHost> {
    let work = crate::default_work().ok_or("cannot find the installer cache folder")?;
    pin_host_keys_in(&work, serial, host_keys)
}

/// At most this many host keys are read from one report.
const MAX_HOST_KEYS: usize = 16;

/// One writer of the cache's `known_hosts` at a time in this process, so
/// overlapping pins for different mixers cannot drop each other's lines.
static KNOWN_HOSTS: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub(crate) fn pin_host_keys_in(
    work: &Path,
    serial: &str,
    host_keys: &[String],
) -> Result<PinnedHost> {
    let alias = mixer_host_alias(serial)?;
    if host_keys.len() > MAX_HOST_KEYS {
        bail!("the mixer reported too many SSH host keys");
    }
    // Only Ed25519 host keys are pinned, each parsed in full. OpenSSH prefers
    // the key types known for a host, so the mixer's other types are not
    // needed; a report without a valid Ed25519 key changes nothing.
    let mut lines = Vec::new();
    for key in host_keys {
        if !key.starts_with("ssh-ed25519 ") {
            continue;
        }
        // The report carries `type key` only, so anything more is malformed.
        if key.split(' ').count() != 2 {
            bail!("the mixer reported a malformed SSH host key");
        }
        let key = rc2fx_core::Ed25519PublicKey::parse(key).map_err(|e| {
            Error(format!(
                "the mixer reported a malformed SSH host key: {}",
                e.0
            ))
        })?;
        lines.push(format!("{alias} {}\n", key.as_str()));
    }
    if lines.is_empty() {
        bail!("the mixer reported no Ed25519 SSH host key");
    }
    let _writer = KNOWN_HOSTS.lock().unwrap_or_else(|e| e.into_inner());
    files::private_dirs(work)?;
    let path = work.join("known_hosts");
    let old = match std::fs::read(&path) {
        Ok(data) => data,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e.into()),
    };
    // Every other line is kept byte for byte; only a missing final newline is
    // added, before the new lines.
    let mut text: Vec<u8> = Vec::with_capacity(old.len() + 256);
    for line in old.split_inclusive(|&b| b == b'\n') {
        let first = line
            .split(|b| b.is_ascii_whitespace())
            .find(|w| !w.is_empty());
        if first != Some(alias.as_bytes()) {
            text.extend_from_slice(line);
        }
    }
    if text.last().is_some_and(|&b| b != b'\n') {
        text.push(b'\n');
    }
    for line in lines {
        text.extend_from_slice(line.as_bytes());
    }
    files::atomic_file(&path, &text)?;
    Ok(PinnedHost {
        alias,
        known_hosts: path,
    })
}

#[cfg(test)]
mod developer_ssh_tests {
    use super::*;

    const HOST: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIEPzD7gW9u5+SC5iq8gzujWEOTBQO1CbVCLO8fAOGkip";
    const HOST2: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIG/gma/lTFUYuXY03/ZrYlH41SUBAkyyg1Z3s8EAqCRR";

    #[test]
    fn pinning_replaces_only_this_mixers_alias() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("installer");
        std::fs::create_dir_all(&work).unwrap();
        let other = "192.168.0.9 ssh-ed25519 AAAAOTHER\r\n# note\nlast-without-newline";
        std::fs::write(work.join("known_hosts"), other).unwrap();
        let first = pin_host_keys_in(&work, "SERIAL-1", &[HOST.to_owned()]).unwrap();
        assert!(first.alias.starts_with("rc2fx-") && first.alias.len() == 18);
        let second = pin_host_keys_in(&work, "SERIAL-2", &[HOST.to_owned()]).unwrap();
        assert_ne!(first.alias, second.alias);
        // Re-pinning SERIAL-1 replaces its line; SERIAL-2 and other lines stay
        // byte for byte, and other key types in the report are ignored.
        let ecdsa = "ecdsa-sha2-nistp256 AAAAE2VjZHNhLXNoYTItbmlzdHAyNTY=".to_owned();
        pin_host_keys_in(&work, "SERIAL-1", &[ecdsa, HOST2.to_owned()]).unwrap();
        let text = std::fs::read_to_string(work.join("known_hosts")).unwrap();
        assert!(
            text.starts_with("192.168.0.9 ssh-ed25519 AAAAOTHER\r\n# note\nlast-without-newline\n")
        );
        assert_eq!(text.matches(&first.alias).count(), 1);
        assert!(text.contains(&format!("{} {HOST2}\n", first.alias)));
        assert!(text.contains(&format!("{} {HOST}\n", second.alias)));
        assert!(!text.contains("ecdsa"));
    }

    #[test]
    fn concurrent_pins_keep_every_mixer() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().to_path_buf();
        let threads: Vec<_> = (0..8)
            .map(|i| {
                let work = work.clone();
                std::thread::spawn(move || {
                    pin_host_keys_in(&work, &format!("SERIAL-{i}"), &[HOST.to_owned()]).unwrap()
                })
            })
            .collect();
        let aliases: Vec<_> = threads
            .into_iter()
            .map(|t| t.join().unwrap().alias)
            .collect();
        let text = std::fs::read_to_string(work.join("known_hosts")).unwrap();
        for alias in aliases {
            assert_eq!(text.matches(&format!("{alias} ")).count(), 1, "{alias}");
        }
    }

    #[test]
    fn malformed_or_missing_host_keys_change_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().to_path_buf();
        for bad in [
            vec![],
            vec!["ssh-ed25519".to_owned()],
            vec![format!("{HOST} comment")],
            vec!["ssh-dss AAAA".to_owned()],
            vec!["ssh-ed25519 AAAA\nevil ssh-ed25519 AAAA".to_owned()],
            vec!["ssh-ed25519 AA$A".to_owned()],
            vec!["ecdsa-sha2-nistp256 AAAAE2VjZHNhLXNoYTItbmlzdHAyNTY=".to_owned()],
            vec!["ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIEPzD7gW9u5".to_owned()],
            vec![HOST.to_owned(); 17],
        ] {
            assert!(pin_host_keys_in(&work, "S", &bad).is_err(), "{bad:?}");
        }
        assert!(pin_host_keys_in(&work, "  ", &[HOST.into()]).is_err());
        assert!(!work.join("known_hosts").exists());
    }
}
