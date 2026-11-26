//! Stop-and-wait RC2FX control framing over RØDE's vendor HID reports. Every
//! reply answers a host request; callers retry failed mutations explicitly.
use serde::{Deserialize, Serialize};
use std::io::Write;

pub const REPORT_SIZE: usize = 256;
pub const HEADER: usize = 32;
pub const PAYLOAD: usize = REPORT_SIZE - HEADER;
/// Legacy 256-byte output report ID (protocol 3/4, kept as a bridge for 0.2.4 clients).
pub const OUTPUT_ID: u8 = 5;
// Declared inside RØDE's top-level collection (usage page 0xff00, where stock
// reports are 1..=4). RØDE's software filters by report ID, so reports 6 and
// 65 are ignored by stock readers.
/// Legacy 256-byte input report ID (protocol 3/4, kept as a bridge for 0.2.4 clients).
pub const INPUT_ID: u8 = 6;
/// Protocol 5 compact output report ID (62 bytes total: 1 ID + 61 bytes),
/// declared inside the same collection so every host write fits a single 63-byte
/// USB packet on Linux, macOS, and Windows.
pub const COMPACT_OUTPUT_ID: u8 = 64;
/// Protocol 5 compact input report ID (256 bytes total, 20-byte header).
pub const COMPACT_INPUT_ID: u8 = 65;
pub const INPUT_REPORT_SIZE: usize = REPORT_SIZE;
pub const INPUT_PAYLOAD: usize = INPUT_REPORT_SIZE - HEADER;
pub const COMPACT_OUT_SIZE: usize = 62;
pub const COMPACT_IN_SIZE: usize = REPORT_SIZE;
pub const COMPACT_HEADER: usize = 20;
pub const COMPACT_OUT_PAYLOAD: usize = COMPACT_OUT_SIZE - COMPACT_HEADER;
pub const COMPACT_IN_PAYLOAD: usize = COMPACT_IN_SIZE - COMPACT_HEADER;
pub const COMPACT_MAGIC: &[u8; 3] = b"\xffFX";
pub const COMPACT_VERSION: u8 = 5;
pub const COMPACT_MAX_U24: usize = 0x00ff_ffff;
pub const MAX_REQUEST: usize = 512 * 1024 + 1024;
pub const MAX_REPLY: usize = 8 * 1024 * 1024 + 1024;
pub const MAX_EVENT_BYTES: usize = 512 * 1024;
pub const MAGIC: &[u8; 7] = b"\xffRC2FX\0";
/// Hello payload/ack for protocol 3 clients.
pub const HELLO_3: &[u8] = b"RC2FX USB 3";
/// Hello payload for protocol 4 clients; the mixer replies with the negotiated window size.
pub const HELLO_4: &[u8] = b"RC2FX USB 4";
/// Hello payload for protocol 5 clients on reports 64/65 (compact 20-byte
/// header, single-packet 62-byte host writes, zstd compression). The mixer
/// replies "RC2FX USB 5 <window>" on report 65.
pub const HELLO_5: &[u8] = b"RC2FX USB 5";
/// Max in-flight reports allowed per client. The native hook forwards reports
/// over a non-blocking datagram socket, so exceeding the kernel's `max_dgram_qlen`
/// would silently drop frames.
pub const MAX_WINDOW: usize = 32;

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

