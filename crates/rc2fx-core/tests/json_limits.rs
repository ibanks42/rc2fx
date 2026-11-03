use rc2fx_core::{from_json, from_json_limited};
use serde_json::Value;

#[test]
fn explicit_limits_retain_duplicate_depth_and_trailing_guards() {
    assert!(from_json_limited::<Value>("[1,2]", 5, 3).is_ok());
    assert!(from_json_limited::<Value>("[1,2]", 4, 3).is_err());
    assert!(from_json_limited::<Value>("[1,2]", 5, 2).is_err());
    assert!(from_json_limited::<Value>(r#"{"a":{"x":1,"x":2}}"#, 100, 100).is_err());
    assert!(from_json_limited::<Value>("[] []", 100, 100).is_err());
    let deep = format!("{}0{}", "[".repeat(65), "]".repeat(65));
    assert!(from_json_limited::<Value>(&deep, 1000, 1000).is_err());
}

#[test]
fn http_default_limits_remain_unchanged() {
    let padded = format!("0{}", " ".repeat(512 * 1024));
    assert!(from_json::<Value>(&padded).is_err());
    assert!(from_json_limited::<Value>(&padded, 1024 * 1024, 200_000).is_ok());
    let values = format!("[{}0]", "0,".repeat(200_000));
    assert!(from_json::<Value>(&values).is_err());
    assert!(from_json_limited::<Value>(&values, 512 * 1024, 400_000).is_ok());
}

#[test]
fn typed_storage_parser_preserves_exact_numeric_and_structural_acceptance() {
    #[derive(Debug, PartialEq, serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Numeric {
        integer: u64,
        signed: i64,
        float: f64,
        values: Vec<Option<f64>>,
    }
    let fixtures = [
        r#"{"integer":18446744073709551615,"signed":-9223372036854775808,"float":-0.0,"values":[null,0,1.25,1e-300,1e300]}"#,
        r#"{"integer":9007199254740993,"signed":0,"float":9007199254740993,"values":[]}"#,
        r#"{"integer":1.0,"signed":0,"float":1,"values":[]}"#,
        r#"{"integer":-1,"signed":0,"float":1,"values":[]}"#,
        r#"{"integer":1,"signed":9223372036854775808,"float":1,"values":[]}"#,
        r#"{"integer":1,"signed":0,"float":1e400,"values":[]}"#,
        r#"{"integer":1,"signed":0,"float":1,"values":null}"#,
        r#"{"integer":1,"signed":0,"float":1}"#,
        r#"{"integer":1,"signed":0,"float":1,"values":[],"other":1}"#,
        r#"{"integer":1,"signed":0,"float":1,"values":[],"\u0069nteger":2}"#,
    ];
    for text in fixtures {
        let tree = from_json::<Numeric>(text);
        let direct = from_json_limited::<Numeric>(text, 512 * 1024, 200_000);
        assert_eq!(tree.is_ok(), direct.is_ok(), "{text}");
        if let (Ok(tree), Ok(direct)) = (tree, direct) {
            assert_eq!(tree, direct, "{text}");
            assert_eq!(tree.float.to_bits(), direct.float.to_bits());
            for (a, b) in tree.values.iter().zip(&direct.values) {
                assert_eq!(a.map(f64::to_bits), b.map(f64::to_bits));
            }
        }
    }
}
