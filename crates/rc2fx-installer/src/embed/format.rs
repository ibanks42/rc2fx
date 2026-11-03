//! The installer payload: which files it ships, the canonical body, and the
//! envelope a built executable carries.
//!
//! This module is compiled twice — into the library, and into `build.rs` via
//! `#[path]` — so it may only use `std`, `sha2` and `serde_json`. It is the
//! one place that decides the payload's contents, its checksums and what a
//! release build changes; the desktop app embeds a single envelope of it, so
//! it needs no files beside the executable.

// Compiled into the build script as well as the library; each uses a subset.
#![allow(dead_code)]

use super::catalog::Catalog;
use std::{
    ops::Range,
    path::{Path, PathBuf},
};

/// The runtime, catalog and scripts the installer adds to official firmware,
/// in the order their bytes appear in an embedded envelope. The order is
/// pinned: the checksummed files first, then the manifest itself.
pub const PAYLOAD_NAMES: [&str; 12] = [
    "librc2fx_native.so",
    "rc2fxd",
    "rc2fx-firmware",
    "firmware-profiles.json",
    "librc2fx_gui.so",
    "librc2fx_jackerr.so",
    "rc2fx_jackerr_snapshot",
    "SHA256SUMS",
    "rc2fx.sh",
    "rc2fx-shim.sh",
    "rc2fx-gui-shim.sh",
    "install.sh",
];
/// The order of the bundle's SHA256SUMS lines.
pub const PINNED: [&str; 7] = [
    "librc2fx_native.so",
    "rc2fxd",
    "rc2fx-firmware",
    "firmware-profiles.json",
    "librc2fx_gui.so",
    "librc2fx_jackerr.so",
    "rc2fx_jackerr_snapshot",
];
/// Payload files that are ARM64 ELF executables or libraries.
pub const BINARIES: [&str; 6] = [
    "librc2fx_native.so",
    "rc2fxd",
    "rc2fx-firmware",
    "librc2fx_gui.so",
    "librc2fx_jackerr.so",
    "rc2fx_jackerr_snapshot",
];
/// Payload files that come from `xtask/mixer` and run in the mixer's shell.
pub const SCRIPTS: [&str; 4] = [
    "rc2fx.sh",
    "rc2fx-shim.sh",
    "rc2fx-gui-shim.sh",
    "install.sh",
];

/// The first line of an embedded envelope.
pub const MAGIC: &str = "RC2FX-EMBEDDED-PAYLOAD 1\n";
/// The line `rc2fx.sh` carries to say what kind of build it is (see its
/// `gui_diagnostics`): development builds log the GUI verbosely by default,
/// release packages quietly.
pub const BUILD_LINE: &str = "rc2fx_build=development";
pub const RELEASE_LINE: &str = "rc2fx_build=release";
pub const DEVELOPMENT: &str = "development";
pub const RELEASE: &str = "release";

/// A sanity limit for the envelope's body: the payload is ~10 MB.
const MAX_BODY: usize = 256 << 20;

/// A payload's files, in [`PAYLOAD_NAMES`] order.
pub type Files = Vec<(String, Vec<u8>)>;

pub fn sum(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(data);
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

pub fn crlf_to_lf(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    for (i, &b) in data.iter().enumerate() {
        if !(b == b'\r' && data.get(i + 1) == Some(&b'\n')) {
            out.push(b);
        }
    }
    out
}

/// The single `rc2fx_build=` line in a script: what it says and where the
/// assignment is, so a rewrite changes nothing else.
struct BuildLine {
    kind: &'static str,
    start: usize,
    end: usize,
}

/// Classify a script's build line. Exactly one exact assignment is supported;
/// any other mention of the variable is refused rather than guessed at, so a
/// payload cannot ship a second, conflicting assignment.
fn build_line(script: &[u8]) -> Result<BuildLine, String> {
    let text = std::str::from_utf8(script).map_err(|e| format!("rc2fx.sh is not UTF-8: {e}"))?;
    let mut found: Option<BuildLine> = None;
    let mut at = 0;
    for raw in text.split_inclusive('\n') {
        let line = raw.trim_end_matches(['\r', '\n']);
        let trimmed = line.trim();
        if trimmed == BUILD_LINE || trimmed == RELEASE_LINE {
            if found.is_some() {
                return Err(format!(
                    "rc2fx.sh must have exactly one `{BUILD_LINE}` line; found more than one"
                ));
            }
            let indent = line.len() - line.trim_start().len();
            found = Some(BuildLine {
                kind: if trimmed == RELEASE_LINE {
                    RELEASE
                } else {
                    DEVELOPMENT
                },
                start: at + indent,
                end: at + line.len(),
            });
        } else if trimmed.starts_with("rc2fx_build") || trimmed.starts_with("export rc2fx_build") {
            return Err(format!(
                "rc2fx.sh has an unsupported `rc2fx_build` line: `{trimmed}`; only one exact `{BUILD_LINE}` line is allowed"
            ));
        }
        at += raw.len();
    }
    found.ok_or_else(|| format!("rc2fx.sh must have exactly one `{BUILD_LINE}` line; found none"))
}

/// Which kind of build a `rc2fx.sh` is, from its own build line.
pub fn build_kind(script: &[u8]) -> Result<&'static str, String> {
    Ok(build_line(script)?.kind)
}

