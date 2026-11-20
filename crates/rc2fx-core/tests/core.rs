use rc2fx_core::*;
use serde_json::{Value, json};

fn fixture(name: &str) -> String {
    std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/easyeffects")
            .join(name),
    )
    .unwrap()
}

/// Strict parsing and validation of saved settings, as the mixer's store
/// reads them.
fn config_from_json(text: &str) -> rc2fx_core::Result<Config> {
    let config: Config = from_json(text)?;
    config.validate()?;
    Ok(config)
}

#[test]
fn engine_restart_requests_name_the_revision_and_generation() {
    let request = EngineRestartRequest {
        expected_revision: 7,
        expected_generation: 8,
    };
    let wire = json!({"expectedRevision":7,"expectedGeneration":8});
    assert_eq!(
        from_json::<EngineRestartRequest>(&wire.to_string()).unwrap(),
        request
    );
    let mut old = wire;
    old.as_object_mut().unwrap().remove("expectedGeneration");
    assert!(from_json::<EngineRestartRequest>(&old.to_string()).is_err());
}

#[test]
fn current_wire_roundtrip_and_defaults() {
    let config = Config::default();
    config.validate().unwrap();
    assert_eq!(config.targets.len(), 25);
    assert!(config.selected_targets.is_empty());
    let text = serde_json::to_string(&config).unwrap();
    assert_eq!(config_from_json(&text).unwrap(), config);
    assert!(text.contains("\"schemaVersion\":9"));
    assert!(!text.contains("algorithm"));
    assert!(!text.contains("screenConversion"));
}

#[test]
fn removed_suppression_options_are_rejected_at_every_config_entry() {
    let config = serde_json::to_value(Config::default()).unwrap();
    for version in [5, 6, 7, 8, 10] {
        let mut invalid = config.clone();
        invalid["schemaVersion"] = json!(version);
        assert!(config_from_json(&invalid.to_string()).is_err());
    }
    let mut obsolete = config.clone();
    obsolete["screenConversion"] = json!("retired");
    assert!(config_from_json(&obsolete.to_string()).is_err());
    let update =
        json!({"expectedRevision":1,"targets":config["targets"],"screenConversion":"retired"});
    assert!(from_json::<ConfigUpdate>(&update.to_string()).is_err());
    for field in ["algorithm", "strength"] {
        let mut invalid = config.clone();
        invalid["targets"][0]["suppression"][field] = json!("retired");
        assert!(config_from_json(&invalid.to_string()).is_err());
        let update = json!({"expectedRevision":1, "targets": invalid["targets"]});
        assert!(from_json::<ConfigUpdate>(&update.to_string()).is_err());
    }
}

#[test]
fn fixed_suppression_roundtrips_without_model_identity() {
    let mut config = Config::default();
    config.targets[0].suppression.enabled = true;
    let text = serde_json::to_string(&config).unwrap();
    assert_eq!(config_from_json(&text).unwrap(), config);
}

#[test]
fn suppression_lookahead_roundtrips_and_rejects_unavailable_geometry() {
    let mut config = Config::default();
    config.targets[0].suppression.lookahead = 1;
    let text = serde_json::to_string(&config).unwrap();
    assert_eq!(config_from_json(&text).unwrap(), config);
    for lookahead in [2, 240] {
        config.targets[0].suppression.lookahead = lookahead;
        assert!(config.validate().is_err());
    }
}

#[test]
fn voice_gate_roundtrips_and_one_strength_sets_every_parameter() {
    let mut config = Config::default();
    config.targets[0].suppression.enabled = true;
    let gate = &mut config.targets[0].suppression.gate;
    gate.strength = 0.7;
    let text = serde_json::to_string(&config).unwrap();
    assert!(
        text.contains("\"gate\":{\"strength\":0.7,\"advanced\":false,"),
        "{text}"
    );
    assert_eq!(config_from_json(&text).unwrap(), config);
    // Default gate is off, with advanced fields initialized to strength 0.
    let default = Config::default().targets[0].suppression.gate.clone();
    assert!(!default.active());
    let mut advanced = default.clone();
    advanced.set_advanced(true);
    assert_eq!(
        advanced,
        VoiceGate {
            advanced: true,
            ..default.clone()
        }
    );
    let shape = GateShape::for_strength;
    let (off, middle, strict) = (shape(0.), shape(0.5), shape(1.));
    assert_eq!(
        (off.threshold, off.reduction_db, off.hold_ms, off.release_ms),
        (0.3, 0., 350., 300.)
    );
    assert_eq!(
        (strict.threshold, strict.reduction_db, strict.hold_ms),
        (0.7, 30., 150.)
    );
    assert!((strict.release_ms - 90.).abs() < 1e-9);
    assert_eq!(
        (middle.threshold, middle.attack_ms, middle.hold_ms),
        (0.5, 5., 250.)
    );
    assert!((middle.reduction_db - 20.).abs() < 1e-9);
    assert!(
        (middle.close_threshold - 0.35).abs() < 1e-12,
        "hysteresis below the threshold"
    );
    assert_eq!(
        GateShape::for_strength(0.).attack_ms,
        GateShape::for_strength(1.).attack_ms
    );
    // Higher strength -> higher threshold, deeper reduction, shorter hold, faster release.
    let mut previous = off;
    for step in 1..=10 {
        let next = shape(step as f64 / 10.);
        assert!(next.threshold > previous.threshold && next.reduction_db > previous.reduction_db);
        assert!(next.hold_ms < previous.hold_ms && next.release_ms < previous.release_ms);
        previous = next;
    }
    for strength in [-0.01, 1.01, f64::NAN] {
        config.targets[0].suppression.gate.strength = strength;
        assert!(config.validate().is_err(), "{strength}");
    }
}

