//! Delta encoding for in-place mixer payload uploads.
//!
//! The signed `RC2FX-EMBEDDED-PAYLOAD 1` envelope remains the source of truth;
//! this crate only shrinks transfers over USB. Each file is sent whole (zstd),
//! copied from disk if unchanged, or patched with zstd against the installed
//! copy. Decoding reconstructs the exact envelope and verifies every file
//! against its manifest before the mixer checks the signature.
//!
//! Uses `zstd_safe` directly because the high-level API does not expose
//! reference prefixes (`--patch-from`), long-distance matching, or a fixed
//! decoder window bound.
//!
//! Production encoding lives in Go (`desktop/delta`); the Rust encoder is gated
//! behind the `encode` feature for tests and not linked into `rc2fxd`.

use sha2::{Digest, Sha256};
#[cfg(any(test, feature = "encode"))]
use std::collections::BTreeMap;
use std::io::{self, Read, Write};
#[cfg(any(test, feature = "encode"))]
use zstd::zstd_safe::{CCtx, CParameter};
use zstd::zstd_safe::{DCtx, DParameter, InBuffer, OutBuffer};

pub const MAGIC: &str = "RC2FX-DELTA 1\n";
pub const ENVELOPE_MAGIC: &str = "RC2FX-EMBEDDED-PAYLOAD 1\n";
const MAX_LINE: usize = 512;
/// Max size of the envelope header + manifest (excluding file payloads).
const MAX_HEAD: usize = 64 * 1024;
const MAX_ENTRIES: usize = 256;
/// Cap decoder window allocation against hostile deltas.
const WINDOW_LOG_MAX: u32 = 25;
/// Slack for compressed frames that slightly exceed uncompressed size.
const OP_OVERHEAD: u64 = 1 << 20;

/// Single file entry from the envelope manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub sha256: String,
    pub size: u64,
}

/// Parses the envelope header and manifest.
///
/// The manifest has no explicit entry count; it ends once the manifest bytes
/// plus all declared file sizes equal the envelope's declared body length.
pub fn envelope_entries(envelope: &[u8]) -> Result<(usize, Vec<Entry>), String> {
    let magic = envelope
        .get(..ENVELOPE_MAGIC.len())
        .ok_or_else(|| "the envelope header is truncated".to_owned())?;
    if magic != ENVELOPE_MAGIC.as_bytes() {
        return Err("this is not an RC2FX envelope".into());
    }
    let (line, mut cursor) = line_at(envelope, ENVELOPE_MAGIC.len())?;
    let length: u64 = line
        .strip_prefix("length ")
        .ok_or_else(|| "the envelope has no length line".to_owned())?
        .parse()
        .map_err(|_| "the envelope's length is not a number".to_owned())?;
    let (line, next) = line_at(envelope, cursor)?;
    cursor = next;
    let digest = line
        .strip_prefix("sha256 ")
        .ok_or_else(|| "the envelope has no digest line".to_owned())?;
    if !valid_hash(digest) {
        return Err("the envelope's digest is malformed".into());
    }
    let body = cursor;

    let mut entries: Vec<Entry> = Vec::new();
    let mut sizes: u64 = 0;
    loop {
        // A payload with no files is still a complete manifest.
        if ((cursor - body) as u64).saturating_add(sizes) == length {
            return Ok((cursor, entries));
        }
        if cursor > MAX_HEAD {
            return Err("the envelope's manifest is too long".into());
        }
        if entries.len() >= MAX_ENTRIES {
            return Err("the envelope has too many files".into());
        }
        let (line, next) = line_at(envelope, cursor)?;
        cursor = next;
        let mut parts = line.splitn(3, "  ");
        let (Some(hash), Some(size), Some(name)) = (parts.next(), parts.next(), parts.next())
        else {
            return Err("an envelope manifest line is malformed".into());
        };
        if !valid_hash(hash) {
            return Err(format!("{name}'s checksum in the envelope is malformed"));
        }
        if !valid_name(name) {
            return Err(format!("{name:?} is not a payload file name"));
        }
        if entries.iter().any(|entry| entry.name == name) {
            return Err(format!("the envelope names {name} twice"));
        }
        let size: u64 = size
            .parse()
            .map_err(|_| format!("{name}'s size in the envelope is not a number"))?;
        sizes = sizes
            .checked_add(size)
            .ok_or_else(|| "the envelope's sizes overflow".to_owned())?;
        let expected = ((cursor - body) as u64)
            .checked_add(sizes)
            .ok_or_else(|| "the envelope's length overflows".to_owned())?;
        if expected > length {
            return Err("the envelope's manifest is longer than its length".into());
        }
        entries.push(Entry {
            name: name.to_owned(),
            sha256: hash.to_ascii_lowercase(),
            size,
        });
        if expected == length {
            return Ok((cursor, entries));
        }
    }
}

