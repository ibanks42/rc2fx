//! RC2FX control over USB 1: requests and replies framed into RC2FX's reports
//! in the mixer's vendor HID collection (see docs/reference/usb-control.md). Blocking; one client owns
//! the mixer's session at a time.

use crate::{
    Reports,
    platform::{self, Hid},
};
use rc2fx_usb::{
    Assembly, Frame, INPUT_ID, Kind, MAX_EVENT_REPLY, MAX_REPLY, MAX_REQUEST, OUTPUT_ID, PAYLOAD,
    REPORT_SIZE,
};
pub use rc2fx_usb::{Reply, Request};
use std::time::{Duration, Instant};

/// The longest a request may take, including the mixer executing it.
const DEADLINE: Duration = Duration::from_secs(60);
/// The mixer answers a report at once; longer silence means it was lost.
const REPORT_WAIT: Duration = Duration::from_secs(3);
/// A request executes before its final acknowledgement, for up to 15 s.
const EXECUTE_WAIT: Duration = Duration::from_secs(17);
const HELLO: &[u8] = b"RC2FX USB 3";
/// The mixer's own bound on a saved file's density, applied to a reply's size:
/// real replies stay far below it, and a parsed value costs many times its text.
const BYTES_PER_NODE: usize = 4;
/// The event poll runs twenty times a second and is never a whole settings file.
const EVENTS_PATH: &str = "api/v1/events";

/// Why a request failed.
#[derive(Debug)]
pub enum Failure {
    /// Nothing reached the mixer; the session is intact.
    NotSent(String),
    /// The session is broken and the request's outcome may be unknown. It is
    /// never replayed; reopen before sending more.
    Broken(String),
}

impl Failure {
    pub fn message(&self) -> &str {
        match self {
            Failure::NotSent(m) | Failure::Broken(m) => m,
        }
    }
}

/// A session with one mixer. Its nonce and request numbers last for the
/// client's life, so reopening after a lost handle continues the same session
/// on the mixer instead of waiting out another client's lockout.
pub struct Client {
    reports: Option<Box<dyn Reports>>,
    serial: String,
    model: Option<rc2fx_firmware::Model>,
    nonce: u64,
    sequence: u32,
    report_wait: Duration,
    execute_wait: Duration,
}

impl Client {
    /// Open the one connected mixer's RC2FX control.
    pub fn open() -> Result<Self, String> {
        let mut nonce = [0; 8];
        getrandom::fill(&mut nonce).map_err(|e| e.to_string())?;
        let mut client = Client {
            reports: None,
            serial: String::new(),
            model: None,
            nonce: u64::from_le_bytes(nonce).max(1),
            sequence: 0,
            report_wait: REPORT_WAIT,
            execute_wait: EXECUTE_WAIT,
        };
        client.connect(None)?;
        Ok(client)
    }

    /// The connected mixer's USB serial.
    pub fn serial(&self) -> &str {
        &self.serial
    }

    pub fn is_open(&self) -> bool {
        self.reports.is_some()
    }

    /// Close the handle; queued reports for it are never read.
    pub fn close(&mut self) {
        self.reports = None;
    }

    /// Reopen after a broken request: the same mixer only.
    pub fn reopen(&mut self) -> Result<(), String> {
        let serial = self.serial.clone();
        self.connect(Some(&serial))
    }

