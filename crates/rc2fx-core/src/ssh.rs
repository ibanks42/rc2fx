//! Developer SSH access managed over USB 1 without reflashing.
//!
//! Stock firmware runs OpenSSH with key-only root login and an empty
//! `authorized_keys`. RC2FX manages a single tagged line in that file and leaves
//! all other keys untouched (though still reported). Removing the key blocks
//! new logins without killing active sessions.

pub use crate::schema_gen::{MAX_SSH_PUBLIC_KEY, SSH_MANAGED_MARKER, SshRequest, SshStatus};

/// Canonical `ssh-ed25519 BASE64` public key with options and comment stripped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ed25519PublicKey(String);

impl Ed25519PublicKey {
    /// Parses a single `ssh-ed25519 BASE64 [comment]` line and validates its wire blob.
    /// Rejects key options, other key types, control characters, multi-line input,
    /// and oversized lines; strips any trailing comment.
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

    /// OpenSSH `SHA256:<unpadded-base64>` fingerprint, matching `ssh-keygen -l`.
    pub fn fingerprint(&self) -> String {
        use sha2::{Digest, Sha256};
        let blob = decode_base64(&self.0["ssh-ed25519 ".len()..]).expect("checked by parse");
        format!("SHA256:{}", encode_base64_unpadded(&Sha256::digest(blob)))
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

pub fn decode_base64(text: &str) -> Option<Vec<u8>> {
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

    // Throwaway Ed25519 test key (private half discarded).
    pub(crate) const KEY: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIEPzD7gW9u5+SC5iq8gzujWEOTBQO1CbVCLO8fAOGkip";

    #[test]
    fn a_plain_ed25519_key_is_accepted_and_its_comment_dropped() {
        let key = Ed25519PublicKey::parse(&format!("{KEY} me@laptop\n")).unwrap();
        assert_eq!(key.as_str(), KEY);
    }

    #[test]
    fn the_fingerprint_matches_ssh_keygen() {
        // Expected value from `ssh-keygen -lf`.
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
        // Unknown future fields are ignored.
        let newer: SshStatus =
            serde_json::from_str(r#"{"managed":false,"unmanagedKeys":0,"hostKeys":[],"future":1}"#)
                .unwrap();
        assert!(!newer.managed);
        // Missing required fields must fail rather than default to "off".
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