/// Recognition for compact protocol 5 reports (report 64 OUT / report 65 IN).
pub fn marked_compact(bytes: &[u8], report: u8) -> bool {
    bytes.first() == Some(&report) && bytes.get(1..4) == Some(COMPACT_MAGIC.as_slice())
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

/// Bytes past a short report that the mixer's controller may count into the
/// read: it reports 66 bytes for a 62-byte transfer (and 257 for 256), the
/// extra bytes being stale buffer contents.
pub const SHORT_REPORT_SLACK: usize = 7;

/// Validates a protocol 5 compact output report (report 64, 62 bytes) from
/// `/dev/hidg0`, trimming up to `SHORT_REPORT_SLACK` trailing DWC2 DMA slack
/// bytes and rejecting padded 256-byte reports or oversized lengths.
pub fn compact_output(bytes: &[u8]) -> Option<&[u8]> {
    if !(COMPACT_OUT_SIZE..=COMPACT_OUT_SIZE + SHORT_REPORT_SLACK).contains(&bytes.len()) {
        return None;
    }
    let bytes = &bytes[..COMPACT_OUT_SIZE];
    if !marked_compact(bytes, COMPACT_OUTPUT_ID) {
        return None;
    }
    if usize::from(bytes[5]) > COMPACT_OUT_PAYLOAD {
        return None;
    }
    Some(bytes)
}

fn kind_from_byte(byte: u8) -> Option<Kind> {
    match byte {
        1 => Some(Kind::Hello),
        2 => Some(Kind::Request),
        3 => Some(Kind::Fetch),
        4 => Some(Kind::Ack),
        5 => Some(Kind::Reply),
        6 => Some(Kind::Error),
        _ => None,
    }
}

fn read_u24_le(bytes: &[u8]) -> usize {
    usize::from(bytes[0]) | (usize::from(bytes[1]) << 8) | (usize::from(bytes[2]) << 16)
}

fn write_u24_le(dst: &mut [u8], value: usize) {
    dst[0] = (value & 0xff) as u8;
    dst[1] = ((value >> 8) & 0xff) as u8;
    dst[2] = ((value >> 16) & 0xff) as u8;
}

impl<'a> Frame<'a> {
    pub fn decode(bytes: &'a [u8], report: u8) -> Option<Self> {
        if bytes.len() != REPORT_SIZE || !marked(bytes, report) || bytes[8] != 3 {
            return None;
        }
        let kind = kind_from_byte(bytes[9])?;
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
    pub fn decode_compact(bytes: &'a [u8], report: u8) -> Option<Self> {
        let expected_len = match report {
            COMPACT_OUTPUT_ID => COMPACT_OUT_SIZE,
            COMPACT_INPUT_ID => COMPACT_IN_SIZE,
            _ => return None,
        };
        if bytes.len() != expected_len || !marked_compact(bytes, report) {
            return None;
        }
        if (bytes[4] >> 4) != COMPACT_VERSION {
            return None;
        }
        let kind = kind_from_byte(bytes[4] & 0x0f)?;
        let len = usize::from(bytes[5]);
        if len > expected_len - COMPACT_HEADER {
            return None;
        }
        Some(Self {
            kind,
            session: u64::from(u32::from_le_bytes(bytes[6..10].try_into().ok()?)),
            request: u32::from_le_bytes(bytes[10..14].try_into().ok()?),
            offset: read_u24_le(&bytes[14..17]),
            total: read_u24_le(&bytes[17..20]),
            data: &bytes[COMPACT_HEADER..COMPACT_HEADER + len],
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
    fn write_compact_header(&self, bytes: &mut [u8], report: u8) {
        assert!(
            self.session <= u64::from(u32::MAX)
                && self.offset <= COMPACT_MAX_U24
                && self.total <= COMPACT_MAX_U24
                && self.data.len() <= bytes.len() - COMPACT_HEADER
        );
        bytes[0] = report;
        bytes[1..4].copy_from_slice(COMPACT_MAGIC);
        bytes[4] = (COMPACT_VERSION << 4) | (self.kind as u8);
        bytes[5] = self.data.len() as u8;
        bytes[6..10].copy_from_slice(&(self.session as u32).to_le_bytes());
        bytes[10..14].copy_from_slice(&self.request.to_le_bytes());
        write_u24_le(&mut bytes[14..17], self.offset);
        write_u24_le(&mut bytes[17..20], self.total);
        bytes[COMPACT_HEADER..COMPACT_HEADER + self.data.len()].copy_from_slice(self.data);
    }
    pub fn encode_compact_out(&self) -> [u8; COMPACT_OUT_SIZE] {
        let mut bytes = [0; COMPACT_OUT_SIZE];
        self.write_compact_header(&mut bytes, COMPACT_OUTPUT_ID);
        bytes
    }
    pub fn encode_compact_in(&self) -> [u8; COMPACT_IN_SIZE] {
        let mut bytes = [0; COMPACT_IN_SIZE];
        self.write_compact_header(&mut bytes, COMPACT_INPUT_ID);
        bytes
    }
}

/// Compress complete JSON messages with zlib, off the audio thread (legacy
/// protocol 3/4 sessions). Explicit decoded size and bounded decompression
/// reject bombs, truncation and trailing data.
pub fn pack(json: &[u8], limit: usize) -> Result<Vec<u8>, String> {
    if json.len() > limit || json.len() > u32::MAX as usize {
        return Err("USB message too large".into());
    }
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(json).map_err(|e| e.to_string())?;
    let compressed = encoder.finish().map_err(|e| e.to_string())?;
    finish_packed(b'Z', json, &compressed, limit)
}

/// Compress complete JSON messages with zstd (level 3), used on protocol 5
/// sessions. Falls back to raw `J` when compression does not shrink the message.
pub fn pack_zstd(json: &[u8], limit: usize) -> Result<Vec<u8>, String> {
    if json.len() > limit || json.len() > u32::MAX as usize {
        return Err("USB message too large".into());
    }
    let compressed = zstd::bulk::compress(json, 3).map_err(|e| e.to_string())?;
    finish_packed(b'S', json, &compressed, limit)
}

fn finish_packed(tag: u8, json: &[u8], compressed: &[u8], limit: usize) -> Result<Vec<u8>, String> {
    let mut encoded = Vec::new();
    if compressed.len() + 5 < json.len() + 1 {
        encoded.push(tag);
        encoded.extend_from_slice(&(json.len() as u32).to_le_bytes());
        encoded.extend_from_slice(compressed);
    } else {
        encoded.push(b'J');
        encoded.extend_from_slice(json);
    }
    if encoded.len() > limit {
        return Err("USB message too large".into());
    }
    Ok(encoded)
}

fn decompress_zstd(src: &[u8], size: usize) -> Result<Vec<u8>, String> {
    use zstd::stream::raw::Operation;
    if src.is_empty() {
        return Err("Invalid compressed USB message".into());
    }
    let mut decoder = zstd::stream::raw::Decoder::new().map_err(|e| e.to_string())?;
    let mut decoded = Vec::with_capacity(size.min(64 * 1024));
    let mut pos = 0usize;
    let mut scratch = [0u8; 16 * 1024];
    loop {
        let status = decoder
            .run_on_buffers(&src[pos..], &mut scratch)
            .map_err(|e| e.to_string())?;
        pos += status.bytes_read;
        if status.bytes_written > 0 {
            if decoded.len().saturating_add(status.bytes_written) > size {
                return Err("Invalid compressed USB message".into());
            }
            decoded.extend_from_slice(&scratch[..status.bytes_written]);
        }
        if status.remaining == 0 {
            break;
        }
        if status.bytes_read == 0 && status.bytes_written == 0 {
            return Err("Invalid compressed USB message".into());
        }
    }
    if pos != src.len() || decoded.len() != size {
        return Err("Invalid compressed USB message".into());
    }
    Ok(decoded)
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
        Some(b'S') if encoded.len() >= 5 => {
            let size = u32::from_le_bytes(encoded[1..5].try_into().unwrap()) as usize;
            if size > limit {
                return Err("Decoded USB message too large".into());
            }
            decompress_zstd(&encoded[5..], size)
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
    fn bounded_zstd_codec_rejects_truncation_trailing_data_and_expansion_lies() {
        let json = br#"{"value":"repeat repeat repeat repeat repeat repeat"}"#.repeat(100);
        let encoded = pack_zstd(&json, 8192).unwrap();
        assert_eq!(encoded[0], b'S');
        assert!(encoded.len() < json.len() / 10);
        assert_eq!(unpack(&encoded, 8192).unwrap(), json);
        for length in 0..encoded.len() {
            assert!(unpack(&encoded[..length], 8192).is_err(), "{length}");
        }
        let mut trailing = encoded.clone();
        trailing.push(0);
        assert!(unpack(&trailing, 8192).is_err());
        // Concatenated second zstd frame must be rejected.
        let mut doubled = encoded.clone();
        doubled.extend_from_slice(&encoded[5..]);
        assert!(unpack(&doubled, 16384).is_err());
        let mut bomb = encoded.clone();
        bomb[1..5].copy_from_slice(&1u32.to_le_bytes());
        assert!(unpack(&bomb, 8192).is_err());
        bomb[1..5].copy_from_slice(&8193u32.to_le_bytes());
        assert!(unpack(&bomb, 8192).is_err());
        assert_eq!(pack_zstd(b"{}", 64).unwrap(), b"J{}");
        assert!(pack_zstd(b"{}", 1).is_err());
        assert!(unpack(b"S\0\0", 64).is_err());
    }
    #[test]
    fn compact_reports_roundtrip_and_refuse_invalid_lengths_or_wrong_ids() {
        let frame = Frame {
            kind: Kind::Request,
            session: 0x1234_5678,
            request: 3,
            offset: COMPACT_OUT_PAYLOAD,
            total: 100,
            data: &[0x42; COMPACT_OUT_PAYLOAD],
        };
        let wire = frame.encode_compact_out();
        assert_eq!(wire.len(), COMPACT_OUT_SIZE);
        for n in [
            COMPACT_OUT_SIZE,
            COMPACT_OUT_SIZE + 4,
            COMPACT_OUT_SIZE + SHORT_REPORT_SLACK,
        ] {
            let mut buf = [0x52; REPORT_SIZE];
            buf[..COMPACT_OUT_SIZE].copy_from_slice(&wire);
            let trimmed = compact_output(&buf[..n]).unwrap();
            assert_eq!(trimmed, &wire[..]);
            let decoded = Frame::decode_compact(trimmed, COMPACT_OUTPUT_ID).unwrap();
            assert_eq!(
                (
                    decoded.kind,
                    decoded.session,
                    decoded.request,
                    decoded.offset,
                    decoded.total,
                    decoded.data
                ),
                (
                    Kind::Request,
                    0x1234_5678,
                    3,
                    COMPACT_OUT_PAYLOAD,
                    100,
                    &[0x42; COMPACT_OUT_PAYLOAD][..]
                )
            );
        }
        // Compact IN report carries 236 payload bytes.
        let reply = Frame {
            kind: Kind::Reply,
            session: 0xdead_beef,
            request: 99,
            offset: 0x0012_3456,
            total: 0x00ab_cdef,
            data: &[0x7a; COMPACT_IN_PAYLOAD],
        };
        let in_wire = reply.encode_compact_in();
        let decoded_in = Frame::decode_compact(&in_wire, COMPACT_INPUT_ID).unwrap();
        assert_eq!(
            (
                decoded_in.kind,
                decoded_in.session,
                decoded_in.request,
                decoded_in.offset,
                decoded_in.total,
                decoded_in.data
            ),
            (
                Kind::Reply,
                0xdead_beef,
                99,
                0x0012_3456,
                0x00ab_cdef,
                &[0x7a; COMPACT_IN_PAYLOAD][..]
            )
        );
        // Reject lengths outside the short-report window, including a padded 256B report 64.
        let mut padded = [0; REPORT_SIZE];
        padded[..COMPACT_OUT_SIZE].copy_from_slice(&wire);
        for n in [
            4,
            COMPACT_OUT_SIZE - 1,
            COMPACT_OUT_SIZE + SHORT_REPORT_SLACK + 1,
            63 + 62,
            189 + 62,
            REPORT_SIZE,
        ] {
            assert!(compact_output(&padded[..n]).is_none(), "{n}");
        }
        // Only COMPACT_OUTPUT_ID (64) is accepted as a compact output report.
        for wrong_id in [OUTPUT_ID, INPUT_ID, COMPACT_INPUT_ID] {
            let mut wrong = wire;
            wrong[0] = wrong_id;
            assert!(compact_output(&wrong).is_none());
            assert!(Frame::decode_compact(&wrong, COMPACT_OUTPUT_ID).is_none());
        }
        // Reject payload length larger than COMPACT_OUT_PAYLOAD.
        let mut over = wire;
        over[5] = (COMPACT_OUT_PAYLOAD + 1) as u8;
        assert!(compact_output(&over).is_none());
        assert!(Frame::decode_compact(&over, COMPACT_OUTPUT_ID).is_none());
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
}
