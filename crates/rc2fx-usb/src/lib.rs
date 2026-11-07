//! RC2FX control messages carried directly by the existing vendor HID reports.
//! Stop-and-wait framing: no unsolicited writes, no automatic mutation replay.
use serde::{Deserialize, Serialize};
use std::io::Write;

pub const REPORT_SIZE: usize = 256;
pub const HEADER: usize = 32;
pub const PAYLOAD: usize = REPORT_SIZE - HEADER;
pub const OUTPUT_ID: u8 = 5;
// RC2FX declares these IDs inside RODE's one top-level collection (usage page
// ff00), whose own reports are 1-4. Every reader of that collection receives
// input report 6 and must ignore it; RODE's software reads by report ID.
pub const INPUT_ID: u8 = 6;
pub const INPUT_REPORT_SIZE: usize = REPORT_SIZE;
pub const INPUT_PAYLOAD: usize = INPUT_REPORT_SIZE - HEADER;
pub const MAX_REQUEST: usize = 512 * 1024 + 1024;
pub const MAX_REPLY: usize = 8 * 1024 * 1024 + 1024;
/// The largest event a mixer publishes.
pub const MAX_EVENT_BYTES: usize = 512 * 1024;
/// The largest reply to an event poll: one event and its envelope.
pub const MAX_EVENT_REPLY: usize = MAX_EVENT_BYTES + 1024;
pub const MAGIC: &[u8; 7] = b"\xffRC2FX\0";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Kind {
    Hello = 1,
    Request = 2,
    Fetch = 3,
    Ack = 4,
    Reply = 5,
    Error = 6,
}

#[derive(Debug)]
pub struct Frame<'a> {
    pub kind: Kind,
    pub session: u64,
    pub request: u32,
    pub offset: usize,
    pub total: usize,
    pub data: &'a [u8],
}

/// Recognition deliberately precedes validation: malformed RC2FX packets must
/// never be forwarded into RODE's command parser.
pub fn marked(bytes: &[u8], report: u8) -> bool {
    bytes.first() == Some(&report) && bytes.get(1..8) == Some(MAGIC.as_slice())
}

/// Whether a HID report descriptor declares RC2FX's output and input reports,
/// that is, whether RC2FX's USB layout is live. Parsing sends nothing.
pub fn declares_control(descriptor: &[u8]) -> bool {
    let (mut report, mut stack) = (0u32, Vec::new());
    let (mut output, mut input) = (false, false);
    let mut i = 0;
    while i < descriptor.len() {
        let prefix = descriptor[i];
        i += 1;
        if prefix == 0xfe {
            let Some(&size) = descriptor.get(i) else {
                return false;
            };
            i += 2 + size as usize;
            continue;
        }
        let n = match prefix & 3 {
            3 => 4,
            n => n as usize,
        };
        let Some(bytes) = descriptor.get(i..i + n) else {
            return false;
        };
        i += n;
        let value = bytes
            .iter()
            .rev()
            .fold(0u32, |v, b| (v << 8) | u32::from(*b));
        match prefix & 0xfc {
            0x84 => report = value,
            0xa4 => stack.push(report),
            0xb4 => match stack.pop() {
                Some(r) => report = r,
                None => return false,
            },
            0x80 => input |= report == u32::from(INPUT_ID),
            0x90 => output |= report == u32::from(OUTPUT_ID),
            _ => {}
        }
    }
    output && input
}

/// The Windows/RODE OUT path delivers 257 bytes for a 256-byte report. The
/// extra byte is outside the descriptor's report and has been observed as 1.
/// Ignore exactly that byte, then require normal frame validation. Do not
/// combine reads or accept arbitrary oversize buffers as a report.
pub fn canonical_output(bytes: &[u8]) -> Option<&[u8]> {
    match bytes.len() {
        REPORT_SIZE => Some(bytes),
        n if n == REPORT_SIZE + 1 => Some(&bytes[..REPORT_SIZE]),
        _ => None,
    }
}

impl<'a> Frame<'a> {
    pub fn decode(bytes: &'a [u8], report: u8) -> Option<Self> {
        if bytes.len() != REPORT_SIZE || !marked(bytes, report) || bytes[8] != 3 {
            return None;
        }
        let kind = match bytes[9] {
            1 => Kind::Hello,
            2 => Kind::Request,
            3 => Kind::Fetch,
            4 => Kind::Ack,
            5 => Kind::Reply,
            6 => Kind::Error,
            _ => return None,
        };
        let len = u16::from_le_bytes(bytes[30..32].try_into().ok()?) as usize;
        if len > PAYLOAD {
            return None;
        }
        Some(Self {
            kind,
            session: u64::from_le_bytes(bytes[10..18].try_into().ok()?),
            request: u32::from_le_bytes(bytes[18..22].try_into().ok()?),
            offset: u32::from_le_bytes(bytes[22..26].try_into().ok()?) as usize,
            total: u32::from_le_bytes(bytes[26..30].try_into().ok()?) as usize,
            data: &bytes[HEADER..HEADER + len],
        })
    }
    pub fn encode(&self, report: u8) -> [u8; REPORT_SIZE] {
        assert!(
            self.data.len() <= PAYLOAD
                && self.offset <= u32::MAX as usize
                && self.total <= u32::MAX as usize
        );
        let mut bytes = [0; REPORT_SIZE];
        bytes[0] = report;
        bytes[1..8].copy_from_slice(MAGIC);
        bytes[8] = 3;
        bytes[9] = self.kind as u8;
        bytes[10..18].copy_from_slice(&self.session.to_le_bytes());
        bytes[18..22].copy_from_slice(&self.request.to_le_bytes());
        bytes[22..26].copy_from_slice(&(self.offset as u32).to_le_bytes());
        bytes[26..30].copy_from_slice(&(self.total as u32).to_le_bytes());
        bytes[30..32].copy_from_slice(&(self.data.len() as u16).to_le_bytes());
        bytes[HEADER..HEADER + self.data.len()].copy_from_slice(self.data);
        bytes
    }
}

