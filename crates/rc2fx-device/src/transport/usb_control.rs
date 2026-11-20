//! Direct HID control, fed by the native owner's filtered reports over a local
//! datagram socket. This process writes HID but never competes for HID reads.
use crate::{
    api::{Api, Options},
    coordinator::Handle,
};
use rc2fx_usb::{Assembly, Frame, INPUT_ID, INPUT_PAYLOAD, Kind, MAX_REQUEST, REPORT_SIZE};
use rc2fx_usb::{INPUT_REPORT_SIZE, MAX_REPLY, OUTPUT_ID};
use std::time::{Duration, Instant};
use std::{
    io,
    path::{Path, PathBuf},
};

/// Silence timeout after which a client is treated as disconnected, dropping
/// its event subscription and allowing another client to take over. The app
/// polls events roughly every 50 ms.
const CLIENT_IDLE: Duration = Duration::from_secs(5);

#[derive(Default)]
struct Session {
    nonce: u64,
    /// When the owner last sent a valid report or had a request answered.
    /// None until the first client says hello, so that hello is never
    /// refused. Another client may take the session over once this is
    /// `CLIENT_IDLE` old. The server's own `active` clock, on tokio's time,
    /// decides when the owner's event subscription lapses.
    seen: Option<Instant>,
    /// The newest request ID admitted in this session. A request at or
    /// below it is a replay and is refused, whatever `exchange` holds.
    last: u32,
    /// What became of request `last`.
    exchange: Exchange,
    /// Protocol 4, which acknowledges a non-final request fragment only at a
    /// window boundary and fetches several reply reports at once. Empty data
    /// in a hello keeps protocol 3, exactly as before.
    v4: bool,
    /// The window the server sized for this link when it started.
    window: usize,
}
/// The request `Session::last` names, from its first fragment to its reply.
#[derive(Default)]
enum Exchange {
    /// No request yet, its fragments were refused, or it is running.
    #[default]
    Idle,
    /// Its fragments are arriving in order.
    Receiving(Assembly),
    /// It ran, and this packed reply can be fetched.
    Answered(Vec<u8>),
}
enum Action {
    /// Zero reports means the fragment needs no acknowledgement.
    Send(Vec<[u8; REPORT_SIZE]>),
    Execute(Vec<u8>),
    Reset(Box<[u8; REPORT_SIZE]>),
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
            Action::Send(vec![response(
                f,
                Kind::Error,
                0,
                0,
                &text.as_bytes()[..text.len().min(INPUT_PAYLOAD)],
            )])
        };
        if f.session == 0 {
            return error("Invalid USB session");
        }
        if f.kind == Kind::Hello {
            let v4 = if f.data.is_empty() {
                false
            } else if f.data == rc2fx_usb::HELLO_4 {
                true
            } else {
                return error("Invalid USB hello");
            };
            if f.request != 0 || f.offset != 0 || f.total != 0 {
                return error("Invalid USB hello");
            }
            if self.nonce != f.session
                && self
                    .seen
                    .is_some_and(|seen| now.duration_since(seen) < CLIENT_IDLE)
            {
                return error("USB is in use by another RC2FX client");
            }
            let ack = if v4 {
                format!("RC2FX USB 4 {}", self.window)
            } else {
                // No window: protocol 3's acknowledgement is exactly HELLO_3.
                "RC2FX USB 3".to_owned()
            };
            if self.nonce != f.session {
                let window = self.window;
                *self = Self {
                    nonce: f.session,
                    seen: Some(now),
                    v4,
                    window,
                    ..Self::default()
                };
                return Action::Reset(Box::new(response(f, Kind::Ack, 0, 0, ack.as_bytes())));
            }
            self.v4 = v4;
            self.seen = Some(now);
            return Action::Send(vec![response(f, Kind::Ack, 0, 0, ack.as_bytes())]);
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
                    self.exchange = Exchange::Receiving(Assembly::default());
                }
                let Exchange::Receiving(assembly) = &mut self.exchange else {
                    return error("Invalid USB request sequence");
                };
                if f.request != self.last {
                    return error("Invalid USB request sequence");
                }
                match assembly.push(f, MAX_REQUEST) {
                    Ok(Some(request)) => {
                        self.exchange = Exchange::Idle;
                        Action::Execute(request)
                    }
                    Ok(None) => {
                        // Protocol 4 lets the host keep one window of fragments
                        // in flight. Only the fragment that ends a window is
                        // acknowledged; one acknowledgement then covers all of
                        // them, and errors are still sent as they happen.
                        let end = f.offset + f.data.len();
                        if self.v4 && !(end / rc2fx_usb::PAYLOAD).is_multiple_of(self.window.max(1))
                        {
                            Action::Send(Vec::new())
                        } else {
                            Action::Send(vec![response(f, Kind::Ack, end, f.total, &[])])
                        }
                    }
                    Err(message) => {
                        self.exchange = Exchange::Idle;
                        error(message)
                    }
                }
            }
            Kind::Fetch
                if f.request == self.last
                    && let Exchange::Answered(reply) = &self.exchange
                    && !reply.is_empty()
                    && f.data.is_empty()
                    && f.offset < reply.len()
                    && f.offset.is_multiple_of(INPUT_PAYLOAD) =>
            {
                // Protocol 3 asks for one report and requires `total` to be
                // zero; protocol 4 names how many reports it can take, and
                // fetches them in order until the reply ends.
                let count = if self.v4 {
                    if f.total > self.window.max(1) {
                        return error("Invalid USB fetch window");
                    }
                    f.total.max(1)
                } else if f.total == 0 {
                    1
                } else {
                    return error("Invalid USB control packet");
                };
                let mut replies = Vec::with_capacity(count);
                for index in 0..count {
                    let start = f.offset + index * INPUT_PAYLOAD;
                    if start >= reply.len() {
                        break;
                    }
                    let end = (start + INPUT_PAYLOAD).min(reply.len());
                    replies.push(response(
                        f,
                        Kind::Reply,
                        start,
                        reply.len(),
                        &reply[start..end],
                    ));
                }
                Action::Send(replies)
            }
            _ => error("Invalid USB control packet"),
        }
    }
}