/// Encodes a signed `envelope` as a delta.
///
/// `installed` maps mixer filenames to their SHA-256 digests. `bases` holds
/// local copies used as patch prefixes only if they match `installed`; stale
/// bases fall back to sending the file whole.
#[cfg(any(test, feature = "encode"))]
pub fn encode(
    envelope: &[u8],
    installed: &BTreeMap<String, String>,
    bases: &BTreeMap<String, Vec<u8>>,
    level: i32,
) -> Result<Vec<u8>, String> {
    let (head, entries) = envelope_entries(envelope)?;
    let sizes = entries.iter().try_fold(0u64, |sum, entry| {
        sum.checked_add(entry.size)
            .ok_or_else(|| "the envelope's sizes overflow".to_owned())
    })?;
    if (head as u64).checked_add(sizes) != Some(envelope.len() as u64) {
        return Err("the envelope's files do not fill its declared length".into());
    }
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC.as_bytes());
    out.extend_from_slice(format!("head {head}\n").as_bytes());
    out.extend_from_slice(&envelope[..head]);

    let mut offset = head;
    for entry in &entries {
        let size = usize::try_from(entry.size)
            .map_err(|_| format!("{} is too large for this app", entry.name))?;
        let data = envelope
            .get(offset..offset + size)
            .ok_or_else(|| format!("{} is truncated in the envelope", entry.name))?;
        offset += size;
        let current = installed.get(&entry.name);
        if current.is_some_and(|hash| hash.eq_ignore_ascii_case(&entry.sha256)) {
            out.extend_from_slice(b"copy\n");
            continue;
        }
        let base = bases.get(&entry.name).and_then(|base| {
            current
                .is_some_and(|hash| hash.eq_ignore_ascii_case(&sum(base)))
                .then_some(base)
        });
        match base {
            Some(base) => {
                let hash = sum(base);
                let window = window_log(base.len() as u64 + size as u64);
                let compressed = compress(data, Some(base), level, window)?;
                out.extend_from_slice(format!("patch {hash} {}\n", compressed.len()).as_bytes());
                out.extend_from_slice(&compressed);
            }
            None => {
                let window = window_log(size as u64);
                let compressed = compress(data, None, level, window)?;
                out.extend_from_slice(format!("zstd {}\n", compressed.len()).as_bytes());
                out.extend_from_slice(&compressed);
            }
        }
    }
    Ok(out)
}

