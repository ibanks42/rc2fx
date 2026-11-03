//! Blocking device-local control client. No audio/runtime or allocator hooks.
use crate::{
    MAX_REQUEST, MAX_RESPONSE, Operation, Reply, Request, Response, Session, Transaction, VERSION,
};
use std::{
    io,
    os::{
        linux::net::SocketAddrExt,
        unix::net::{SocketAddr, UnixDatagram},
    },
    path::Path,
    time::{Duration, Instant},
};
const TIMEOUT: Duration = Duration::from_millis(500);
impl Session {
    pub fn local() -> io::Result<Self> {
        Ok(Self {
            pid: std::process::id(),
            nonce: nonce()?,
        })
    }
}
fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
fn nonce() -> io::Result<String> {
    let mut bytes = [0_u8; 16];
    let mut filled = 0;
    for _ in 0..32 {
        if filled == bytes.len() {
            break;
        }
        let count = unsafe {
            libc::getrandom(
                bytes[filled..].as_mut_ptr().cast(),
                bytes.len() - filled,
                libc::GRND_NONBLOCK,
            )
        };
        if count > 0 {
            filled += count as usize;
        } else if count < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
            continue;
        } else {
            return Err(io::Error::other("control session entropy unavailable"));
        }
    }
    if filled != bytes.len() {
        return Err(io::Error::other("control session entropy interrupted"));
    }
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}
pub struct Client {
    socket: UnixDatagram,
    next: u64,
    session: Option<Session>,
    buffer: Box<[u8]>,
}
impl Client {
    pub fn session(&self) -> Option<&Session> {
        self.session.as_ref()
    }
    /// A receipt pins this connection's exact native process session even when
    /// retried through a later connection. It contains no address or audio data.
    pub fn transaction(
        &self,
        context: u64,
        target_id: &str,
        revision: u64,
    ) -> io::Result<Transaction> {
        let transaction = Transaction {
            session: self
                .session
                .clone()
                .ok_or_else(|| invalid("native hello required"))?,
            id: nonce()?,
            context,
            target_id: target_id.into(),
            revision,
        };
        if !transaction.valid() {
            return Err(invalid("invalid native transaction identity"));
        }
        Ok(transaction)
    }
    pub fn connect(path: &Path) -> io::Result<Self> {
        let name = format!("rc2fx-native-{}-{}", std::process::id(), nonce()?);
        let socket = UnixDatagram::bind_addr(&SocketAddr::from_abstract_name(name.as_bytes())?)?;
        socket.set_write_timeout(Some(TIMEOUT))?;
        socket.connect(path)?;
        let mut client = Self {
            socket,
            next: 1,
            session: None,
            buffer: vec![0; MAX_RESPONSE + 1].into_boxed_slice(),
        };
        let response = client.request(Operation::Hello)?;
        if !matches!(response.reply, Reply::Hello) {
            return Err(io::Error::other("native hello refused"));
        }
        client.session = Some(response.session);
        Ok(client)
    }
    pub fn request(&mut self, operation: Operation) -> io::Result<Response> {
        let request_id = self.next;
        self.next = self
            .next
            .checked_add(1)
            .ok_or_else(|| invalid("request counter exhausted"))?;
        let request = Request {
            version: VERSION,
            request_id,
            session: self.session.clone(),
            operation,
        };
        let bytes = serde_json::to_vec(&request).map_err(io::Error::other)?;
        if bytes.len() > MAX_REQUEST {
            return Err(invalid("native request too large"));
        }
        let deadline = Instant::now() + TIMEOUT;
        if self.socket.send(&bytes)? != bytes.len() {
            return Err(io::Error::other("short native message"));
        }
        let buffer = &mut self.buffer;
        loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::TimedOut, "native response deadline")
                })?;
            self.socket.set_read_timeout(Some(remaining))?;
            let length = self.socket.recv(buffer)?;
            if length > MAX_RESPONSE {
                return Err(invalid("native response too large"));
            }
            let response: Response =
                serde_json::from_slice(&buffer[..length]).map_err(io::Error::other)?;
            if response.request_id != request_id {
                continue;
            } // Discard a delayed older reply.
            if response.version != VERSION
                || self
                    .session
                    .as_ref()
                    .is_some_and(|session| *session != response.session)
            {
                return Err(io::Error::other("native process session changed"));
            }
            return Ok(response);
        }
    }
}