#[test]
fn advanced_voice_gate_starts_from_strength_and_keeps_its_own_values() {
    let mut gate = Config::default().targets[0].suppression.gate.clone();
    gate.strength = 1.0;
    gate.set_advanced(true);
    assert_eq!(
        gate.shape(),
        GateShape::for_strength(1.0),
        "no jump when switching"
    );
    gate.threshold = 0.3;
    gate.hold = 900.0;
    gate.strength = 0.0;
    assert_eq!(
        (gate.shape().threshold, gate.shape().hold_ms),
        (0.3, 900.0),
        "decoupled from strength"
    );
    gate.set_advanced(false);
    assert_eq!(gate.shape(), GateShape::for_strength(0.0));
    assert_eq!(
        (gate.threshold, gate.hold),
        (0.3, 900.0),
        "custom values are kept"
    );
    let mut config = Config::default();
    for (field, value) in [
        ("threshold", 1.5),
        ("reduction", 61.0),
        ("attack", 0.1),
        ("hold", 2001.0),
        ("release", 29.0),
    ] {
        let mut wire = serde_json::to_value(&config).unwrap();
        wire["targets"][0]["suppression"]["gate"][field] = json!(value);
        assert!(config_from_json(&wire.to_string()).is_err(), "{field}");
    }
    config.targets[0].suppression.gate = gate;
    config.validate().unwrap();
}

#[test]
fn suppression_latency_includes_model_frame_and_worker_allowance() {
    let geometry = Suppression::admitted_geometry(0).unwrap();
    assert_eq!(geometry.frame_samples, 240);
    assert_eq!(geometry.intrinsic_delay_samples, 120);
    assert_eq!(geometry.worker_lead_samples, 320);
    assert_eq!(geometry.latency_samples(), 680);
}

#[test]
fn lookahead_one_adds_exactly_one_hop_to_the_admitted_latency() {
    let la0 = Suppression::admitted_geometry(0).unwrap();
    let la1 = Suppression::admitted_geometry(1).unwrap();
    assert_eq!(
        la1.latency_samples() - la0.latency_samples(),
        DFN_HOP_SAMPLES
    );
    assert!(Suppression::admitted_geometry(2).is_none());
}