/// The `rc2fx.sh` a release ships: its build line says `release`, and nothing
/// else changes. A release script is returned as it is.
pub fn release_script(script: &[u8]) -> Result<Vec<u8>, String> {
    let line = build_line(script)?;
    if line.kind == RELEASE {
        return Ok(script.to_vec());
    }
    let text = std::str::from_utf8(script).map_err(|e| format!("rc2fx.sh is not UTF-8: {e}"))?;
    let mut out = String::with_capacity(text.len());
    out.push_str(&text[..line.start]);
    out.push_str(RELEASE_LINE);
    out.push_str(&text[line.end..]);
    let out = out.into_bytes();
    // The rewritten script must say release, or the payload is not shippable.
    if build_kind(&out)? != RELEASE {
        return Err("rc2fx.sh still does not say `release` after rewriting".to_owned());
    }
    Ok(out)
}

/// Where a payload's runtime files sit: `runtime/` in a release folder, or the
/// mixer build's `target/mixer` in a source checkout.
pub fn source_dir(root: &Path) -> PathBuf {
    let runtime = root.join("runtime");
    if runtime.is_dir() {
        runtime
    } else {
        root.join("target").join("mixer")
    }
}

/// Read and check the payload that sits in a source checkout or a release
/// folder: `runtime/` (a release folder) or `target/mixer` (a checkout) for
/// the runtime, `xtask/mixer` for the scripts, and the installer catalog in
/// `config/`. `release` applies the release build line.
pub fn read(root: &Path, release: bool) -> Result<Files, String> {
    let source = source_dir(root);
    let mut files = Files::with_capacity(PAYLOAD_NAMES.len());
    for name in PAYLOAD_NAMES {
        let script = SCRIPTS.contains(&name);
        let path = if script {
            root.join("xtask").join("mixer").join(name)
        } else {
            source.join(name)
        };
        let mut data =
            std::fs::read(&path).map_err(|e| format!("runtime bundle missing {name}: {e}"))?;
        if script {
            data = crlf_to_lf(&data);
            if name == "rc2fx.sh" {
                build_kind(&data)?;
                if release {
                    data = release_script(&data)?;
                }
            }
        }
        files.push((name.to_owned(), data));
    }
    let catalog_path = root.join("config").join("firmware-profiles.json");
    let installer: serde_json::Value = serde_json::from_slice(
        &std::fs::read(&catalog_path)
            .map_err(|e| format!("installer catalog missing {catalog_path:?}: {e}"))?,
    )
    .unwrap_or_default();
    let view = view(&files);
    let bundled: serde_json::Value =
        serde_json::from_slice(data_of(&view, "firmware-profiles.json")?).unwrap_or_default();
    if installer != bundled {
        return Err("runtime and installer catalogs differ".to_owned());
    }
    validate(&view)?;
    Ok(files)
}

/// A borrowed view of a payload, in [`PAYLOAD_NAMES`] order.
pub fn view(files: &Files) -> Vec<(&str, &[u8])> {
    files
        .iter()
        .map(|(name, data)| (name.as_str(), data.as_slice()))
        .collect()
}

