use rc2fx_core::easyeffects::{self, ImportOptions, ImportResult, Pipeline};
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
fn imported(text: &str, stereo: bool) -> EqSettings {
    match easyeffects::import(
        text,
        &ImportOptions {
            stereo,
            ..Default::default()
        },
    )
    .unwrap()
    {
        ImportResult::Ready { eq, .. } => eq,
        other => panic!("Unexpected import result: {other:?}"),
    }
}
fn state(revision: u64) -> DeviceState {
    let selected: Vec<String> = ["mic-1", "usb-main", "send-chat"].map(str::to_owned).into();
    let channels = 5;
    DeviceState {
        selection: SelectionStatus {
            channel_capacity: None,
            status: SelectionPhase::Idle,
            allowed: true,
            qualified_target_ids: selected.clone(),
            active_target_ids: selected.clone(),
            pending_target_ids: vec![],
            message: String::new(),
        },
        routing: RoutingStatus {
            status: RoutingPhase::Active,
            channels,
            recovered: 0,
            message: String::new(),
        },
        config: Config {
            revision,
            selected_targets: selected,
            ..Config::default()
        },
        engine: EngineStatus {
            generation: 1,
            restart_allowed: false,
            mode: EngineMode::Jack,
            status: EnginePhase::Running,
            applied_revision: revision,
            sample_rate: 48000,
            block_size: 112,
            message: String::new(),
            audio_advancing: true,
        },
    }
}

fn restartable_state() -> DeviceState {
    let mut state = state(7);
    state.engine.status = EnginePhase::Error;
    state.engine.generation = 8;
    state.engine.restart_allowed = true;
    state.engine.audio_advancing = false;
    state
}

fn restart_ack(state: &DeviceState) -> DeviceState {
    let mut ack = state.clone();
    ack.engine.generation += 1;
    ack.engine.status = EnginePhase::Restarting;
    ack.engine.restart_allowed = false;
    ack
}

#[test]
fn restart_wire_requires_generation_and_explicit_eligibility() {
    let original = restartable_state();
    let value = serde_json::to_value(&original).unwrap();
    assert_eq!(
        DeviceState::from_json(&value.to_string()).unwrap(),
        original
    );
    for key in ["generation", "restartAllowed"] {
        let mut value = value.clone();
        value["engine"].as_object_mut().unwrap().remove(key);
        assert!(DeviceState::from_json(&value.to_string()).is_err());
    }
    let request = EngineRestartRequest {
        expected_revision: 7,
        expected_generation: 8,
    };
    assert_eq!(
        serde_json::to_value(request).unwrap(),
        json!({"expectedRevision":7,"expectedGeneration":8})
    );
    let meter = json!({"type":"meter","revision":7,"generation":8,"synthetic":false,"targets":[]});
    assert!(rc2fx_core::from_json::<Meter>(&meter.to_string()).is_ok());
    let mut old = meter;
    old.as_object_mut().unwrap().remove("generation");
    assert!(rc2fx_core::from_json::<Meter>(&old.to_string()).is_err());
}

#[test]
fn restart_preserves_saved_revision_settings_and_unsaved_draft() {
    let saved = restartable_state();
    let mut session = Session::default();
    session.receive(saved.clone()).unwrap();
    session.draft[0].gain.db = -9.;
    let draft = session.draft.clone();
    let pending = session.begin_restart().unwrap();
    assert_eq!(pending.request.expected_revision, 7);
    assert_eq!(pending.request.expected_generation, 8);
    session
        .acknowledge_restart(&pending, restart_ack(&saved))
        .unwrap();
    assert_eq!(session.saved.as_ref().unwrap().config, saved.config);
    assert_eq!(session.draft, draft);
    assert!(!session.is_applied());
    assert!(session.begin_apply().is_err());
    assert!(session.begin_restart().is_err());
}

