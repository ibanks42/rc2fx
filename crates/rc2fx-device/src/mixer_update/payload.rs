//! The signed payload's envelope, read and checked on the mixer.
//!
//! The desktop installer ships one standalone envelope
//! (`RC2FX-EMBEDDED-PAYLOAD 1`): two header lines, one
//! `<sha256>  <size>  <name>` line per file in [`PAYLOAD_NAMES`] order, then
//! the files' bytes in that order. This module reads exactly that, and applies
//! the same rules the desktop applies before it embeds a payload, so a
//! malformed or non-release bundle never reaches `/opt`.

use super::install::{BINARIES, PAYLOAD_NAMES, PINNED, sum};
use sha2::Digest;
use std::io::{self, Read};

pub const MAGIC: &str = rc2fx_delta::ENVELOPE_MAGIC;
/// The envelope's sanity limit: the payload is about 11 MiB.
pub const MAX_BODY: u64 = 256 << 20;
const MAX_HEADER_LINE: u64 = 512;
const DEVELOPMENT: &str = "rc2fx_build=development";
const RELEASE: &str = "rc2fx_build=release";

/// The payload's files, in [`PAYLOAD_NAMES`] order, each checked against its
/// manifest line.
pub type Files = Vec<(String, Vec<u8>)>;

/// Read one standalone envelope. `reader` must contain nothing else.
pub fn read(reader: &mut impl Read) -> Result<Files, String> {
    let mut magic = [0u8; MAGIC.len()];
    reader
        .read_exact(&mut magic)
        .map_err(|e| format!("the payload header is truncated: {e}"))?;
    if magic != MAGIC.as_bytes() {
        return Err("this is not an RC2FX mixer payload".into());
    }
    let length: u64 = line(reader, "length ")?
        .parse()
        .map_err(|_| "the payload's length line is not a number".to_owned())?;
    if length == 0 || length > MAX_BODY {
        return Err("the payload's length is outside the supported range".into());
    }
    let digest = line(reader, "sha256 ")?;
    if digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("the payload's digest is malformed".into());
    }
    let digest = digest.to_ascii_lowercase();

    // The body is the manifest followed by the files. Every byte read here is
    // hashed, so the whole body is checked as well as each file.
    let mut body = DigestReader::new((&mut *reader).take(length));
    let mut manifest = Vec::with_capacity(PAYLOAD_NAMES.len());
    for name in PAYLOAD_NAMES {
        let line = body.line(MAX_HEADER_LINE)?;
        let mut parts = line.splitn(3, "  ");
        let (Some(hash), Some(size), Some(entry)) = (parts.next(), parts.next(), parts.next())
        else {
            return Err(format!(
                "the payload's manifest line for {name} is malformed"
            ));
        };
        if entry != name {
            return Err(format!("the payload has {entry} where {name} belongs"));
        }
        if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(format!("{name}'s checksum in the payload is malformed"));
        }
        let size: usize = size
            .parse()
            .map_err(|_| format!("{name}'s size in the payload is not a number"))?;
        if size as u64 > length {
            return Err(format!("{name}'s size in the payload is impossible"));
        }
        manifest.push((hash.to_ascii_lowercase(), size));
    }
    let mut files = Files::with_capacity(PAYLOAD_NAMES.len());
    // The manifest was read in PAYLOAD_NAMES order and each entry checked
    // against its name, so the files follow in that order.
    for ((hash, size), name) in manifest.iter().zip(PAYLOAD_NAMES) {
        let mut data = vec![0u8; *size];
        body.read_exact(&mut data)
            .map_err(|e| format!("{name} is truncated in the payload: {e}"))?;
        if sum(&data) != *hash {
            return Err(format!("{name} does not match its checksum in the payload"));
        }
        files.push(((*name).to_owned(), data));
    }
    let mut trailing = [0u8; 1];
    if body.read(&mut trailing).unwrap_or(0) != 0 || body.consumed() != length {
        return Err("the payload has bytes after its last file".into());
    }
    if body.digest() != digest {
        return Err("the payload does not match its digest".into());
    }
    let mut extra = [0u8; 1];
    if reader.read(&mut extra).unwrap_or(0) != 0 {
        return Err("the payload has trailing bytes".into());
    }
    validate(&files)?;
    Ok(files)
}

