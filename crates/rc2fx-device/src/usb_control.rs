//! Direct HID control, fed by the native owner's filtered reports over a local
//! datagram socket. This process writes HID but never competes for HID reads.
#[cfg(target_os = "linux")]
use crate::{
    api::{Api, Options},
    coordinator::Handle,
};
use rc2fx_usb::{Assembly, Frame, INPUT_ID, INPUT_PAYLOAD, Kind, MAX_REQUEST, REPORT_SIZE};
#[cfg(target_os = "linux")]
use rc2fx_usb::{INPUT_REPORT_SIZE, MAX_REPLY, OUTPUT_ID};
use std::time::{Duration, Instant};
#[cfg(target_os = "linux")]
use std::{
    io,
    path::{Path, PathBuf},
};

#[derive(Default)]
struct Session {
    nonce: u64,
    seen: Option<Instant>,
    last: u32,
    receiving: bool,
    assembly: Assembly,
    reply: Vec<u8>,
}
enum Action {
    Send([u8; REPORT_SIZE]),
    Execute(Vec<u8>),
    Reset([u8; REPORT_SIZE]),
}

fn response(
    frame: &Frame<'_>,
    kind: Kind,
    offset: usize,
    total: usize,
    data: &[u8],
) -> [u8; REPORT_SIZE] {
    Frame {
        kind,
        session: frame.session,
        request: frame.request,
        offset,
        total,
        data,
    }
    .encode(INPUT_ID)
}
impl Session {
    fn accept(&mut self, f: &Frame<'_>, now: Instant) -> Action {
        let error = |text: &'static str| {
            Action::Send(response(
                f,
                Kind::Error,
                0,
                0,
                &text.as_bytes()[..text.len().min(INPUT_PAYLOAD)],
            ))
        };
        if f.session == 0 {
            return error("Invalid USB session");
        }
        if f.kind == Kind::Hello {
            if f.request != 0 || f.offset != 0 || f.total != 0 || !f.data.is_empty() {
                return error("Invalid USB hello");
            }
            if self.nonce != f.session
                && self
                    .seen
                    .is_some_and(|seen| now.duration_since(seen) < Duration::from_secs(5))
            {
                return error("USB is in use by another RC2FX client");
            }
            if self.nonce != f.session {
                *self = Self {
                    nonce: f.session,
                    seen: Some(now),
                    ..Self::default()
                };
                return Action::Reset(response(f, Kind::Ack, 0, 0, b"RC2FX USB 3"));
            }
            self.seen = Some(now);
            return Action::Send(response(f, Kind::Ack, 0, 0, b"RC2FX USB 3"));
        }
        if f.session != self.nonce {
            return error("USB session expired; reconnect");
        }
        self.seen = Some(now);
        match f.kind {
            Kind::Request => {
                if f.offset == 0 {
                    if f.request <= self.last {
                        return error("USB request was already admitted; not replayed");
                    }
                    self.last = f.request;
                    self.receiving = true;
                    self.reply.clear();
                    self.assembly.clear();
                }
                if !self.receiving || f.request != self.last {
                    return error("Invalid USB request sequence");
                }
                match self.assembly.push(f, MAX_REQUEST) {
                    Ok(Some(request)) => {
                        self.receiving = false;
                        Action::Execute(request)
                    }
                    Ok(None) => Action::Send(response(
                        f,
                        Kind::Ack,
                        f.offset + f.data.len(),
                        f.total,
                        &[],
                    )),
                    Err(message) => {
                        self.receiving = false;
                        error(message)
                    }
                }
            }
            Kind::Fetch
                if f.request == self.last
                    && !self.reply.is_empty()
                    && f.data.is_empty()
                    && f.offset < self.reply.len()
                    && f.offset.is_multiple_of(INPUT_PAYLOAD)
                    && f.total == 0 =>
            {
                let end = (f.offset + INPUT_PAYLOAD).min(self.reply.len());
                Action::Send(response(
                    f,
                    Kind::Reply,
                    f.offset,
                    self.reply.len(),
                    &self.reply[f.offset..end],
                ))
            }
            _ => error("Invalid USB control packet"),
        }
    }
}

/// Removes the socket this daemon bound, and nothing a successor put there.
#[cfg(target_os = "linux")]
struct SocketFile {
    path: PathBuf,
    identity: (u64, u64),
}

#[cfg(target_os = "linux")]
impl SocketFile {
    fn identity(path: &Path) -> io::Result<(u64, u64)> {
        use std::os::unix::fs::MetadataExt;
        let metadata = std::fs::symlink_metadata(path)?;
        Ok((metadata.dev(), metadata.ino()))
    }

    fn bound(path: &Path) -> io::Result<Self> {
        Ok(Self {
            path: path.to_owned(),
            identity: Self::identity(path)?,
        })
    }
}