/// Reconstructs the envelope from `delta` into `out`, returning bytes written.
///
/// `installed(name)` loads the mixer's current copy for `copy` ops and patch
/// prefixes; its digest must match the manifest or patch base hash.
pub fn decode(
    delta: &mut impl Read,
    installed: &mut dyn FnMut(&str) -> io::Result<Vec<u8>>,
    out: &mut impl Write,
    max_envelope: u64,
) -> Result<u64, String> {
    let mut magic = vec![0u8; MAGIC.len()];
    read_exact(&mut *delta, &mut magic, "the delta header")?;
    if magic != MAGIC.as_bytes() {
        return Err("this is not an RC2FX delta".into());
    }
    let line = op_line(&mut *delta)?;
    let head_len: u64 = line
        .strip_prefix("head ")
        .ok_or_else(|| "the delta has no head line".to_owned())?
        .parse()
        .map_err(|_| "the delta's head length is not a number".to_owned())?;
    if head_len == 0 || head_len > MAX_HEAD as u64 {
        return Err("the delta's head length is outside the supported range".into());
    }
    let mut head = vec![0u8; head_len as usize];
    read_exact(&mut *delta, &mut head, "the delta's head")?;
    let (manifest_len, entries) = envelope_entries(&head)?;
    if manifest_len != head.len() {
        return Err("the delta's head has bytes after the envelope's manifest".into());
    }
    let sizes = entries.iter().try_fold(0u64, |sum, entry| {
        sum.checked_add(entry.size)
            .ok_or_else(|| "the envelope's sizes overflow".to_owned())
    })?;
    let envelope_len = head_len
        .checked_add(sizes)
        .ok_or_else(|| "the envelope's length overflows".to_owned())?;
    if envelope_len > max_envelope {
        return Err(format!(
            "the envelope would be {envelope_len} bytes, over the {max_envelope}-byte limit"
        ));
    }
    out.write_all(&head)
        .map_err(|e| format!("cannot write the envelope: {e}"))?;
    let mut written = head_len;
    let op_limit = max_envelope.saturating_add(OP_OVERHEAD);

    for entry in &entries {
        let line = op_line(&mut *delta)?;
        if line == "copy" {
            let bytes = installed(&entry.name)
                .map_err(|e| format!("cannot read the installed {}: {e}", entry.name))?;
            if bytes.len() as u64 != entry.size {
                return Err(format!(
                    "the installed {} is {} bytes; the envelope says {}",
                    entry.name,
                    bytes.len(),
                    entry.size
                ));
            }
            if !sum(&bytes).eq_ignore_ascii_case(&entry.sha256) {
                return Err(format!(
                    "the installed {} does not match the envelope",
                    entry.name
                ));
            }
            out.write_all(&bytes)
                .map_err(|e| format!("cannot write {}: {e}", entry.name))?;
            written += bytes.len() as u64;
            continue;
        }
        let (base_hash, compressed_len) = parse_op(&line, &entry.name, op_limit)?;
        let mut frame = vec![0u8; compressed_len];
        read_exact(
            &mut *delta,
            &mut frame,
            &format!("{}'s compressed data", entry.name),
        )?;
        let base = match base_hash {
            Some(base_hash) => {
                let bytes = installed(&entry.name)
                    .map_err(|e| format!("cannot read the installed {}: {e}", entry.name))?;
                if !sum(&bytes).eq_ignore_ascii_case(&base_hash) {
                    return Err(format!(
                        "the installed {} is not the base this patch was made from",
                        entry.name
                    ));
                }
                Some(bytes)
            }
            None => None,
        };
        decompress(
            &frame,
            base.as_deref(),
            entry,
            &mut *out,
            &mut written,
            max_envelope,
        )?;
    }
    if !at_end(&mut *delta)? {
        return Err("the delta has bytes after its last file".into());
    }
    Ok(written)
}

fn parse_op(line: &str, name: &str, op_limit: u64) -> Result<(Option<String>, usize), String> {
    let mut parts = line.split(' ');
    let base = match parts.next() {
        Some("zstd") => None,
        Some("patch") => {
            let hash = parts
                .next()
                .filter(|hash| valid_hash(hash))
                .ok_or_else(|| format!("{name}'s patch base is malformed"))?;
            Some(hash.to_ascii_lowercase())
        }
        _ => return Err(format!("{name} has an unknown delta operation")),
    };
    let len: u64 = parts
        .next()
        .ok_or_else(|| format!("{name}'s compressed length is missing"))?
        .parse()
        .map_err(|_| format!("{name}'s compressed length is not a number"))?;
    if parts.next().is_some() {
        return Err(format!("{name}'s delta operation has trailing fields"));
    }
    if len == 0 || len > op_limit {
        return Err(format!(
            "{name}'s compressed length is outside the supported range"
        ));
    }
    let len = usize::try_from(len).map_err(|_| format!("{name}'s compressed data is too large"))?;
    Ok((base, len))
}