#[test]
fn late_restart_ack_cannot_overwrite_terminal_stream_state() {
    for phase in [EnginePhase::Running, EnginePhase::Error] {
        let saved = restartable_state();
        let mut session = Session::default();
        session.receive(saved.clone()).unwrap();
        let pending = session.begin_restart().unwrap();
        let ack = restart_ack(&saved);
        let mut terminal = ack.clone();
        terminal.engine.status = phase;
        terminal.engine.message = "Stream result".into();
        session.receive(terminal.clone()).unwrap();
        session.acknowledge_restart(&pending, ack).unwrap();
        assert_eq!(session.saved.as_ref().unwrap(), &terminal);
        session.receive_snapshot(restart_ack(&saved)).unwrap();
        assert_eq!(session.saved.as_ref().unwrap(), &terminal);
        assert_eq!(session.receive(saved.clone()).unwrap(), RemoteUpdate::Stale);
        assert!(!session.receive_engine(saved.engine.clone()));
        // A deliberate new connection can target a different mixer with its
        // own generation sequence. A transient event-stream reconnect does not reset it.
        session.disconnect();
        assert_ne!(session.receive(saved).unwrap(), RemoteUpdate::Stale);
    }
}

#[test]
fn restart_rejects_wrong_generation_mutated_settings_or_unreserved_response() {
    let saved = restartable_state();
    let mut session = Session::default();
    session.receive(saved.clone()).unwrap();
    let pending = session.begin_restart().unwrap();
    for mutation in 0..6 {
        let mut ack = restart_ack(&saved);
        match mutation {
            0 => ack.engine.generation += 1,
            1 => ack.config.revision += 1,
            2 => ack.config.targets[0].gain.db = -5.,
            3 => ack.engine.status = EnginePhase::Running,
            4 => ack.engine.restart_allowed = true,
            _ => ack.engine.audio_advancing = true,
        }
        assert!(session.acknowledge_restart(&pending, ack).is_err());
        assert_eq!(session.saved.as_ref().unwrap(), &saved);
    }
    session.disconnect();
    assert!(session.begin_restart().is_err());
}

#[test]
fn applied_requires_all_hardware_channels_routed() {
    let mut value = state(1);
    assert!(value.is_applied());
    value.routing.channels -= 1;
    assert!(!value.is_applied());
    for phase in [
        RoutingPhase::Unmanaged,
        RoutingPhase::Starting,
        RoutingPhase::Bypassing,
        RoutingPhase::Bypassed,
        RoutingPhase::Error,
    ] {
        value.routing.status = phase;
        assert!(!value.is_applied());
    }
    value.engine.mode = EngineMode::Simulation;
    assert!(value.is_applied());
}

#[test]
fn current_wire_roundtrip_and_defaults() {
    let config = Config::default();
    config.validate().unwrap();
    assert_eq!(config.targets.len(), 25);
    assert!(config.selected_targets.is_empty());
    let text = serde_json::to_string(&config).unwrap();
    assert_eq!(Config::from_json(&text).unwrap(), config);
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
        assert!(Config::from_json(&invalid.to_string()).is_err());
        let mut snapshot = serde_json::to_value(state(1)).unwrap();
        snapshot["config"] = invalid;
        assert!(DeviceState::from_json(&snapshot.to_string()).is_err());
    }
    let mut obsolete = config.clone();
    obsolete["screenConversion"] = json!("retired");
    assert!(Config::from_json(&obsolete.to_string()).is_err());
    let update =
        json!({"expectedRevision":1,"targets":config["targets"],"screenConversion":"retired"});
    assert!(from_json::<ConfigUpdate>(&update.to_string()).is_err());
    for field in ["algorithm", "strength"] {
        let mut invalid = config.clone();
        invalid["targets"][0]["suppression"][field] = json!("retired");
        assert!(Config::from_json(&invalid.to_string()).is_err());
        let update = json!({"expectedRevision":1, "targets": invalid["targets"]});
        assert!(from_json::<ConfigUpdate>(&update.to_string()).is_err());
    }
}

#[test]
fn fixed_suppression_roundtrips_without_model_identity() {
    let mut config = Config::default();
    config.targets[0].suppression.enabled = true;
    let text = serde_json::to_string(&config).unwrap();
    assert_eq!(Config::from_json(&text).unwrap(), config);
}