/// The checks every payload passes: at build time, again when an embedded one
/// is used, and for the folder a developer tool points at.
pub fn validate(files: &[(&str, &[u8])]) -> Result<(), String> {
    if files.len() != PAYLOAD_NAMES.len() {
        return Err("incomplete runtime bundle".to_owned());
    }
    for (want, (name, _)) in PAYLOAD_NAMES.iter().zip(files) {
        if name != want {
            return Err(format!("runtime missing {want}"));
        }
    }
    let mut manifest = String::new();
    for name in PINNED {
        manifest.push_str(&format!("{}  {name}\n", sum(data_of(files, name)?)));
    }
    if manifest.as_bytes() != data_of(files, "SHA256SUMS")? {
        return Err("runtime checksum mismatch".to_owned());
    }
    for name in BINARIES {
        let b = data_of(files, name)?;
        if b.len() < 64 || b[..6] != [127, b'E', b'L', b'F', 2, 1] || b[18] != 0xb7 || b[19] != 0 {
            return Err(format!("{name} is not ARM64 ELF"));
        }
    }
    let data = data_of(files, "firmware-profiles.json")?;
    // Syntax first, so a broken file says so, then the exact shape the
    // installer requires at run time: a catalog it would reject must never
    // reach a binary.
    let _: serde_json::Value = serde_json::from_slice(data)
        .map_err(|e| format!("the runtime catalog is not JSON: {e}"))?;
    let catalog: Catalog = serde_json::from_slice(data)
        .map_err(|e| format!("the runtime catalog is not what this installer supports: {e}"))?;
    if catalog.schema != 1 {
        return Err("unsupported runtime catalog".to_owned());
    }
    build_kind(data_of(files, "rc2fx.sh")?)?;
    Ok(())
}

fn data_of<'a>(files: &[(&str, &'a [u8])], name: &str) -> Result<&'a [u8], String> {
    files
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, data)| *data)
        .ok_or_else(|| format!("runtime missing {name}"))
}

/// The canonical body: one `<sha256>  <size>  <name>` line per file in
/// [`PAYLOAD_NAMES`] order, then the files' bytes concatenated in that order.
/// Deterministic: no timestamps, owners or platform details.
pub fn body(files: &[(&str, &[u8])]) -> Vec<u8> {
    let total: usize = files.iter().map(|(_, data)| data.len()).sum();
    let mut out = Vec::with_capacity(total + 96 * files.len());
    for (name, data) in files {
        out.extend_from_slice(format!("{}  {}  {name}\n", sum(data), data.len()).as_bytes());
    }
    for (_, data) in files {
        out.extend_from_slice(data);
    }
    out
}

/// Frame a body as the envelope an executable carries.
pub fn wrap(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(MAGIC.len() + 96 + body.len());
    out.extend_from_slice(MAGIC.as_bytes());
    out.extend_from_slice(format!("length {}\n", body.len()).as_bytes());
    out.extend_from_slice(format!("sha256 {}\n", sum(body)).as_bytes());
    out.extend_from_slice(body);
    out
}

/// One verified embedded payload: the canonical encoded blob an executable
/// carries, parsed and checked.
#[derive(Clone, Debug, PartialEq)]
pub struct Envelope {
    blob: Vec<u8>,
    body: Range<usize>,
}

impl Envelope {
    /// The payload these sources must embed.
    pub fn expected(root: &Path, release: bool) -> Result<Self, String> {
        let files = read(root, release)?;
        let blob = wrap(&body(&view(&files)));
        Self::find(&blob)
    }

    /// Find the payload embedded in an executable's bytes. The marker string
    /// can also occur in ordinary compiled data, so every occurrence is
    /// checked and exactly one complete, checksummed envelope is required.
    pub fn find(executable: &[u8]) -> Result<Self, String> {
        let mut valid = Vec::new();
        let mut candidates = 0;
        let mut damaged = None;
        for at in occurrences(executable, MAGIC.as_bytes()) {
            candidates += 1;
            match parse_at(executable, at) {
                Ok(envelope) => valid.push(envelope),
                Err(e) => {
                    damaged.get_or_insert(e);
                }
            }
        }
        match (valid.len(), candidates) {
            (1, _) => Ok(valid.pop().expect("one envelope")),
            (0, 0) => Err(
                "the executable does not embed an installer payload; rebuild it from a staged payload"
                    .to_owned(),
            ),
            (0, _) => Err(format!(
                "the embedded installer payload is damaged: {}",
                damaged.unwrap_or_default()
            )),
            (n, _) => Err(format!(
                "the executable embeds {n} installer payloads; exactly one is required"
            )),
        }
    }

    /// The canonical encoded blob: prefix it and append anything to make a
    /// stand-in executable.
    pub fn as_bytes(&self) -> &[u8] {
        &self.blob
    }

    pub fn body(&self) -> &[u8] {
        &self.blob[self.body.clone()]
    }

