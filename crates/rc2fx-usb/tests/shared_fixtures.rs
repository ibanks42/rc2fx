//! The USB framing fixtures the desktop's Go port (desktop/usbproto) reads
//! too, so the mixer and the app cannot drift. The packed messages were
//! written by the Go side.
use rc2fx_usb::{Frame, Reply, Request};
use serde_json::Value;
use std::path::PathBuf;

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/usb")
}

fn cases() -> Value {
    let text = std::fs::read_to_string(dir().join("cases.json")).unwrap();
    serde_json::from_str(&text).unwrap()
}

fn hex(text: &str) -> Vec<u8> {
    assert!(text.len().is_multiple_of(2), "{text}");
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
        .collect()
}

fn list<'a>(cases: &'a Value, key: &str) -> &'a Vec<Value> {
    let list = cases[key].as_array().unwrap();
    assert!(!list.is_empty(), "no {key} cases");
    list
}

fn text<'a>(case: &'a Value, key: &str) -> &'a str {
    case[key].as_str().unwrap()
}

#[test]
fn hello_windows_match() {
    for case in list(&cases(), "hello4") {
        let data = hex(text(case, "data"));
        let want = case["window"].as_u64().map(|w| w as usize);
        assert_eq!(rc2fx_usb::hello_4_window(&data), want, "{data:?}");
    }
}

#[test]
fn descriptors_match() {
    for case in list(&cases(), "descriptors") {
        assert_eq!(
            rc2fx_usb::declares_control(&hex(text(case, "hex"))),
            case["control"].as_bool().unwrap(),
            "{}",
            text(case, "name")
        );
    }
}

#[test]
fn frames_match() {
    for case in list(&cases(), "frames") {
        let name = text(case, "name");
        let report = hex(text(case, "hex"));
        let id = case["report"].as_u64().unwrap() as u8;
        assert_eq!(
            rc2fx_usb::marked(&report, id),
            case["marked"].as_bool().unwrap(),
            "{name}"
        );
        let decoded = Frame::decode(&report, id);
        let want = &case["frame"];
        if want.is_null() {
            assert!(decoded.is_none(), "{name} decoded");
            continue;
        }
        let f = decoded.unwrap_or_else(|| panic!("{name} did not decode"));
        assert_eq!(f.kind as u64, want["kind"].as_u64().unwrap(), "{name}");
        assert_eq!(f.session, want["session"].as_u64().unwrap(), "{name}");
        assert_eq!(
            u64::from(f.request),
            want["request"].as_u64().unwrap(),
            "{name}"
        );
        assert_eq!(f.offset as u64, want["offset"].as_u64().unwrap(), "{name}");
        assert_eq!(f.total as u64, want["total"].as_u64().unwrap(), "{name}");
        assert_eq!(f.data, hex(text(want, "data")).as_slice(), "{name}");
        assert_eq!(
            f.encode(id).as_slice(),
            report.as_slice(),
            "{name} re-encoded"
        );
    }
}

#[test]
fn canonical_output_matches() {
    for case in list(&cases(), "canonical") {
        let bytes = hex(text(case, "hex"));
        let got = rc2fx_usb::canonical_output(&bytes);
        assert_eq!(
            got.is_some(),
            case["ok"].as_bool().unwrap(),
            "{} bytes",
            bytes.len()
        );
        if let Some(got) = got {
            assert_eq!(got, &bytes[..rc2fx_usb::REPORT_SIZE]);
        }
    }
}

#[test]
fn go_packed_messages_unpack_the_same_way() {
    for case in list(&cases(), "messages") {
        let name = text(case, "name");
        let packed = std::fs::read(dir().join("messages").join(format!("{name}.bin"))).unwrap();
        let limit = case["limit"].as_u64().unwrap() as usize;
        let got = rc2fx_usb::unpack(&packed, limit);
        if let Some(decoded) = case["decoded"].as_str() {
            let want = std::fs::read(dir().join(decoded)).unwrap();
            assert_eq!(got.as_deref(), Ok(want.as_slice()), "{name}");
            let repacked = rc2fx_usb::pack(&want, limit).unwrap();
            assert_eq!(rc2fx_usb::unpack(&repacked, limit).unwrap(), want, "{name}");
        } else {
            let error = got.expect_err(name);
            let want = text(case, "error");
            if want != "*" {
                assert_eq!(error, want, "{name}");
            }
        }
    }
}

#[test]
fn requests_serialize_to_the_same_bytes() {
    for case in list(&cases(), "requests") {
        let body = &case["body"];
        let request = Request {
            method: text(case, "method").into(),
            path: text(case, "path").into(),
            body: (!body.is_null()).then(|| body.clone()),
        };
        assert_eq!(serde_json::to_string(&request).unwrap(), text(case, "json"));
    }
}

#[test]
fn replies_and_requests_parse_the_same_way() {
    for case in list(&cases(), "replies") {
        let json = text(case, "json");
        let parsed = serde_json::from_str::<Reply>(json);
        // `rustOk` marks a case where serde is knowingly laxer than the Go
        // decoder (the case's `note` says why); `ok` is the Go verdict.
        let ok = case.get("rustOk").unwrap_or(&case["ok"]);
        assert_eq!(parsed.is_ok(), ok.as_bool().unwrap(), "{json}: {parsed:?}");
        if let (Ok(reply), Some(status)) = (parsed, case["status"].as_u64()) {
            assert_eq!(u64::from(reply.status), status, "{json}");
        }
    }
    for case in list(&cases(), "requestsIn") {
        let json = text(case, "json");
        let parsed = serde_json::from_str::<Request>(json);
        assert_eq!(parsed.is_ok(), case["ok"].as_bool().unwrap(), "{json}");
    }
}