/// Decompresses one file into `out`, verifying exact size and SHA-256 on the fly.
fn decompress(
    data: &[u8],
    prefix: Option<&[u8]>,
    entry: &Entry,
    out: &mut impl Write,
    written: &mut u64,
    max_envelope: u64,
) -> Result<(), String> {
    if entry.size > max_envelope.saturating_sub(*written) {
        return Err(format!(
            "{} would take the envelope over the {max_envelope}-byte limit",
            entry.name
        ));
    }
    let mut context = DCtx::create();
    context.init().map_err(zstd_error)?;
    context
        .set_parameter(DParameter::WindowLogMax(WINDOW_LOG_MAX))
        .map_err(zstd_error)?;
    if let Some(prefix) = prefix {
        context.ref_prefix(prefix).map_err(zstd_error)?;
    }
    let mut input = InBuffer::around(data);
    let mut buffer = vec![0u8; 64 * 1024];
    let mut total = 0u64;
    let mut hasher = Sha256::new();
    loop {
        let mut output = OutBuffer::around(&mut buffer[..]);
        let hint = context
            .decompress_stream(&mut output, &mut input)
            .map_err(zstd_error)?;
        let count = output.pos();
        if count > 0 {
            total += count as u64;
            if total > entry.size {
                return Err(format!(
                    "{} decoded to more than the {} bytes its manifest line says",
                    entry.name, entry.size
                ));
            }
            let chunk = &buffer[..count];
            hasher.update(chunk);
            out.write_all(chunk)
                .map_err(|e| format!("cannot write {}: {e}", entry.name))?;
        }
        if hint == 0 {
            if input.pos() != data.len() {
                return Err(format!(
                    "{}'s compressed frame has trailing bytes",
                    entry.name
                ));
            }
            break;
        }
        if input.pos() == data.len() && count == 0 {
            return Err(format!("{}'s compressed frame is truncated", entry.name));
        }
    }
    if total != entry.size {
        return Err(format!(
            "{} decoded to {total} bytes; its manifest line says {}",
            entry.name, entry.size
        ));
    }
    if !hex(hasher.finalize()).eq_ignore_ascii_case(&entry.sha256) {
        return Err(format!(
            "{} does not match its checksum in the envelope",
            entry.name
        ));
    }
    *written += total;
    Ok(())
}

#[cfg(any(test, feature = "encode"))]
fn compress(
    data: &[u8],
    prefix: Option<&[u8]>,
    level: i32,
    window: u32,
) -> Result<Vec<u8>, String> {
    let mut context = CCtx::create();
    context
        .set_parameter(CParameter::CompressionLevel(level))
        .map_err(zstd_error)?;
    context
        .set_parameter(CParameter::WindowLog(window))
        .map_err(zstd_error)?;
    if prefix.is_some() {
        context
            .set_parameter(CParameter::EnableLongDistanceMatching(true))
            .map_err(zstd_error)?;
    }
    // Prefix is single-use and resets session state, so set pledged size after it.
    if let Some(prefix) = prefix {
        context.ref_prefix(prefix).map_err(zstd_error)?;
    }
    context
        .set_pledged_src_size(Some(data.len() as u64))
        .map_err(zstd_error)?;
    let mut out = Vec::with_capacity(zstd::zstd_safe::compress_bound(data.len()));
    context.compress2(&mut out, data).map_err(zstd_error)?;
    Ok(out)
}

/// Window log covering the patch base + target (or just target for plain zstd).
#[cfg(any(test, feature = "encode"))]
fn window_log(bytes: u64) -> u32 {
    let needed = if bytes <= 1 {
        0
    } else {
        64 - (bytes - 1).leading_zeros()
    };
    needed.clamp(20, WINDOW_LOG_MAX)
}