    /// The payload's files, in [`PAYLOAD_NAMES`] order, each checked against
    /// the manifest.
    pub fn files(&self) -> Result<Vec<(&str, &[u8])>, String> {
        let mut rest = self.body();
        // All manifest lines come first, then the files' bytes.
        let mut pinned = Vec::with_capacity(PAYLOAD_NAMES.len());
        for name in PAYLOAD_NAMES {
            let end = rest
                .iter()
                .position(|&b| b == b'\n')
                .ok_or_else(|| format!("the embedded payload has no manifest line for {name}"))?;
            let line = std::str::from_utf8(&rest[..end])
                .map_err(|_| "the embedded payload's manifest is not UTF-8".to_owned())?;
            rest = &rest[end + 1..];
            let mut parts = line.splitn(3, "  ");
            let (Some(digest), Some(size), Some(entry)) =
                (parts.next(), parts.next(), parts.next())
            else {
                return Err(format!(
                    "the embedded payload's manifest is malformed: {line}"
                ));
            };
            if entry != name {
                return Err(format!(
                    "the embedded payload has {entry} where {name} belongs"
                ));
            }
            if digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(format!("{name}'s embedded checksum is malformed"));
            }
            let size: usize = size
                .parse()
                .map_err(|_| format!("{name}'s embedded size `{size}` is not a number"))?;
            pinned.push((name, digest, size));
        }
        let mut files = Vec::with_capacity(pinned.len());
        for (name, digest, size) in pinned {
            let (data, tail) = rest
                .split_at_checked(size)
                .ok_or_else(|| format!("{name} is truncated in the embedded payload"))?;
            if sum(data) != digest {
                return Err(format!("{name} does not match its embedded checksum"));
            }
            files.push((name, data));
            rest = tail;
        }
        if !rest.is_empty() {
            return Err("the embedded payload has bytes after its last file".to_owned());
        }
        Ok(files)
    }

    /// The payload as name → bytes, after the same checks a folder's payload
    /// passes (`rc2fx_installer::Bundle` is this map).
    pub fn bundle(&self) -> Result<std::collections::HashMap<String, Vec<u8>>, String> {
        let files = self.files()?;
        validate(&files)?;
        Ok(files
            .into_iter()
            .map(|(name, data)| (name.to_owned(), data.to_vec()))
            .collect())
    }

    /// Refuse a payload that is not byte-for-byte this build's.
    pub fn require_matches(&self, expected: &Envelope) -> Result<(), String> {
        if self.blob == expected.blob {
            return Ok(());
        }
        let mut differing = Vec::new();
        if let (Ok(got), Ok(want)) = (self.files(), expected.files()) {
            for ((_, got), (name, want)) in got.iter().zip(&want) {
                if got != want {
                    differing.push(*name);
                }
            }
        }
        if differing.is_empty() {
            differing.push("(the payload's length or order)");
        }
        Err(format!(
            "the executable's embedded payload is not this build's: differs in {}",
            differing.join(", ")
        ))
    }
}

fn occurrences(haystack: &[u8], needle: &[u8]) -> Vec<usize> {
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(at) = haystack[from..]
        .windows(needle.len())
        .position(|w| w == needle)
    {
        found.push(from + at);
        from += at + 1;
    }
    found
}

/// Parse and verify one envelope starting at `at`, returning the canonical
/// blob (marker, headers and body).
fn parse_at(bytes: &[u8], at: usize) -> Result<Envelope, String> {
    let mut used = 0;
    let offset = at + MAGIC.len();
    let (length, n) = header(&bytes[offset..], "length ")?;
    used += n;
    let (digest, n) = header(&bytes[offset + used..], "sha256 ")?;
    used += n;
    if length.is_empty() || !length.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!(
            "the embedded payload's length `{length}` is not a number"
        ));
    }
    let length: usize = length
        .parse()
        .map_err(|_| format!("the embedded payload's length `{length}` is not a number"))?;
    if length > MAX_BODY {
        return Err("the embedded payload is implausibly large".to_owned());
    }
    if digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("the embedded payload's digest is malformed".to_owned());
    }
    let body_at = offset + used;
    let body = bytes
        .get(body_at..body_at + length)
        .ok_or("the embedded payload is truncated")?;
    if sum(body) != digest {
        return Err("the embedded payload does not match its digest".to_owned());
    }
    let envelope = Envelope {
        blob: bytes[at..body_at + length].to_vec(),
        body: body_at - at..body_at - at + length,
    };
    // The body's own structure must hold before this candidate counts.
    envelope.files()?;
    Ok(envelope)
}