    fn connect(&mut self, expected: Option<&str>) -> Result<(), String> {
        self.reports = None;
        let platform = platform::host();
        let items = platform.inventory().map_err(|e| e.to_string())?;
        let hid = control_interface(&items)?;
        let model = rc2fx_firmware::Model::from_usb(rc2fx_firmware::Model::VENDOR_ID, hid.pid);
        if self.model.is_some() && self.model != model {
            return Err("A different mixer model is connected now".into());
        }
        if let Some(serial) = expected
            && hid.serial != serial
        {
            return Err("A different RØDECaster Pro II is connected now".into());
        }
        // Windows reports descriptor lengths; elsewhere the inventory gives
        // buffer capacities and the HID API takes the 256-byte reports.
        if cfg!(windows) && (hid.input != REPORT_SIZE || hid.output != REPORT_SIZE) {
            return Err("The mixer's HID report layout is not supported".into());
        }
        self.reports = Some(platform.open_control(hid).map_err(|e| e.to_string())?);
        self.serial = hid.serial.clone();
        self.model = model;
        let hello = Frame {
            kind: Kind::Hello,
            session: self.nonce,
            request: 0,
            offset: 0,
            total: 0,
            data: &[],
        };
        let reply = self
            .exchange(&hello, Instant::now() + self.report_wait, self.report_wait)
            .map_err(|e| {
                self.reports = None;
                if e.starts_with("USB is in use") {
                    format!("{e}; try again in a few seconds")
                } else {
                    format!("RC2FX did not answer over USB: {e}")
                }
            })?;
        let reply = Frame::decode(&reply, INPUT_ID).ok_or("Invalid USB hello reply")?;
        if reply.kind != Kind::Ack || reply.data != HELLO {
            self.reports = None;
            return Err("RC2FX's USB protocol version does not match this app".into());
        }
        Ok(())
    }

    /// Send one request and wait for its reply.
    pub fn request(&mut self, request: &Request) -> Result<Reply, Failure> {
        let json = serde_json::to_vec(request).map_err(|e| Failure::NotSent(e.to_string()))?;
        let bytes = rc2fx_usb::pack(&json, MAX_REQUEST).map_err(Failure::NotSent)?;
        if self.reports.is_none() {
            return Err(Failure::NotSent("USB is not connected".into()));
        }
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or_else(|| Failure::NotSent("USB session exhausted; reconnect".into()))?;
        let limit = if request.path == EVENTS_PATH {
            MAX_EVENT_REPLY
        } else {
            MAX_REPLY
        };
        let result = self.send(&bytes).and_then(|()| self.fetch(limit));
        if result.is_err() {
            self.reports = None;
        }
        result.map_err(Failure::Broken)
    }