#[cfg(target_os = "linux")]
impl Drop for SocketFile {
    fn drop(&mut self) {
        if Self::identity(&self.path).is_ok_and(|identity| identity == self.identity) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

#[cfg(target_os = "linux")]
pub async fn serve(path: &Path, controller: Handle, options: Options) -> io::Result<()> {
    use std::os::{
        fd::AsRawFd,
        unix::{
            fs::{OpenOptionsExt, PermissionsExt},
            net::UnixDatagram,
        },
    };
    use tokio::io::unix::AsyncFd;
    // Do not unlink an existing listener (another daemon may own it).
    let socket = UnixDatagram::bind(path)?;
    let _cleanup = SocketFile::bound(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    socket.set_nonblocking(true)?;
    let socket = tokio::net::UnixDatagram::from_std(socket)?;
    let mut session = Session::default();
    let mut api = Api::new(controller, options);
    let mut write_errors = 0;
    let mut bytes = [0; REPORT_SIZE + 1];
    loop {
        let n = socket.recv(&mut bytes).await?;
        let Some(frame) = Frame::decode(&bytes[..n], OUTPUT_ID) else {
            continue;
        };
        let reply = match session.accept(&frame, Instant::now()) {
            Action::Send(reply) => reply,
            Action::Reset(reply) => {
                api.reset();
                reply
            }
            Action::Execute(bytes) => {
                // The same error page the API builds, as validated raw JSON.
                let fail = |status, message| crate::api::failure(status, message);
                let unpacked = rc2fx_usb::unpack(&bytes, MAX_REQUEST).ok();
                let reply = match unpacked
                    .as_deref()
                    .and_then(|bytes| std::str::from_utf8(bytes).ok())
                    .and_then(|s| rc2fx_core::from_json(s).ok())
                {
                    Some(request) => {
                        tokio::time::timeout(Duration::from_secs(15), api.request(request))
                            .await
                            .unwrap_or_else(|_| {
                                fail(
                                    504,
                                    "USB operation timed out; outcome may be unknown; not replayed",
                                )
                            })
                    }
                    None => fail(400, "Invalid USB request JSON"),
                };
                // A long request still counts as activity: another client
                // must not take the session over before its reply is fetched.
                session.seen = Some(Instant::now());
                let json = serde_json::to_vec(&reply).map_err(io::Error::other)?;
                session.reply = match rc2fx_usb::pack(&json, MAX_REPLY) {
                    Ok(bytes) => bytes,
                    Err(_) => rc2fx_usb::pack(
                        &serde_json::to_vec(&fail(413, "USB reply too large"))
                            .map_err(io::Error::other)?,
                        MAX_REPLY,
                    )
                    .map_err(io::Error::other)?,
                };
                response(&frame, Kind::Ack, frame.total, frame.total, &[])
            }
        };
        // Shared gadget writes are serialized by the kernel. Wait asynchronously
        // when another writer owns the pending report; no audio thread waits here.
        let sent = tokio::time::timeout(Duration::from_secs(2), async {
            // Keep the handle scoped to one response. The launcher retires the
            // node and stops this daemon before rebuilding the gadget.
            let output = std::fs::OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
                .open("/dev/hidg0")?;
            // SAFETY: AsyncFd owns the File and its descriptor until it is dropped.
            let output = unsafe { AsyncFd::register(output) }.map_err(io::Error::from)?;
            loop {
                let mut ready = output.writable().await?;
                match ready.try_io(|inner| {
                    let n = unsafe {
                        libc::write(
                            inner.get_ref().as_raw_fd(),
                            reply.as_ptr().cast(),
                            INPUT_REPORT_SIZE,
                        )
                    };
                    if n < 0 {
                        Err(io::Error::last_os_error())
                    } else if n as usize != INPUT_REPORT_SIZE {
                        Err(io::Error::other("Short HID write"))
                    } else {
                        Ok(())
                    }
                }) {
                    Ok(result) => return result,
                    Err(_) => continue,
                }
            }
        })
        .await;
        if !matches!(sent, Ok(Ok(()))) {
            if write_errors < 3 {
                eprintln!("USB response write failed: {sent:?}");
            }
            write_errors += 1;
            // Leave the admitted ID and cached reply intact. A timed-out write
            // is ambiguous and must not permit the operation to execute again.
            api.reset();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn admitted_mutations_cannot_execute_twice_and_clients_cannot_steal_active_session() {
        let now = Instant::now();
        let mut s = Session::default();
        let mut f = Frame {
            kind: Kind::Hello,
            session: 4,
            request: 0,
            offset: 0,
            total: 0,
            data: &[],
        };
        assert!(
            matches!(s.accept(&f, now), Action::Reset(b) if Frame::decode(&b,INPUT_ID).unwrap().kind==Kind::Ack)
        );
        f.kind = Kind::Request;
        f.request = 1;
        f.total = 1;
        f.data = b"x";
        assert!(matches!(s.accept(&f, now), Action::Execute(b) if b == b"x"));
        assert!(
            matches!(s.accept(&f,now), Action::Send(b) if Frame::decode(&b,INPUT_ID).unwrap().kind==Kind::Error)
        );
        f.kind = Kind::Hello;
        f.request = 0;
        f.total = 0;
        f.data = &[];
        f.session = 5;
        assert!(
            matches!(s.accept(&f,now), Action::Send(b) if Frame::decode(&b,INPUT_ID).unwrap().kind==Kind::Error)
        );
        assert!(matches!(
            s.accept(&f, now + Duration::from_secs(6)),
            Action::Reset(_)
        ));
    }
}

#[cfg(all(test, target_os = "linux"))]
mod socket_file_tests {
    use super::SocketFile;
    use std::os::unix::net::UnixDatagram;

    #[test]
    fn only_the_socket_this_daemon_bound_is_removed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usb.sock");

        let socket = UnixDatagram::bind(&path).unwrap();
        drop(SocketFile::bound(&path).unwrap());
        assert!(!path.exists(), "its own socket is removed");
        drop(socket);

        let _old = UnixDatagram::bind(&path).unwrap();
        let cleanup = SocketFile::bound(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        let _successor = UnixDatagram::bind(&path).unwrap();
        drop(cleanup);
        assert!(path.exists(), "a successor's socket stays");
    }
}
