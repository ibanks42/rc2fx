//! Shared EQ biquad fixture tests (`fixtures/eq/biquads.json`).
//! Go checks the same file in `desktop/core/eq_fixtures_test.go`.

use rc2fx_core::{Band, BandMode, BandType, eq};
use serde_json::Value;
use std::path::PathBuf;

fn float(case: &Value, field: &str) -> f64 {
    let hex = case[field].as_str().unwrap_or_else(|| panic!("{field}"));
    f64::from_bits(u64::from_str_radix(hex, 16).unwrap())
}

#[test]
fn eq_biquads_match_the_shared_fixture() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/eq/biquads.json");
    let cases: Vec<Value> = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert!(!cases.is_empty());
    for case in &cases {
        let band_type: BandType = serde_json::from_value(case["bandType"].clone()).unwrap();
        let mode: BandMode = serde_json::from_value(case["mode"].clone()).unwrap();
        let band = Band {
            id: "b1".into(),
            enabled: true,
            solo: false,
            band_type,
            frequency: float(case, "frequency"),
            gain: float(case, "gain"),
            q: float(case, "q"),
            mode,
        };
        let got_q = eq::design_q(&band);
        let want_q = float(case, "designQ");
        assert_eq!(
            got_q.to_bits(),
            want_q.to_bits(),
            "designQ for {band:?}: {got_q} != {want_q}"
        );
        let coeffs = eq::coefficients(&band).unwrap();
        for (field, got) in [
            ("b0", coeffs.b0),
            ("b1", coeffs.b1),
            ("b2", coeffs.b2),
            ("a1", coeffs.a1),
            ("a2", coeffs.a2),
        ] {
            let want = float(case, field);
            assert_eq!(
                got.to_bits(),
                want.to_bits(),
                "{field} for {band:?}: {got} != {want}"
            );
        }
    }
}