fn zstd_error(code: usize) -> String {
    format!("zstd error {code}")
}

fn line_at(bytes: &[u8], start: usize) -> Result<(String, usize), String> {
    if start >= bytes.len() {
        return Err("the envelope header is truncated".into());
    }
    let end = start.saturating_add(MAX_LINE).min(bytes.len());
    for (offset, byte) in bytes[start..end].iter().enumerate() {
        if *byte == b'\n' {
            let index = start + offset;
            let text = std::str::from_utf8(&bytes[start..index])
                .map_err(|_| "the envelope header is not text".to_owned())?;
            if !text.is_ascii() {
                return Err("the envelope header is not text".into());
            }
            return Ok((text.to_owned(), index + 1));
        }
    }
    Err("an envelope header line is too long".into())
}

/// One `\n`-terminated operation line from the delta stream.
fn op_line(reader: &mut impl Read) -> Result<String, String> {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match reader.read(&mut byte) {
            Ok(0) => return Err("the delta ends in the middle of a line".into()),
            Ok(_) if byte[0] == b'\n' && line.len() <= MAX_LINE => {
                let text = String::from_utf8(line)
                    .map_err(|_| "the delta operation is not text".to_owned())?;
                return Ok(text);
            }
            Ok(_) if line.len() >= MAX_LINE => {
                return Err("a delta operation line is too long".into());
            }
            Ok(_) if !byte[0].is_ascii() => {
                return Err("the delta operation is not text".into());
            }
            Ok(_) => line.push(byte[0]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(format!("the delta could not be read: {e}")),
        }
    }
}

fn read_exact(reader: &mut impl Read, bytes: &mut [u8], what: &str) -> Result<(), String> {
    reader
        .read_exact(bytes)
        .map_err(|e| format!("{what} could not be read: {e}"))
}

fn at_end(reader: &mut impl Read) -> Result<bool, String> {
    let mut byte = [0u8; 1];
    loop {
        match reader.read(&mut byte) {
            Ok(0) => return Ok(true),
            Ok(_) => return Ok(false),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(format!("the delta could not be read: {e}")),
        }
    }
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.bytes().any(|b| matches!(b, b'/' | b'\\' | 0))
}

fn valid_hash(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| b.is_ascii_hexdigit())
}

fn sum(data: &[u8]) -> String {
    hex(Sha256::digest(data))
}