/// One `prefix` line from the header; the value without its newline.
fn line(reader: &mut impl Read, prefix: &str) -> Result<String, String> {
    let mut line = String::new();
    let mut byte = [0u8; 1];
    loop {
        match reader.read(&mut byte) {
            Ok(0) => return Err("the payload header is truncated".into()),
            Ok(_) => {
                if byte[0] == b'\n' {
                    break;
                }
                if line.len() as u64 >= MAX_HEADER_LINE {
                    return Err("the payload header line is too long".into());
                }
                if !byte[0].is_ascii() {
                    return Err("the payload header is not text".into());
                }
                line.push(byte[0] as char);
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(format!("the payload header could not be read: {e}")),
        }
    }
    line.strip_prefix(prefix)
        .map(str::to_owned)
        .ok_or_else(|| format!("the payload header has no `{prefix}` line"))
}

/// A reader that hashes and counts everything it returns, bounded by the
/// body's declared length.
struct DigestReader<R> {
    inner: R,
    hasher: sha2::Sha256,
    consumed: u64,
}

impl<R: Read> DigestReader<R> {
    fn new(inner: R) -> Self {
        Self {
            inner,
            hasher: sha2::Sha256::new(),
            consumed: 0,
        }
    }
    fn consumed(&self) -> u64 {
        self.consumed
    }
    fn digest(&self) -> String {
        let digest = self.hasher.clone().finalize();
        let mut out = String::with_capacity(64);
        for byte in digest {
            out.push_str(&format!("{byte:02x}"));
        }
        out
    }
    fn line(&mut self, limit: u64) -> Result<String, String> {
        let mut line = String::new();
        let mut byte = [0u8; 1];
        loop {
            self.read_exact(&mut byte)
                .map_err(|e| format!("the payload's manifest is truncated: {e}"))?;
            if byte[0] == b'\n' {
                return Ok(line);
            }
            if line.len() as u64 >= limit {
                return Err("the payload's manifest line is too long".into());
            }
            if !byte[0].is_ascii() {
                return Err("the payload's manifest is not text".into());
            }
            line.push(byte[0] as char);
        }
    }
}

impl<R: Read> Read for DigestReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let read = self.inner.read(buf)?;
        self.hasher.update(&buf[..read]);
        self.consumed += read as u64;
        Ok(read)
    }
}

/// The checks the desktop applies before embedding a payload: the pinned
/// manifest, ARM64 ELF binaries and libraries, a catalog and release manifest
/// this build understands, and a release `rc2fx.sh`. A development payload is
/// refused: this API has no developer path.
pub fn validate(files: &Files) -> Result<(), String> {
    if files.len() != PAYLOAD_NAMES.len() {
        return Err("the payload does not carry every runtime file".into());
    }
    for (want, (name, _)) in PAYLOAD_NAMES.iter().zip(files) {
        if name.as_str() != *want {
            return Err(format!("the payload has {name} where {want} belongs"));
        }
    }
    let mut manifest = String::new();
    for name in PINNED {
        manifest.push_str(&format!("{}  {name}\n", sum(find_file(files, name)?)));
    }
    if manifest.as_bytes() != find_file(files, "SHA256SUMS")? {
        return Err("the payload's SHA256SUMS does not match its files".into());
    }
    for name in BINARIES {
        let bytes = find_file(files, name)?;
        if bytes.len() < 64
            || bytes[..6] != [127, b'E', b'L', b'F', 2, 1]
            || bytes[18] != 0xb7
            || bytes[19] != 0
        {
            return Err(format!("{name} is not an ARM64 executable"));
        }
    }
    let catalog = std::str::from_utf8(find_file(files, "firmware-profiles.json")?)
        .map_err(|_| "the payload's firmware catalog is not UTF-8".to_owned())?;
    rc2fx_firmware::Catalog::parse(catalog)
        .map_err(|e| format!("the payload's firmware catalog is not supported: {e}"))?;
    rc2fx_core::MixerRelease::parse(find_file(files, "mixer-release.json")?)
        .map_err(|e| format!("the payload's mixer-release.json is not supported: {e}"))?;
    release_script(find_file(files, "rc2fx.sh")?)
}

fn find_file<'a>(files: &'a Files, name: &str) -> Result<&'a [u8], String> {
    files
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, data)| data.as_slice())
        .ok_or_else(|| format!("the payload is missing {name}"))
}

/// Exactly one `rc2fx_build=` line, and it must say `release`.
fn release_script(script: &[u8]) -> Result<(), String> {
    let text = std::str::from_utf8(script)
        .map_err(|_| "the payload's rc2fx.sh is not UTF-8".to_owned())?;
    let mut found: Option<&str> = None;
    for raw in text.split_inclusive('\n') {
        let line = raw.trim();
        if line == DEVELOPMENT || line == RELEASE {
            if found.is_some() {
                return Err("the payload's rc2fx.sh has more than one build line".into());
            }
            found = Some(line);
        } else if line.starts_with("rc2fx_build") || line.starts_with("export rc2fx_build") {
            return Err("the payload's rc2fx.sh has an unsupported build line".into());
        }
    }
    // The detached commit step is the new release's own script: a release
    // without it would be installed and then never committed or rolled back.
    if !text
        .lines()
        .any(|line| line.trim_start().starts_with("update-commit)"))
    {
        return Err(
            "this release can't finish an in-place install; use the installer's full flash".into(),
        );
    }
    match found {
        Some(RELEASE) => Ok(()),
        Some(_) => Err("the payload is a development build; only release builds install".into()),
        None => Err("the payload's rc2fx.sh has no build line".into()),
    }
}
