//! Live Go <-> Rust EQ biquad agreement test: runs Go's `core.DesignQ` and
//! `core.DesignCoefficients` via `desktop/internal/interop` and checks that
//! Rust matches within libm/Go math ULP bounds across every BandType and BandMode.

use rc2fx_core::{Band, BandMode, BandType, eq};
use serde_json::Value;
use std::path::PathBuf;
use std::process::Command;

fn float(case: &Value, field: &str) -> f64 {
    let hex = case[field].as_str().unwrap_or_else(|| panic!("{field}"));
    f64::from_bits(u64::from_str_radix(hex, 16).unwrap())
}

fn check_ulp(label: &str, got: f64, want: f64, max_ulp: u64) {
    if got == want {
        return;
    }
    let (gb, wb) = (got.to_bits(), want.to_bits());
    assert!(
        (gb >> 63) == (wb >> 63) && gb.abs_diff(wb) <= max_ulp,
        "{label}: rust {got} ({gb:016x}) != go {want} ({wb:016x})"
    );
}

#[test]
fn eq_biquads_match_go_desktop() {
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
    let cases = doc["eq"].as_array().unwrap();
    assert!(!cases.is_empty());
    for case in cases {
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
        check_ulp(
            &format!("{band:?} designQ"),
            eq::design_q(&band),
            float(case, "designQ"),
            4,
        );
        let coeffs = eq::coefficients(&band).unwrap();
        for (field, got) in [
            ("b0", coeffs.b0),
            ("b1", coeffs.b1),
            ("b2", coeffs.b2),
            ("a1", coeffs.a1),
            ("a2", coeffs.a2),
        ] {
            check_ulp(&format!("{band:?} {field}"), got, float(case, field), 16);
        }
    }
}
