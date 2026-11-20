//! Verifies that deltas written by the Go desktop encoder (`desktop/delta`)
//! decode identically here, and corrupt deltas fail with matching errors.
//! See `fixtures/delta/README.md`.
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io,
    path::{Path, PathBuf},
};

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/delta")
}

fn cases(kind: &str) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(dir().join(kind))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.is_dir())
        .collect();
    dirs.sort();
    assert!(!dirs.is_empty(), "no {kind} fixtures");
    dirs
}

fn installed(case: &Path) -> BTreeMap<String, Vec<u8>> {
    let Ok(entries) = std::fs::read_dir(case.join("installed")) else {
        return BTreeMap::new();
    };
    entries
        .map(|entry| {
            let entry = entry.unwrap();
            (
                entry.file_name().into_string().unwrap(),
                std::fs::read(entry.path()).unwrap(),
            )
        })
        .collect()
}

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

/// desktop/delta/internal/fixture: Envelope.
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

/// desktop/delta/internal/fixture: Random (xorshift64 13/7/17, low byte).
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

/// desktop/delta/internal/fixture: Large.
fn large() -> (Vec<u8>, BTreeMap<String, Vec<u8>>) {
    let base = random(0x9e37_79b9_7f4a_7c15, 1_200_000);
    let mut changed = base.clone();
    for byte in changed.iter_mut().skip(50_000).step_by(100_000) {
        *byte ^= 0xff;
    }
    let mut updated = random(2, 1000);
    updated.extend_from_slice(&changed);
    updated.extend_from_slice(&random(3, 10_000));
    let shared = random(4, 4096);
    let envelope = envelope(&[
        ("shared", shared.as_slice()),
        ("rc2fxd", updated.as_slice()),
    ]);
    let installed = BTreeMap::from([("shared".to_owned(), shared), ("rc2fxd".to_owned(), base)]);
    (envelope, installed)
}

#[test]
fn go_encoded_deltas_rebuild_their_envelopes() {
    for case in cases("accept") {
        let delta = std::fs::read(case.join("delta")).unwrap();
        let (envelope, files) = if case.join("generated").exists() {
            large()
        } else {
            (
                std::fs::read(case.join("envelope")).unwrap(),
                installed(&case),
            )
        };
        let (restored, written) = decode(&delta, &files, envelope.len() as u64)
            .unwrap_or_else(|e| panic!("{}: {e}", case.display()));
        assert!(
            restored == envelope,
            "{} rebuilt a different envelope",
            case.display()
        );
        assert_eq!(written, envelope.len() as u64, "{}", case.display());
    }
}

#[test]
fn corrupt_deltas_are_refused_with_the_same_messages() {
    for case in cases("reject") {
        let delta = std::fs::read(case.join("delta")).unwrap();
        let limit = std::fs::read_to_string(case.join("max"))
            .map(|text| text.trim().parse().unwrap())
            .unwrap_or(u64::MAX);
        let error = decode(&delta, &installed(&case), limit)
            .err()
            .unwrap_or_else(|| panic!("{} was accepted", case.display()));
        if let Ok(want) = std::fs::read_to_string(case.join("error")) {
            assert_eq!(error, want.trim_end_matches('\n'), "{}", case.display());
        }
    }
}