#[test]
fn performance_reports_helper_counters_and_accepts_payloads_without_them() {
    use rc2fx_core::{Meter, Performance, TargetMeter, Timing};
    let timing = Timing {
        calls: 9,
        total_ns: 900,
        max_ns: 200,
        overruns: 1,
        clock_errors: 0,
        histogram: [1, 2, 3, 4, 5, 6, 7, 8],
    };
    let performance = Performance {
        callback_budget_ns: 2_333_333,
        worker_budget_ns: 9_333_333,
        xruns: 3,
        callback: timing.clone(),
        worker_wall: timing.clone(),
        worker_cpu: timing,
        helper_late: Some(2),
        helper_stalls: Some(1),
        callback_recent_max_ns: Some(180),
        worker_recent_max_ns: Some(900),
    };
    let target = TargetMeter {
        id: "mic-1".into(),
        input_peak: 0.,
        output_peak: 0.,
        rms: 0.,
        gain_reduction: 0.,
        voice: 0.,
        gate_open: 0.,
        clipped: 0,
        spectrum: vec![],
        callbacks: 9,
        processed_frames: 900,
        suppression_frames: 9,
        missed_deadlines: 0,
        stale_results: 0,
        input_drops: 0,
        output_drops: 0,
        latency_samples: 808,
        worker_failed: false,
        performance: Some(performance.clone()),
    };
    let meter = Meter {
        message_type: rc2fx_core::MeterType::Meter,
        revision: 1,
        generation: 1,
        synthetic: false,
        diagnostics_epoch: 0,
        running_for_ns: None,
        targets: vec![target],
    };
    let text = serde_json::to_string(&meter).unwrap();
    assert!(text.contains("\"helperLate\":2") && text.contains("\"helperStalls\":1"));
    assert!(
        text.contains("\"callbackRecentMaxNs\":180") && text.contains("\"workerRecentMaxNs\":900")
    );
    let back: Meter = serde_json::from_str(&text).unwrap();
    assert_eq!(back, meter);
    // Older firmware omits these fields; they deserialize as None rather than 0
    // so "not reported" stays distinct from zero.
    let mut old: serde_json::Value = serde_json::to_value(&meter).unwrap();
    let object = old["targets"][0]["performance"].as_object_mut().unwrap();
    object.remove("helperLate");
    object.remove("helperStalls");
    object.remove("callbackRecentMaxNs");
    object.remove("workerRecentMaxNs");
    let old: Meter = serde_json::from_value(old).unwrap();
    let performance = old.targets[0].performance.as_ref().unwrap();
    assert_eq!(
        (performance.helper_late, performance.helper_stalls),
        (None, None)
    );
    assert_eq!(
        (
            performance.callback_recent_max_ns,
            performance.worker_recent_max_ns
        ),
        (None, None)
    );
    assert_eq!(performance.xruns, 3);
}

#[test]
fn meters_without_a_diagnostics_epoch_load_as_zero_and_event_pages_round_trip() {
    use rc2fx_core::{DiagnosticsEvent, DiagnosticsEvents, DiagnosticsKind, EventDetails};
    // Older firmware omits diagnosticsEpoch; default to 0.
    let mut value = serde_json::json!({
        "type": "meter", "revision": 1, "generation": 1, "synthetic": false, "targets": []
    });
    let meter: rc2fx_core::Meter = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(meter.diagnostics_epoch, 0);
    value["diagnosticsEpoch"] = serde_json::json!(7);
    let meter: rc2fx_core::Meter = serde_json::from_value(value).unwrap();
    assert_eq!(meter.diagnostics_epoch, 7);

    let page = DiagnosticsEvents {
        now_ns: 10,
        next_seq: 3,
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
                    ..EventDetails::default()
                },
            },
            DiagnosticsEvent {
                seq: 2,
                kind: DiagnosticsKind::OutputDrops,
                path: Some("mic-1".into()),
                at_ns: 6,
                message: "3 output drops on mic-1".into(),
                details: EventDetails {
                    delta: Some(3),
                    interval_ns: Some(20_000_000),
                    ..EventDetails::default()
                },
            },
        ],
    };
    let text = serde_json::to_string(&page).unwrap();
    assert!(text.contains("\"kind\":\"xrun\""));
    assert!(text.contains("\"kind\":\"outputDrops\""));
    assert_eq!(
        serde_json::from_str::<DiagnosticsEvents>(&text).unwrap(),
        page
    );
    // Empty details should be omitted from JSON.
    let plain = DiagnosticsEvent {
        seq: 3,
        kind: DiagnosticsKind::Reset,
        path: None,
        at_ns: 7,
        message: "Diagnostics reset".into(),
        details: EventDetails::default(),
    };
    let text = serde_json::to_string(&plain).unwrap();
    assert!(!text.contains("details"), "{text}");
    assert_eq!(
        serde_json::from_str::<DiagnosticsEvent>(&text).unwrap(),
        plain
    );
}

