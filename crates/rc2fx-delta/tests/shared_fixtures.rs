//! Frozen `RC2FX-DELTA 1` wire-spec contract tests, decoder rejection tests,
//! and live Go-encoder -> Rust-decoder interoperability tests.
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::{self, Write},
    path::PathBuf,
    process::{Command, Stdio},
};

fn decode(
    delta: &[u8],
    files: &BTreeMap<String, Vec<u8>>,
    limit: u64,
) -> Result<(Vec<u8>, u64), String> {
    let mut out = Vec::new();
    let written = rc2fx_delta::decode(
        &mut &delta[..],
        &mut |name| {
            files.get(name).cloned().ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotFound, format!("no installed {name}"))
            })
        },
        &mut out,
        limit,
    )?;
    Ok((out, written))
}

fn sum(data: &[u8]) -> String {
    Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn envelope(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut body = Vec::new();
    for (name, data) in files {
        body.extend_from_slice(format!("{}  {}  {name}\n", sum(data), data.len()).as_bytes());
    }
    for (_, data) in files {
        body.extend_from_slice(data);
    }
    let mut out = b"RC2FX-EMBEDDED-PAYLOAD 1\n".to_vec();
    out.extend_from_slice(format!("length {}\n", body.len()).as_bytes());
    out.extend_from_slice(format!("sha256 {}\n", sum(&body)).as_bytes());
    out.extend_from_slice(&body);
    out
}

fn sample() -> (Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>) {
    let shared = b"a file the mixer already has\n".repeat(20);
    let old = b"the previous rc2fxd build\n".repeat(200);
    let mut updated = old.clone();
    updated[7] ^= 1;
    updated.extend_from_slice(&b"and its new tail\n".repeat(20));
    let gui = b"compressible gui bytes ".repeat(2000);
    (shared, old, updated, gui)
}

fn random(seed: u64, n: usize) -> Vec<u8> {
    let mut x = seed;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect()
}

fn text_bytes(seed: u64, n: usize) -> Vec<u8> {
    const WORDS: &[&[u8]] = &[
        b"mixer ", b"route ", b"input ", b"gain ", b"level ", b"rc2fx ", b"\n", b"0x7f ", b"fader ",
    ];
    let picks = random(seed, n);
    let mut out = Vec::with_capacity(n + 16);
    let mut i = 0;
    while out.len() < n {
        out.extend_from_slice(WORDS[usize::from(picks[i]) % WORDS.len()]);
        i += 1;
    }
    out.truncate(n);
    out
}

fn large_case() -> (Vec<u8>, BTreeMap<String, Vec<u8>>) {
    let base = random(0x9e37_79b9_7f4a_7c15, 1_200_000);
    let mut changed = base.clone();
    for byte in changed.iter_mut().skip(50_000).step_by(100_000) {
        *byte ^= 0xff;
    }
    let mut updated = random(2, 1000);
    updated.extend_from_slice(&changed);
    updated.extend_from_slice(&random(3, 10_000));
    let shared = random(4, 4096);
    let env = envelope(&[
        ("shared", shared.as_slice()),
        ("rc2fxd", updated.as_slice()),
    ]);
    let installed = BTreeMap::from([("shared".to_owned(), shared), ("rc2fxd".to_owned(), base)]);
    (env, installed)
}

/// Minimal single-raw-block zstd frame (RFC 8878). When `single` is false,
/// emits an explicit 2^window_log window descriptor with no content size.
fn raw_zstd_frame(data: &[u8], window_log: u8, single: bool) -> Vec<u8> {
    let mut frame = 0xfd2f_b528u32.to_le_bytes().to_vec();
    if single {
        frame.push(0x20);
        frame.push(u8::try_from(data.len()).unwrap());
    } else {
        frame.push(0x00);
        frame.push((window_log - 10) << 3);
    }
    let header = ((data.len() as u32) << 3) | 1; // raw block, last block
    frame.extend_from_slice(&[header as u8, (header >> 8) as u8, (header >> 16) as u8]);
    frame.extend_from_slice(data);
    frame
}

/// Pins the frozen `RC2FX-DELTA 1` wire grammar directly in code without calling
/// any shared encoder, so older v1 desktop apps stay compatible even if a future
/// encoder changes.
#[test]
fn frozen_v1_wire_format_contract() {
    let copied = b"already on mixer".to_vec();
    let old_base = b"old base bytes 0123456789".to_vec();
    let patched = b"new patched bytes 0123456789".to_vec();
    let whole = b"fresh file".to_vec();
    let env = envelope(&[("a.bin", &copied), ("b.bin", &patched), ("c.bin", &whole)]);
    let (head_len, _) = rc2fx_delta::envelope_entries(&env).unwrap();

    // Hand-assemble literal RC2FX-DELTA 1 bytes with raw RFC 8878 zstd frames.
    let patch_frame = raw_zstd_frame(&patched, 20, true);
    let zstd_frame = raw_zstd_frame(&whole, 20, true);
    let mut delta = Vec::new();
    delta.extend_from_slice(b"RC2FX-DELTA 1\n");
    delta.extend_from_slice(format!("head {head_len}\n").as_bytes());
    delta.extend_from_slice(&env[..head_len]);
    delta.extend_from_slice(b"copy\n");
    delta.extend_from_slice(format!("patch {} {}\n", sum(&old_base), patch_frame.len()).as_bytes());
    delta.extend_from_slice(&patch_frame);
    delta.extend_from_slice(format!("zstd {}\n", zstd_frame.len()).as_bytes());
    delta.extend_from_slice(&zstd_frame);

    let files = BTreeMap::from([("a.bin".to_owned(), copied), ("b.bin".to_owned(), old_base)]);
    let (restored, written) = decode(&delta, &files, env.len() as u64).unwrap();
    assert_eq!(restored, env);
    assert_eq!(written, env.len() as u64);
}

struct OpSpan {
    line: (usize, usize),
    frame: (usize, usize),
}

fn parse_ops(delta: &[u8]) -> Vec<OpSpan> {
    let mut at = rc2fx_delta::MAGIC.len();
    let end = at + delta[at..].iter().position(|&b| b == b'\n').unwrap();
    let head: usize = std::str::from_utf8(&delta[at..end])
        .unwrap()
        .strip_prefix("head ")
        .unwrap()
        .parse()
        .unwrap();
    at = end + 1 + head;
    let mut out = Vec::new();
    while at < delta.len() {
        let end = at + delta[at..].iter().position(|&b| b == b'\n').unwrap();
        let line = std::str::from_utf8(&delta[at..end]).unwrap();
        let span_line = (at, end);
        at = end + 1;
        let frame = if line == "copy" {
            (at, at)
        } else {
            let n: usize = line.split_whitespace().last().unwrap().parse().unwrap();
            let f = (at, at + n);
            at += n;
            f
        };
        out.push(OpSpan {
            line: span_line,
            frame,
        });
    }
    out
}

fn replace_line(d: &[u8], op: &OpSpan, line: &[u8]) -> Vec<u8> {
    let mut out = d[..op.line.0].to_vec();
    out.extend_from_slice(line);
    out.extend_from_slice(&d[op.line.1..]);
    out
}

fn with_frame(d: &[u8], op: &OpSpan, line: &[u8], frame: &[u8]) -> Vec<u8> {
    let mut out = d[..op.line.0].to_vec();
    out.extend_from_slice(line);
    out.push(b'\n');
    out.extend_from_slice(frame);
    out.extend_from_slice(&d[op.frame.1..]);
    out
}

fn replace_head(d: &[u8], head_line_end: usize, line: &[u8]) -> Vec<u8> {
    let mut out = rc2fx_delta::MAGIC.as_bytes().to_vec();
    out.extend_from_slice(line);
    out.extend_from_slice(&d[head_line_end..]);
    out
}

#[test]
fn corrupt_and_hostile_deltas_are_refused() {
    let (shared, old, updated, _) = sample();
    let installed = BTreeMap::from([
        ("librc2fx_native.so".to_owned(), shared.clone()),
        ("rc2fxd".to_owned(), old.clone()),
    ]);
    let env = envelope(&[("librc2fx_native.so", &shared), ("rc2fxd", &updated)]);
    let valid = go_encode_batch(&[("reject-base", &env, &installed, 19)])
        .into_iter()
        .next()
        .unwrap()
        .1;
    let ops = parse_ops(&valid);
    let (copy_op, patch_op) = (&ops[0], &ops[1]);
    let head_line_end = rc2fx_delta::MAGIC.len()
        + valid[rc2fx_delta::MAGIC.len()..]
            .iter()
            .position(|&b| b == b'\n')
            .unwrap();
    let head_start = head_line_end + 1;
    let head_len = copy_op.line.0 - head_start;

    let check = |name: &str,
                 d: &[u8],
                 files: &BTreeMap<String, Vec<u8>>,
                 limit: u64,
                 want: Option<&str>| {
        let err = decode(d, files, limit)
            .err()
            .unwrap_or_else(|| panic!("{name} was accepted"));
        if let Some(want) = want {
            assert_eq!(err, want, "{name}");
        }
    };

    let mut bad_magic = valid.clone();
    bad_magic[0] = b'X';
    check(
        "bad-magic",
        &bad_magic,
        &installed,
        u64::MAX,
        Some("this is not an RC2FX delta"),
    );
    check(
        "short-magic",
        &valid[..5],
        &installed,
        u64::MAX,
        Some("the delta header could not be read: failed to fill whole buffer"),
    );
    check(
        "empty",
        &[],
        &installed,
        u64::MAX,
        Some("the delta header could not be read: failed to fill whole buffer"),
    );
    check(
        "truncated",
        &valid[..valid.len() - 1],
        &installed,
        u64::MAX,
        Some("rc2fxd's compressed data could not be read: failed to fill whole buffer"),
    );
    let mut trailing = valid.clone();
    trailing.push(0);
    check(
        "trailing-byte",
        &trailing,
        &installed,
        u64::MAX,
        Some("the delta has bytes after its last file"),
    );
    check(
        "unknown-operation",
        &replace_line(&valid, copy_op, b"nope"),
        &installed,
        u64::MAX,
        Some("librc2fx_native.so has an unknown delta operation"),
    );
    let over_msg = format!(
        "the envelope would be {} bytes, over the {}-byte limit",
        env.len(),
        env.len() - 1
    );
    check(
        "over-the-limit",
        &valid,
        &installed,
        (env.len() - 1) as u64,
        Some(&over_msg),
    );

    let wrong_base = BTreeMap::from([
        ("librc2fx_native.so".to_owned(), shared.clone()),
        (
            "rc2fxd".to_owned(),
            b"some other installed rc2fxd\n".to_vec(),
        ),
    ]);
    check(
        "wrong-base",
        &valid,
        &wrong_base,
        u64::MAX,
        Some("the installed rc2fxd is not the base this patch was made from"),
    );

    let stale_copy = BTreeMap::from([
        ("librc2fx_native.so".to_owned(), vec![b'x'; shared.len()]),
        ("rc2fxd".to_owned(), old.clone()),
    ]);
    check(
        "stale-copy",
        &valid,
        &stale_copy,
        u64::MAX,
        Some("the installed librc2fx_native.so does not match the envelope"),
    );

    let short_copy = BTreeMap::from([
        ("librc2fx_native.so".to_owned(), shared[1..].to_vec()),
        ("rc2fxd".to_owned(), old.clone()),
    ]);
    let short_msg = format!(
        "the installed librc2fx_native.so is {} bytes; the envelope says {}",
        shared.len() - 1,
        shared.len()
    );
    check(
        "copy-wrong-size",
        &valid,
        &short_copy,
        u64::MAX,
        Some(&short_msg),
    );

    let missing_copy = BTreeMap::from([("rc2fxd".to_owned(), old.clone())]);
    check("copy-not-installed", &valid, &missing_copy, u64::MAX, None);

    let mut headless = rc2fx_delta::MAGIC.as_bytes().to_vec();
    headless.extend_from_slice(&valid[head_start..]);
    check(
        "no-head-line",
        &headless,
        &installed,
        u64::MAX,
        Some("the delta has no head line"),
    );
    check(
        "head-zero",
        &replace_head(&valid, head_line_end, b"head 0"),
        &installed,
        u64::MAX,
        Some("the delta's head length is outside the supported range"),
    );
    check(
        "head-too-long",
        &replace_head(&valid, head_line_end, b"head 65537"),
        &installed,
        u64::MAX,
        Some("the delta's head length is outside the supported range"),
    );
    check(
        "head-not-a-number",
        &replace_head(&valid, head_line_end, b"head 1x"),
        &installed,
        u64::MAX,
        Some("the delta's head length is not a number"),
    );
    check(
        "head-not-head",
        &replace_head(&valid, head_line_end, b"size 10"),
        &installed,
        u64::MAX,
        Some("the delta has no head line"),
    );

    let mut longer = valid[..rc2fx_delta::MAGIC.len()].to_vec();
    longer.extend_from_slice(format!("head {}\n", head_len + 1).as_bytes());
    longer.extend_from_slice(&valid[head_start..head_start + head_len]);
    longer.push(b'\n');
    longer.extend_from_slice(&valid[head_start + head_len..]);
    check(
        "head-after-manifest",
        &longer,
        &installed,
        u64::MAX,
        Some("the delta's head has bytes after the envelope's manifest"),
    );
    check(
        "ends-mid-line",
        &valid[..copy_op.line.0 + 2],
        &installed,
        u64::MAX,
        Some("the delta ends in the middle of a line"),
    );
    check(
        "line-too-long",
        &replace_line(&valid, copy_op, &vec![b'c'; 600]),
        &installed,
        u64::MAX,
        Some("a delta operation line is too long"),
    );
    check(
        "line-not-text",
        &replace_line(&valid, copy_op, b"copy\xc3\xa9"),
        &installed,
        u64::MAX,
        Some("the delta operation is not text"),
    );

    let patch_line = std::str::from_utf8(&valid[patch_op.line.0..patch_op.line.1]).unwrap();
    let fields: Vec<&str> = patch_line.split_whitespace().collect();
    let frame = &valid[patch_op.frame.0..patch_op.frame.1];
    check(
        "patch-base-malformed",
        &replace_line(
            &valid,
            patch_op,
            format!("patch xyz {}", fields[2]).as_bytes(),
        ),
        &installed,
        u64::MAX,
        Some("rc2fxd's patch base is malformed"),
    );
    check(
        "patch-base-missing",
        &replace_line(&valid, patch_op, b"patch"),
        &installed,
        u64::MAX,
        Some("rc2fxd's patch base is malformed"),
    );
    check(
        "length-missing",
        &replace_line(&valid, patch_op, format!("patch {}", fields[1]).as_bytes()),
        &installed,
        u64::MAX,
        Some("rc2fxd's compressed length is missing"),
    );
    check(
        "length-not-a-number",
        &replace_line(
            &valid,
            patch_op,
            format!("patch {} 12z", fields[1]).as_bytes(),
        ),
        &installed,
        u64::MAX,
        Some("rc2fxd's compressed length is not a number"),
    );
    check(
        "length-zero",
        &replace_line(
            &valid,
            patch_op,
            format!("patch {} 0", fields[1]).as_bytes(),
        ),
        &installed,
        u64::MAX,
        Some("rc2fxd's compressed length is outside the supported range"),
    );
    check(
        "length-over-limit",
        &replace_line(
            &valid,
            patch_op,
            format!("zstd {}", env.len() + (1 << 20) + 1).as_bytes(),
        ),
        &installed,
        env.len() as u64,
        Some("rc2fxd's compressed length is outside the supported range"),
    );
    check(
        "trailing-fields",
        &replace_line(&valid, patch_op, format!("{patch_line} 1").as_bytes()),
        &installed,
        u64::MAX,
        Some("rc2fxd's delta operation has trailing fields"),
    );
    check(
        "double-space",
        &replace_line(
            &valid,
            patch_op,
            format!("patch  {} {}", fields[1], fields[2]).as_bytes(),
        ),
        &installed,
        u64::MAX,
        Some("rc2fxd's patch base is malformed"),
    );

    let mut frame_plus = frame.to_vec();
    frame_plus.push(0);
    check(
        "frame-trailing-byte",
        &with_frame(
            &valid,
            patch_op,
            format!("patch {} {}", fields[1], frame_plus.len()).as_bytes(),
            &frame_plus,
        ),
        &installed,
        u64::MAX,
        Some("rc2fxd's compressed frame has trailing bytes"),
    );
    let mut two_frames = frame.to_vec();
    two_frames.extend_from_slice(frame);
    check(
        "frame-two-frames",
        &with_frame(
            &valid,
            patch_op,
            format!("patch {} {}", fields[1], two_frames.len()).as_bytes(),
            &two_frames,
        ),
        &installed,
        u64::MAX,
        Some("rc2fxd's compressed frame has trailing bytes"),
    );
    check(
        "frame-truncated",
        &with_frame(
            &valid,
            patch_op,
            format!("patch {} {}", fields[1], frame.len() - 1).as_bytes(),
            &frame[..frame.len() - 1],
        ),
        &installed,
        u64::MAX,
        Some("rc2fxd's compressed frame is truncated"),
    );
    check(
        "patch-as-zstd",
        &replace_line(&valid, patch_op, format!("zstd {}", fields[2]).as_bytes()),
        &installed,
        u64::MAX,
        None,
    );
    let mut corrupt = frame.to_vec();
    let mid = corrupt.len() / 2;
    corrupt[mid] ^= 0x55;
    check(
        "frame-corrupt",
        &with_frame(&valid, patch_op, patch_line.as_bytes(), &corrupt),
        &installed,
        u64::MAX,
        None,
    );

    // Hand-crafted raw-block frames controlling decoded output length exactly.
    let env_one = envelope(&[("file", b"0123456789")]);
    let (head_one, _) = rc2fx_delta::envelope_entries(&env_one).unwrap();
    let one_delta = |f: &[u8]| -> Vec<u8> {
        let mut d = rc2fx_delta::MAGIC.as_bytes().to_vec();
        d.extend_from_slice(format!("head {head_one}\n").as_bytes());
        d.extend_from_slice(&env_one[..head_one]);
        d.extend_from_slice(format!("zstd {}\n", f.len()).as_bytes());
        d.extend_from_slice(f);
        d
    };
    let empty_files = BTreeMap::new();
    check(
        "decodes-longer",
        &one_delta(&raw_zstd_frame(b"0123456789A", 20, true)),
        &empty_files,
        u64::MAX,
        Some("file decoded to more than the 10 bytes its manifest line says"),
    );
    check(
        "decodes-shorter",
        &one_delta(&raw_zstd_frame(b"012345678", 20, true)),
        &empty_files,
        u64::MAX,
        Some("file decoded to 9 bytes; its manifest line says 10"),
    );
    check(
        "decodes-different",
        &one_delta(&raw_zstd_frame(b"0123456788", 20, true)),
        &empty_files,
        u64::MAX,
        Some("file does not match its checksum in the envelope"),
    );
    check(
        "window-too-large",
        &one_delta(&raw_zstd_frame(b"0123456789", 26, false)),
        &empty_files,
        u64::MAX,
        None,
    );
    let one_over = format!(
        "the envelope would be {} bytes, over the {}-byte limit",
        env_one.len(),
        env_one.len() - 1
    );
    check(
        "file-over-the-limit",
        &one_delta(&raw_zstd_frame(b"0123456789", 20, true)),
        &empty_files,
        (env_one.len() - 1) as u64,
        Some(&one_over),
    );
    let mut not_zstd = raw_zstd_frame(b"0123456789", 20, true);
    not_zstd[0] ^= 1;
    check(
        "not-a-zstd-frame",
        &one_delta(&not_zstd),
        &empty_files,
        u64::MAX,
        None,
    );
}

fn push_bytes(buf: &mut Vec<u8>, data: &[u8]) {
    buf.extend_from_slice(&(data.len() as u32).to_be_bytes());
    buf.extend_from_slice(data);
}

fn take_u32(slice: &mut &[u8]) -> u32 {
    let n = u32::from_be_bytes(slice[..4].try_into().unwrap());
    *slice = &slice[4..];
    n
}

fn take_bytes<'a>(slice: &mut &'a [u8]) -> &'a [u8] {
    let n = take_u32(slice) as usize;
    let out = &slice[..n];
    *slice = &slice[n..];
    out
}

