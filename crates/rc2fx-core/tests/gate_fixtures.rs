//! The shared gate shapes in `fixtures/gate`. The desktop's Go
//! (`desktop/core/gate_fixtures_test.go`) checks its own `GateShapeForStrength`
//! against the same file. `fixtures/gate/generate.py` writes it.
//!
//! The two fields that take a power (reduction and release) go through the C
//! library's `pow`, whose last bit differs between glibc versions, so they may
//! land up to two ulps from the fixture, as on the Go side. Every other field
//! must match exactly.

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
                "{field} at strength {strength}: {got} != {want}"
            );
        }
    }
}