#[test]
fn strict_config_rejects_ambiguous_or_invalid_data() {
    let base = serde_json::to_value(Config::default()).unwrap();
    let mut changes = Vec::new();
    for (pointer, value) in [
        ("/schemaVersion", json!(1)),
        ("/schemaVersion", json!(2)),
        ("/schemaVersion", json!(3)),
        ("/schemaVersion", json!(4)),
        ("/schemaVersion", json!(10)),
        ("/targets/0/suppression/wet", json!(1.01)),
        ("/targets/0/suppression/gate/strength", json!(1.5)),
        (
            "/targets/0/suppression/gate",
            json!({"enabled": true, "strength": 0.5}),
        ),
        ("/revision", json!(0)),
        ("/targets/0/channels", json!(1.5)),
        ("/targets/0/channels", json!(256)),
        ("/targets/0/eq/bands", Value::Null),
        ("/targets/0/gain/db", json!(13)),
    ] {
        let mut next = base.clone();
        *next.pointer_mut(pointer).unwrap() = value;
        changes.push(next);
    }
    let mut null_right = base.clone();
    null_right["targets"][0]["eq"]["rightBands"] = Value::Null;
    changes.push(null_right);
    let mut unknown = base.clone();
    unknown["targets"][0]["eq"]["legacy"] = json!(true);
    changes.push(unknown);
    let mut duplicated_target = base.clone();
    duplicated_target["targets"][1]["id"] = json!("mic-1");
    changes.push(duplicated_target);
    let mut bad_order = base.clone();
    bad_order["targets"][0]["order"][1] = json!("eq");
    changes.push(bad_order);
    for value in changes {
        assert!(config_from_json(&value.to_string()).is_err(), "{value}")
    }
    let text = serde_json::to_string(&Config::default()).unwrap();
    assert!(
        config_from_json(&text.replacen(
            "\"schemaVersion\":9",
            "\"schemaVersion\":9,\"schemaVersion\":9",
            1
        ))
        .is_err()
    );
    assert!(
        config_from_json(&text.replacen(
            "\"frequency\":80.0",
            "\"frequency\":80.0,\"frequency\":90.0",
            1
        ))
        .is_err()
    );
    assert!(config_from_json(&(text + " {}")).is_err());
    assert!(
        rc2fx_core::from_json::<Value>(&format!("{}0{}", "[".repeat(66), "]".repeat(66))).is_err()
    );
    assert!(rc2fx_core::from_json::<Value>(r#"{"x":1e400}"#).is_err());
    assert!(rc2fx_core::from_json::<Value>(r#"{"x":"\ud800"}"#).is_err());
}
#[test]
fn typed_validation() {
    let mut config = Config::default();
    config.targets[4].suppression.enabled = true;
    assert!(config.validate().is_err());
    config.targets[4].suppression.enabled = false;
    config.targets[0].gain.db = f64::NAN;
    assert!(config.validate().is_err());
    config.targets[0].gain.db = 0.0;
    let mut targets = config.targets.clone();
    targets[0].channels = 2;
    assert!(config.validate_update(&targets).is_err());
    let eq = &mut config.targets[0].eq;
    eq.bands = (1..=32).map(|n| Band::new(format!("band-{n}"))).collect();
    eq.validate(1).unwrap();
    eq.bands.push(Band::new("band-33"));
    assert!(eq.validate(1).is_err());
    eq.bands.truncate(32);
    eq.bands[0].mode = BandMode::RlcBt;
    eq.validate(1).unwrap();
    eq.bands[0].mode = BandMode::ApoDr;
    eq.right_bands = Some(Vec::new());
    assert!(eq.validate(1).is_err());
    eq.validate(2).unwrap();
}
#[test]
fn shared_rlc_reference_coefficients_match() {
    let vectors: Value = serde_json::from_str(&fixture("rlc-bt-x1-mapping-vectors.json")).unwrap();
    for vector in vectors["vectors"].as_array().unwrap() {
        let kind = match vector["type"].as_str().unwrap() {
            "Bell" => BandType::Bell,
            "Lo-shelf" => BandType::LowShelf,
            "Hi-shelf" => BandType::HighShelf,
            "Lo-pass" => BandType::LowPass,
            "Hi-pass" => BandType::HighPass,
            "Notch" => BandType::Notch,
            other => panic!("Unknown fixture type {other}"),
        };
        let band = Band {
            band_type: kind,
            mode: BandMode::RlcBt,
            frequency: vector["frequency"].as_f64().unwrap(),
            gain: vector["gain"].as_f64().unwrap(),
            q: vector["easyeffectsQ"].as_f64().unwrap(),
            ..Band::new("band-1")
        };
        assert!((eq::design_q(&band) - vector["rbjQ"].as_f64().unwrap()).abs() < 1e-12);
        for (actual, want) in eq::coefficients(&band)
            .unwrap()
            .as_array()
            .into_iter()
            .zip(vector["coefficients"].as_array().unwrap())
        {
            assert!(
                (actual - want.as_f64().unwrap()).abs() < 2e-12,
                "{vector}: {actual}"
            )
        }
    }
}
#[test]
fn coefficients_clamp_the_frequency_below_nyquist() {
    let mut band = Band {
        frequency: 24000.0,
        ..Band::new("band-1")
    };
    let high = eq::coefficients(&band).unwrap();
    band.frequency = 23520.0;
    assert_eq!(high, eq::coefficients(&band).unwrap());
}