/// Removes the socket this daemon bound, and nothing a successor put there.
struct SocketFile {
    path: PathBuf,
    identity: (u64, u64),
}

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

impl Drop for SocketFile {
    fn drop(&mut self) {
        if Self::identity(&self.path).is_ok_and(|identity| identity == self.identity) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// One USB client's session and its view of the API.
pub(crate) struct Server {
    session: Session,
    api: Api,
    /// When the session's owner last sent a report or had a request answered,
    /// on tokio's clock. It decides when the owner's event subscription
    /// lapses; `Session::seen` decides when another client may take over.
    /// They move together, except that a report the session refuses before
    /// it checks the session ID (a malformed hello from the owner) counts
    /// here and not there.
    active: tokio::time::Instant,
}

impl Server {
    /// `window` is the protocol 4 receive window, sized once when the server
    /// starts. Tests pass it in; the daemon reads it from the kernel queue that
    /// the native hook's non-blocking forward uses.
    pub(crate) fn new(controller: Handle, options: Options, window: usize) -> Self {
        Self {
            session: Session {
                window,
                ..Session::default()
            },
            api: Api::new(controller, options),
            active: tokio::time::Instant::now(),
        }
    }

    /// Waits for the next report from the native library.
    ///
    /// Drops the event subscription if the owner is silent for `CLIENT_IDLE`
    /// (stopping fast engine polling and meter serialization), while keeping
    /// the session nonce, last request ID, and cached reply for replay safety.
    /// Unsubscribed servers wait directly on the socket with no timer.
    pub(crate) async fn receive(
        &mut self,
        socket: &tokio::net::UnixDatagram,
        bytes: &mut [u8],
    ) -> io::Result<usize> {
        loop {
            if !self.api.subscribed() {
                return socket.recv(bytes).await;
            }
            let deadline = self.active + CLIENT_IDLE;
            // Checked before the socket, so a queue that is never empty
            // cannot put the expiry off.
            if tokio::time::Instant::now() >= deadline {
                self.api.reset();
                continue;
            }
            // `recv` is cancel-safe: a datagram is either returned or stays queued.
            match tokio::time::timeout_at(deadline, socket.recv(bytes)).await {
                Ok(received) => return received,
                Err(_) => self.api.reset(),
            }
        }
    }

    /// Handles one report and returns the reports to answer with, in order.
    /// Not an RC2FX frame and a protocol 4 fragment inside a window answer
    /// with nothing.
    pub(crate) async fn handle(&mut self, bytes: &[u8]) -> io::Result<Vec<[u8; REPORT_SIZE]>> {
        let Some(frame) = Frame::decode(bytes, OUTPUT_ID) else {
            return Ok(Vec::new());
        };
        let replies = match self.session.accept(&frame, Instant::now()) {
            Action::Send(replies) => replies,
            Action::Reset(reply) => {
                self.api.reset();
                vec![*reply]
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
                        tokio::time::timeout(Duration::from_secs(15), self.api.request(request))
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
                self.session.seen = Some(Instant::now());
                let json = serde_json::to_vec(&reply).map_err(io::Error::other)?;
                let packed = match rc2fx_usb::pack(&json, MAX_REPLY) {
                    Ok(bytes) => bytes,
                    Err(_) => rc2fx_usb::pack(
                        &serde_json::to_vec(&fail(413, "USB reply too large"))
                            .map_err(io::Error::other)?,
                        MAX_REPLY,
                    )
                    .map_err(io::Error::other)?,
                };
                self.session.exchange = Exchange::Answered(packed);
                vec![response(&frame, Kind::Ack, frame.total, frame.total, &[])]
            }
        };
        // Only the session's owner keeps its subscription alive, and the time
        // a request took to run is not silence.
        if frame.session == self.session.nonce {
            self.active = tokio::time::Instant::now();
        }
        Ok(replies)
    }
}

/// How many request fragments a protocol 4 client sends per acknowledgement.
/// The native hook waits for room in this daemon's datagram queue (up to
/// 10 ms) instead of dropping a report, so the window is not bounded by
/// `net.unix.max_dgram_qlen`; USB itself holds the host back while the hook
/// is waiting.
fn transport_window() -> usize {
    rc2fx_usb::MAX_WINDOW
}

/// Writes one batch of input reports to the HID gadget, opening it once. The
/// launcher retires the node and stops this daemon before rebuilding the
/// gadget, so a handle is kept only for the batch.
async fn write_reports(replies: &[[u8; REPORT_SIZE]]) -> io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    use tokio::io::unix::AsyncFd;
    let output = std::fs::OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open("/dev/hidg0")?;
    // SAFETY: AsyncFd owns the File and its descriptor until it is dropped.
    let output = unsafe { AsyncFd::register(output) }.map_err(io::Error::from)?;
    for reply in replies {
        // Shared gadget writes are serialized by the kernel. Wait asynchronously
        // when another writer owns the pending report; no audio thread waits here.
        match tokio::time::timeout(Duration::from_secs(2), write_report(&output, reply)).await {
            Ok(result) => result?,
            Err(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "HID write timed out",
                ));
            }
        }
    }
    Ok(())
}

