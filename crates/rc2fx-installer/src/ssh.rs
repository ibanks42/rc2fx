//! The SSH key built into prepared firmware. The installer itself never
//! connects over SSH; the key lets `cargo xtask mixer` reach the mixer.

use crate::{
    Error, Result, bail,
    files::{self, exists},
    runtime::Entry,
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

/// The mixer's SSH host key, kept in the cache folder so every flash with SSH
/// access gives the mixer the same identity (stock firmware has no host keys
/// and generates new ones on its first boot). Returns the image entries and
/// the public line.
pub(crate) fn host_key(work: &Path) -> Result<(Vec<Entry>, Vec<u8>)> {
    let (private, public) = ensure_key(&work.join("ssh_host_ed25519_key").to_string_lossy())?;
    let entries = vec![
        Entry::File {
            path: "etc/ssh/ssh_host_ed25519_key".into(),
            mode: 0o600,
            data: std::fs::read(&private)?,
        },
        Entry::File {
            path: "etc/ssh/ssh_host_ed25519_key.pub".into(),
            mode: 0o644,
            data: public.clone(),
        },
    ];
    Ok((entries, public))
}

/// Trust `public` as `host` in the cache folder's `known_hosts`, which the
/// mixer tooling reads, replacing any earlier key for that host.
pub(crate) fn remember_host(work: &Path, host: &str, public: &[u8]) -> Result<()> {
    let host = host.trim();
    if host.is_empty() || host.contains(char::is_whitespace) {
        return Ok(());
    }
    let path = work.join("known_hosts");
    let old = std::fs::read_to_string(&path).unwrap_or_default();
    let mut text: String = old
        .lines()
        .filter(|l| l.split_whitespace().next() != Some(host))
        .map(|l| format!("{l}\n"))
        .collect();
    text.push_str(host);
    text.push(' ');
    text.push_str(String::from_utf8_lossy(public).trim());
    text.push('\n');
    files::atomic_file(&path, text.as_bytes())
}
