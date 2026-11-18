//! The shared accept and reject fixtures in `fixtures/core`. The desktop's Go
//! port (`desktop/core/shared_fixtures_test.go`) reads the same files. Each
//! side checks its own direction: the mixer must write accepted values back
//! byte for byte as the `canonical` (compact) and `pretty` files hold them,
//! and must accept and reject alike the documents it reads itself. Documents
//! only the desktop reads (device state, versions, SSH status, meters,
//! diagnostics events and preset lists) are checked here for the encoding
//! only; their reject cases are the desktop's. A rejected case that names an
//! `error` must fail with exactly that message.
use rc2fx_core::{
    Config, ConfigUpdate, CoreError, DeviceState, DeviceVersions, DiagnosticsEvents, Meter,
    MixerRelease, PresetSummary, SshStatus, StockInventory, from_json,
};

/// The kinds of document only the desktop reads. The mixer writes them.
const WRITTEN_ONLY: [&str; 6] = [
    "deviceState",
    "deviceVersions",
    "sshStatus",
    "meter",
    "diagnosticsEvents",
    "presetSummaries",
];
use serde_json::Value;
use std::path::PathBuf;

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/core")
}

fn read(name: &str) -> Vec<u8> {
    std::fs::read(fixtures().join(name)).unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn message(error: CoreError) -> String {
    error.to_string()
}

fn written<T: serde::Serialize>(value: &T) -> (String, String) {
    (
        serde_json::to_string(value).unwrap(),
        serde_json::to_string_pretty(value).unwrap(),
    )
}

/// How the mixer reads each kind of document, and what it writes back.
fn decode(kind: &str, data: &[u8]) -> Result<(String, String), String> {
    if kind == "mixerRelease" {
        return MixerRelease::parse(data)
            .map(|r| written(&r))
            .map_err(message);
    }
    let text = std::str::from_utf8(data).map_err(|e| e.to_string())?;
    Ok(match kind {
        "config" => {
            let config: Config = from_json(text).map_err(message)?;
            config.validate().map_err(message)?;
            written(&config)
        }
        "deviceState" => written(&from_json::<DeviceState>(text).map_err(message)?),
        "configUpdate" => written(&from_json::<ConfigUpdate>(text).map_err(message)?),
        "presetSummaries" => written(&from_json::<Vec<PresetSummary>>(text).map_err(message)?),
        "deviceVersions" => written(&from_json::<DeviceVersions>(text).map_err(message)?),
        "sshStatus" => written(&from_json::<SshStatus>(text).map_err(message)?),
        "meter" => written(&from_json::<Meter>(text).map_err(message)?),
        "diagnosticsEvents" => written(&from_json::<DiagnosticsEvents>(text).map_err(message)?),
        "stockInventory" => {
            let inventory: StockInventory = from_json(text).map_err(message)?;
            inventory.validate().map_err(message)?;
            written(&inventory)
        }
        "json" => written(&from_json::<Value>(text).map_err(message)?),
        other => panic!("unknown fixture type {other}"),
    })
}

#[test]
fn rust_reaches_the_shared_verdicts_and_encodings() {
    let manifest: Value = serde_json::from_slice(&read("cases.json")).unwrap();
    let cases = manifest["cases"].as_array().expect("cases");
    assert!(cases.len() > 100, "{} cases", cases.len());
    let mut failures = Vec::new();
    for case in cases {
        let name = case["name"].as_str().expect("name");
        let kind = case["type"].as_str().expect("type");
        let accept = case["accept"].as_bool().expect("accept");
        if !accept && WRITTEN_ONLY.contains(&kind) {
            continue;
        }
        let result = decode(kind, &read(case["file"].as_str().expect("file")));
        match result {
            Ok((compact, pretty)) if accept => {
                for (key, got) in [("canonical", &compact), ("pretty", &pretty)] {
                    if let Some(file) = case.get(key).and_then(Value::as_str) {
                        let want = String::from_utf8(read(file)).unwrap();
                        if *got != want {
                            failures
                                .push(format!("{name}: {key} differs\n got {got}\nwant {want}"));
                        }
                    }
                }
            }
            Ok(_) => failures.push(format!("{name}: accepted, expected a rejection")),
            Err(error) if accept => failures.push(format!("{name}: rejected: {error}")),
            Err(error) => {
                if let Some(want) = case.get("error").and_then(Value::as_str)
                    && error != want
                {
                    failures.push(format!("{name}: error differs\n got {error}\nwant {want}"));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
