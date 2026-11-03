//! Read-only USB 1 observation on a control thread. Never call from audio.
use rc2fx_core::{UsbLinkState, UsbStatus};

pub(crate) fn observe() -> UsbStatus {
    #[cfg(target_os = "linux")]
    let usb1 = read_state(std::path::Path::new("/sys/class/udc/ff400000.usb/state"));
    #[cfg(not(target_os = "linux"))]
    let usb1 = UsbLinkState::Unavailable;
    UsbStatus { usb1 }
}

#[cfg(any(target_os = "linux", test))]
fn read_state(path: &std::path::Path) -> UsbLinkState {
    use std::io::Read;
    let mut bytes = [0; 65];
    let result = (|| {
        let mut file = std::fs::File::open(path)?;
        let mut length = 0;
        while length < bytes.len() {
            let count = file.read(&mut bytes[length..])?;
            if count == 0 {
                return Ok::<_, std::io::Error>(parse(&bytes[..length]));
            }
            length += count;
        }
        Ok(UsbLinkState::Unavailable)
    })();
    result.unwrap_or(UsbLinkState::Unavailable)
}

#[cfg(any(target_os = "linux", test))]
fn parse(bytes: &[u8]) -> UsbLinkState {
    // Accept exactly one kernel state, with its optional trailing newline.
    match bytes.strip_suffix(b"\n").unwrap_or(bytes) {
        b"not attached" => UsbLinkState::NotAttached,
        b"attached" => UsbLinkState::Attached,
        b"powered" => UsbLinkState::Powered,
        b"reconnecting" => UsbLinkState::Reconnecting,
        b"unauthenticated" => UsbLinkState::Unauthenticated,
        b"default" => UsbLinkState::Default,
        b"addressed" => UsbLinkState::Addressed,
        b"configured" => UsbLinkState::Configured,
        b"suspended" => UsbLinkState::Suspended,
        _ => UsbLinkState::Unavailable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observes_disconnect_suspend_and_reconnect_without_caching_success() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state");
        for (raw, expected) in [
            ("configured\n", UsbLinkState::Configured),
            ("suspended\n", UsbLinkState::Suspended),
            ("not attached\n", UsbLinkState::NotAttached),
            ("addressed\n", UsbLinkState::Addressed),
            ("configured\n", UsbLinkState::Configured),
        ] {
            std::fs::write(&path, raw).unwrap();
            assert_eq!(read_state(&path), expected);
        }
        std::fs::remove_file(&path).unwrap();
        assert_eq!(read_state(&path), UsbLinkState::Unavailable);
    }

    #[test]
    fn malformed_missing_and_oversize_observations_are_not_disconnections() {
        for raw in [
            b"".as_slice(),
            b"configured\nnot attached",
            b"unknown\n",
            b"\xff",
            b"configured\n\n",
        ] {
            assert_eq!(parse(raw), UsbLinkState::Unavailable);
        }
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), vec![b'x'; 1024]).unwrap();
        assert_eq!(read_state(file.path()), UsbLinkState::Unavailable);
    }
}
