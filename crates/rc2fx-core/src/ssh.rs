//! Developer SSH access managed over USB 1 without reflashing.
//!
//! RØDE's firmware runs OpenSSH with key-only root login and an empty
//! `authorized_keys`. RC2FX can add one key of its own to that file, marked as
//! RC2FX's, and remove it again; every other key is reported and left alone.
//! Removing the key stops new logins; it does not end sessions already open.

use serde::{Deserialize, Serialize};

/// The comment that marks the one `authorized_keys` line RC2FX manages.
pub const SSH_MANAGED_MARKER: &str = "rc2fx-managed-developer-key";
/// The longest public key line accepted, far above an Ed25519 key's length.
pub const MAX_SSH_PUBLIC_KEY: usize = 512;

/// What the mixer reports about SSH access. `managed`, `unmanagedKeys` and
/// `hostKeys` are required, so a malformed reply is an error rather than a
/// confident "off"; unknown extra fields are accepted.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SshStatus {
    /// The RC2FX-managed key is installed.
    pub managed: bool,
    /// `SHA256:…` of the managed key, when installed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub managed_fingerprint: Option<String>,
    /// Other `authorized_keys` lines, such as a key built into an earlier
    /// flash. RC2FX never changes them.
    pub unmanaged_keys: u32,
    /// The mixer's SSH server public host keys, for pinning.
    pub host_keys: Vec<String>,
    /// Whether the SSH server was seen running; `None` if that could not be
    /// determined.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_running: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// `PUT api/v1/ssh`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SshRequest {
    pub enabled: bool,
    /// Required when enabling: one plain `ssh-ed25519` public key line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_key: Option<String>,
}

/// An Ed25519 public key in its canonical `ssh-ed25519 BASE64` form, without
/// options or comment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ed25519PublicKey(String);

impl Ed25519PublicKey {
    /// Accept exactly one line `ssh-ed25519 BASE64 [comment]` whose blob is a
    /// well-formed Ed25519 key. Options, other key types, control characters,
    /// extra lines and oversized input are refused. The comment is dropped.
    pub fn parse(text: &str) -> crate::Result<Self> {
        let fail = |why: &str| Err(crate::CoreError(format!("invalid SSH public key: {why}")));
        if text.len() > MAX_SSH_PUBLIC_KEY {
            return fail("too long");
        }
        let line = text.strip_suffix('\n').unwrap_or(text);
        if line.contains(['\n', '\r']) {
            return fail("more than one line");
        }
        if line.chars().any(|c| c.is_control() && c != '\t') {
            return fail("control characters");
        }
        let mut parts = line.split_ascii_whitespace();
        let (Some(kind), Some(blob)) = (parts.next(), parts.next()) else {
            return fail("expected `ssh-ed25519 KEY`");
        };
        if kind != "ssh-ed25519" {
            return fail("only ssh-ed25519 keys are accepted");
        }
        let Some(bytes) = decode_base64(blob) else {
            return fail("the key is not base64");
        };
        // string "ssh-ed25519" then string of 32 key bytes.
        let mut expected = Vec::with_capacity(51);
        expected.extend_from_slice(&11u32.to_be_bytes());
        expected.extend_from_slice(b"ssh-ed25519");
        expected.extend_from_slice(&32u32.to_be_bytes());
        if bytes.len() != 51 || bytes[..19] != expected[..] {
            return fail("the key data is not an Ed25519 key");
        }
        Ok(Self(format!("ssh-ed25519 {blob}")))
    }

    /// `ssh-ed25519 BASE64`.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// OpenSSH's fingerprint, `SHA256:` and the unpadded base64 of the
    /// SHA-256 of the key blob, as `ssh-keygen -l` prints it.
    pub fn fingerprint(&self) -> String {
        use sha2::{Digest, Sha256};
        let blob = decode_base64(&self.0["ssh-ed25519 ".len()..]).expect("checked by parse");
        format!("SHA256:{}", encode_base64_unpadded(&Sha256::digest(blob)))
    }

    /// The line RC2FX writes: the key and its marker comment.
    pub fn managed_line(&self) -> String {
        format!("{} {SSH_MANAGED_MARKER}", self.0)
    }
}

