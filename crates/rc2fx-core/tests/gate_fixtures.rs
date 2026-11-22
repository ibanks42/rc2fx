//! Live Go <-> Rust voice-gate curve agreement test: runs Go's
//! `core.GateShapeForStrength` via `desktop/internal/interop` and checks that
//! Rust matches bit-for-bit on linear fields and within 2 ULPs on pow fields.

use rc2fx_core::GateShape;
use serde_json::Value;
use std::path::PathBuf;
use std::process::Command;

fn float(case: &Value, field: &str) -> f64 {
    let hex = case[field].as_str().unwrap_or_else(|| panic!("{field}"));
    f64::from_bits(u64::from_str_radix(hex, 16).unwrap())
}

#[test]
fn gate_shapes_match_go_desktop() {
    let desktop = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../desktop");
    let output = Command::new("go")
        .args(["run", "./internal/interop", "eq-gate"])
        .current_dir(&desktop)
        .output()
        .expect("spawn `go run ./internal/interop eq-gate`");
    assert!(
        output.status.success(),
        "go interop eq-gate failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let doc: Value = serde_json::from_slice(&output.stdout).unwrap();
    let cases = doc["gate"].as_array().unwrap();
    assert!(!cases.is_empty());
    for case in cases {
        let strength = float(case, "strength");
        let shape = GateShape::for_strength(strength);
        for (field, got, ulps) in [
            ("threshold", shape.threshold, 0),
            ("closeThreshold", shape.close_threshold, 0),
            ("reductionDb", shape.reduction_db, 2),
            ("attackMs", shape.attack_ms, 0),
            ("holdMs", shape.hold_ms, 0),
            ("releaseMs", shape.release_ms, 2),
        ] {
            let want = float(case, field);
            assert!(
                got.to_bits().abs_diff(want.to_bits()) <= ulps,
                "{field} at strength {strength}: rust {got} != go {want}"
            );
        }
    }
}