/// Compress complete JSON messages, off the audio thread. Explicit decoded
/// size and bounded decompression reject bombs, truncation and trailing data.
pub fn pack(json: &[u8], limit: usize) -> Result<Vec<u8>, String> {
    if json.len() > limit || json.len() > u32::MAX as usize {
        return Err("USB message too large".into());
    }
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(json).map_err(|e| e.to_string())?;
    let compressed = encoder.finish().map_err(|e| e.to_string())?;
    let mut encoded = Vec::new();
    if compressed.len() + 5 < json.len() + 1 {
        encoded.push(b'Z');
        encoded.extend_from_slice(&(json.len() as u32).to_le_bytes());
        encoded.extend_from_slice(&compressed);
    } else {
        encoded.push(b'J');
        encoded.extend_from_slice(json);
    }
    if encoded.len() > limit {
        return Err("USB message too large".into());
    }
    Ok(encoded)
}

pub fn unpack(encoded: &[u8], limit: usize) -> Result<Vec<u8>, String> {
    if encoded.len() > limit {
        return Err("USB message too large".into());
    }
    match encoded.first() {
        Some(b'J') => Ok(encoded[1..].to_vec()),
        Some(b'Z') if encoded.len() >= 5 => {
            let size = u32::from_le_bytes(encoded[1..5].try_into().unwrap()) as usize;
            if size > limit {
                return Err("Decoded USB message too large".into());
            }
            let mut decoder = flate2::Decompress::new(true);
            let mut decoded = Vec::with_capacity(size + 1);
            let status = decoder
                .decompress_vec(&encoded[5..], &mut decoded, flate2::FlushDecompress::Finish)
                .map_err(|e| e.to_string())?;
            if status != flate2::Status::StreamEnd
                || decoded.len() != size
                || decoder.total_in() as usize != encoded.len() - 5
            {
                return Err("Invalid compressed USB message".into());
            }
            Ok(decoded)
        }
        _ => Err("Invalid USB message encoding".into()),
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub method: String,
    pub path: String,
    pub body: Option<serde_json::Value>,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reply<B = serde_json::Value> {
    pub status: u16,
    pub body: B,
}

/// Exactly ordered assembly. A duplicate, gap, new ID, or inconsistent total
/// invalidates the in-flight message; callers never execute partial requests.
#[derive(Default)]
pub struct Assembly {
    bytes: Vec<u8>,
    total: usize,
    request: u32,
}
impl Assembly {
    pub fn push(
        &mut self,
        frame: &Frame<'_>,
        limit: usize,
    ) -> Result<Option<Vec<u8>>, &'static str> {
        if frame.total == 0
            || frame.total > limit
            || frame.data.is_empty()
            || frame
                .offset
                .checked_add(frame.data.len())
                .is_none_or(|end| end > frame.total)
            || (frame.offset != 0 && (frame.total != self.total || frame.request != self.request))
            || frame.offset != self.bytes.len()
        {
            self.clear();
            return Err("Invalid USB fragment sequence");
        }
        if frame.offset == 0 {
            self.total = frame.total;
            self.request = frame.request;
        }
        self.bytes.extend_from_slice(frame.data);
        if self.bytes.len() == self.total {
            self.total = 0;
            Ok(Some(std::mem::take(&mut self.bytes)))
        } else {
            Ok(None)
        }
    }
    pub fn clear(&mut self) {
        self.bytes.clear();
        self.total = 0;
        self.request = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_message_codec_rejects_truncation_trailing_data_and_expansion_lies() {
        let json = br#"{"value":"repeat repeat repeat repeat repeat repeat"}"#.repeat(100);
        let encoded = pack(&json, 8192).unwrap();
        assert_eq!(encoded[0], b'Z');
        assert!(encoded.len() < json.len() / 4);
        assert_eq!(unpack(&encoded, 8192).unwrap(), json);
        for length in 0..encoded.len() {
            assert!(unpack(&encoded[..length], 8192).is_err());
        }
        let mut trailing = encoded.clone();
        trailing.push(0);
        assert!(unpack(&trailing, 8192).is_err());
        let mut bomb = encoded.clone();
        bomb[1..5].copy_from_slice(&1u32.to_le_bytes());
        assert!(unpack(&bomb, 8192).is_err());
        bomb[1..5].copy_from_slice(&8193u32.to_le_bytes());
        assert!(unpack(&bomb, 8192).is_err());
        assert_eq!(pack(b"{}", 64).unwrap(), b"J{}");
        assert_eq!(unpack(b"J{}", 64).unwrap(), b"{}");
        assert!(pack(b"{}", 1).is_err());
        assert!(unpack(b"J{}", 2).is_err());
        assert!(unpack(b"X{}", 64).is_err());
    }
    #[test]
    fn isolated_input_report_rejects_vendor_ids_and_excess_payload() {
        let f = Frame {
            kind: Kind::Reply,
            session: 1,
            request: 1,
            offset: 0,
            total: PAYLOAD,
            data: &[42; PAYLOAD],
        };
        let mut packet = f.encode(INPUT_ID);
        assert_eq!(
            Frame::decode(&packet[..INPUT_REPORT_SIZE], INPUT_ID)
                .unwrap()
                .data,
            &[42; PAYLOAD]
        );
        assert_eq!(
            Frame::decode(&packet, INPUT_ID).unwrap().data,
            &[42; PAYLOAD]
        );
        assert!(!marked(&packet, 2));
        assert!(!marked(&packet, 4));
        packet[30] = (PAYLOAD + 1) as u8;
        assert!(Frame::decode(&packet, INPUT_ID).is_none());
    }
    #[test]
    fn boundary_roundtrips_and_rejects_reordered_or_oversized_messages() {
        for size in [1, PAYLOAD, PAYLOAD + 1, 65537] {
            let original = vec![0xa7; size];
            let mut assembly = Assembly::default();
            let mut result = None;
            for (index, chunk) in original.chunks(PAYLOAD).enumerate() {
                let bytes = Frame {
                    kind: Kind::Request,
                    session: 7,
                    request: 12,
                    offset: index * PAYLOAD,
                    total: size,
                    data: chunk,
                }
                .encode(OUTPUT_ID);
                let f = Frame::decode(&bytes, OUTPUT_ID).unwrap();
                assert_eq!(f.session, 7);
                result = assembly.push(&f, MAX_REQUEST).unwrap();
            }
            assert_eq!(result.unwrap(), original);
        }
        let mut a = Assembly::default();
        let mut f = Frame {
            kind: Kind::Request,
            session: 1,
            request: 1,
            offset: 1,
            total: 2,
            data: &[1],
        };
        assert!(a.push(&f, 10).is_err());
        f.offset = 0;
        f.total = 11;
        assert!(a.push(&f, 10).is_err());
        f.total = 2;
        assert!(a.push(&f, 10).unwrap().is_none());
        assert!(a.push(&f, 10).is_err());
    }
    #[test]
    fn unrelated_reports_pass_but_malformed_marked_reports_are_owned() {
        let mut b = [0; REPORT_SIZE];
        b[0] = OUTPUT_ID;
        b[1..8].copy_from_slice(MAGIC);
        assert!(marked(&b, OUTPUT_ID));
        assert!(Frame::decode(&b, OUTPUT_ID).is_none());
        b[1] = b'M';
        assert!(!marked(&b, OUTPUT_ID));
    }

    /// RODE's 1.7.6/1.7.7 descriptor and the copy `rc2fx.sh` stages from it.
    const STOCK: &str = "0600ff0901a1017508150025ff09028501953f910209038502953f81020904850395ff91020905850495ff8102c0";
    const STAGED: &str = "0600ff0901a1017508150025ff09028501953f910209038502953f81020904850395ff91020905850495ff81020906850595ff91020907850695ff8102c0";

    fn hex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn staged_layout_is_one_collection_with_the_control_reports() {
        let (stock, staged) = (hex(STOCK), hex(STAGED));
        assert_eq!(staged[..stock.len() - 1], stock[..stock.len() - 1]);
        // One application collection, closed once, at the very end.
        assert_eq!(staged.iter().filter(|b| **b == 0xa1).count(), 1);
        assert_eq!(staged.last(), Some(&0xc0));
        assert!(declares_control(&staged));
        assert!(!declares_control(&stock));
        // Each half alone, or the IDs swapped, is not RC2FX's layout.
        assert!(!declares_control(&hex(&STAGED.replace("850595ff9102", ""))));
        assert!(!declares_control(&hex(&STAGED.replace("850695ff8102", ""))));
        assert!(!declares_control(&staged[..staged.len() - 3]));
    }

    #[test]
    fn accepts_exactly_one_transport_byte_beyond_the_descriptor_report() {
        let mut bytes = vec![0; REPORT_SIZE + 1];
        assert_eq!(canonical_output(&bytes).unwrap().len(), REPORT_SIZE);
        bytes[REPORT_SIZE] = 1;
        assert_eq!(canonical_output(&bytes).unwrap().len(), REPORT_SIZE);
        bytes.push(0);
        assert!(canonical_output(&bytes).is_none());
        assert!(canonical_output(&bytes[..REPORT_SIZE - 1]).is_none());
    }
}