fn encode_base64_unpadded(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, &b)| n | (b as u32) << (16 - 8 * i));
        for i in 0..=chunk.len() {
            out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    out
}

fn decode_base64(text: &str) -> Option<Vec<u8>> {
    let value = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32)
    };
    let bytes = text.as_bytes();
    if bytes.is_empty() || !bytes.len().is_multiple_of(4) {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for (index, chunk) in bytes.chunks(4).enumerate() {
        let last = index == bytes.len() / 4 - 1;
        let pad = chunk.iter().rev().take_while(|&&c| c == b'=').count();
        if pad > 2 || (pad > 0 && !last) {
            return None;
        }
        let mut n = 0u32;
        for &c in &chunk[..4 - pad] {
            n = (n << 6) | value(c)?;
        }
        n <<= 6 * pad as u32;
        let decoded = [(n >> 16) as u8, (n >> 8) as u8, n as u8];
        out.extend_from_slice(&decoded[..3 - pad]);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // A real Ed25519 public key, generated for these tests; its private half
    // was discarded.
    pub(crate) const KEY: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIEPzD7gW9u5+SC5iq8gzujWEOTBQO1CbVCLO8fAOGkip";

    #[test]
    fn a_plain_ed25519_key_is_accepted_and_its_comment_dropped() {
        let key = Ed25519PublicKey::parse(&format!("{KEY} me@laptop\n")).unwrap();
        assert_eq!(key.as_str(), KEY);
        assert_eq!(
            key.managed_line(),
            format!("{KEY} rc2fx-managed-developer-key")
        );
    }

    #[test]
    fn the_fingerprint_matches_ssh_keygen() {
        // `ssh-keygen -lf` of this key printed this fingerprint.
        assert_eq!(
            Ed25519PublicKey::parse(KEY).unwrap().fingerprint(),
            "SHA256:QbAgxtEX0z3ZCUzJ86NO6a0PYcnvrkRmVh0+cMm05TY"
        );
    }

    #[test]
    fn anything_else_is_refused() {
        let blob = &KEY["ssh-ed25519 ".len()..];
        for bad in [
            String::new(),
            "ssh-ed25519".into(),
            format!("ssh-rsa {blob}"),
            format!("command=\"sh\" {KEY}"),
            format!("no-pty {KEY}"),
            format!("{KEY}\n{KEY}"),
            format!("{KEY}\rinjected"),
            format!("{KEY} \u{1b}[31m"),
            format!("ssh-ed25519 {}", &blob[..blob.len() - 4]),
            "ssh-ed25519 AAAAB3NzaC1yc2EAAAADAQABAAABAQ==".into(),
            "ssh-ed25519 !!!!".into(),
            format!("{KEY} {}", "x".repeat(MAX_SSH_PUBLIC_KEY)),
        ] {
            assert!(Ed25519PublicKey::parse(&bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn status_and_request_round_trip() {
        let status = SshStatus {
            managed: true,
            managed_fingerprint: Some("SHA256:abc".into()),
            unmanaged_keys: 1,
            host_keys: vec![KEY.into()],
            server_running: Some(true),
            detail: None,
        };
        let text = serde_json::to_string(&status).unwrap();
        assert_eq!(serde_json::from_str::<SshStatus>(&text).unwrap(), status);
        assert!(text.contains("\"unmanagedKeys\":1"));
        // Newer fields are tolerated by an older reader...
        let newer: SshStatus =
            serde_json::from_str(r#"{"managed":false,"unmanagedKeys":0,"hostKeys":[],"future":1}"#)
                .unwrap();
        assert!(!newer.managed);
        // ...but a reply missing what the status means is an error, never "off".
        for bad in [
            "{}",
            "null",
            "[]",
            r#"{"managed":false}"#,
            r#"{"managed":false,"unmanagedKeys":0}"#,
            r#"{"unmanagedKeys":0,"hostKeys":[]}"#,
        ] {
            assert!(serde_json::from_str::<SshStatus>(bad).is_err(), "{bad}");
        }
        let request: SshRequest =
            serde_json::from_str(&format!(r#"{{"enabled":true,"publicKey":"{KEY}"}}"#)).unwrap();
        assert!(request.enabled);
        assert!(serde_json::from_str::<SshRequest>(r#"{"enabled":true,"x":1}"#).is_err());
    }
}