    fn send(&mut self, bytes: &[u8]) -> Result<(), String> {
        let deadline = Instant::now() + DEADLINE;
        let count = bytes.len().div_ceil(PAYLOAD);
        for (index, chunk) in bytes.chunks(PAYLOAD).enumerate() {
            let offset = index * PAYLOAD;
            let last = index + 1 == count;
            let frame = Frame {
                kind: Kind::Request,
                session: self.nonce,
                request: self.sequence,
                offset,
                total: bytes.len(),
                data: chunk,
            };
            let wait = if last {
                self.execute_wait
            } else {
                self.report_wait
            };
            match self.exchange(&frame, deadline, wait) {
                Ok(reply) => {
                    let ack =
                        Frame::decode(&reply, INPUT_ID).ok_or("Invalid USB acknowledgement")?;
                    if ack.kind != Kind::Ack
                        || ack.offset != offset + chunk.len()
                        || ack.total != bytes.len()
                        || !ack.data.is_empty()
                    {
                        return Err("USB fragment was not acknowledged; not replayed".into());
                    }
                }
                // The last fragment's acknowledgement can be lost after the
                // mixer executed it; its reply is still cached for fetching.
                Err(e) if last && e == TIMED_OUT => return Ok(()),
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    /// Fetch the reply. Fetching is idempotent, so a lost report is asked for
    /// again rather than ending the request.
    fn fetch(&mut self, limit: usize) -> Result<Reply, String> {
        let deadline = Instant::now() + DEADLINE;
        let mut assembly = Assembly::default();
        let mut offset = 0;
        loop {
            let frame = Frame {
                kind: Kind::Fetch,
                session: self.nonce,
                request: self.sequence,
                offset,
                total: 0,
                data: &[],
            };
            let bytes = match self.exchange(&frame, deadline, self.report_wait) {
                Ok(bytes) => bytes,
                Err(e) if e == TIMED_OUT && Instant::now() < deadline => continue,
                // Before the mixer has a reply (still executing, or its final
                // acknowledgement was lost) a fetch is refused; ask again.
                Err(e) if e == NOT_READY && Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(100));
                    continue;
                }
                Err(e) if e == TIMED_OUT || e == NOT_READY => {
                    return Err(
                        "USB request timed out; its outcome is unknown; not replayed".into(),
                    );
                }
                Err(e) => return Err(e),
            };
            let reply = Frame::decode(&bytes, INPUT_ID).ok_or("Invalid USB reply")?;
            if reply.kind != Kind::Reply || reply.offset != offset {
                return Err("Invalid USB reply sequence".into());
            }
            offset += reply.data.len();
            if let Some(bytes) = assembly.push(&reply, limit)? {
                return parse_reply(&rc2fx_usb::unpack(&bytes, limit)?, limit);
            }
        }
    }

    /// Write one report and read this session's answer to it, waiting at most
    /// `wait` (and never past `deadline`).
    fn exchange(
        &mut self,
        frame: &Frame<'_>,
        deadline: Instant,
        wait: Duration,
    ) -> Result<[u8; REPORT_SIZE], String> {
        let until = deadline.min(Instant::now() + wait);
        let remaining = || {
            until
                .checked_duration_since(Instant::now())
                .ok_or(TIMED_OUT)
        };
        let reports = self.reports.as_mut().ok_or("USB is not connected")?;
        let bytes = frame.encode(OUTPUT_ID);
        let n = reports
            .write(&bytes, remaining()?)
            .map_err(|e| e.to_string())?;
        if n < bytes.len() {
            return Err("Short USB write; not replayed".into());
        }
        loop {
            let mut bytes = [0; REPORT_SIZE];
            // A read that fails at the deadline is a timeout, whatever the
            // platform calls it.
            let n = match reports.read(&mut bytes, remaining()?) {
                Ok(n) => n,
                Err(_) if remaining().is_err() => return Err(TIMED_OUT.into()),
                Err(e) => return Err(e.to_string()),
            };
            let report = &bytes[..n.min(REPORT_SIZE)];
            // RØDE's reports and other sessions' may arrive on the same handle.
            if !rc2fx_usb::marked(report, INPUT_ID) {
                continue;
            }
            let reply = Frame::decode(report, INPUT_ID).ok_or("Malformed USB reply")?;
            if reply.session != self.nonce || reply.request != frame.request {
                continue;
            }
            if reply.kind == Kind::Error {
                let text = String::from_utf8_lossy(reply.data);
                return Err(
                    if frame.kind == Kind::Fetch && text == "Invalid USB control packet" {
                        NOT_READY.into()
                    } else {
                        text.into_owned()
                    },
                );
            }
            return Ok(bytes);
        }
    }
}

/// A reply is the mixer's data, so it gets the same duplicate-key, depth and
/// size checks as any other JSON RC2FX reads.
fn parse_reply(bytes: &[u8], limit: usize) -> Result<Reply, String> {
    let text = std::str::from_utf8(bytes).map_err(|e| e.to_string())?;
    rc2fx_core::from_json_limited(text, limit, limit / BYTES_PER_NODE).map_err(|e| e.to_string())
}

const TIMED_OUT: &str = "timed out";
const NOT_READY: &str = "reply not ready";

/// The single mixer's RC2FX control interface: RØDE's vendor collection, once
/// RC2FX's reports are declared in it.
fn control_interface(items: &[Hid]) -> Result<&Hid, String> {
    let mixers: Vec<_> = items
        .iter()
        .filter(|h| {
            h.usage == 0xff00
                && !h.serial.is_empty()
                && rc2fx_firmware::Model::from_usb(rc2fx_firmware::Model::VENDOR_ID, h.pid)
                    .is_some_and(|m| m.normal_pids().contains(&h.pid))
        })
        .collect();
    match mixers.as_slice() {
        // The descriptor and handshake prove this is an intentionally installed
        // RC2FX runtime. The saved flash opt-in is not an uninstall switch.
        [one] if one.control => Ok(one),
        [_] => Err("The mixer is connected, but RC2FX isn't running on it".into()),
        [] => Err("No RØDECaster Pro II connected".into()),
        _ => Err("More than one RØDECaster Pro II is connected; connect only one".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        collections::VecDeque,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    /// A mixer for the client to talk to, with ways to lose reports.
    #[derive(Default)]
    struct Mixer {
        inbox: VecDeque<Vec<u8>>,
        assembly: Assembly,
        reply: Vec<u8>,
        executed: usize,
        writes: Arc<AtomicUsize>,
        short: bool,
        lose_final_ack: bool,
        lose_fetches: usize,
    }

    impl Mixer {
        fn answer(&mut self, f: &Frame<'_>, kind: Kind, offset: usize, total: usize, data: &[u8]) {
            // Real handles also see RØDE's reports and other sessions'.
            self.inbox.push_back(vec![2, b'A']);
            for session in [f.session + 1, f.session] {
                let frame = Frame {
                    kind,
                    session,
                    request: f.request,
                    offset,
                    total,
                    data,
                };
                self.inbox.push_back(frame.encode(INPUT_ID).to_vec());
            }
        }
    }

    impl Reports for Mixer {
        fn write(&mut self, bytes: &[u8], _: Duration) -> crate::Result<usize> {
            self.writes.fetch_add(1, Ordering::SeqCst);
            if self.short {
                return Ok(bytes.len() - 1);
            }
            let f = Frame::decode(bytes, OUTPUT_ID).unwrap();
            match f.kind {
                Kind::Hello => self.answer(&f, Kind::Ack, 0, 0, HELLO),
                Kind::Request => {
                    let done = self.assembly.push(&f, MAX_REQUEST).unwrap().is_some();
                    if done {
                        self.executed += 1;
                    }
                    if !(done && self.lose_final_ack) {
                        self.answer(&f, Kind::Ack, f.offset + f.data.len(), f.total, &[]);
                    }
                }
                Kind::Fetch if self.executed == 0 => {
                    self.answer(&f, Kind::Error, 0, 0, b"Invalid USB control packet")
                }
                Kind::Fetch if self.lose_fetches > 0 => self.lose_fetches -= 1,
                Kind::Fetch => {
                    let end = (f.offset + rc2fx_usb::INPUT_PAYLOAD).min(self.reply.len());
                    let reply = self.reply.clone();
                    self.answer(
                        &f,
                        Kind::Reply,
                        f.offset,
                        reply.len(),
                        &reply[f.offset..end],
                    );
                }
                _ => panic!("unexpected host operation"),
            }
            Ok(bytes.len())
        }

        fn read(&mut self, bytes: &mut [u8], timeout: Duration) -> crate::Result<usize> {
            match self.inbox.pop_front() {
                Some(next) => {
                    bytes[..next.len()].copy_from_slice(&next);
                    Ok(next.len())
                }
                None => {
                    std::thread::sleep(timeout);
                    Err(crate::Error("timed out".into()))
                }
            }
        }
    }

    fn client(mixer: Mixer) -> (Client, Arc<AtomicUsize>) {
        let writes = mixer.writes.clone();
        let json = serde_json::to_vec(&Reply {
            status: 200,
            body: serde_json::json!({"large": "y".repeat(4000)}),
        })
        .unwrap();
        let mixer = Mixer {
            reply: rc2fx_usb::pack(&json, MAX_REPLY).unwrap(),
            ..mixer
        };
        let client = Client {
            reports: Some(Box::new(mixer)),
            serial: "GV1".into(),
            model: Some(rc2fx_firmware::Model::ProIi),
            nonce: 5,
            sequence: 0,
            report_wait: Duration::from_millis(20),
            execute_wait: Duration::from_millis(20),
        };
        (client, writes)
    }

    /// A multi-report write, as an Apply sends.
    fn apply() -> Request {
        let mut seed = 1u32;
        let large: String = (0..2000)
            .map(|_| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                char::from(b' ' + ((seed >> 24) % 90) as u8)
            })
            .collect();
        Request {
            method: "PUT".into(),
            path: "api/v1/config".into(),
            body: Some(serde_json::json!({"large": large})),
        }
    }

    #[test]
    fn multi_report_request_and_reply_ignore_other_reports() {
        let (mut c, _) = client(Mixer::default());
        let reply = c.request(&apply()).unwrap();
        assert_eq!(reply.status, 200);
        assert_eq!(reply.body["large"].as_str().unwrap().len(), 4000);
        assert!(c.is_open());
    }

    fn with_reply(json: &[u8]) -> Client {
        let (mut c, _) = client(Mixer::default());
        let mixer = Mixer {
            reply: rc2fx_usb::pack(json, MAX_REPLY).unwrap(),
            ..Mixer::default()
        };
        c.reports = Some(Box::new(mixer));
        c
    }

    #[test]
    fn replies_with_duplicate_keys_or_excessive_depth_are_refused() {
        let duplicate = br#"{"status":200,"body":{"a":1,"a":2}}"#.to_vec();
        let deep = format!(
            r#"{{"status":200,"body":{}1{}}}"#,
            "[".repeat(65),
            "]".repeat(65)
        );
        for json in [duplicate, deep.into_bytes()] {
            let mut c = with_reply(&json);
            assert!(matches!(c.request(&apply()), Err(Failure::Broken(_))));
        }
        let mut c = with_reply(br#"{"status":200,"body":{"a":[1]}}"#);
        assert_eq!(c.request(&apply()).unwrap().body["a"][0], 1);
    }

    #[test]
    fn an_event_reply_is_held_to_the_event_limit() {
        let events = Request {
            method: "GET".into(),
            path: EVENTS_PATH.into(),
            body: None,
        };
        let json = |size: usize| {
            serde_json::to_vec(&Reply {
                status: 200,
                body: serde_json::json!({"event": "meter", "data": "y".repeat(size)}),
            })
            .unwrap()
        };
        let mut c = with_reply(&json(rc2fx_usb::MAX_EVENT_BYTES));
        assert_eq!(c.request(&events).unwrap().status, 200);
        let mut c = with_reply(&json(MAX_EVENT_REPLY));
        assert!(matches!(c.request(&events), Err(Failure::Broken(_))));
        // Other requests keep the full reply limit.
        let mut c = with_reply(&json(MAX_EVENT_REPLY));
        assert_eq!(c.request(&apply()).unwrap().status, 200);
    }

    #[test]
    fn a_short_write_breaks_the_session_and_is_never_replayed() {
        let (mut c, writes) = client(Mixer {
            short: true,
            ..Default::default()
        });
        let error = c.request(&apply()).unwrap_err();
        assert!(matches!(&error, Failure::Broken(m) if m.contains("not replayed")));
        assert_eq!(writes.load(Ordering::SeqCst), 1);
        assert!(!c.is_open());
        assert!(matches!(c.request(&apply()), Err(Failure::NotSent(_))));
    }

    #[test]
    fn a_lost_final_acknowledgement_recovers_the_cached_reply() {
        let (mut c, _) = client(Mixer {
            lose_final_ack: true,
            ..Default::default()
        });
        assert_eq!(c.request(&apply()).unwrap().status, 200);
    }

    #[test]
    fn lost_replies_are_fetched_again() {
        let (mut c, _) = client(Mixer {
            lose_fetches: 2,
            ..Default::default()
        });
        assert_eq!(c.request(&apply()).unwrap().status, 200);
    }

    #[test]
    fn only_the_single_mixer_control_interface_is_chosen() {
        let hid = |pid, serial: &str| Hid {
            path: String::new(),
            serial: serial.into(),
            pid,
            usage: 0xff00,
            input: REPORT_SIZE,
            output: REPORT_SIZE,
            control: true,
        };
        assert_eq!(control_interface(&[hid(0x0037, "A")]).unwrap().serial, "A");
        assert!(
            control_interface(&[hid(0x0030, "A")]).is_err(),
            "update mode"
        );
        assert!(control_interface(&[hid(0x0037, "A"), hid(0x0078, "B")]).is_err());
        assert!(control_interface(&[]).is_err());
        for pid in rc2fx_firmware::Model::Duo.normal_pids() {
            assert_eq!(
                control_interface(&[hid(*pid, "DUO")]).unwrap().serial,
                "DUO"
            );
            let stock = Hid {
                control: false,
                ..hid(*pid, "DUO")
            };
            assert!(control_interface(&[stock]).is_err());
            assert!(control_interface(&[hid(0x0037, "A"), hid(*pid, "DUO")]).is_err());
        }
        let stock = Hid {
            control: false,
            ..hid(0x0094, "A")
        };
        assert!(
            control_interface(&[stock])
                .unwrap_err()
                .contains("isn't running")
        );
        let other_page = Hid {
            usage: 0xff52,
            ..hid(0x0094, "A")
        };
        assert!(control_interface(&[other_page]).is_err());
    }
}