type EncodeCase<'a> = (&'a str, &'a [u8], &'a BTreeMap<String, Vec<u8>>, i32);

fn go_encode_batch(cases: &[EncodeCase<'_>]) -> Vec<(String, Vec<u8>)> {
    let mut req = Vec::new();
    req.extend_from_slice(&(cases.len() as u32).to_be_bytes());
    for (name, env, installed, level) in cases {
        push_bytes(&mut req, name.as_bytes());
        push_bytes(&mut req, env);
        req.extend_from_slice(&(*level as u32).to_be_bytes());
        req.extend_from_slice(&(installed.len() as u32).to_be_bytes());
        for (k, v) in *installed {
            push_bytes(&mut req, k.as_bytes());
            push_bytes(&mut req, sum(v).as_bytes());
        }
        req.extend_from_slice(&(installed.len() as u32).to_be_bytes());
        for (k, v) in *installed {
            push_bytes(&mut req, k.as_bytes());
            push_bytes(&mut req, v);
        }
    }

    let desktop_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../desktop");
    let mut child = Command::new("go")
        .args(["run", "./internal/interop", "delta"])
        .current_dir(&desktop_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn `go run ./internal/interop delta`");
    child.stdin.take().unwrap().write_all(&req).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "go interop delta failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let mut resp = output.stdout.as_slice();
    let count = take_u32(&mut resp) as usize;
    assert_eq!(count, cases.len());
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let name = std::str::from_utf8(take_bytes(&mut resp))
            .unwrap()
            .to_owned();
        let delta = take_bytes(&mut resp).to_vec();
        out.push((name, delta));
    }
    assert!(resp.is_empty(), "trailing bytes from interop");
    out
}

/// Live Go-to-Rust interoperability test: encodes every delta case with the
/// real Go desktop encoder (`desktop/delta`) and decodes every byte with this
/// crate's Rust decoder.
#[test]
fn go_encoded_deltas_rebuild_their_envelopes() {
    let (shared, old, updated, gui) = sample();
    let std_installed = BTreeMap::from([
        ("librc2fx_native.so".to_owned(), shared.clone()),
        ("rc2fxd".to_owned(), old.clone()),
    ]);
    let all_env = envelope(&[
        ("librc2fx_native.so", &shared),
        ("rc2fxd", &updated),
        ("librc2fx_gui.so", &gui),
    ]);
    let whole_env = envelope(&[("librc2fx_gui.so", &gui)]);
    let no_files_env = envelope(&[]);
    let empty_env = envelope(&[
        ("empty", b""),
        ("empty-patch", b""),
        ("one-byte", b"x"),
        ("tiny-patch", b"the previous"),
    ]);
    let empty_installed = BTreeMap::from([
        ("empty-patch".to_owned(), old.clone()),
        ("tiny-patch".to_owned(), old.clone()),
    ]);
    let sized_files: Vec<(String, Vec<u8>)> = [255usize, 256, 65791, 65792, 131073]
        .into_iter()
        .enumerate()
        .map(|(i, n)| (format!("sized-{n}"), text_bytes(i as u64 + 10, n)))
        .chain(std::iter::once(("random".to_owned(), random(9, 2000))))
        .collect();
    let sized_refs: Vec<(&str, &[u8])> = sized_files
        .iter()
        .map(|(n, d)| (n.as_str(), d.as_slice()))
        .collect();
    let sized_env = envelope(&sized_refs);
    let (large_env, large_installed) = large_case();

    struct Case {
        name: String,
        env: Vec<u8>,
        installed: BTreeMap<String, Vec<u8>>,
        level: i32,
    }
    let mut cases = Vec::new();
    for level in [1, 3, 7, 19] {
        cases.push(Case {
            name: format!("copy-patch-zstd-level-{level}"),
            env: all_env.clone(),
            installed: std_installed.clone(),
            level,
        });
    }
    cases.push(Case {
        name: "whole".into(),
        env: whole_env,
        installed: BTreeMap::new(),
        level: 19,
    });
    cases.push(Case {
        name: "no-files".into(),
        env: no_files_env,
        installed: BTreeMap::new(),
        level: 19,
    });
    cases.push(Case {
        name: "empty-and-tiny".into(),
        env: empty_env,
        installed: empty_installed,
        level: 19,
    });
    cases.push(Case {
        name: "content-sizes".into(),
        env: sized_env,
        installed: BTreeMap::new(),
        level: 19,
    });
    cases.push(Case {
        name: "large-patch".into(),
        env: large_env,
        installed: large_installed,
        level: 19,
    });

    let batch: Vec<EncodeCase<'_>> = cases
        .iter()
        .map(|c| (c.name.as_str(), c.env.as_slice(), &c.installed, c.level))
        .collect();
    let encoded = go_encode_batch(&batch);
    for (c, (name, delta)) in cases.iter().zip(encoded) {
        assert_eq!(name, c.name);
        if c.name == "large-patch" {
            assert!(
                delta.len() <= 64 * 1024,
                "large-patch was not a small patch: {} bytes",
                delta.len()
            );
        }
        let (restored, written) = decode(&delta, &c.installed, c.env.len() as u64)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(restored == c.env, "{name} rebuilt a different envelope");
        assert_eq!(written, c.env.len() as u64, "{name}");
    }
}
