//! Live Rust <-> Go wire-contract and backward-compatibility tests for all
//! `rc2fx-core` document types (replaces the 181 static JSON files previously
//! under `fixtures/core`).
//!
//! Wire structs and enums are generated from a single source (`schema/core.go`).
//! This test verifies that Rust (`serde_json` + `rc2fx-core`) and live Go
//! (`sjson` + `desktop/core`, via `desktop/internal/interop core`) agree on
//! accept/reject verdicts, canonical JSON values, and frozen older-mixer wire
//! payloads across every document type.

use rc2fx_core::{
    Config, ConfigUpdate, CoreError, DeviceState, DeviceVersions, DiagnosticsEvent,
    DiagnosticsEvents, DiagnosticsKind, EngineMode, EnginePhase, EventDetails, Meter, MeterType,
    MixerRelease, Performance, PresetSummary, RoutingPhase, RoutingStatus, SelectionPhase,
    SelectionStatus, SshStatus, StockInventory, StockSource, TargetMeter, Timing, from_json,
};
use serde_json::{Value, json};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

const WRITTEN_ONLY: [&str; 6] = [
    "deviceState",
    "deviceVersions",
    "sshStatus",
    "meter",
    "diagnosticsEvents",
    "presetSummaries",
];

fn message(error: CoreError) -> String {
    error.to_string()
}

fn written<T: serde::Serialize>(value: &T) -> (String, String) {
    (
        serde_json::to_string(value).unwrap(),
        serde_json::to_string_pretty(value).unwrap(),
    )
}

fn decode_rust(kind: &str, data: &[u8]) -> Result<(String, String), String> {
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
        other => panic!("unknown kind {other}"),
    })
}

fn hex_encode(data: &[u8]) -> String {
    data.iter().map(|b| format!("{b:02x}")).collect()
}