#[test]
fn suppression_lookahead_roundtrips_and_rejects_unavailable_geometry() {
    let mut config = Config::default();
    config.targets[0].suppression.select_lookahead(1);
    let text = serde_json::to_string(&config).unwrap();
    assert_eq!(Config::from_json(&text).unwrap(), config);
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
    assert_eq!(Config::from_json(&text).unwrap(), config);
    // The default is off, its advanced parameters what 0 stands for.
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
    // Stricter: surer voice to open, deeper, shorter hold, faster close.
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
fn easyeffects_width_and_slope_are_ignored_on_import_and_not_exported() {
    fn set_everywhere(value: &mut Value) {
        match value {
            Value::Object(map) => {
                if map.contains_key("frequency") {
                    map.insert("slope".into(), json!("x4"));
                    map.insert("width".into(), json!(11.0));
                }
                map.values_mut().for_each(set_everywhere);
            }
            Value::Array(items) => items.iter_mut().for_each(set_everywhere),
            _ => {}
        }
    }
    let text = fixture("output-linked-apo.json");
    let plain = imported(&text, true);
    let mut steep: Value = serde_json::from_str(&text).unwrap();
    set_everywhere(&mut steep);
    assert_eq!(imported(&steep.to_string(), true), plain);
    let exported = easyeffects::export(&plain, Pipeline::Output).unwrap();
    assert!(!exported.contains("\"slope\"") && !exported.contains("\"width\""));
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
        assert!(Config::from_json(&wire.to_string()).is_err(), "{field}");
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
fn the_worker_lead_and_the_latency_add_up() {
    let geometry = Suppression::admitted_geometry(0)
        .unwrap()
        .with_worker_lead(112);
    assert_eq!(geometry.latency_samples(), 240 + 120 + 112);
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
        targets: vec![target],
    };
    let text = serde_json::to_string(&meter).unwrap();
    assert!(text.contains("\"helperLate\":2") && text.contains("\"helperStalls\":1"));
    let back: Meter = serde_json::from_str(&text).unwrap();
    assert_eq!(back, meter);
    // A mixer that predates the field sends no helper counts: they are absent
    // rather than zero, so "not reported" stays distinguishable from "none".
    let mut old: serde_json::Value = serde_json::to_value(&meter).unwrap();
    let object = old["targets"][0]["performance"].as_object_mut().unwrap();
    object.remove("helperLate");
    object.remove("helperStalls");
    let old: Meter = serde_json::from_value(old).unwrap();
    let performance = old.targets[0].performance.as_ref().unwrap();
    assert_eq!(
        (performance.helper_late, performance.helper_stalls),
        (None, None)
    );
    assert_eq!(performance.xruns, 3);
}

#[test]
fn meters_without_a_diagnostics_epoch_load_as_zero_and_event_pages_round_trip() {
    use rc2fx_core::{DiagnosticsEvent, DiagnosticsEvents, DiagnosticsKind, EventDetails};
    // An older mixer sends no epoch: the app reads 0 and keeps working.
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
    // Empty details are omitted rather than sent as an object of nulls.
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
        assert!(Config::from_json(&value.to_string()).is_err(), "{value}")
    }
    let text = serde_json::to_string(&Config::default()).unwrap();
    assert!(
        Config::from_json(&text.replacen(
            "\"schemaVersion\":9",
            "\"schemaVersion\":9,\"schemaVersion\":9",
            1
        ))
        .is_err()
    );
    assert!(
        Config::from_json(&text.replacen(
            "\"frequency\":80.0",
            "\"frequency\":80.0,\"frequency\":90.0",
            1
        ))
        .is_err()
    );
    assert!(Config::from_json(&(text + " {}")).is_err());
    assert!(
        rc2fx_core::from_json::<Value>(&format!("{}0{}", "[".repeat(66), "]".repeat(66))).is_err()
    );
    assert!(rc2fx_core::from_json::<Value>(r#"{"x":1e400}"#).is_err());
    assert!(rc2fx_core::from_json::<Value>(r#"{"x":"\ud800"}"#).is_err());
}
#[test]
fn typed_validation_and_band_operations() {
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
    for n in 1..=32 {
        assert_eq!(eq.add_band(Channel::Left).unwrap().id, format!("band-{n}"));
    }
    assert!(eq.add_band(Channel::Left).is_err());
    assert!(eq.remove_band(Channel::Left, "band-2"));
    assert_eq!(eq.add_band(Channel::Left).unwrap().id, "band-33");
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
fn response_curve_matches_per_point_response() {
    let eq = EqSettings {
        bands: vec![
            Band {
                gain: 4.0,
                frequency: 120.0,
                band_type: BandType::LowShelf,
                ..Band::new("band-1")
            },
            Band {
                gain: -3.0,
                frequency: 2500.0,
                q: 2.0,
                ..Band::new("band-2")
            },
            Band {
                frequency: 60.0,
                band_type: BandType::HighPass,
                ..Band::new("band-3")
            },
        ],
        input_gain_db: -1.5,
        ..Default::default()
    };
    let frequencies = eq::log_frequencies(64, 10.0, 24000.0);
    let curve = eq::response_curve(&eq, Channel::Left, &frequencies, true);
    for (value, frequency) in curve.iter().zip(&frequencies) {
        let expected = eq::response_db(&eq, Channel::Left, *frequency, true);
        assert!((value - expected).abs() < 1e-12, "{frequency} Hz");
    }
}
#[test]
fn response_gains_solo_bypass_and_channel_isolation() {
    let mut eq = EqSettings {
        bands: vec![Band {
            gain: 6.0,
            ..Band::new("band-1")
        }],
        input_gain_db: -2.0,
        output_gain_db: 1.0,
        ..Default::default()
    };
    assert!((eq::response_db(&eq, Channel::Left, 1000.0, false) - 6.0).abs() < 1e-9);
    assert!((eq::response_db(&eq, Channel::Right, 1000.0, true) - 5.0).abs() < 1e-9);
    eq.right_bands = Some(Vec::new());
    assert_eq!(eq::response_db(&eq, Channel::Right, 1000.0, true), -1.0);
    eq.bands.push(Band {
        solo: true,
        enabled: false,
        ..Band::new("band-2")
    });
    assert_eq!(eq::response_db(&eq, Channel::Left, 1000.0, false), 0.0);
    let frequencies = [10.0, 1000.0, 30000.0];
    for channel in [Channel::Left, Channel::Right] {
        for gains in [false, true] {
            let curve = eq::response_curve(&eq, channel, &frequencies, gains);
            for (value, frequency) in curve.iter().zip(frequencies) {
                assert_eq!(*value, eq::response_db(&eq, channel, frequency, gains));
            }
        }
    }
    eq.enabled = false;
    assert_eq!(
        eq::response_curve(&eq, Channel::Left, &frequencies, true),
        vec![0.0; 3]
    );
    assert_eq!(eq::response_db(&eq, Channel::Left, 1000.0, true), 0.0);
    assert_eq!(eq::log_frequencies(1, 10.0, 24000.0), vec![10.0]);
    assert!(eq::log_frequencies(0, 10.0, 24000.0).is_empty());
    let mut band = Band {
        frequency: 24000.0,
        ..Band::new("band-1")
    };
    let high = eq::coefficients(&band).unwrap();
    band.frequency = 23520.0;
    assert_eq!(high, eq::coefficients(&band).unwrap());
}
#[test]
fn all_supported_shared_presets_import_and_roundtrip() {
    for name in [
        "community-loudness-eq-extract.json",
        "output-linked-apo.json",
        "output-apo-common-types.json",
        "output-split-different.json",
        "input-v6-unindexed.json",
        "input-empty-as-off.json",
    ] {
        let eq = imported(&fixture(name), true);
        eq.validate(2).unwrap();
        let roundtrip = imported(&easyeffects::export(&eq, Pipeline::Output).unwrap(), true);
        assert_eq!(roundtrip, eq, "Roundtrip {name}");
    }
}
#[test]
fn empty_and_unequal_channel_export_never_emit_zero_bands() {
    let empty = EqSettings::default();
    let text = easyeffects::export(&empty, Pipeline::Input).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&text).unwrap()["input"]["equalizer#0"]["num-bands"],
        1
    );
    assert_eq!(imported(&text, true), empty);
    let split = EqSettings {
        bands: vec![Band::new("band-1")],
        right_bands: Some(Vec::new()),
        ..Default::default()
    };
    assert_eq!(
        imported(
            &easyeffects::export(&split, Pipeline::Output).unwrap(),
            true
        ),
        split
    );
    let mut nonneutral = Band {
        band_type: BandType::Off,
        gain: 5.0,
        ..Band::new("band-1")
    };
    nonneutral.enabled = false;
    let eq = EqSettings {
        bands: vec![nonneutral],
        ..Default::default()
    };
    assert_eq!(
        imported(&easyeffects::export(&eq, Pipeline::Output).unwrap(), true),
        eq
    );
}
#[test]
fn explicit_equalizer_and_mono_channel_selection() {
    let text = fixture("output-split-different.json");
    assert!(matches!(
        easyeffects::import(&text, &ImportOptions::default()).unwrap(),
        ImportResult::ChooseChannel { .. }
    ));
    let left = easyeffects::import(
        &text,
        &ImportOptions {
            channel: Some(Channel::Left),
            ..Default::default()
        },
    )
    .unwrap();
    let right = easyeffects::import(
        &text,
        &ImportOptions {
            channel: Some(Channel::Right),
            ..Default::default()
        },
    )
    .unwrap();
    assert_ne!(left, right);
    let mut multiple: Value = serde_json::from_str(&text).unwrap();
    multiple["input"] = multiple["output"].clone();
    assert!(
        matches!(easyeffects::import(&multiple.to_string(),&ImportOptions::default()).unwrap(),ImportResult::ChooseEqualizer{candidates} if candidates.len()==2)
    );
    assert!(matches!(
        easyeffects::import(
            &multiple.to_string(),
            &ImportOptions {
                stereo: true,
                selected: Some("input/equalizer#0".into()),
                channel: None
            }
        )
        .unwrap(),
        ImportResult::Ready { .. }
    ));
}
#[test]
fn unsupported_imports_and_strict_json_leave_caller_unchanged() {
    let base: Value = serde_json::from_str(&fixture("output-linked-apo.json")).unwrap();
    for (pointer, value) in [
        ("/output/equalizer#0/mode", json!("FIR")),
        ("/output/equalizer#0/decramp", json!("x2")),
        ("/output/equalizer#0/balance", json!(1)),
        ("/output/equalizer#0/pitch-left", json!(2)),
        ("/output/equalizer#0/left/band0/type", json!("Allpass")),
        ("/output/equalizer#0/left/band0/mode", json!("BWC (BT)")),
        ("/output/equalizer#0/left/band0/gain", json!(37)),
        ("/output/equalizer#0/num-bands", json!(0)),
        ("/output/equalizer#0/num-bands", json!(33)),
        ("/output/equalizer#0/left/band0/mute", json!("false")),
    ] {
        let mut value_tree = base.clone();
        *value_tree.pointer_mut(pointer).unwrap() = value;
        assert!(
            easyeffects::import(
                &value_tree.to_string(),
                &ImportOptions {
                    stereo: true,
                    ..Default::default()
                }
            )
            .is_err(),
            "Accepted {pointer}"
        );
    }
    assert!(
        easyeffects::import("{\"output\":{},\"output\":{}}", &ImportOptions::default()).is_err()
    );
    let mut unknown = base.clone();
    unknown["output"]["equalizer#0"]["oversampling"] = json!(2);
    assert!(easyeffects::import(&unknown.to_string(), &ImportOptions::default()).is_err());
    let before = EqSettings::default();
    let unchanged = before.clone();
    let _ = easyeffects::import("garbage", &ImportOptions::default());
    assert_eq!(before, unchanged);
}
#[test]
fn muted_solo_and_model_defaults_are_preserved() {
    let mut value: Value = serde_json::from_str(&fixture("output-linked-apo.json")).unwrap();
    for side in ["left", "right"] {
        value["output"]["equalizer#0"][side]["band0"]
            .as_object_mut()
            .unwrap()
            .remove("mode");
        value["output"]["equalizer#0"][side]["band0"]["solo"] = json!(true);
        value["output"]["equalizer#0"][side]["band0"]["mute"] = json!(true);
    }
    let eq = imported(&value.to_string(), true);
    assert_eq!(eq.bands[0].mode, BandMode::RlcBt);
    assert!(eq.bands[0].solo && !eq.bands[0].enabled);
    assert_eq!(eq::response_db(&eq, Channel::Left, 1000.0, false), 0.0);
}
#[test]
fn session_conflict_preserves_edits_and_requires_choice() {
    let mut session = Session::default();
    session.receive(state(1)).unwrap();
    session.draft[0].gain.db = -4.0;
    assert_eq!(session.receive(state(2)).unwrap(), RemoteUpdate::Conflict);
    assert_eq!(session.draft[0].gain.db, -4.0);
    assert!(session.begin_apply().is_err());
    session.rebase_edits().unwrap();
    assert_eq!(session.begin_apply().unwrap().request.expected_revision, 2);
    session.reload_saved();
    assert!(!session.dirty());
    assert_eq!(session.draft[0].gain.db, 0.0);
    assert_eq!(session.receive(state(1)).unwrap(), RemoteUpdate::Stale);
    assert_eq!(session.saved.as_ref().unwrap().config.revision, 2);
}
#[test]
fn late_save_keeps_newer_edit_and_applied_is_not_just_saved() {
    let mut session = Session::default();
    session.receive(state(1)).unwrap();
    session.draft[0].gain.db = -4.0;
    let pending = session.begin_apply().unwrap();
    session.draft[0].gain.db = -5.0;
    let mut response = state(2);
    response.config.targets = pending.request.targets.clone();
    response.engine.applied_revision = 1;
    session.acknowledge(&pending, response).unwrap();
    assert_eq!(session.draft[0].gain.db, -5.0);
    assert!(session.dirty());
    assert!(!session.is_applied());
    session.reload_saved();
    assert!(!session.is_applied());
    let mut active = session.saved.clone().unwrap();
    active.engine.applied_revision = 2;
    session.receive(active).unwrap();
    assert!(session.is_applied());
    let mut stalled = session.saved.clone().unwrap();
    stalled.engine.audio_advancing = false;
    session.receive(stalled).unwrap();
    assert!(!session.is_applied());
    let mut failed = session.saved.clone().unwrap();
    failed.engine.status = EnginePhase::Error;
    session.receive(failed).unwrap();
    assert!(!session.is_applied());
    session.disconnect();
    assert!(!session.is_applied());
}
#[test]
fn offline_draft_not_silently_overwritten_on_connection() {
    let mut session = Session::default();
    session.draft[0].gain.db = -6.0;
    assert!(session.dirty());
    assert_eq!(session.receive(state(1)).unwrap(), RemoteUpdate::Conflict);
    assert_eq!(session.draft[0].gain.db, -6.0);
}
#[test]
fn preset_reply_preserves_edits_made_while_loading() {
    let mut session = Session::default();
    session.receive(state(1)).unwrap();
    let original = session.draft.clone();
    let mut loaded = state(2);
    loaded.config.targets[0].gain.db = -6.0;
    session
        .acknowledge_preset(1, &original, loaded.clone())
        .unwrap();
    assert_eq!(session.draft[0].gain.db, -6.0);
    assert!(!session.conflict);
    assert!(!session.dirty());

    let mut edited = Session::default();
    edited.receive(state(1)).unwrap();
    edited.draft[0].gain.db = -3.0;
    edited.acknowledge_preset(1, &original, loaded).unwrap();
    assert_eq!(edited.draft[0].gain.db, -3.0);
    assert!(edited.conflict);
    let before = edited.draft.clone();
    assert!(edited.acknowledge_preset(2, &before, state(4)).is_err());
    assert_eq!(edited.draft, before);
}
#[test]
fn meters_require_current_counters_and_finite_values() {
    let mut target = json!({"id":"mic-1","inputPeak":0.1,"outputPeak":0.05,"rms":0.02,"gainReduction":0,"voice":0.4,"gateOpen":1,"clipped":0,"spectrum":[],"callbacks":1,"processedFrames":112,"suppressionFrames":0,"missedDeadlines":0,"staleResults":0,"inputDrops":0,"outputDrops":0,"latencySamples":1440,"workerFailed":false});
    let decoded: TargetMeter = rc2fx_core::from_json(&target.to_string()).unwrap();
    decoded.validate().unwrap();
    target.as_object_mut().unwrap().remove("workerFailed");
    assert!(rc2fx_core::from_json::<TargetMeter>(&target.to_string()).is_err());
    let mut invalid = decoded.clone();
    invalid.rms = f64::NAN;
    assert!(invalid.validate().is_err());
    // Voice score and gate position are fractions.
    for (voice, gate_open) in [(1.01, 1.), (-0.01, 1.), (0.5, 1.5), (0.5, f64::NAN)] {
        let mut invalid = decoded.clone();
        (invalid.voice, invalid.gate_open) = (voice, gate_open);
        assert!(invalid.validate().is_err(), "{voice} {gate_open}");
    }
}
