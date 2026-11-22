//! Shared accept/reject fixtures in `fixtures/core` (also run by the Go desktop
//! port in `desktop/core/shared_fixtures_test.go`).
//!
//! Rust owns the `canonical` and `pretty` golden bytes and the exact `error`
//! wording: accepted documents must round-trip byte for byte against their
//! `canonical` and `pretty` snapshots, and every rejected case Rust reads must
//! pin its exact `error` message in `cases.json`. Desktop-only response types
//! (`WRITTEN_ONLY`) are checked for encoding only; their rejection cases are
//! verified by Go.
//!
//! To regenerate the `canonical` and `pretty` golden files after an intentional
//! format change, run:
//! `RC2FX_UPDATE_FIXTURES=1 cargo test -p rc2fx-core --test shared_fixtures`
//! Update mode writes each changed path directly to stderr and skips any golden
//! that is also an input `file`, comparing those strictly so differences fail
//! and can be updated by hand.

use rc2fx_core::{
    Config, ConfigUpdate, CoreError, DeviceState, DeviceVersions, DiagnosticsEvents, Meter,
    MixerRelease, PresetSummary, SshStatus, StockInventory, from_json,
};
use serde_json::Value;
use std::collections::HashSet;
use std::io::Write;
use std::path::PathBuf;

/// Document types written by the mixer and only parsed by the desktop app.
const WRITTEN_ONLY: [&str; 6] = [
    "deviceState",
    "deviceVersions",
    "sshStatus",
    "meter",
    "diagnosticsEvents",
    "presetSummaries",
];

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
    let update = std::env::var("RC2FX_UPDATE_FIXTURES").as_deref() == Ok("1");
    let manifest: Value = serde_json::from_slice(&read("cases.json")).unwrap();
    let cases = manifest["cases"].as_array().expect("cases");
    assert!(cases.len() > 100, "{} cases", cases.len());
    let inputs: HashSet<&str> = cases
        .iter()
        .map(|case| case["file"].as_str().expect("file"))
        .collect();
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
                        if update && !inputs.contains(file) {
                            let path = fixtures().join(file);
                            let existing = std::fs::read(&path).ok();
                            if existing.as_deref() != Some(got.as_bytes()) {
                                std::fs::write(&path, got.as_bytes())
                                    .unwrap_or_else(|e| panic!("write {file}: {e}"));
                                let _ = writeln!(std::io::stderr(), "updated fixtures/core/{file}");
                            }
                        } else {
                            let want = String::from_utf8(read(file)).unwrap();
                            if *got != want {
                                failures.push(format!(
                                    "{name}: {key} differs\n got {got}\nwant {want}"
                                ));
                            }
                        }
                    }
                }
            }
            Ok(_) => failures.push(format!("{name}: accepted, expected a rejection")),
            Err(error) if accept => failures.push(format!("{name}: rejected: {error}")),
            Err(error) => match case.get("error").and_then(Value::as_str) {
                Some(want) if error != want => {
                    let got_json = serde_json::to_string(&error).unwrap();
                    failures.push(format!(
                        "{name}: error differs\n got {error}\nwant {want}\njson {got_json}"
                    ));
                }
                Some(_) => {}
                None => {
                    let got_json = serde_json::to_string(&error).unwrap();
                    failures.push(format!("MISSING_ERROR\t{name}\t{got_json}"));
                }
            },
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
