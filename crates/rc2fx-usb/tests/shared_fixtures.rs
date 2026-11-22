//! Frozen USB framing wire-contract tests and live Go <-> Rust interop tests
//! (using `desktop/internal/interop usb` instead of committed fixtures).

use rc2fx_usb::{Frame, Kind, REPORT_SIZE, Request};
use serde_json::Value;
use std::path::PathBuf;
use std::process::Command;

fn hex(text: &str) -> Vec<u8> {
    assert!(text.len().is_multiple_of(2), "{text}");
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
        .collect()
}

fn go_usb_interop() -> Value {
    let desktop = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../desktop");
    let output = Command::new("go")
        .args(["run", "./internal/interop", "usb"])
        .current_dir(&desktop)
        .output()
        .expect("spawn `go run ./internal/interop usb`");
    assert!(
        output.status.success(),
        "go interop usb failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

/// Pins the frozen v3 HID report framing bytes and verifies live Go-encoded
/// frames decode identically in Rust and re-encode byte-for-byte.
#[test]
fn frames_and_go_interop_match() {
    let interop = go_usb_interop();
    for case in interop["frames"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let report = hex(case["raw"].as_str().unwrap());
        let id = case["report"].as_u64().unwrap() as u8;
        assert!(rc2fx_usb::marked(&report, id), "{name} marked");
        let f = Frame::decode(&report, id).unwrap_or_else(|| panic!("{name} did not decode"));
        assert_eq!(f.kind as u64, case["kind"].as_u64().unwrap(), "{name}");
        assert_eq!(f.session, case["session"].as_u64().unwrap(), "{name}");
        assert_eq!(
            u64::from(f.request),
            case["request"].as_u64().unwrap(),
            "{name}"
        );
        assert_eq!(f.offset as u64, case["offset"].as_u64().unwrap(), "{name}");
        assert_eq!(f.total as u64, case["total"].as_u64().unwrap(), "{name}");
        assert_eq!(
            f.data,
            hex(case["data"].as_str().unwrap()).as_slice(),
            "{name}"
        );
        assert_eq!(f.encode(id).as_slice(), report.as_slice(), "{name}");
    }

    // Frozen wire-layout checks across all 6 kinds and invalid frame mutations.
    for (i, kind) in [
        Kind::Hello,
        Kind::Request,
        Kind::Fetch,
        Kind::Ack,
        Kind::Reply,
        Kind::Error,
    ]
    .into_iter()
    .enumerate()
    {
        let raw = Frame {
            kind,
            session: (i + 1) as u64,
            request: ((i + 1) * 3) as u32,
            offset: 0,
            total: 0,
            data: rc2fx_usb::HELLO_4,
        }
        .encode(rc2fx_usb::OUTPUT_ID);
        assert!(rc2fx_usb::marked(&raw, rc2fx_usb::OUTPUT_ID));
        let dec = Frame::decode(&raw, rc2fx_usb::OUTPUT_ID).unwrap();
        assert_eq!(dec.kind, kind);
        assert_eq!(dec.data, rc2fx_usb::HELLO_4);
    }

    let base = Frame {
        kind: Kind::Reply,
        session: 1,
        request: 1,
        offset: 0,
        total: 4,
        data: b"data",
    }
    .encode(rc2fx_usb::INPUT_ID);
    assert!(!rc2fx_usb::marked(&base, rc2fx_usb::OUTPUT_ID));
    assert!(Frame::decode(&base, rc2fx_usb::OUTPUT_ID).is_none());

    for (name, mutate, expect_marked) in [
        (
            "bad-magic",
            Box::new(|b: &mut Vec<u8>| b[1] = 0x4d) as Box<dyn Fn(&mut Vec<u8>)>,
            false,
        ),
        ("bad-magic-nul", Box::new(|b: &mut Vec<u8>| b[7] = 1), false),
        ("version-2", Box::new(|b: &mut Vec<u8>| b[8] = 2), true),
        ("version-4", Box::new(|b: &mut Vec<u8>| b[8] = 4), true),
        ("kind-0", Box::new(|b: &mut Vec<u8>| b[9] = 0), true),
        ("kind-7", Box::new(|b: &mut Vec<u8>| b[9] = 7), true),
        (
            "length-over-payload",
            Box::new(|b: &mut Vec<u8>| {
                b[30] = 0xe1;
                b[31] = 0;
            }),
            true,
        ),
        (
            "length-max",
            Box::new(|b: &mut Vec<u8>| {
                b[30] = 0xff;
                b[31] = 0xff;
            }),
            true,
        ),
        (
            "short-255",
            Box::new(|b: &mut Vec<u8>| b.truncate(255)),
            true,
        ),
        ("long-257", Box::new(|b: &mut Vec<u8>| b.push(1)), true),
        (
            "header-only",
            Box::new(|b: &mut Vec<u8>| b.truncate(8)),
            true,
        ),
        ("short-7", Box::new(|b: &mut Vec<u8>| b.truncate(7)), false),
        ("empty", Box::new(|b: &mut Vec<u8>| b.clear()), false),
    ] {
        let mut buf = base.to_vec();
        mutate(&mut buf);
        assert_eq!(
            rc2fx_usb::marked(&buf, rc2fx_usb::INPUT_ID),
            expect_marked,
            "{name}"
        );
        assert!(Frame::decode(&buf, rc2fx_usb::INPUT_ID).is_none(), "{name}");
    }
}

#[test]
fn canonical_output_matches() {
    for (len, ok) in [
        (0usize, false),
        (1, false),
        (REPORT_SIZE - 1, false),
        (REPORT_SIZE, true),
        (REPORT_SIZE + 1, true),
        (REPORT_SIZE + 2, false),
        (REPORT_SIZE * 2, false),
    ] {
        let bytes: Vec<u8> = (0..len).map(|i| (i & 0xff) as u8).collect();
        let got = rc2fx_usb::canonical_output(&bytes);
        assert_eq!(got.is_some(), ok, "{len} bytes");
        if let Some(got) = got {
            assert_eq!(got, &bytes[..REPORT_SIZE]);
        }
    }
}

#[test]
fn go_packed_messages_unpack_and_malformed_messages_are_rejected() {
    let interop = go_usb_interop();
    let mut compressed_packed = Vec::new();
    for case in interop["messages"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let packed = hex(case["packed"].as_str().unwrap());
        let want = hex(case["decoded"].as_str().unwrap());
        let limit = case["limit"].as_u64().unwrap() as usize;
        if name == "compressed" {
            compressed_packed = packed.clone();
        }
        assert_eq!(
            rc2fx_usb::unpack(&packed, limit).as_deref(),
            Ok(want.as_slice()),
            "{name}"
        );
        let repacked = rc2fx_usb::pack(&want, limit).unwrap();
        assert_eq!(rc2fx_usb::unpack(&repacked, limit).unwrap(), want, "{name}");
    }

    assert_eq!(rc2fx_usb::unpack(b"J", 64).unwrap(), Vec::<u8>::new());

    let mut trailing = compressed_packed.clone();
    trailing.push(0);
    let mut understated = compressed_packed.clone();
    understated[1..5].copy_from_slice(&10u32.to_le_bytes());
    let mut overstated = compressed_packed.clone();
    overstated[1..5].copy_from_slice(&7000u32.to_le_bytes());
    let mut over_limit = compressed_packed.clone();
    over_limit[1..5].copy_from_slice(&9000u32.to_le_bytes());
    let mut bad_checksum = compressed_packed.clone();
    let last = bad_checksum.len() - 1;
    bad_checksum[last] ^= 0xff;
    let mut bad_header = compressed_packed.clone();
    bad_header[5] = 0x00;
    bad_header[6] = 0x00;

    for (name, data, limit, want_err) in [
        (
            "truncated",
            &compressed_packed[..compressed_packed.len() - 4],
            8192usize,
            Some("Invalid compressed USB message"),
        ),
        (
            "truncated-header",
            &compressed_packed[..6],
            8192,
            Some("Invalid compressed USB message"),
        ),
        (
            "size-field-only",
            &compressed_packed[..5],
            8192,
            Some("Invalid compressed USB message"),
        ),
        (
            "trailing-byte",
            trailing.as_slice(),
            8192,
            Some("Invalid compressed USB message"),
        ),
        (
            "size-understated",
            understated.as_slice(),
            8192,
            Some("Invalid compressed USB message"),
        ),
        (
            "size-overstated",
            overstated.as_slice(),
            8192,
            Some("Invalid compressed USB message"),
        ),
        (
            "size-over-limit",
            over_limit.as_slice(),
            8192,
            Some("Decoded USB message too large"),
        ),
        (
            "message-over-limit",
            compressed_packed.as_slice(),
            compressed_packed.len() - 1,
            Some("USB message too large"),
        ),
        (
            "plain-over-limit",
            b"Jabc",
            2,
            Some("USB message too large"),
        ),
        (
            "unknown-encoding",
            &[9, 1, 2],
            64,
            Some("Invalid USB message encoding"),
        ),
        (
            "z-short",
            b"Z\0\0",
            8192,
            Some("Invalid USB message encoding"),
        ),
        ("empty", &[], 64, Some("Invalid USB message encoding")),
        ("bad-checksum", bad_checksum.as_slice(), 8192, None),
        ("bad-header", bad_header.as_slice(), 8192, None),
    ] {
        let err = rc2fx_usb::unpack(data, limit).expect_err(name);
        if let Some(want) = want_err {
            assert_eq!(err, want, "{name}");
        }
    }
}

#[test]
fn requests_parse_the_same_way() {
    for (json, ok) in [
        (r#"{"method":"GET","path":"p"}"#, true),
        (r#"{"method":"GET","path":"p","body":null}"#, true),
        (r#"{"path":"p","method":"GET","body":{"x":1}}"#, true),
        (r#"{"method":"GET"}"#, false),
        (r#"{"path":"p"}"#, false),
        (r#"{"method":"GET","path":"p","extra":1}"#, false),
        (r#"{"method":"GET","method":"PUT","path":"p"}"#, false),
        (r#"{"method":"GET","path":"p","body":1,"body":2}"#, false),
        (r#"{"method":null,"path":"p"}"#, false),
        (r#"{"method":1,"path":"p"}"#, false),
        (r#"{"method":"\ud800","path":"p"}"#, false),
        (r#"{"method":"\ud83d\ude00","path":"p"}"#, true),
        ("{\"method\":\"😀\",\"path\":\"p\"}", true),
        ("null", false),
    ] {
        assert_eq!(serde_json::from_str::<Request>(json).is_ok(), ok, "{json}");
    }
}