async fn write_report(
    output: &tokio::io::unix::AsyncFd<std::fs::File>,
    reply: &[u8; REPORT_SIZE],
) -> io::Result<()> {
    use std::os::fd::AsRawFd;
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
}

pub async fn serve(path: &Path, controller: Handle, options: Options) -> io::Result<()> {
    use std::os::unix::{fs::PermissionsExt, net::UnixDatagram};
    // Do not unlink an existing listener (another daemon may own it).
    let socket = UnixDatagram::bind(path)?;
    let _cleanup = SocketFile::bound(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    socket.set_nonblocking(true)?;
    let socket = tokio::net::UnixDatagram::from_std(socket)?;
    let mut server = Server::new(controller, options, transport_window());
    let mut write_errors = 0;
    let mut bytes = [0; REPORT_SIZE + 1];
    loop {
        let n = server.receive(&socket, &mut bytes).await?;
        let replies = server.handle(&bytes[..n]).await?;
        if replies.is_empty() {
            continue;
        }
        if let Err(error) = write_reports(&replies).await {
            if write_errors < 3 {
                eprintln!("USB response write failed: {error}");
            }
            write_errors += 1;
            // Leave the admitted ID and cached reply intact. A timed-out write
            // is ambiguous and must not permit the operation to execute again.
            server.api.reset();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    static FILL: [u8; rc2fx_usb::PAYLOAD] = [0x5a; rc2fx_usb::PAYLOAD];

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
            matches!(s.accept(&f, now), Action::Reset(b) if Frame::decode(&b[..],INPUT_ID).unwrap().kind==Kind::Ack)
        );
        f.kind = Kind::Request;
        f.request = 1;
        f.total = 1;
        f.data = b"x";
        assert!(matches!(s.accept(&f, now), Action::Execute(b) if b == b"x"));
        assert!(
            matches!(s.accept(&f,now), Action::Send(b) if b.len()==1 && Frame::decode(&b[0],INPUT_ID).unwrap().kind==Kind::Error)
        );
        f.kind = Kind::Hello;
        f.request = 0;
        f.total = 0;
        f.data = &[];
        f.session = 5;
        assert!(
            matches!(s.accept(&f,now), Action::Send(b) if b.len()==1 && Frame::decode(&b[0],INPUT_ID).unwrap().kind==Kind::Error)
        );
        assert!(matches!(
            s.accept(&f, now + Duration::from_secs(6)),
            Action::Reset(_)
        ));
    }

    fn hello(session: u64, data: &'static [u8]) -> Frame<'static> {
        Frame {
            kind: Kind::Hello,
            session,
            request: 0,
            offset: 0,
            total: 0,
            data,
        }
    }

    #[test]
    fn a_protocol_four_hello_negotiates_the_window_and_empty_data_stays_three() {
        let now = Instant::now();
        let mut session = Session {
            window: 12,
            ..Session::default()
        };
        // Empty data is protocol 3, byte for byte as before.
        let Action::Reset(reply) = session.accept(&hello(1, b""), now) else {
            panic!("a new hello resets")
        };
        assert_eq!(
            Frame::decode(&reply[..], INPUT_ID).unwrap().data,
            rc2fx_usb::HELLO_3
        );
        assert!(!session.v4);
        // A protocol 4 client names the version and the mixer answers with the
        // window it sized for this link.
        let Action::Send(replies) = session.accept(&hello(1, rc2fx_usb::HELLO_4), now) else {
            panic!("a hello on the same session is acknowledged")
        };
        assert_eq!(replies.len(), 1);
        assert_eq!(
            Frame::decode(&replies[0], INPUT_ID).unwrap().data,
            b"RC2FX USB 4 12"
        );
        assert!(session.v4);
        // Anything else is not a hello, including a version with a window.
        for junk in [&b"RC2FX USB 5"[..], b"RC2FX USB 4 12", b"junk"] {
            let Action::Send(replies) = session.accept(&hello(1, junk), now) else {
                panic!("hello errors are immediate")
            };
            let frame = Frame::decode(&replies[0], INPUT_ID).unwrap();
            assert_eq!(frame.kind, Kind::Error);
            assert_eq!(frame.data, b"Invalid USB hello");
        }
    }

    #[test]
    fn protocol_four_acknowledges_only_window_boundaries_and_three_still_acks_every_fragment() {
        let now = Instant::now();
        let mut session = Session {
            window: 2,
            ..Session::default()
        };
        assert!(matches!(
            session.accept(&hello(1, rc2fx_usb::HELLO_4), now),
            Action::Reset(_)
        ));
        let total = 3 * rc2fx_usb::PAYLOAD;
        let fragment = |index: usize| {
            let offset = index * rc2fx_usb::PAYLOAD;
            let len = rc2fx_usb::PAYLOAD.min(total - offset);
            Frame {
                kind: Kind::Request,
                session: 1,
                request: 7,
                offset,
                total,
                data: &FILL[..len],
            }
        };
        // The first fragment ends inside a window: no reply.
        assert!(matches!(session.accept(&fragment(0), now), Action::Send(r) if r.is_empty()));
        // The second ends the window: one acknowledgement.
        let Action::Send(replies) = session.accept(&fragment(1), now) else {
            panic!("a boundary is acknowledged")
        };
        assert_eq!(replies.len(), 1);
        assert_eq!(
            Frame::decode(&replies[0], INPUT_ID).unwrap().offset,
            2 * rc2fx_usb::PAYLOAD
        );
        // The final fragment executes whether or not it ends a window.
        assert!(matches!(
            session.accept(&fragment(2), now),
            Action::Execute(bytes) if bytes.len() == total
        ));
        // Protocol 3 acknowledges every non-final fragment.
        let mut session = Session {
            window: 2,
            ..Session::default()
        };
        assert!(matches!(
            session.accept(&hello(1, b""), now),
            Action::Reset(_)
        ));
        for index in 0..2 {
            let Action::Send(replies) = session.accept(&fragment_with(index, total), now) else {
                panic!("protocol 3 acknowledges")
            };
            assert_eq!(replies.len(), 1);
        }
    }

    fn fragment_with(index: usize, total: usize) -> Frame<'static> {
        let offset = index * rc2fx_usb::PAYLOAD;
        let len = rc2fx_usb::PAYLOAD.min(total - offset);
        Frame {
            kind: Kind::Request,
            session: 1,
            request: 9,
            offset,
            total,
            data: &FILL[..len],
        }
    }

    /// A request's fragments are taken only while it is being received, and
    /// its reply can be fetched only once it was answered and until the next
    /// request is admitted.
    #[test]
    fn a_request_is_received_then_answered_and_a_new_one_replaces_it() {
        let now = Instant::now();
        let mut session = Session::default();
        assert!(matches!(
            session.accept(&hello(1, b""), now),
            Action::Reset(_)
        ));
        let kind = |action: Action| match action {
            Action::Send(replies) if replies.len() == 1 => {
                Frame::decode(&replies[0], INPUT_ID).unwrap().kind
            }
            _ => panic!("one report answers"),
        };
        let fragment = |request: u32, offset: usize, total: usize| Frame {
            kind: Kind::Request,
            session: 1,
            request,
            offset,
            total,
            data: &FILL[..1],
        };
        let fetch = |request: u32| Frame {
            kind: Kind::Fetch,
            session: 1,
            request,
            offset: 0,
            total: 0,
            data: &[],
        };
        // Nothing is received yet: a continuation and a fetch are refused.
        assert_eq!(kind(session.accept(&fragment(1, 1, 2), now)), Kind::Error);
        assert_eq!(kind(session.accept(&fetch(1), now)), Kind::Error);
        assert!(matches!(session.exchange, Exchange::Idle));
        // The first fragment starts receiving and the second completes it.
        assert_eq!(kind(session.accept(&fragment(1, 0, 2), now)), Kind::Ack);
        assert!(matches!(session.exchange, Exchange::Receiving(_)));
        assert!(matches!(
            session.accept(&fragment(1, 1, 2), now),
            Action::Execute(bytes) if bytes.len() == 2
        ));
        // While it runs there is no reply to fetch and nothing to continue.
        assert!(matches!(session.exchange, Exchange::Idle));
        assert_eq!(kind(session.accept(&fetch(1), now)), Kind::Error);
        assert_eq!(kind(session.accept(&fragment(1, 1, 2), now)), Kind::Error);
        // Answered, its reply can be fetched, but only by its own ID.
        session.exchange = Exchange::Answered(vec![0x33; 3]);
        assert_eq!(kind(session.accept(&fetch(1), now)), Kind::Reply);
        assert_eq!(kind(session.accept(&fetch(2), now)), Kind::Error);
        // A replay of its first fragment is refused and keeps the reply.
        assert_eq!(kind(session.accept(&fragment(1, 0, 2), now)), Kind::Error);
        assert_eq!(kind(session.accept(&fetch(1), now)), Kind::Reply);
        // The next request drops the reply.
        assert_eq!(kind(session.accept(&fragment(2, 0, 2), now)), Kind::Ack);
        assert_eq!(kind(session.accept(&fetch(1), now)), Kind::Error);
        // A fragment that breaks the sequence ends the receipt for good.
        assert_eq!(kind(session.accept(&fragment(2, 1, 3), now)), Kind::Error);
        assert!(matches!(session.exchange, Exchange::Idle));
        assert_eq!(kind(session.accept(&fragment(2, 1, 2), now)), Kind::Error);
        assert_eq!(session.last, 2);
    }

    #[test]
    fn protocol_four_fetches_several_reply_reports_but_never_more_than_its_window() {
        let now = Instant::now();
        let mut session = Session {
            window: 3,
            ..Session::default()
        };
        assert!(matches!(
            session.accept(&hello(1, rc2fx_usb::HELLO_4), now),
            Action::Reset(_)
        ));
        session.last = 4;
        session.exchange = Exchange::Answered(vec![0x33; 2 * INPUT_PAYLOAD + 5]);
        let fetch = |total: usize, offset: usize| Frame {
            kind: Kind::Fetch,
            session: 1,
            request: 4,
            offset,
            total,
            data: &[],
        };
        let Action::Send(replies) = session.accept(&fetch(2, 0), now) else {
            panic!("a fetch answers")
        };
        assert_eq!(replies.len(), 2);
        let frames: Vec<_> = replies
            .iter()
            .map(|r| Frame::decode(r, INPUT_ID).unwrap())
            .collect();
        assert_eq!(frames[0].offset, 0);
        assert_eq!(frames[1].offset, INPUT_PAYLOAD);
        assert_eq!(frames[0].total, 2 * INPUT_PAYLOAD + 5);
        assert_eq!(frames[1].data.len(), INPUT_PAYLOAD);
        // The last report stops at the end of the reply.
        let Action::Send(replies) = session.accept(&fetch(3, 2 * INPUT_PAYLOAD), now) else {
            panic!("a fetch answers")
        };
        assert_eq!(replies.len(), 1);
        assert_eq!(Frame::decode(&replies[0], INPUT_ID).unwrap().data.len(), 5);
        // Zero means one in protocol 4, and more than the window is refused.
        let Action::Send(replies) = session.accept(&fetch(0, 0), now) else {
            panic!("a fetch answers")
        };
        assert_eq!(replies.len(), 1);
        let Action::Send(replies) = session.accept(&fetch(4, 0), now) else {
            panic!("an over-window fetch is an immediate error")
        };
        assert_eq!(
            Frame::decode(&replies[0], INPUT_ID).unwrap().kind,
            Kind::Error
        );
        // Protocol 3 still takes exactly one report and requires total 0.
        let mut session = Session::default();
        assert!(matches!(
            session.accept(&hello(1, b""), now),
            Action::Reset(_)
        ));
        session.last = 4;
        session.exchange = Exchange::Answered(vec![0x33; 2 * INPUT_PAYLOAD + 5]);
        let Action::Send(replies) = session.accept(&fetch(0, 0), now) else {
            panic!("a protocol 3 fetch answers")
        };
        assert_eq!(replies.len(), 1);
        let Action::Send(replies) = session.accept(&fetch(1, 0), now) else {
            panic!("a protocol 3 fetch refusal is immediate")
        };
        assert_eq!(
            Frame::decode(&replies[0], INPUT_ID).unwrap().kind,
            Kind::Error
        );
    }
}

/// A USB client for tests: builds the reports the native library forwards
/// and decodes the reports the server answers with.
#[cfg(test)]
pub(crate) mod testing {
    use super::*;

    pub(crate) fn report(
        kind: Kind,
        session: u64,
        request: u32,
        total: usize,
        data: &[u8],
    ) -> [u8; REPORT_SIZE] {
        Frame {
            kind,
            session,
            request,
            offset: 0,
            total,
            data,
        }
        .encode(OUTPUT_ID)
    }
    pub(crate) fn hello(session: u64) -> [u8; REPORT_SIZE] {
        report(Kind::Hello, session, 0, 0, &[])
    }
    pub(crate) fn hello_v4(session: u64) -> [u8; REPORT_SIZE] {
        report(Kind::Hello, session, 0, 0, rc2fx_usb::HELLO_4)
    }
    /// A whole API request in one report.
    pub(crate) fn request(session: u64, id: u32, method: &str, path: &str) -> [u8; REPORT_SIZE] {
        let json = serde_json::to_vec(&rc2fx_usb::Request {
            method: method.into(),
            path: path.into(),
            body: None,
        })
        .unwrap();
        let packed = rc2fx_usb::pack(&json, MAX_REQUEST).unwrap();
        report(Kind::Request, session, id, packed.len(), &packed)
    }
    /// Asks for the first report of a request's cached reply.
    pub(crate) fn fetch(session: u64, id: u32) -> [u8; REPORT_SIZE] {
        fetch_from(session, id, 0)
    }
    fn fetch_from(session: u64, id: u32, offset: usize) -> [u8; REPORT_SIZE] {
        Frame {
            kind: Kind::Fetch,
            session,
            request: id,
            offset,
            total: 0,
            data: &[],
        }
        .encode(OUTPUT_ID)
    }
    /// A protocol 4 fetch for `count` reports at once.
    pub(crate) fn fetch_count(
        session: u64,
        id: u32,
        offset: usize,
        count: usize,
    ) -> [u8; REPORT_SIZE] {
        Frame {
            kind: Kind::Fetch,
            session,
            request: id,
            offset,
            total: count,
            data: &[],
        }
        .encode(OUTPUT_ID)
    }
    /// Every report the server answered with, decoded.
    pub(crate) async fn answers(server: &mut Server, report: &[u8]) -> Vec<(Kind, Vec<u8>)> {
        server
            .handle(report)
            .await
            .unwrap()
            .into_iter()
            .map(|reply| {
                let frame = Frame::decode(&reply, INPUT_ID).unwrap();
                (frame.kind, frame.data.to_vec())
            })
            .collect()
    }
    /// The kind and payload of the one report the server answered with.
    pub(crate) async fn answer(server: &mut Server, report: &[u8]) -> (Kind, Vec<u8>) {
        let mut replies = answers(server, report).await;
        assert_eq!(replies.len(), 1, "expected exactly one report");
        replies.remove(0)
    }
    /// Sends one request and fetches its whole reply.
    pub(crate) async fn call(
        server: &mut Server,
        session: u64,
        id: u32,
        method: &str,
        path: &str,
    ) -> rc2fx_usb::Reply {
        let (kind, _) = answer(server, &request(session, id, method, path)).await;
        assert_eq!(kind, Kind::Ack, "{method} {path} was not admitted");
        let mut packed = Vec::new();
        loop {
            let report = fetch_from(session, id, packed.len());
            let replies = server.handle(&report).await.unwrap();
            assert_eq!(replies.len(), 1);
            let frame = Frame::decode(&replies[0], INPUT_ID).unwrap();
            assert_eq!(frame.kind, Kind::Reply);
            packed.extend_from_slice(frame.data);
            if packed.len() == frame.total {
                break;
            }
        }
        serde_json::from_slice(&rc2fx_usb::unpack(&packed, MAX_REPLY).unwrap()).unwrap()
    }
}

/// The event subscription of a client that went away. Time is paused: a timer
/// that is armed moves the clock when nothing else can run, so the tests take
/// no real time, and a clock that did not move proves that no timer was armed.
#[cfg(test)]
mod subscription_tests {
    use super::{testing::*, *};
    use crate::{controller::Controller, coordinator::Coordinator, store::Store};
    use tokio::time::Instant as Clock;

    const SESSION: u64 = 0x5eed;

    struct Rig {
        _directory: tempfile::TempDir,
        handle: Handle,
        /// Never run: a command sent to it is accepted and never answered.
        _owner: Coordinator,
        server: Server,
        socket: tokio::net::UnixDatagram,
        peer: tokio::net::UnixDatagram,
    }
    impl Rig {
        fn new() -> Self {
            let directory = tempfile::tempdir().unwrap();
            let store = Store::open(directory.path(), rc2fx_core::Config::default()).unwrap();
            let (handle, owner) = Coordinator::new(Controller::new(store)).unwrap();
            let (socket, peer) = tokio::net::UnixDatagram::pair().unwrap();
            Self {
                _directory: directory,
                // The tests speak protocol 3, so the window never applies.
                server: Server::new(handle.clone(), Options::default(), 8),
                handle,
                _owner: owner,
                socket,
                peer,
            }
        }
        /// A rig whose client has said hello and fetched its first event.
        async fn subscribed() -> Self {
            let mut rig = Self::new();
            assert_eq!(answer(&mut rig.server, &hello(SESSION)).await.0, Kind::Ack);
            assert!(!rig.handle.has_clients());
            let first = rig.events(1).await;
            assert_eq!(first.body["event"], "state");
            assert!(rig.handle.has_clients());
            rig
        }
        async fn events(&mut self, id: u32) -> rc2fx_usb::Reply {
            call(&mut self.server, SESSION, id, "GET", "api/v1/events").await
        }
        /// Waits for a report as `serve` does, for at most `limit`.
        async fn receive(&mut self, limit: Duration) -> Option<Vec<u8>> {
            let mut bytes = [0; REPORT_SIZE + 1];
            let received =
                tokio::time::timeout(limit, self.server.receive(&self.socket, &mut bytes)).await;
            received.ok().map(|n| bytes[..n.unwrap()].to_vec())
        }
        /// Delivers one report through the socket and handles it.
        async fn deliver(&mut self, report: &[u8]) -> Vec<[u8; REPORT_SIZE]> {
            self.peer.send(report).await.unwrap();
            let received = self.receive(Duration::from_secs(1)).await.unwrap();
            assert_eq!(received, report);
            self.server.handle(&received).await.unwrap()
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_silent_client_loses_its_subscription_and_resumes_with_the_state() {
        let mut rig = Rig::subscribed().await;
        // Just short of the window nothing happens.
        assert!(
            rig.receive(CLIENT_IDLE - Duration::from_millis(1))
                .await
                .is_none()
        );
        assert!(rig.handle.has_clients());
        // At the window the subscription goes, and the wait goes on.
        assert!(rig.receive(Duration::from_millis(2)).await.is_none());
        assert!(!rig.handle.has_clients());
        // The same session polls again: a fresh subscription that starts with
        // the state, as for a new client. Not a 503 "subscription closed".
        let resumed = rig.events(2).await;
        assert_eq!(resumed.status, 200);
        assert_eq!(resumed.body["event"], "state");
        assert!(rig.handle.has_clients());
        // The new subscription expires in its turn.
        assert!(rig.receive(CLIENT_IDLE * 2).await.is_none());
        assert!(!rig.handle.has_clients());
    }

    #[tokio::test(start_paused = true)]
    async fn a_client_that_polls_events_is_never_dropped() {
        let mut rig = Rig::subscribed().await;
        // The app's cadence: one poll, then 50 ms, for over a minute.
        for id in 2..1300 {
            let report = request(SESSION, id, "GET", "api/v1/events");
            assert_eq!(rig.deliver(&report).await.len(), 1);
            let replies = rig.deliver(&fetch(SESSION, id)).await;
            assert_eq!(replies.len(), 1);
            assert_eq!(
                Frame::decode(&replies[0], INPUT_ID).unwrap().kind,
                Kind::Reply
            );
            assert!(rig.handle.has_clients());
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    #[tokio::test(start_paused = true)]
    async fn any_report_from_the_owner_keeps_the_subscription() {
        let mut rig = Rig::subscribed().await;
        // No event poll at all for a minute, only other traffic inside the
        // window: requests, fetches of a cached reply and hellos.
        for round in 0..15u32 {
            let report = match round % 3 {
                0 => request(SESSION, 2 + round, "GET", "api/v1/version"),
                1 => fetch(SESSION, 1 + round),
                _ => hello(SESSION),
            };
            tokio::time::sleep(CLIENT_IDLE - Duration::from_secs(1)).await;
            assert_eq!(rig.deliver(&report).await.len(), 1);
            assert!(rig.handle.has_clients(), "round {round}");
        }
        // The window still counts from the last report.
        assert!(
            rig.receive(CLIENT_IDLE - Duration::from_millis(1))
                .await
                .is_none()
        );
        assert!(rig.handle.has_clients());
        assert!(rig.receive(Duration::from_millis(2)).await.is_none());
        assert!(!rig.handle.has_clients());
    }

    #[tokio::test(start_paused = true)]
    async fn a_request_that_runs_longer_than_the_window_is_not_silence() {
        let mut rig = Rig::subscribed().await;
        let started = Clock::now();
        // The coordinator is not running, so this request waits out the
        // server's whole 15 s limit inside the API.
        let reply = call(
            &mut rig.server,
            SESSION,
            2,
            "POST",
            "api/v1/diagnostics/reset",
        )
        .await;
        assert_eq!(reply.status, 504);
        assert!(started.elapsed() >= Duration::from_secs(15));
        assert!(rig.handle.has_clients());
        // The next poll arrives 50 ms later, as the app sends it.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(
            rig.deliver(&request(SESSION, 3, "GET", "api/v1/events"))
                .await
                .len(),
            1
        );
        assert!(rig.handle.has_clients());
        // The window starts when the request ends, not when it was sent.
        assert!(
            rig.receive(CLIENT_IDLE - Duration::from_millis(1))
                .await
                .is_none()
        );
        assert!(rig.handle.has_clients());
    }

    /// A report sent from another thread after real time has passed.
    fn send_later(rig: &Rig, report: [u8; REPORT_SIZE]) -> std::thread::JoinHandle<()> {
        use std::os::fd::AsFd;
        let peer = rig.peer.as_fd().try_clone_to_owned().unwrap();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            let peer = std::os::unix::net::UnixDatagram::from(peer);
            // The descriptor is shared with tokio's socket, so it does not block.
            peer.send(&report).unwrap();
        })
    }

    #[tokio::test(start_paused = true)]
    async fn no_timer_is_armed_without_a_subscription() {
        let mut rig = Rig::new();
        assert_eq!(answer(&mut rig.server, &hello(SESSION)).await.0, Kind::Ack);
        // With the clock paused, an armed timer would move it while the test
        // waits for the report. It stays where it was: the wait is a plain recv.
        let before = Clock::now();
        let sender = send_later(&rig, hello(SESSION));
        let mut bytes = [0; REPORT_SIZE + 1];
        let n = rig.server.receive(&rig.socket, &mut bytes).await.unwrap();
        assert_eq!(n, REPORT_SIZE);
        assert_eq!(Clock::now(), before, "a timer ran with nobody subscribed");
        sender.join().unwrap();

        // The same wait with a subscription does arm the timer: the clock
        // moves to the expiry and the subscription is dropped.
        let mut rig = Rig::subscribed().await;
        let before = Clock::now();
        let sender = send_later(&rig, hello(SESSION));
        let n = rig.server.receive(&rig.socket, &mut bytes).await.unwrap();
        assert_eq!(n, REPORT_SIZE);
        assert_eq!(Clock::now(), before + CLIENT_IDLE);
        assert!(!rig.handle.has_clients());
        sender.join().unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn an_expiry_keeps_the_session_so_a_replay_is_still_refused() {
        let mut rig = Rig::subscribed().await;
        let cached = answer(&mut rig.server, &fetch(SESSION, 1)).await;
        assert_eq!(cached.0, Kind::Reply);
        assert!(rig.receive(CLIENT_IDLE * 2).await.is_none());
        assert!(!rig.handle.has_clients());
        // The admitted request ID is remembered: the same request does not
        // run a second time, and so does not subscribe again.
        let (kind, text) = answer(
            &mut rig.server,
            &request(SESSION, 1, "GET", "api/v1/events"),
        )
        .await;
        assert_eq!(kind, Kind::Error);
        assert_eq!(text, b"USB request was already admitted; not replayed");
        assert!(!rig.handle.has_clients());
        // Its cached reply can still be fetched, byte for byte.
        assert_eq!(answer(&mut rig.server, &fetch(SESSION, 1)).await, cached);
        // The session was not reset: its nonce is still the owner's.
        assert_eq!(
            answer(&mut rig.server, &hello(SESSION)).await,
            (Kind::Ack, b"RC2FX USB 3".to_vec())
        );
        assert_eq!(rig.server.session.last, 1);
    }

    #[tokio::test(start_paused = true)]
    async fn reports_from_others_do_not_keep_a_dead_subscription() {
        let mut rig = Rig::subscribed().await;
        // The owner is gone. A second app instance keeps saying hello and is
        // refused, and something else writes bytes that are not a frame.
        for second in 0..CLIENT_IDLE.as_secs() + 1 {
            let refused = rig.deliver(&hello(SESSION + 1)).await;
            assert_eq!(refused.len(), 1);
            assert_eq!(
                Frame::decode(&refused[0], INPUT_ID).unwrap().kind,
                Kind::Error
            );
            assert!(rig.deliver(&[0xa5; REPORT_SIZE]).await.is_empty());
            assert_eq!(rig.handle.has_clients(), second < CLIENT_IDLE.as_secs());
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        assert!(!rig.handle.has_clients());
    }

    #[tokio::test(start_paused = true)]
    async fn protocol_four_and_its_fetch_window_are_answered_through_the_server() {
        let mut rig = Rig::new();
        assert_eq!(
            answer(&mut rig.server, &hello_v4(SESSION)).await,
            (Kind::Ack, b"RC2FX USB 4 8".to_vec())
        );
        // A request is admitted as before, then the windowed fetch answers.
        assert_eq!(
            answer(&mut rig.server, &request(SESSION, 1, "GET", "api/v1/state"))
                .await
                .0,
            Kind::Ack
        );
        let replies = answers(&mut rig.server, &fetch_count(SESSION, 1, 0, 2)).await;
        assert!(!replies.is_empty());
        assert!(replies.iter().all(|(kind, _)| *kind == Kind::Reply));
        // One report more than the server's window is refused before fetching.
        let (kind, text) = answer(&mut rig.server, &fetch_count(SESSION, 1, 0, 9)).await;
        assert_eq!(kind, Kind::Error);
        assert_eq!(text, b"Invalid USB fetch window");
    }

    #[tokio::test(start_paused = true)]
    async fn a_queue_that_is_never_empty_does_not_put_the_expiry_off() {
        let mut rig = Rig::subscribed().await;
        tokio::time::sleep(CLIENT_IDLE).await;
        // A report is already waiting when the window ends.
        rig.peer.send(&hello(SESSION + 1)).await.unwrap();
        assert!(rig.receive(Duration::from_secs(1)).await.is_some());
        assert!(!rig.handle.has_clients());
    }

    #[tokio::test(start_paused = true)]
    async fn a_second_client_takes_the_session_and_the_first_subscription_goes() {
        let mut rig = Rig::subscribed().await;
        // Within the window the second instance is refused and the first
        // keeps its subscription.
        assert_eq!(
            answer(&mut rig.server, &hello(SESSION + 1)).await.0,
            Kind::Error
        );
        assert!(rig.handle.has_clients());
        // The takeover window runs on the real clock: age the session.
        rig.server.session.seen = Some(Instant::now() - CLIENT_IDLE);
        assert_eq!(
            answer(&mut rig.server, &hello(SESSION + 1)).await.0,
            Kind::Ack
        );
        assert!(!rig.handle.has_clients());
        // The new owner subscribes for itself and starts with the state.
        let first = call(&mut rig.server, SESSION + 1, 1, "GET", "api/v1/events").await;
        assert_eq!(first.body["event"], "state");
        assert!(rig.handle.has_clients());
        // The first instance is now a stranger: its reports are refused and
        // do not keep the new owner's subscription alive either.
        assert_eq!(
            answer(
                &mut rig.server,
                &request(SESSION, 2, "GET", "api/v1/events")
            )
            .await,
            (Kind::Error, b"USB session expired; reconnect".to_vec())
        );
    }
}

#[cfg(test)]
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
