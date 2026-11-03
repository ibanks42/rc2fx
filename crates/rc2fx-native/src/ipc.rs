//! Opt-in audit-namespace receiver. No audio buffer or Rust pointer crosses IPC.
use rc2fx_core::Target;
use rc2fx_native_control::{
    ContextStatus, ErrorCode, MAX_REQUEST, MAX_RESPONSE, Operation, PathMode, Reply, Request,
    Response, Session, TargetStatus, Transaction, VERSION,
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
/// One callback-side atomic check. Zero is the explicit standalone research
/// mode; required receivers must become ready before DSP is admitted.
pub struct Admission {
    state: AtomicU8,
    skipped: AtomicU64,
}
impl Admission {
    pub const fn new() -> Self {
        Self {
            state: AtomicU8::new(0),
            skipped: AtomicU64::new(0),
        }
    }
    pub fn require(&self) {
        self.state.store(1, Ordering::Release);
    }
    fn activate(&self) -> bool {
        self.state
            .compare_exchange(1, 2, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
    fn stop(&self) {
        self.state.store(3, Ordering::Release);
    }
    pub fn admit(&self) -> bool {
        if matches!(self.state.load(Ordering::Acquire), 0 | 2) {
            true
        } else {
            self.skipped.fetch_add(1, Ordering::Relaxed);
            false
        }
    }
    pub fn required(&self) -> bool {
        self.state.load(Ordering::Acquire) != 0
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
    fn status(&mut self, context: u64, id: &str) -> Result<TargetStatus, ErrorCode>;
    fn prepare(
        &mut self,
        transaction: &Transaction,
        target: &Target,
        mode: PathMode,
    ) -> Result<(), ErrorCode>;
    fn commit(&mut self, transaction: &Transaction) -> Result<(), ErrorCode>;
    fn cancel(&mut self, transaction: &Transaction) -> Result<(), ErrorCode>;
}

fn dispatch(session: &Session, request: Request, backend: &mut impl Backend) -> Response {
    let result = if request.version != VERSION {
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
            Operation::Status { context, id } => {
                if context == 0 || !rc2fx_core::valid_id(&id) {
                    Err(ErrorCode::InvalidParameters)
                } else {
                    backend
                        .status(context, &id)
                        .map(|target| Reply::Status { context, target })
                }
            }
            Operation::Prepare {
                transaction,
                target,
                mode,
            } => {
                if transaction.session != *session {
                    Err(ErrorCode::WrongSession)
                } else if !transaction.valid()
                    || transaction.target_id != target.id
                    || target.validate().is_err()
                {
                    Err(ErrorCode::InvalidParameters)
                } else {
                    backend
                        .prepare(&transaction, &target, mode)
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
    Response {
        version: VERSION,
        request_id: request.request_id,
        session: session.clone(),
        reply: result.unwrap_or_else(|code| Reply::Error { code }),
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
    // bind refuses an existing entry. Never unlink an old or unrelated socket.
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
                // Do not inherit an elevated policy from a vendor control caller.
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
                    // No timeout polling: no periodic wakeups while the app is closed.
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
    /// Control only. Keeps the handle on timeout; the caller must not unload a
    /// module with its receiver still executing. No callback ever joins this task.
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
    use rc2fx_native_control::Client;
    use std::os::unix::fs::DirBuilderExt;

    #[derive(Default)]
    struct Fake {
        mutations: usize,
        revision: u64,
        pending: Option<Transaction>,
        pending_mode: Option<PathMode>,
        mode: Option<PathMode>,
    }
    impl Backend for Fake {
        fn inventory(&mut self) -> Result<Vec<ContextStatus>, ErrorCode> {
            Ok(vec![ContextStatus {
                context: 7,
                targets: vec![self.status(7, "headphones")?],
                timing: None,
                counters: Default::default(),
                events: Vec::new(),
            }])
        }
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
            })
        }
        fn prepare(
            &mut self,
            transaction: &Transaction,
            target: &Target,
            mode: PathMode,
        ) -> Result<(), ErrorCode> {
            self.status(transaction.context, &target.id)?;
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
                target: Box::new(Target::new(
                    "headphones",
                    "Headphones",
                    TargetKind::Output,
                    2,
                )),
            },
        }
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
    fn actual_local_socket_reports_publication_separately_from_audio_and_cleans_up() {
        let folder = Folder::new();
        let path = folder.0.join("control.sock");
        let mut server = Server::start(&path, Fake::default(), None).unwrap();
        let mut client = Client::connect(&path).unwrap();
        let response = client.request(Operation::Inventory).unwrap();
        assert!(matches!(response.reply, Reply::Inventory { .. }));
        let target = Target::new("headphones", "Headphones", TargetKind::Output, 2);
        let transaction = client.transaction(7, &target.id, 2).unwrap();
        let response = client
            .request(Operation::Prepare {
                transaction: transaction.clone(),
                mode: PathMode::Processed,
                target: Box::new(target),
            })
            .unwrap();
        assert!(matches!(response.reply, Reply::Prepared { .. }));
        let response = client
            .request(Operation::Status {
                context: 7,
                id: "headphones".into(),
            })
            .unwrap();
        assert!(matches!(
            response.reply,
            Reply::Status {
                target: TargetStatus { published: 1, .. },
                ..
            }
        ));
        let response = client.request(Operation::Commit { transaction }).unwrap();
        assert!(matches!(response.reply, Reply::Published { .. }));
        let response = client
            .request(Operation::Status {
                context: 7,
                id: "headphones".into(),
            })
            .unwrap();
        match response.reply {
            Reply::Status { target, .. } => {
                assert_eq!(target.published, 2);
                assert_eq!(target.applied, 1);
            }
            _ => panic!("status reply required"),
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
                target: Box::new(target),
            })
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(server.shutdown());
        assert_eq!(server.status().received, 1); // Hello only.
    }
}