fn call_go_core(cases: &[(&str, &str, Vec<u8>)]) -> Vec<Value> {
    let req: Vec<Value> = cases
        .iter()
        .map(|(name, kind, data)| {
            json!({
                "name": name,
                "kind": kind,
                "dataHex": hex_encode(data),
            })
        })
        .collect();
    let desktop = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../desktop");
    let mut child = Command::new("go")
        .args(["run", "./internal/interop", "core"])
        .current_dir(&desktop)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn `go run ./internal/interop core`");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&serde_json::to_vec(&req).unwrap())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "go interop core failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn rust_and_go_core_agree_across_wire_types_and_backward_compat_payloads() {
    let default_cfg = Config::default();
    let default_cfg_json = serde_json::to_string(&default_cfg).unwrap();

    // Legacy config payload without suppression.lookahead (older app/config compat).
    let mut legacy_cfg_val: Value = serde_json::from_str(&default_cfg_json).unwrap();
    for target in legacy_cfg_val["targets"].as_array_mut().unwrap() {
        target["suppression"]
            .as_object_mut()
            .unwrap()
            .remove("lookahead");
    }
    let legacy_cfg_json = serde_json::to_string(&legacy_cfg_val).unwrap();

    // Invalid config with explicit null rightBands (must be rejected on both sides).
    let mut null_right_val: Value = serde_json::from_str(&default_cfg_json).unwrap();
    null_right_val["targets"][0]["eq"]["rightBands"] = Value::Null;
    let null_right_json = serde_json::to_string(&null_right_val).unwrap();

    let device_state = DeviceState {
        config: default_cfg.clone(),
        engine: rc2fx_core::EngineStatus {
            generation: 3,
            restart_allowed: false,
            mode: EngineMode::Native,
            status: EnginePhase::Running,
            applied_revision: 1,
            sample_rate: 48_000,
            block_size: 48,
            message: String::new(),
            audio_advancing: true,
        },
        routing: RoutingStatus {
            status: RoutingPhase::Active,
            channels: 2,
            recovered: 0,
            message: String::new(),
        },
        selection: SelectionStatus {
            channel_capacity: Some(6),
            status: SelectionPhase::Idle,
            allowed: true,
            qualified_target_ids: vec!["mic-1".into()],
            active_target_ids: vec!["mic-1".into()],
            pending_target_ids: Vec::new(),
            message: String::new(),
        },
    };
    let config_update = ConfigUpdate {
        expected_revision: 1,
        targets: default_cfg.targets.clone(),
    };
    let presets = vec![
        PresetSummary {
            id: "broadcast".into(),
            name: "Broadcast".into(),
        },
        PresetSummary {
            id: "warm".into(),
            name: "Warm".into(),
        },
    ];
    let meter = Meter {
        message_type: MeterType::Meter,
        revision: 4,
        generation: 2,
        synthetic: false,
        diagnostics_epoch: 1,
        running_for_ns: Some(9_000_000_000),
        targets: vec![TargetMeter {
            id: "mic-1".into(),
            input_peak: 0.25,
            output_peak: 0.5,
            rms: 0.125,
            gain_reduction: 1.5,
            voice: 0.8,
            gate_open: 1.0,
            clipped: 0,
            spectrum: vec![-60.0, -40.0, -20.0],
            callbacks: 100,
            processed_frames: 100,
            suppression_frames: 20,
            missed_deadlines: 0,
            stale_results: 0,
            input_drops: 0,
            output_drops: 0,
            latency_samples: 680,
            worker_failed: false,
            performance: Some(Performance {
                callback_budget_ns: 1_000_000,
                worker_budget_ns: 5_000_000,
                xruns: 0,
                callback: Timing {
                    calls: 100,
                    total_ns: 12_000_000,
                    max_ns: 200_000,
                    overruns: 0,
                    clock_errors: 0,
                    histogram: [10, 20, 30, 40, 0, 0, 0, 0],
                },
                worker_wall: Timing::default(),
                worker_cpu: Timing::default(),
                helper_late: Some(1),
                helper_stalls: Some(0),
                callback_recent_max_ns: Some(180_000),
                worker_recent_max_ns: None,
            }),
        }],
    };
    let diag = DiagnosticsEvents {
        now_ns: 10,
        next_seq: 4,
        lost: 1,
        more: false,
        events: vec![
            DiagnosticsEvent {
                seq: 1,
                kind: DiagnosticsKind::Xrun,
                path: None,
                at_ns: 5,
                message: "JACK xrun".into(),
                details: EventDetails {
                    value_ns: Some(1120),
                    delayed_us: Some(240),
                    delta: None,
                    interval_ns: None,
                },
            },
            DiagnosticsEvent {
                seq: 2,
                kind: DiagnosticsKind::OutputDrops,
                path: Some("mic-1".into()),
                at_ns: 6,
                message: "3 output drops".into(),
                details: EventDetails {
                    value_ns: None,
                    delayed_us: None,
                    delta: Some(3),
                    interval_ns: Some(20_000_000),
                },
            },
            DiagnosticsEvent {
                seq: 3,
                kind: DiagnosticsKind::Reset,
                path: None,
                at_ns: 7,
                message: "Diagnostics reset".into(),
                details: EventDetails::default(),
            },
        ],
    };
    let inventory = StockInventory {
        available: true,
        show_name: "Default Show".into(),
        revision: "a".repeat(64),
        message: String::new(),
        inputs: vec![StockSource {
            id: "input-3".into(),
            source_id: 3,
            fader: Some(1),
            name: "Mic 4".into(),
            color: "fffdba0e".into(),
            target_id: "mic-1".into(),
        }],
    };

    // Frozen older-mixer / forward-compat payloads + strict rejections.
    let cases: Vec<(&str, &str, Vec<u8>, bool)> = vec![
        ("config-default", "config", default_cfg_json.into_bytes(), true),
        ("config-legacy-no-lookahead", "config", legacy_cfg_json.into_bytes(), true),
        ("config-null-right-bands", "config", null_right_json.into_bytes(), false),
        ("config-duplicate-key", "config", br#"{"schemaVersion":9,"schemaVersion":9}"#.to_vec(), false),
        ("config-unknown-field", "config", br#"{"schemaVersion":9,"revision":0,"selectedTargets":[],"targets":[],"extra":1}"#.to_vec(), false),
        ("config-bad-utf8", "config", vec![0xff, 0xfe], false),
        ("device-state-valid", "deviceState", serde_json::to_vec(&device_state).unwrap(), true),
        ("device-state-bad-mode", "deviceState", serde_json::to_string(&device_state).unwrap().replace(r#""native""#, r#""bogus""#).into_bytes(), false),
        ("config-update-valid", "configUpdate", serde_json::to_vec(&config_update).unwrap(), true),
        ("config-update-extra", "configUpdate", br#"{"expectedRevision":1,"targets":[],"x":1}"#.to_vec(), false),
        ("presets-valid", "presetSummaries", serde_json::to_vec(&presets).unwrap(), true),
        ("presets-extra-field", "presetSummaries", br#"[{"id":"a","name":"A","x":1}]"#.to_vec(), false),
        ("versions-empty", "deviceVersions", b"{}".to_vec(), true),
        ("versions-future-field", "deviceVersions", br#"{"runtime":"0.2.3","protocol":1,"inPlaceUpdates":2,"futureFlag":true}"#.to_vec(), true),
        ("versions-bad-type", "deviceVersions", br#"{"protocol":"1"}"#.to_vec(), false),
        ("ssh-status-valid", "sshStatus", br#"{"managed":true,"managedFingerprint":"SHA256:abc","unmanagedKeys":1,"hostKeys":["ssh-ed25519 AAAA"],"serverRunning":true}"#.to_vec(), true),
        ("ssh-status-future-field", "sshStatus", br#"{"managed":false,"unmanagedKeys":0,"hostKeys":[],"future":42}"#.to_vec(), true),
        ("ssh-status-missing-required", "sshStatus", br#"{"managed":false}"#.to_vec(), false),
        ("meter-full", "meter", serde_json::to_vec(&meter).unwrap(), true),
        // Older mixer meter without diagnosticsEpoch / runningForNs / helper counters, plus unknown future field.
        (
            "meter-legacy-and-future",
            "meter",
            br#"{"type":"meter","revision":1,"generation":1,"synthetic":false,"futureField":123,"targets":[]}"#.to_vec(),
            true,
        ),
        ("meter-bad-type", "meter", br#"{"type":"other","revision":1,"generation":1,"synthetic":false,"targets":[]}"#.to_vec(), false),
        ("diagnostics-full", "diagnosticsEvents", serde_json::to_vec(&diag).unwrap(), true),
        (
            "diagnostics-unknown-kind-fallback",
            "diagnosticsEvents",
            br#"{"nowNs":10,"nextSeq":2,"lost":0,"more":false,"events":[{"seq":1,"kind":"futureKindFromNewerMixer","atNs":5,"message":"hi"}]}"#.to_vec(),
            true,
        ),
        ("inventory-valid", "stockInventory", serde_json::to_vec(&inventory).unwrap(), true),
        ("inventory-unavailable-with-inputs", "stockInventory", serde_json::to_string(&inventory).unwrap().replace(r#""available":true"#, r#""available":false"#).into_bytes(), false),
        ("mixer-release-valid", "mixerRelease", br#"{"schemaVersion":1,"version":"0.2.3","protocolVersion":1}"#.to_vec(), true),
        ("mixer-release-bad-schema", "mixerRelease", br#"{"schemaVersion":2,"version":"0.2.3","protocolVersion":1}"#.to_vec(), false),
        ("mixer-release-dup-key", "mixerRelease", br#"{"schemaVersion":1,"version":"0.2.3","version":"0.2.4","protocolVersion":1}"#.to_vec(), false),
        ("json-valid", "json", r#"{"a":[1,2.5,-0.0,true,false,null],"b":"é"}"#.as_bytes().to_vec(), true),
        ("json-dup-key", "json", br#"{"a":1,"a":2}"#.to_vec(), false),
        ("json-trailing-garbage", "json", br#"{} x"#.to_vec(), false),
    ];

    let go_input: Vec<(&str, &str, Vec<u8>)> = cases
        .iter()
        .map(|(n, k, d, _)| (*n, *k, d.clone()))
        .collect();
    let go_results = call_go_core(&go_input);
    assert_eq!(go_results.len(), cases.len());

    for ((name, kind, data, want_accept), go) in cases.iter().zip(&go_results) {
        let go_ok = go["ok"].as_bool().unwrap();
        assert_eq!(
            go_ok, *want_accept,
            "go verdict mismatch for {name}: got ok={go_ok} ({})",
            go["error"]
        );

        if !*want_accept && WRITTEN_ONLY.contains(kind) {
            continue;
        }
        let rs = decode_rust(kind, data);
        assert_eq!(
            rs.is_ok(),
            *want_accept,
            "rust verdict mismatch for {name}: {rs:?}"
        );
        if let Ok((rs_compact, rs_pretty)) = rs {
            let go_compact = go["compact"].as_str().unwrap();
            let go_pretty = go["pretty"].as_str().unwrap();
            assert_eq!(
                go_compact, rs_compact,
                "{name}: Go compact JSON differs from Rust serde_json"
            );
            assert_eq!(
                go_pretty, rs_pretty,
                "{name}: Go pretty JSON differs from Rust serde_json"
            );
        }
    }
}