/// One `prefix` line; returns its value and how many bytes it took, newline
/// included.
fn header<'a>(bytes: &'a [u8], prefix: &str) -> Result<(&'a str, usize), String> {
    let end = bytes
        .iter()
        .position(|&b| b == b'\n')
        .ok_or("the embedded payload header is truncated")?;
    let line = std::str::from_utf8(&bytes[..end])
        .map_err(|_| "the embedded payload header is not UTF-8".to_owned())?;
    let value = line
        .strip_prefix(prefix)
        .ok_or_else(|| format!("the embedded payload header has no `{prefix}` line"))?;
    Ok((value, end + 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CATALOG: &[u8] = br#"{"schemaVersion":1,"profiles":[]}"#;

    fn elf(marker: u8) -> Vec<u8> {
        let mut b = vec![marker; 96];
        b[..6].copy_from_slice(&[127, b'E', b'L', b'F', 2, 1]);
        b[18] = 0xb7;
        b[19] = 0;
        b
    }

    /// Recompute SHA256SUMS from the payload's files.
    fn resum(files: &mut Files) {
        let manifest: String = {
            let view = view(files);
            PINNED
                .iter()
                .map(|n| format!("{}  {n}\n", sum(data_of(&view, n).unwrap())))
                .collect()
        };
        let at = files.iter().position(|(n, _)| n == "SHA256SUMS").unwrap();
        files[at].1 = manifest.into_bytes();
    }

    /// A complete, valid payload in [`PAYLOAD_NAMES`] order.
    fn payload(marker: u8) -> Files {
        let mut files: Files = PAYLOAD_NAMES
            .iter()
            .map(|name| {
                let data = match *name {
                    "firmware-profiles.json" => CATALOG.to_vec(),
                    "SHA256SUMS" => Vec::new(),
                    "rc2fx.sh" => format!("#!/bin/sh\n{BUILD_LINE}\nexit 0\n").into_bytes(),
                    n if SCRIPTS.contains(&n) => b"#!/bin/sh\nexit 0\n".to_vec(),
                    _ => elf(marker),
                };
                ((*name).to_owned(), data)
            })
            .collect();
        resum(&mut files);
        files
    }

    fn set(files: &mut Files, name: &str, data: Vec<u8>) {
        let at = files.iter().position(|(n, _)| n == name).unwrap();
        files[at].1 = data;
        resum(files);
    }

    fn as_release(files: &mut Files) {
        let at = files.iter().position(|(n, _)| n == "rc2fx.sh").unwrap();
        let script = release_script(&files[at].1).unwrap();
        set(files, "rc2fx.sh", script);
    }

    fn wrapped(marker: u8) -> Vec<u8> {
        let files = payload(marker);
        validate(&view(&files)).unwrap();
        wrap(&body(&view(&files)))
    }

    #[test]
    fn a_payload_round_trips_through_the_envelope() {
        let files = payload(1);
        assert!(
            validate(&view(&files)).is_ok(),
            "the fixture itself is valid"
        );
        let wrapped = wrapped(1);
        let envelope = Envelope::find(&wrapped).unwrap();
        assert_eq!(envelope.as_bytes(), wrapped.as_slice());
        assert_eq!(envelope.body(), body(&view(&files)).as_slice());
        let back = envelope.files().unwrap();
        for ((name, want), (got_name, got)) in files.iter().zip(&back) {
            assert_eq!(name, got_name);
            assert_eq!(want.as_slice(), *got);
        }
        let bundle = envelope.bundle().unwrap();
        assert_eq!(bundle.len(), PAYLOAD_NAMES.len());
        let script = &files.iter().find(|(name, _)| name == "rc2fx.sh").unwrap().1;
        assert_eq!(&bundle["rc2fx.sh"], script);
        // A stand-in executable: prefix and suffix around the blob.
        let mut exe = b"\x7fELF not really an executable".to_vec();
        exe.extend_from_slice(&wrapped);
        exe.extend_from_slice(b"trailing .rodata");
        assert_eq!(Envelope::find(&exe).unwrap(), envelope);
    }

    #[test]
    fn exactly_one_complete_envelope_is_required() {
        let one = wrapped(1);
        // The marker in unrelated compiled data is not a payload.
        let mut exe = one.clone();
        exe.extend_from_slice(MAGIC.as_bytes());
        exe.extend_from_slice(b"not a header\nand not a body");
        assert!(
            Envelope::find(&exe).is_ok(),
            "an invalid candidate is ignored"
        );
        // Two complete payloads are ambiguous: refuse rather than guess.
        let mut twice = one.clone();
        twice.extend_from_slice(&wrapped(2));
        let error = Envelope::find(&twice).unwrap_err();
        assert!(error.contains("2 installer payloads"), "{error}");
        // Nothing at all.
        let error = Envelope::find(b"\x7fELF with no payload").unwrap_err();
        assert!(
            error.contains("does not embed an installer payload"),
            "{error}"
        );
    }

    #[test]
    fn a_damaged_or_truncated_envelope_is_refused() {
        let wrapped = wrapped(1);
        let header = MAGIC.len();
        type Damage = Box<dyn Fn(&mut Vec<u8>)>;
        let cases: [(&str, Damage); 6] = [
            (
                "body byte",
                Box::new(|b: &mut Vec<u8>| *b.last_mut().unwrap() ^= 0xff),
            ),
            (
                "digest",
                Box::new(move |b: &mut Vec<u8>| b[header + 20] = b'0'),
            ),
            (
                "length not a number",
                Box::new(move |b: &mut Vec<u8>| b[header + 7] = b'x'),
            ),
            (
                "length larger",
                Box::new(move |b: &mut Vec<u8>| b[header + 7] = b'9'),
            ),
            (
                "truncated",
                Box::new(|b: &mut Vec<u8>| b.truncate(b.len() - 1)),
            ),
            (
                "header",
                Box::new(move |b: &mut Vec<u8>| {
                    b.drain(header..header + 8);
                }),
            ),
        ];
        for (what, damage) in cases {
            let mut damaged = wrapped.clone();
            damage(&mut damaged);
            let error = Envelope::find(&damaged).unwrap_err();
            assert!(
                error.contains("does not embed")
                    || error.contains("damaged")
                    || error.contains("truncated"),
                "{what}: {error}"
            );
        }
        // A body whose per-file checksums no longer hold, with its outer
        // digest consistent: the manifest still has to hold.
        let files = payload(1);
        let mut wrong = body(&view(&files));
        wrong[..64].fill(b'0');
        let error = Envelope::find(&wrap(&wrong)).unwrap_err();
        assert!(
            error.contains("librc2fx_native.so does not match its embedded checksum"),
            "{error}"
        );
    }

    #[test]
    fn the_manifest_pins_names_order_count_and_trailing_bytes() {
        let files = payload(1);
        // Swapped order.
        let mut swapped = files.clone();
        swapped.swap(0, 1);
        assert!(validate(&view(&swapped)).is_err());
        // A renamed entry.
        let mut renamed = files.clone();
        renamed[0].0 = "librc2fx_native.so ".into();
        assert!(validate(&view(&renamed)).is_err());
        // A missing file.
        let mut short = files.clone();
        short.pop();
        assert!(validate(&view(&short)).is_err());
        // Bytes after the last file.
        let mut extra = body(&view(&files));
        extra.extend_from_slice(b"extra");
        let error = Envelope::find(&wrap(&extra)).unwrap_err();
        assert!(error.contains("bytes after its last file"), "{error}");
        // A size that does not add up: one byte short for the last file.
        let mut sizes = body(&view(&files));
        let (last_name, last_data) = files.last().unwrap();
        let old = format!("{}  {}  {last_name}\n", sum(last_data), last_data.len());
        let new = format!("{}  {}  {last_name}\n", sum(last_data), last_data.len() - 1);
        let at = sizes
            .windows(old.len())
            .position(|w| w == old.as_bytes())
            .unwrap();
        sizes.splice(at..at + old.len(), new.into_bytes());
        let error = Envelope::find(&wrap(&sizes)).unwrap_err();
        assert!(
            error.contains("does not match its embedded checksum")
                || error.contains("bytes after its last file"),
            "{error}"
        );
        // A size one byte too large cannot be met either.
        let mut sizes = body(&view(&files));
        let new = format!("{}  {}  {last_name}\n", sum(last_data), last_data.len() + 1);
        let at = sizes
            .windows(old.len())
            .position(|w| w == old.as_bytes())
            .unwrap();
        sizes.splice(at..at + old.len(), new.into_bytes());
        assert!(Envelope::find(&wrap(&sizes)).is_err());
    }

    #[test]
    fn checksums_the_sums_manifest_and_arm64_elfs_are_checked() {
        let mut files = payload(1);
        set(&mut files, "rc2fxd", b"not an ARM64 ELF".to_vec());
        let error = validate(&view(&files)).unwrap_err();
        assert!(error.contains("is not ARM64 ELF"), "{error}");
        let mut files = payload(1);
        files
            .iter_mut()
            .find(|(n, _)| n == "SHA256SUMS")
            .unwrap()
            .1
            .extend_from_slice(b"\n");
        let error = validate(&view(&files)).unwrap_err();
        assert!(error.contains("checksum mismatch"), "{error}");
        let mut files = payload(1);
        set(
            &mut files,
            "firmware-profiles.json",
            br#"{"schemaVersion":2}"#.to_vec(),
        );
        let error = validate(&view(&files)).unwrap_err();
        assert!(error.contains("unsupported runtime catalog"), "{error}");
        // A catalog that is not JSON at all.
        let mut files = payload(1);
        set(&mut files, "firmware-profiles.json", b"{".to_vec());
        let error = validate(&view(&files)).unwrap_err();
        assert!(error.contains("not JSON"), "{error}");
    }

    #[test]
    fn a_build_line_with_trailing_spaces_still_rewrites_to_release() {
        // A stray space must not let a development payload ship as a release.
        for script in [
            &b"#!/bin/sh\nrc2fx_build=development  \nexit 0\n"[..],
            &b"#!/bin/sh\nrc2fx_build=development\t\nexit 0\n"[..],
            &b"#!/bin/sh\r\nrc2fx_build=development\r\nexit 0\r\n"[..],
        ] {
            assert_eq!(build_kind(script).unwrap(), DEVELOPMENT, "{script:?}");
            let released = release_script(script).unwrap();
            assert_eq!(build_kind(&released).unwrap(), RELEASE, "{script:?}");
            let text = String::from_utf8(released).unwrap();
            assert!(!text.contains("rc2fx_build=development"), "{text}");
        }
        assert_eq!(
            release_script(b"#!/bin/sh\nrc2fx_build=development  \nexit 0\n").unwrap(),
            b"#!/bin/sh\nrc2fx_build=release\nexit 0\n",
            "the rewrite is exact"
        );
    }

    #[test]
    fn a_second_or_unsupported_build_assignment_is_refused() {
        for script in [
            &b"#!/bin/sh\nrc2fx_build=development\nexport rc2fx_build=release\n"[..],
            &b"#!/bin/sh\nrc2fx_build=development\nrc2fx_build = release\n"[..],
            &b"#!/bin/sh\nrc2fx_build=development\nrc2fx_build=development2\n"[..],
            &b"#!/bin/sh\nrc2fx_build=development\nrc2fx_build=release\n"[..],
        ] {
            let error = build_kind(script).unwrap_err();
            assert!(error.contains("rc2fx_build"), "{script:?}: {error}");
            assert!(release_script(script).is_err(), "{script:?}");
        }
        // A comment or a use of the variable is not another assignment.
        assert_eq!(
            build_kind(b"#!/bin/sh\n# rc2fx_build is set below\nrc2fx_build=development\n")
                .unwrap(),
            DEVELOPMENT
        );
    }

    #[test]
    fn a_catalog_that_does_not_deserialize_is_refused() {
        for catalog in [
            &br#"{"schemaVersion":1,"profiles":42}"#[..],
            &br#"{"schemaVersion":1,"profiles":[{"id":"x","runtime":{"sampleRate":"x"}}]}"#[..],
            &br#"{"schemaVersion":1,"profiles":[{"runtime":{"blockFrames":-1}}]}"#[..],
            &br#"{"schemaVersion":"1"}"#[..],
        ] {
            let mut files = payload(1);
            set(&mut files, "firmware-profiles.json", catalog.to_vec());
            let error = validate(&view(&files)).unwrap_err();
            assert!(
                error.contains("catalog"),
                "{}: {error}",
                String::from_utf8_lossy(catalog)
            );
        }
    }

    #[test]
    fn exactly_one_build_line_says_which_kind_of_build_this_is() {
        let development = b"#!/bin/sh\nrc2fx_build=development\nexit 0\n";
        assert_eq!(build_kind(development).unwrap(), DEVELOPMENT);
        let release = release_script(development).unwrap();
        assert_eq!(build_kind(&release).unwrap(), RELEASE);
        assert_eq!(
            String::from_utf8(release).unwrap(),
            "#!/bin/sh\nrc2fx_build=release\nexit 0\n"
        );
        assert!(build_kind(b"#!/bin/sh\nexit 0\n").is_err());
        assert!(build_kind(b"rc2fx_build=development\nrc2fx_build=release\n").is_err());
        assert!(
            build_kind(b" # rc2fx_build=development\n").is_err(),
            "an indented comment is not the build line"
        );
        assert_eq!(
            release_script(b"#!/bin/sh\nrc2fx_build=release\nexit 0\n").unwrap(),
            b"#!/bin/sh\nrc2fx_build=release\nexit 0\n",
            "rewriting a release script changes nothing"
        );
        // The payload carries the same rule even in a development build.
        let mut files = payload(1);
        set(&mut files, "rc2fx.sh", b"#!/bin/sh\nexit 0\n".to_vec());
        let error = validate(&view(&files)).unwrap_err();
        assert!(error.contains("exactly one `rc2fx_build"), "{error}");
    }

    #[test]
    fn a_release_payload_differs_from_a_development_one() {
        let development = payload(1);
        let mut release = payload(1);
        as_release(&mut release);
        assert!(validate(&view(&release)).is_ok());
        assert_ne!(
            body(&view(&development)),
            body(&view(&release)),
            "a release payload is not a development payload"
        );
        let script = release
            .iter()
            .find(|(n, _)| n == "rc2fx.sh")
            .unwrap()
            .1
            .clone();
        assert_eq!(build_kind(&script).unwrap(), RELEASE);
    }

    #[test]
    fn crlf_scripts_are_normalized() {
        assert_eq!(crlf_to_lf(b"a\r\nb\rc\n"), b"a\nb\rc\n");
        assert_eq!(crlf_to_lf(b""), b"");
        assert_eq!(crlf_to_lf(b"\r\r\n"), b"\r\n");
    }

    /// A release folder on disk: runtime/, xtask/mixer/ and config/.
    fn folder(marker: u8, release: bool) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let mut files = payload(marker);
        if release {
            as_release(&mut files);
        }
        for (name, data) in &files {
            let path = if SCRIPTS.contains(&name.as_str()) {
                dir.path().join("xtask").join("mixer").join(name)
            } else {
                dir.path().join("runtime").join(name)
            };
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, data).unwrap();
        }
        std::fs::create_dir_all(dir.path().join("config")).unwrap();
        std::fs::write(
            dir.path().join("config").join("firmware-profiles.json"),
            &files[3].1,
        )
        .unwrap();
        dir
    }

    fn script_of(envelope: &Envelope) -> Vec<u8> {
        envelope
            .files()
            .unwrap()
            .into_iter()
            .find(|(n, _)| *n == "rc2fx.sh")
            .unwrap()
            .1
            .to_vec()
    }

    #[test]
    fn expected_builds_the_same_blob_from_a_folder() {
        let dir = folder(1, false);
        let expected = Envelope::expected(dir.path(), false).unwrap();
        let files = payload(1);
        assert_eq!(expected.body(), body(&view(&files)).as_slice());
        assert_eq!(read(dir.path(), false).unwrap(), files);
        // A release folder is embedded as a release payload.
        let release = folder(1, true);
        let expected = Envelope::expected(release.path(), true).unwrap();
        assert_eq!(build_kind(&script_of(&expected)).unwrap(), RELEASE);
        // The folder's own script already says release, and reading it again
        // for development does not turn it back.
        let expected = Envelope::expected(release.path(), false).unwrap();
        assert_eq!(build_kind(&script_of(&expected)).unwrap(), RELEASE);
    }

    #[test]
    fn a_folder_whose_catalogs_differ_or_whose_files_are_missing_is_refused() {
        let dir = folder(1, false);
        std::fs::write(
            dir.path().join("config").join("firmware-profiles.json"),
            br#"{"schemaVersion":1,"profiles":[{"id":"other"}]}"#,
        )
        .unwrap();
        let error = read(dir.path(), false).unwrap_err();
        assert!(error.contains("catalogs differ"), "{error}");
        let dir = folder(1, false);
        std::fs::remove_file(dir.path().join("runtime").join("rc2fxd")).unwrap();
        let error = read(dir.path(), false).unwrap_err();
        assert!(error.contains("rc2fxd"), "{error}");
        // An old bundle without the recorder is a missing file, not a pass.
        let dir = folder(1, false);
        std::fs::remove_file(dir.path().join("runtime").join("rc2fx_jackerr_snapshot")).unwrap();
        let error = read(dir.path(), false).unwrap_err();
        assert!(error.contains("rc2fx_jackerr_snapshot"), "{error}");
    }

    #[test]
    fn require_matches_compares_the_whole_blob_and_names_what_differs() {
        let one = Envelope::find(&wrapped(1)).unwrap();
        let same = Envelope::find(&wrapped(1)).unwrap();
        assert!(one.require_matches(&same).is_ok());
        let other = Envelope::find(&wrapped(2)).unwrap();
        let error = one.require_matches(&other).unwrap_err();
        assert!(error.contains("is not this build's"), "{error}");
        assert!(error.contains("librc2fx_native.so"), "{error}");
    }
}