fn hex(digest: impl AsRef<[u8]>) -> String {
    let digest = digest.as_ref();
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn envelope(files: &[(&str, Vec<u8>)]) -> Vec<u8> {
        let mut body = Vec::new();
        for (name, data) in files {
            body.extend_from_slice(format!("{}  {}  {name}\n", sum(data), data.len()).as_bytes());
        }
        for (_, data) in files {
            body.extend_from_slice(data);
        }
        let mut out = Vec::new();
        out.extend_from_slice(ENVELOPE_MAGIC.as_bytes());
        out.extend_from_slice(format!("length {}\n", body.len()).as_bytes());
        out.extend_from_slice(format!("sha256 {}\n", sum(&body)).as_bytes());
        out.extend_from_slice(&body);
        out
    }

    fn sample() -> (Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>) {
        let shared = b"a file the mixer already has\n".repeat(20);
        let old = b"the previous rc2fxd build\n".repeat(200);
        let mut new = old.clone();
        new[7] ^= 1;
        new.extend_from_slice(&b"and its new tail\n".repeat(20));
        let gui = b"compressible gui bytes ".repeat(2000);
        (shared, old, new, gui)
    }

    fn installed_for(shared: &[u8], old: &[u8]) -> BTreeMap<String, String> {
        BTreeMap::from([
            ("librc2fx_native.so".to_owned(), sum(shared)),
            ("rc2fxd".to_owned(), sum(old)),
        ])
    }

    fn bases(old: &[u8]) -> BTreeMap<String, Vec<u8>> {
        BTreeMap::from([("rc2fxd".to_owned(), old.to_vec())])
    }

    #[test]
    fn copy_zstd_and_patch_round_trip_to_the_same_envelope() {
        let (shared, old, new, gui) = sample();
        let envelope = envelope(&[
            ("librc2fx_native.so", shared.clone()),
            ("rc2fxd", new.clone()),
            ("librc2fx_gui.so", gui.clone()),
        ]);
        let delta = encode(&envelope, &installed_for(&shared, &old), &bases(&old), 19).unwrap();
        assert!(delta.starts_with(MAGIC.as_bytes()));
        let text = String::from_utf8_lossy(&delta);
        assert!(text.contains("copy\n"), "an unchanged file is copied");
        assert!(
            text.contains("patch "),
            "a changed file with a base is patched"
        );
        assert!(
            text.contains("zstd "),
            "a file without a base is sent whole"
        );
        assert!(delta.len() < envelope.len());

        let mut restored = Vec::new();
        let written = decode(
            &mut delta.as_slice(),
            &mut |name| match name {
                "librc2fx_native.so" => Ok(shared.clone()),
                "rc2fxd" => Ok(old.clone()),
                other => Err(io::Error::other(format!("no installed {other}"))),
            },
            &mut restored,
            envelope.len() as u64,
        )
        .unwrap();
        assert_eq!(restored, envelope);
        assert_eq!(written, envelope.len() as u64);
    }

    #[test]
    fn the_manifest_is_read_without_a_fixed_file_count() {
        let files: Vec<(String, Vec<u8>)> = (0..20)
            .map(|index| (format!("file-{index}.bin"), vec![index as u8; index]))
            .collect();
        let refs: Vec<(&str, Vec<u8>)> = files
            .iter()
            .map(|(name, data)| (name.as_str(), data.clone()))
            .collect();
        let envelope = envelope(&refs);
        let (head, entries) = envelope_entries(&envelope).unwrap();
        assert_eq!(entries.len(), 20);
        assert_eq!(
            head + entries.iter().map(|e| e.size as usize).sum::<usize>(),
            envelope.len()
        );
        for (entry, (name, data)) in entries.iter().zip(&files) {
            assert_eq!(&entry.name, name);
            assert_eq!(entry.size, data.len() as u64);
            assert_eq!(entry.sha256, sum(data));
        }
    }

    // Decoder rejection cases live in fixtures/delta/reject (shared with Go via
    // tests/shared_fixtures.rs); this test covers envelope header parsing.
    #[test]
    fn malformed_envelope_heads_are_refused() {
        for bad in [
            Vec::new(),
            b"RC2FX-EMBEDDED-PAYLOAD 1\nlength 1\n".to_vec(),
            b"RC2FX-EMBEDDED-PAYLOAD 1\nlength 1\nsha256 x\n".to_vec(),
            format!("length 3\nsha256 {}\n", "0".repeat(64)).into_bytes(),
        ] {
            assert!(
                envelope_entries(&bad).is_err(),
                "{:?}",
                String::from_utf8_lossy(&bad)
            );
        }
        // Invalid manifest lines: path traversal, ".", non-numeric size, duplicate name.
        for line in [
            format!("{}  1  ../escape\n", "a".repeat(64)),
            format!("{}  1  .\n", "a".repeat(64)),
            format!("{}  x  file\n", "a".repeat(64)),
            format!("{}  1  file\n{}  2  file\n", "a".repeat(64), "b".repeat(64)),
        ] {
            let mut bytes = Vec::new();
            bytes.extend_from_slice(ENVELOPE_MAGIC.as_bytes());
            bytes.extend_from_slice(format!("length {}\n", line.len()).as_bytes());
            bytes.extend_from_slice(format!("sha256 {}\n", "0".repeat(64)).as_bytes());
            bytes.extend_from_slice(line.as_bytes());
            assert!(envelope_entries(&bytes).is_err(), "{line:?}");
        }
    }
}
