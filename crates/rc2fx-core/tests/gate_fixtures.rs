//! The shared gate shapes in `fixtures/gate`. The desktop's Go
//! (`desktop/core/gate_fixtures_test.go`) checks its own `GateShapeForStrength`
//! against the same file, so both sides draw and run the same gate bit for
//! bit. `fixtures/gate/generate.py` writes it.

use rc2fx_core::GateShape;
use serde_json::Value;
use std::path::PathBuf;

fn float(case: &Value, field: &str) -> f64 {
    let hex = case[field].as_str().unwrap_or_else(|| panic!("{field}"));
    f64::from_bits(u64::from_str_radix(hex, 16).unwrap())
}

#[test]
fn gate_shapes_match_the_shared_fixture() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/gate/shapes.json");
    let cases: Vec<Value> = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert!(!cases.is_empty());
    for case in &cases {
        let strength = float(case, "strength");
        let shape = GateShape::for_strength(strength);
        for (field, got) in [
            ("threshold", shape.threshold),
            ("closeThreshold", shape.close_threshold),
            ("reductionDb", shape.reduction_db),
            ("attackMs", shape.attack_ms),
            ("holdMs", shape.hold_ms),
            ("releaseMs", shape.release_ms),
        ] {
            let want = float(case, field);
            assert_eq!(
                got.to_bits(),
                want.to_bits(),
                "{field} at strength {strength}: {got} != {want}"
            );
        }
    }
}
