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

/// How long a client may stay silent before it counts as gone. Another client
/// may then take the session over, and the silent client's event subscription
/// is dropped. The protocol has no goodbye, so silence is the only sign that
/// the app quit or crashed, the cable was pulled or the host went to sleep.
/// The app polls events about every 50 ms, so five seconds is never a pause.
const CLIENT_IDLE: Duration = Duration::from_secs(5);

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
                    .is_some_and(|seen| now.duration_since(seen) < CLIENT_IDLE)
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

/// One USB client's session and its view of the API.
#[cfg(target_os = "linux")]
pub(crate) struct Server {
    session: Session,
    api: Api,
    /// When the session's owner last sent a report or had a request answered.
    active: tokio::time::Instant,
}

#[cfg(target_os = "linux")]
impl Server {
    pub(crate) fn new(controller: Handle, options: Options) -> Self {
        Self {
            session: Session::default(),
            api: Api::new(controller, options),
            active: tokio::time::Instant::now(),
        }
    }

    /// Waits for the next report from the native library.
    ///
    /// A subscribed client that stays silent for `CLIENT_IDLE` loses its event
    /// subscription, so the controller stops polling the engine at the watched
    /// rate and stops serialising meter events for it. The session itself is
    /// kept: its admitted request ID and cached reply still refuse a replay.
    /// When the client returns, its next event poll subscribes again and
    /// starts with the current state.
    ///
    /// The deadline counts from the owner's last activity, not from the last
    /// datagram, so reports from another client or malformed reports do not
    /// keep a dead subscription alive. With no subscription there is no timer:
    /// an idle controller only waits on the socket.
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

    /// Handles one report and returns the report to answer with, or `None`
    /// for bytes that are not an RC2FX frame.
    pub(crate) async fn handle(&mut self, bytes: &[u8]) -> io::Result<Option<[u8; REPORT_SIZE]>> {
        let Some(frame) = Frame::decode(bytes, OUTPUT_ID) else {
            return Ok(None);
        };
        let reply = match self.session.accept(&frame, Instant::now()) {
            Action::Send(reply) => reply,
            Action::Reset(reply) => {
                self.api.reset();
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
                self.session.reply = match rc2fx_usb::pack(&json, MAX_REPLY) {
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
        // Only the session's owner keeps its subscription alive, and the time
        // a request took to run is not silence.
        if frame.session == self.session.nonce {
            self.active = tokio::time::Instant::now();
        }
        Ok(Some(reply))
    }
}

/// Writes one input report to the HID gadget.
#[cfg(target_os = "linux")]
async fn write_report(reply: &[u8; REPORT_SIZE]) -> io::Result<()> {
    use std::os::{fd::AsRawFd, unix::fs::OpenOptionsExt};
    use tokio::io::unix::AsyncFd;
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
}

#[cfg(target_os = "linux")]
pub async fn serve(path: &Path, controller: Handle, options: Options) -> io::Result<()> {
    use std::os::unix::{fs::PermissionsExt, net::UnixDatagram};
    // Do not unlink an existing listener (another daemon may own it).
    let socket = UnixDatagram::bind(path)?;
    let _cleanup = SocketFile::bound(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    socket.set_nonblocking(true)?;
    let socket = tokio::net::UnixDatagram::from_std(socket)?;
    let mut server = Server::new(controller, options);
    let mut write_errors = 0;
    let mut bytes = [0; REPORT_SIZE + 1];
    loop {
        let n = server.receive(&socket, &mut bytes).await?;
        let Some(reply) = server.handle(&bytes[..n]).await? else {
            continue;
        };
        // Shared gadget writes are serialized by the kernel. Wait asynchronously
        // when another writer owns the pending report; no audio thread waits here.
        let sent = tokio::time::timeout(Duration::from_secs(2), write_report(&reply)).await;
        if !matches!(sent, Ok(Ok(()))) {
            if write_errors < 3 {
                eprintln!("USB response write failed: {sent:?}");
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

/// A USB client for tests: builds the reports the native library forwards
/// and decodes the reports the server answers with.
#[cfg(all(test, target_os = "linux"))]
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
    /// The kind and payload of the report the server answered with.
    pub(crate) async fn answer(server: &mut Server, report: &[u8]) -> (Kind, Vec<u8>) {
        let reply = server
            .handle(report)
            .await
            .unwrap()
            .expect("an RC2FX frame is answered");
        let frame = Frame::decode(&reply, INPUT_ID).unwrap();
        (frame.kind, frame.data.to_vec())
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
            let reply = server.handle(&report).await.unwrap().unwrap();
            let frame = Frame::decode(&reply, INPUT_ID).unwrap();
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
#[cfg(all(test, target_os = "linux"))]
mod subscription_tests {
    use super::{testing::*, *};
    use crate::{controller::Controller, coordinator::Coordinator, store::Store};
    use tokio::time::Instant as Clock;

    const SESSION: u64 = 0x5eed;

    struct Rig {
        _directory: tempfile::TempDir,
        handle: Handle,
        /// Never run: a command sent to it is accepted and never answered.
        _owner: Coordinator<'static>,
        server: Server,
        socket: tokio::net::UnixDatagram,
        peer: tokio::net::UnixDatagram,
    }
    impl Rig {
        fn new() -> Self {
            let directory = tempfile::tempdir().unwrap();
            let store = Store::open(directory.path(), rc2fx_core::Config::default()).unwrap();
            let (handle, owner) = Coordinator::new(Controller::new(store), None).unwrap();
            let (socket, peer) = tokio::net::UnixDatagram::pair().unwrap();
            Self {
                _directory: directory,
                server: Server::new(handle.clone(), Options::default()),
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
        async fn deliver(&mut self, report: &[u8]) -> Option<[u8; REPORT_SIZE]> {
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
            assert!(rig.deliver(&report).await.is_some());
            let reply = rig.deliver(&fetch(SESSION, id)).await.unwrap();
            assert_eq!(Frame::decode(&reply, INPUT_ID).unwrap().kind, Kind::Reply);
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
            assert!(rig.deliver(&report).await.is_some());
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
        assert!(
            rig.deliver(&request(SESSION, 3, "GET", "api/v1/events"))
                .await
                .is_some()
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
            let refused = rig.deliver(&hello(SESSION + 1)).await.unwrap();
            assert_eq!(Frame::decode(&refused, INPUT_ID).unwrap().kind, Kind::Error);
            assert!(rig.deliver(&[0xa5; REPORT_SIZE]).await.is_none());
            assert_eq!(rig.handle.has_clients(), second < CLIENT_IDLE.as_secs());
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        assert!(!rig.handle.has_clients());
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
