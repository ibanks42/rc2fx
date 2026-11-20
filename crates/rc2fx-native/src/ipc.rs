//! Unix datagram control server running inside the audit namespace.
//! No audio buffers or raw pointers cross IPC.
use rc2fx_core::Target;
use rc2fx_native_control::{
    ContextStatus, ErrorCode, MAX_REQUEST, MAX_RESPONSE, MIN_VERSION, Operation, PathMode,
    RESET_POLICY_VERSION, Reply, Request, ResetPolicy, Response, Session, Transaction, VERSION,
};
use serde::Serialize;
use std::{
    fs, io,
    os::{
        fd::AsRawFd,
        unix::{
            ffi::OsStrExt,
            fs::{FileTypeExt, MetadataExt, PermissionsExt},
            net::UnixDatagram,
        },
    },
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
/// Monotonic state machine gating DSP execution behind the control server:
/// `Standalone -> Required -> Active -> Stopped` (or `Required -> Stopped`).
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Gate {
    /// No control socket configured (standalone/research mode); DSP runs immediately.
    Standalone = 0,
    /// Control socket required, waiting for server startup; DSP held off.
    Required = 1,
    /// Control server is live; DSP admitted.
    Active = 2,
    /// Control server stopped or failed; DSP permanently disabled.
    Stopped = 3,
}
impl Gate {
    const STANDALONE: u8 = Self::Standalone as u8;
    const REQUIRED: u8 = Self::Required as u8;
    const ACTIVE: u8 = Self::Active as u8;
    const STOPPED: u8 = Self::Stopped as u8;
    const fn from_u8(value: u8) -> Self {
        match value {
            Self::STANDALONE => Self::Standalone,
            Self::REQUIRED => Self::Required,
            Self::ACTIVE => Self::Active,
            _ => Self::Stopped,
        }
    }
}
/// Lock-free gate checked on every audio callback to ensure DSP only runs
/// while the control server is active (when required).
pub struct Admission {
    state: AtomicU8,
    skipped: AtomicU64,
}
impl Admission {
    pub const fn new() -> Self {
        Self {
            state: AtomicU8::new(Gate::STANDALONE),
            skipped: AtomicU64::new(0),
        }
    }
    /// Transitions `Standalone -> Required`; no-op once active or stopped.
    pub fn require(&self) {
        let _ = self.state.compare_exchange(
            Gate::STANDALONE,
            Gate::REQUIRED,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }
    fn activate(&self) -> bool {
        self.state
            .compare_exchange(
                Gate::REQUIRED,
                Gate::ACTIVE,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }
    fn stop(&self) {
        self.state.store(Gate::STOPPED, Ordering::Release);
    }
    pub fn admit(&self) -> bool {
        if matches!(
            self.state.load(Ordering::Acquire),
            Gate::STANDALONE | Gate::ACTIVE
        ) {
            true
        } else {
            self.skipped.fetch_add(1, Ordering::Relaxed);
            false
        }
    }
    pub fn required(&self) -> bool {
        self.gate() != Gate::Standalone
    }
    fn gate(&self) -> Gate {
        Gate::from_u8(self.state.load(Ordering::Acquire))
    }
    pub fn skipped(&self) -> u64 {
        self.skipped.load(Ordering::Relaxed)
    }
}
struct AdmissionLease(Option<&'static Admission>);
impl Default for Admission {
    fn default() -> Self {
        Self::new()
    }
}
impl Drop for AdmissionLease {
    fn drop(&mut self) {
        if let Some(gate) = self.0 {
            gate.stop();
        }
    }
}

pub trait Backend: Send + 'static {
    fn inventory(&mut self) -> Result<Vec<ContextStatus>, ErrorCode>;
    fn prepare(
        &mut self,
        transaction: &Transaction,
        target: &Target,
        mode: PathMode,
        reset: ResetPolicy,
    ) -> Result<(), ErrorCode>;
    fn commit(&mut self, transaction: &Transaction) -> Result<(), ErrorCode>;
    fn cancel(&mut self, transaction: &Transaction) -> Result<(), ErrorCode>;
}

/// First protocol version that supports `recent_callback_wall_ns` and
/// `worker_recent_max_ns` on inventory replies.
const RECENT_MAXIMA_VERSION: u32 = 7;

fn omit_fields_unknown_to(version: u32, reply: &mut Reply) {
    if version >= RECENT_MAXIMA_VERSION {
        return;
    }
    if let Reply::Inventory { contexts } = reply {
        for context in contexts {
            context.recent_callback_wall_ns = None;
            for target in &mut context.targets {
                target.worker_recent_max_ns = None;
            }
        }
    }
}

fn dispatch(session: &Session, request: Request, backend: &mut impl Backend) -> Response {
    // Reply at the peer's negotiated version if supported, otherwise reject at ours.
    let supported = (MIN_VERSION..=VERSION).contains(&request.version);
    let result = if !supported {
        Err(ErrorCode::UnsupportedVersion)
    } else if request.request_id == 0 {
        Err(ErrorCode::InvalidParameters)
    } else if !matches!(&request.operation, Operation::Hello)
        && request.session.as_ref() != Some(session)
    {
        Err(ErrorCode::WrongSession)
    } else {
        let committing = matches!(&request.operation, Operation::Commit { .. });
        match request.operation {
            Operation::Hello => Ok(Reply::Hello),
            Operation::Inventory => backend
                .inventory()
                .map(|contexts| Reply::Inventory { contexts }),
            // Deprecated; use `Operation::Inventory` instead.
            Operation::Status { .. } => Err(ErrorCode::InvalidParameters),
            Operation::Prepare {
                transaction,
                target,
                mode,
                reset,
            } => {
                if transaction.session != *session {
                    Err(ErrorCode::WrongSession)
                } else if !transaction.valid()
                    || transaction.target_id != target.id
                    || target.validate().is_err()
                    || (reset != ResetPolicy::Allow && request.version < RESET_POLICY_VERSION)
                {
                    Err(ErrorCode::InvalidParameters)
                } else {
                    backend
                        .prepare(&transaction, &target, mode, reset)
                        .map(|()| Reply::Prepared { transaction })
                }
            }
            Operation::Commit { transaction } | Operation::Cancel { transaction } => {
                if transaction.session != *session {
                    Err(ErrorCode::WrongSession)
                } else if !transaction.valid() {
                    Err(ErrorCode::InvalidParameters)
                } else if committing {
                    backend
                        .commit(&transaction)
                        .map(|()| Reply::Published { transaction })
                } else {
                    backend
                        .cancel(&transaction)
                        .map(|()| Reply::Cancelled { transaction })
                }
            }
        }
    };
    let mut reply = result.unwrap_or_else(|code| Reply::Error { code });
    if supported {
        omit_fields_unknown_to(request.version, &mut reply);
    }
    Response {
        version: if supported { request.version } else { VERSION },
        request_id: request.request_id,
        session: session.clone(),
        reply,
    }
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
struct SocketFile {
    path: PathBuf,
    device: u64,
    inode: u64,
}
impl Drop for SocketFile {
    fn drop(&mut self) {
        if let Ok(metadata) = fs::symlink_metadata(&self.path)
            && metadata.file_type().is_socket()
            && (metadata.dev(), metadata.ino()) == (self.device, self.inode)
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}
fn bind(path: &Path) -> io::Result<(UnixDatagram, SocketFile)> {
    if !path.is_absolute() || path.as_os_str().as_bytes().len() > 103 {
        return Err(invalid("absolute bounded socket path required"));
    }
    let parent = path
        .parent()
        .ok_or_else(|| invalid("socket parent required"))?;
    let parent_metadata = fs::symlink_metadata(parent)?;
    if !parent_metadata.is_dir()
        || parent_metadata.uid() != unsafe { libc::geteuid() }
        || parent_metadata.mode() & 0o077 != 0
    {
        return Err(invalid(
            "socket parent must be an existing private directory",
        ));
    }
    // `bind` fails if `path` already exists; never unlink an existing socket.
    let socket = UnixDatagram::bind(path)?;
    let metadata = fs::symlink_metadata(path)?;
    let owned = SocketFile {
        path: path.into(),
        device: metadata.dev(),
        inode: metadata.ino(),
    };
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    socket.set_nonblocking(true)?;
    Ok((socket, owned))
}

#[derive(Default)]
struct Counters {
    received: AtomicU64,
    malformed: AtomicU64,
    replied: AtomicU64,
    transport_errors: AtomicU64,
    started: AtomicBool,
    failed: AtomicBool,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerStatus {
    pub received: u64,
    pub malformed: u64,
    pub replied: u64,
    pub transport_errors: u64,
    pub started: bool,
    pub failed: bool,
}
pub struct Server {
    stop: Arc<AtomicBool>,
    wake: UnixDatagram,
    thread: Option<JoinHandle<()>>,
    counters: Arc<Counters>,
    gate: Option<&'static Admission>,
}
impl Server {
    pub fn start(
        path: &Path,
        mut backend: impl Backend,
        gate: Option<&'static Admission>,
    ) -> io::Result<Self> {
        if let Some(gate) = gate {
            gate.require();
        }
        let admission = AdmissionLease(gate);
        let (socket, owned) = bind(path)?;
        let session = Session::local()?;
        let (wake, wait) = UnixDatagram::pair()?;
        wake.set_nonblocking(true)?;
        wait.set_nonblocking(true)?;
        let stop = Arc::new(AtomicBool::new(false));
        let counters = Arc::new(Counters::default());
        let stopping = Arc::clone(&stop);
        let counts = Arc::clone(&counters);
        let thread = thread::Builder::new()
            .name("rc2fx-native-control".into())
            .spawn(move || {
                let _owned = owned;
                let _admission = admission;
                // Drop any elevated scheduling policy inherited from the caller.
                let policy = libc::sched_param { sched_priority: 0 };
                if unsafe {
                    libc::pthread_setschedparam(libc::pthread_self(), libc::SCHED_OTHER, &policy)
                } != 0
                {
                    counts.failed.store(true, Ordering::Release);
                    return;
                }
                let mut buffer = vec![0_u8; MAX_REQUEST + 1];
                if gate.is_some_and(|gate| !gate.activate()) {
                    return;
                }
                counts.started.store(true, Ordering::Release);
                while !stopping.load(Ordering::Acquire) {
                    let mut poll = [
                        libc::pollfd {
                            fd: socket.as_raw_fd(),
                            events: libc::POLLIN,
                            revents: 0,
                        },
                        libc::pollfd {
                            fd: wait.as_raw_fd(),
                            events: libc::POLLIN,
                            revents: 0,
                        },
                    ];
                    // Block indefinitely so the server doesn't wake up while idle.
                    let ready = unsafe { libc::poll(poll.as_mut_ptr(), 2, -1) };
                    if ready < 0 {
                        if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                            continue;
                        }
                        counts.failed.store(true, Ordering::Release);
                        break;
                    }
                    if stopping.load(Ordering::Acquire) || poll[1].revents != 0 {
                        break;
                    }
                    if poll[0].revents & libc::POLLIN == 0 {
                        counts.failed.store(true, Ordering::Release);
                        break;
                    }
                    let (length, peer) = match socket.recv_from(&mut buffer) {
                        Ok(message) => message,
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => continue,
                        Err(_) => {
                            counts.transport_errors.fetch_add(1, Ordering::Relaxed);
                            continue;
                        }
                    };
                    counts.received.fetch_add(1, Ordering::Relaxed);
                    if length > MAX_REQUEST {
                        counts.malformed.fetch_add(1, Ordering::Relaxed);
                        continue;
                    }
                    let request = match serde_json::from_slice(&buffer[..length]) {
                        Ok(request) => request,
                        Err(_) => {
                            counts.malformed.fetch_add(1, Ordering::Relaxed);
                            continue;
                        }
                    };
                    let response = dispatch(&session, request, &mut backend);
                    match serde_json::to_vec(&response) {
                        Ok(bytes)
                            if bytes.len() <= MAX_RESPONSE
                                && socket.send_to_addr(&bytes, &peer).ok() == Some(bytes.len()) =>
                        {
                            counts.replied.fetch_add(1, Ordering::Relaxed);
                        }
                        _ => {
                            counts.transport_errors.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
            })?;
        Ok(Self {
            stop,
            wake,
            thread: Some(thread),
            counters,
            gate,
        })
    }
    /// Control-only shutdown. Retains the join handle on timeout so callers
    /// know the server thread is still running.
    pub fn shutdown(&mut self) -> bool {
        if let Some(gate) = self.gate {
            gate.stop();
        }
        self.stop.store(true, Ordering::Release);
        let _ = self.wake.send(&[1]);
        let deadline = Instant::now() + Duration::from_secs(1);
        while self
            .thread
            .as_ref()
            .is_some_and(|thread| !thread.is_finished())
        {
            if Instant::now() >= deadline {
                self.counters.failed.store(true, Ordering::Release);
                return false;
            }
            thread::sleep(Duration::from_millis(1));
        }
        let joined = self
            .thread
            .take()
            .is_none_or(|thread| thread.join().is_ok());
        if !joined {
            self.counters.failed.store(true, Ordering::Release);
        }
        joined
    }
    pub fn status(&self) -> ServerStatus {
        ServerStatus {
            received: self.counters.received.load(Ordering::Relaxed),
            malformed: self.counters.malformed.load(Ordering::Relaxed),
            replied: self.counters.replied.load(Ordering::Relaxed),
            transport_errors: self.counters.transport_errors.load(Ordering::Relaxed),
            started: self.counters.started.load(Ordering::Acquire),
            failed: self.counters.failed.load(Ordering::Acquire),
        }
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        if let Some(gate) = self.gate {
            gate.stop();
        }
        self.stop.store(true, Ordering::Release);
        let _ = self.wake.send(&[1]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rc2fx_core::TargetKind;
    use rc2fx_native_control::{Client, TargetStatus};
    use std::os::unix::fs::DirBuilderExt;

    #[derive(Default)]
    struct Fake {
        mutations: usize,
        revision: u64,
        pending: Option<Transaction>,
        pending_mode: Option<PathMode>,
        mode: Option<PathMode>,
        /// The reset policy of the last prepare that reached the backend.
        reset: Option<ResetPolicy>,
    }
    impl Fake {
        /// The one target's status, as the inventory reports it.
        fn status(&mut self, context: u64, id: &str) -> Result<TargetStatus, ErrorCode> {
            if context != 7 || id != "headphones" {
                return Err(ErrorCode::Unavailable);
            }
            Ok(TargetStatus {
                id: id.into(),
                published: self.revision.max(1),
                seen: 1,
                applied: 1,
                fingerprint: [0; 32],
                mode: self.mode.unwrap_or(PathMode::Processed),
                runtime_epoch: if self.mode == Some(PathMode::Passthrough) {
                    0
                } else {
                    1
                },
                retired_workers: Vec::new(),
                retired_worker_count: 0,
                meter: None,
                worker: None,
                misses: None,
                lates: None,
                // The v7-only field, which a reply to a v6 peer must omit.
                worker_recent_max_ns: Some(7_000),
            })
        }
    }
    impl Backend for Fake {
        fn inventory(&mut self) -> Result<Vec<ContextStatus>, ErrorCode> {
            Ok(vec![ContextStatus {
                context: 7,
                targets: vec![self.status(7, "headphones")?],
                timing: None,
                counters: Default::default(),
                events: Vec::new(),
                // The v7-only fields, which a reply to a v6 peer must omit.
                recent_callback_wall_ns: Some(900),
            }])
        }
        fn prepare(
            &mut self,
            transaction: &Transaction,
            target: &Target,
            mode: PathMode,
            reset: ResetPolicy,
        ) -> Result<(), ErrorCode> {
            self.status(transaction.context, &target.id)?;
            self.reset = Some(reset);
            if transaction.revision <= self.revision.max(1) {
                return Err(ErrorCode::StaleRevision);
            }
            self.pending = Some(transaction.clone());
            self.pending_mode = Some(mode);
            Ok(())
        }
        fn commit(&mut self, transaction: &Transaction) -> Result<(), ErrorCode> {
            if self.pending.as_ref() != Some(transaction) {
                return Err(ErrorCode::Unavailable);
            }
            self.mutations += 1;
            self.revision = transaction.revision;
            self.mode = self.pending_mode.take();
            self.pending = None;
            Ok(())
        }
        fn cancel(&mut self, transaction: &Transaction) -> Result<(), ErrorCode> {
            if self.pending.as_ref() == Some(transaction) {
                self.pending = None;
                self.pending_mode = None;
            }
            Ok(())
        }
    }
    fn session(nonce: &str) -> Session {
        Session {
            pid: 42,
            nonce: nonce.into(),
        }
    }
    fn prepare(session: Session) -> Request {
        let transaction = Transaction {
            session: session.clone(),
            id: "a".repeat(32),
            context: 7,
            revision: 2,
            target_id: "headphones".into(),
        };
        Request {
            version: VERSION,
            request_id: 1,
            session: Some(session),
            operation: Operation::Prepare {
                transaction,
                mode: PathMode::Processed,
                reset: ResetPolicy::Allow,
                target: Box::new(Target::new(
                    "headphones",
                    "Headphones",
                    TargetKind::Output,
                    2,
                )),
            },
        }
    }
    /// The reset policy reaches the backend, and only from a peer that
    /// negotiated a version that knows it.
    #[test]
    fn a_refusing_prepare_needs_the_version_that_defines_it() {
        let current = session(&"a".repeat(32));
        let refusing = |version| {
            let mut request = prepare(current.clone());
            request.version = version;
            let Operation::Prepare { reset, .. } = &mut request.operation else {
                unreachable!();
            };
            *reset = ResetPolicy::Refuse;
            request
        };
        let mut backend = Fake::default();
        assert!(matches!(
            dispatch(&current, refusing(VERSION), &mut backend).reply,
            Reply::Prepared { .. }
        ));
        assert_eq!(backend.reset, Some(ResetPolicy::Refuse));
        let mut older = Fake::default();
        assert!(matches!(
            dispatch(&current, refusing(RESET_POLICY_VERSION - 1), &mut older).reply,
            Reply::Error {
                code: ErrorCode::InvalidParameters
            }
        ));
        assert_eq!(older.reset, None, "never reached the backend");
        // An Allow prepare from an older peer is what it always was.
        let mut request = prepare(current.clone());
        request.version = RESET_POLICY_VERSION - 1;
        assert!(matches!(
            dispatch(&current, request, &mut older).reply,
            Reply::Prepared { .. }
        ));
        assert_eq!(older.reset, Some(ResetPolicy::Allow));
    }
    #[test]
    fn requested_path_mode_changes_only_after_commit() {
        let current = session(&"a".repeat(32));
        let mut request = prepare(current.clone());
        let Operation::Prepare {
            transaction, mode, ..
        } = &mut request.operation
        else {
            unreachable!();
        };
        *mode = PathMode::Passthrough;
        let transaction = transaction.clone();
        let mut backend = Fake::default();
        assert!(matches!(
            dispatch(&current, request, &mut backend).reply,
            Reply::Prepared { .. }
        ));
        assert_eq!(
            backend.status(7, "headphones").unwrap().mode,
            PathMode::Processed
        );
        let committed = dispatch(
            &current,
            Request {
                version: VERSION,
                request_id: 2,
                session: Some(current.clone()),
                operation: Operation::Commit { transaction },
            },
            &mut backend,
        );
        assert!(matches!(committed.reply, Reply::Published { .. }));
        let status = backend.status(7, "headphones").unwrap();
        assert_eq!(status.mode, PathMode::Passthrough);
        assert_eq!(status.runtime_epoch, 0);
    }
    #[test]
    fn stale_process_session_is_rejected_before_backend_mutation() {
        let mut backend = Fake::default();
        let response = dispatch(&session("new"), prepare(session("old")), &mut backend);
        assert!(matches!(
            response.reply,
            Reply::Error {
                code: ErrorCode::WrongSession
            }
        ));
        assert_eq!(backend.mutations, 0);
    }
    #[test]
    fn unsupported_version_and_zero_request_id_do_not_mutate_backend() {
        let mut backend = Fake::default();
        let mut request = prepare(session("current"));
        request.version = VERSION + 1;
        assert!(matches!(
            dispatch(&session("current"), request, &mut backend).reply,
            Reply::Error {
                code: ErrorCode::UnsupportedVersion
            }
        ));
        let mut request = prepare(session("current"));
        request.request_id = 0;
        assert!(matches!(
            dispatch(&session("current"), request, &mut backend).reply,
            Reply::Error {
                code: ErrorCode::InvalidParameters
            }
        ));
        assert_eq!(backend.mutations, 0);
    }
    #[test]
    fn a_status_request_is_refused_without_touching_the_backend() {
        let mut backend = Fake::default();
        let request = Request {
            version: VERSION,
            request_id: 1,
            session: Some(session("current")),
            operation: Operation::Status {
                context: 7,
                id: "headphones".into(),
            },
        };
        assert!(matches!(
            dispatch(&session("current"), request, &mut backend).reply,
            Reply::Error {
                code: ErrorCode::InvalidParameters
            }
        ));
        assert_eq!(backend.mutations, 0);
    }
    #[test]
    fn reconnecting_cannot_rebind_an_old_receipt_to_a_new_process_session() {
        let current = session(&"a".repeat(32));
        let transaction = Transaction {
            session: session(&"b".repeat(32)),
            id: "c".repeat(32),
            context: 7,
            revision: 2,
            target_id: "headphones".into(),
        };
        let mut backend = Fake::default();
        for operation in [
            Operation::Prepare {
                transaction: transaction.clone(),
                mode: PathMode::Processed,
                reset: ResetPolicy::Allow,
                target: Box::new(Target::new(
                    "headphones",
                    "Headphones",
                    TargetKind::Output,
                    2,
                )),
            },
            Operation::Commit {
                transaction: transaction.clone(),
            },
            Operation::Cancel { transaction },
        ] {
            let response = dispatch(
                &current,
                Request {
                    version: VERSION,
                    request_id: 1,
                    session: Some(current.clone()),
                    operation,
                },
                &mut backend,
            );
            assert!(matches!(
                response.reply,
                Reply::Error {
                    code: ErrorCode::WrongSession
                }
            ));
        }
        assert_eq!(backend.mutations, 0);
        assert!(backend.pending.is_none());
    }
    #[test]
    fn strict_request_envelope_refuses_unknown_and_duplicate_fields() {
        let extra = br#"{"version":1,"requestId":1,"session":null,"operation":{"op":"hello"},"obsolete":true}"#;
        let duplicate = br#"{"version":1,"requestId":1,"requestId":2,"session":null,"operation":{"op":"hello"}}"#;
        assert!(serde_json::from_slice::<Request>(extra).is_err());
        assert!(serde_json::from_slice::<Request>(duplicate).is_err());
    }
    struct Folder(PathBuf);
    impl Folder {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "rc2fx-native-ipc-{}-{}",
                std::process::id(),
                Session::local().unwrap().nonce
            ));
            fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Folder {
        fn drop(&mut self) {
            // Only remove our newly created directory if it is now empty.
            let _ = fs::remove_dir(&self.0);
        }
    }
    #[test]
    fn requested_receiver_admits_dsp_only_while_ready_and_cannot_reactivate_after_stop() {
        static GATE: Admission = Admission::new();
        let folder = Folder::new();
        let path = folder.0.join("control.sock");
        GATE.require();
        assert!(!GATE.admit());
        let mut server = Server::start(&path, Fake::default(), Some(&GATE)).unwrap();
        let _client = Client::connect(&path).unwrap();
        assert!(GATE.admit());
        assert!(server.shutdown());
        assert!(!GATE.admit());
        assert!(!GATE.activate());
        assert!(GATE.required());
        assert!(GATE.skipped() >= 2);
        fs::write(&path, b"existing owned entry").unwrap();
        assert!(Server::start(&path, Fake::default(), Some(&GATE)).is_err());
        assert!(!GATE.admit());
        assert!(!GATE.activate());
        assert_eq!(fs::read(&path).unwrap(), b"existing owned entry");
        fs::remove_file(path).unwrap();
    }
    #[test]
    fn a_stopped_gate_cannot_be_armed_again() {
        let gate = Admission::new();
        assert_eq!(gate.gate(), Gate::Standalone);
        assert!(gate.admit());
        gate.require();
        assert_eq!(gate.gate(), Gate::Required);
        assert!(gate.activate());
        // Requiring an active gate again leaves DSP admitted.
        gate.require();
        assert_eq!(gate.gate(), Gate::Active);
        assert!(gate.admit());
        gate.stop();
        gate.require();
        assert_eq!(gate.gate(), Gate::Stopped);
        assert!(!gate.activate());
        assert!(!gate.admit());
        assert!(gate.required());
    }
    #[test]
    fn every_gate_value_decodes() {
        for gate in [
            Gate::Standalone,
            Gate::Required,
            Gate::Active,
            Gate::Stopped,
        ] {
            assert_eq!(Gate::from_u8(gate as u8), gate);
        }
        for value in 4..=u8::MAX {
            assert_eq!(Gate::from_u8(value), Gate::Stopped);
        }
    }
    #[test]
    fn actual_local_socket_reports_publication_separately_from_audio_and_cleans_up() {
        let folder = Folder::new();
        let path = folder.0.join("control.sock");
        let mut server = Server::start(&path, Fake::default(), None).unwrap();
        let mut client = Client::connect(&path).unwrap();
        assert_eq!(client.protocol(), VERSION);
        let response = client.request(Operation::Inventory).unwrap();
        assert!(matches!(response.reply, Reply::Inventory { .. }));
        let target = Target::new("headphones", "Headphones", TargetKind::Output, 2);
        let transaction = client.transaction(7, &target.id, 2).unwrap();
        let response = client
            .request(Operation::Prepare {
                transaction: transaction.clone(),
                mode: PathMode::Processed,
                reset: ResetPolicy::Allow,
                target: Box::new(target),
            })
            .unwrap();
        assert!(matches!(response.reply, Reply::Prepared { .. }));
        let response = client.request(Operation::Inventory).unwrap();
        match response.reply {
            Reply::Inventory { contexts } => assert_eq!(contexts[0].targets[0].published, 1),
            _ => panic!("inventory reply required"),
        }
        let response = client.request(Operation::Commit { transaction }).unwrap();
        assert!(matches!(response.reply, Reply::Published { .. }));
        let response = client.request(Operation::Inventory).unwrap();
        match response.reply {
            Reply::Inventory { contexts } => {
                let target = &contexts[0].targets[0];
                assert_eq!(target.published, 2);
                assert_eq!(target.applied, 1);
            }
            _ => panic!("inventory reply required"),
        }
        assert!(server.shutdown());
        let status = server.status();
        assert!(status.started && !status.failed);
        assert_eq!(status.received, 6);
        assert_eq!(status.replied, 6);
        assert_eq!(status.malformed, 0);
        assert_eq!(status.transport_errors, 0);
        assert!(!path.exists());
    }
    #[test]
    fn existing_socket_entry_is_never_replaced() {
        let folder = Folder::new();
        let path = folder.0.join("control.sock");
        fs::write(&path, b"owned by another operation").unwrap();
        assert!(Server::start(&path, Fake::default(), None).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"owned by another operation");
        fs::remove_file(path).unwrap();
    }
    #[test]
    fn cleanup_preserves_a_replacement_entry() {
        let folder = Folder::new();
        let path = folder.0.join("control.sock");
        let mut server = Server::start(&path, Fake::default(), None).unwrap();
        fs::remove_file(&path).unwrap();
        fs::write(&path, b"replacement").unwrap();
        assert!(server.shutdown());
        assert_eq!(fs::read(&path).unwrap(), b"replacement");
        fs::remove_file(path).unwrap();
    }
    #[test]
    fn oversized_client_message_is_refused_before_send() {
        let folder = Folder::new();
        let path = folder.0.join("control.sock");
        let mut server = Server::start(&path, Fake::default(), None).unwrap();
        let mut client = Client::connect(&path).unwrap();
        let mut target = Target::new("headphones", "Headphones", TargetKind::Output, 2);
        target.name = "x".repeat(MAX_REQUEST + 1);
        let transaction = client.transaction(7, &target.id, 2).unwrap();
        let error = client
            .request(Operation::Prepare {
                transaction,
                mode: PathMode::Processed,
                reset: ResetPolicy::Allow,
                target: Box::new(target),
            })
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(server.shutdown());
        assert_eq!(server.status().received, 1); // Hello only.
    }
    #[test]
    fn older_peers_are_answered_at_their_version_without_the_fields_they_lack() {
        let current = session(&"a".repeat(32));
        let mut backend = Fake::default();
        for version in [MIN_VERSION, VERSION] {
            let request = |operation| Request {
                version,
                request_id: 1,
                session: Some(current.clone()),
                operation,
            };
            let newer = version >= RECENT_MAXIMA_VERSION;
            let response = dispatch(&current, request(Operation::Inventory), &mut backend);
            assert_eq!(response.version, version);
            assert_eq!(response.request_id, 1);
            let json = serde_json::to_string(&response).unwrap();
            for field in ["recentCallbackWallNs", "workerRecentMaxNs"] {
                assert_eq!(
                    json.contains(field),
                    newer,
                    "{field} at version {version}: {json}"
                );
            }
            assert_eq!(json.contains("900"), newer, "{json}");
            assert_eq!(json.contains("7000"), newer, "{json}");
        }
    }
    #[test]
    fn only_the_supported_protocol_range_is_answered() {
        let current = session(&"a".repeat(32));
        let mut backend = Fake::default();
        for version in [MIN_VERSION - 1, VERSION + 1] {
            let response = dispatch(
                &current,
                Request {
                    version,
                    request_id: 1,
                    session: None,
                    operation: Operation::Hello,
                },
                &mut backend,
            );
            assert!(matches!(
                response.reply,
                Reply::Error {
                    code: ErrorCode::UnsupportedVersion
                }
            ));
            assert_eq!(response.version, VERSION);
        }
        for version in [MIN_VERSION, VERSION] {
            let response = dispatch(
                &current,
                Request {
                    version,
                    request_id: 1,
                    session: None,
                    operation: Operation::Hello,
                },
                &mut backend,
            );
            assert!(matches!(response.reply, Reply::Hello));
            assert_eq!(response.version, version);
        }
    }
    #[test]
    fn client_negotiates_down_to_a_library_that_speaks_only_v6() {
        let folder = Folder::new();
        let path = folder.0.join("control.sock");
        let socket = UnixDatagram::bind(&path).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let session = Session::local().unwrap();
        let peer_session = session.clone();
        let inventory = serde_json::json!({
            "result": "inventory",
            "contexts": [{
                "context": 42,
                "timing": null,
                "targets": [{
                    "id": "headphones",
                    "mode": "processed",
                    "runtimeEpoch": 1,
                    "retiredWorkers": [],
                    "retiredWorkerCount": 0,
                    "published": 1,
                    "seen": 1,
                    "applied": 1,
                    "fingerprint": vec![0_u8; 32],
                    "meter": null,
                    "worker": null,
                }],
            }],
        });
        let server = thread::spawn(move || {
            let mut buffer = vec![0; MAX_REQUEST + 1];
            let mut seen = Vec::new();
            for (request_version, reply) in [
                (
                    VERSION,
                    serde_json::json!({"result": "error", "code": "unsupportedVersion"}),
                ),
                (MIN_VERSION, serde_json::json!({"result": "hello"})),
                (MIN_VERSION, inventory),
            ] {
                let (length, peer) = socket.recv_from(&mut buffer).unwrap();
                let request: Request = serde_json::from_slice(&buffer[..length]).unwrap();
                assert_eq!(request.version, request_version);
                seen.push(request.version);
                // A version 6 library answers at its own version whatever the
                // request said.
                let response = serde_json::json!({
                    "version": MIN_VERSION,
                    "requestId": request.request_id,
                    "session": {
                        "pid": peer_session.pid,
                        "nonce": peer_session.nonce.as_str(),
                    },
                    "reply": reply,
                });
                socket
                    .send_to_addr(&serde_json::to_vec(&response).unwrap(), &peer)
                    .unwrap();
            }
            seen
        });
        let mut client = Client::connect(&path).unwrap();
        assert_eq!(client.protocol(), MIN_VERSION);
        let response = client.request(Operation::Inventory).unwrap();
        assert_eq!(response.version, MIN_VERSION);
        match response.reply {
            Reply::Inventory { contexts } => {
                assert_eq!(contexts[0].context, 42);
                // A version 6 library never sends the fields 7 added; they
                // arrive as `None`, which the app shows as unknown.
                assert_eq!(contexts[0].recent_callback_wall_ns, None);
                assert_eq!(contexts[0].targets[0].worker_recent_max_ns, None);
            }
            reply => panic!("inventory expected, got {reply:?}"),
        }
        assert_eq!(
            server.join().unwrap(),
            vec![VERSION, MIN_VERSION, MIN_VERSION]
        );
        fs::remove_file(path).unwrap();
    }
    #[test]
    fn a_library_newer_than_this_daemon_fails_the_connection() {
        let folder = Folder::new();
        let path = folder.0.join("control.sock");
        let socket = UnixDatagram::bind(&path).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let session = Session::local().unwrap();
        let server = thread::spawn(move || {
            let mut buffer = vec![0; MAX_REQUEST + 1];
            let (length, peer) = socket.recv_from(&mut buffer).unwrap();
            let request: Request = serde_json::from_slice(&buffer[..length]).unwrap();
            // A library built to demand exact equality refuses 7 and answers
            // at its own, newer version.
            let response = serde_json::json!({
                "version": VERSION + 1,
                "requestId": request.request_id,
                "session": {"pid": session.pid, "nonce": session.nonce.as_str()},
                "reply": {"result": "error", "code": "unsupportedVersion"},
            });
            socket
                .send_to_addr(&serde_json::to_vec(&response).unwrap(), &peer)
                .unwrap();
        });
        let error = match Client::connect(&path) {
            Ok(_) => panic!("a newer library must not be accepted"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), io::ErrorKind::Other);
        server.join().unwrap();
        fs::remove_file(path).unwrap();
    }
}
