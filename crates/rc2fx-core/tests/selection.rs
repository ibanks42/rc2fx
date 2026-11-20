use rc2fx_core::*;
use serde_json::json;

fn ids(values: &[&str]) -> Vec<String> {
    values.iter().map(|s| s.to_string()).collect()
}

/// Strict parsing and validation of saved settings, as the mixer's store
/// reads them.
fn config_from_json(text: &str) -> rc2fx_core::Result<Config> {
    let config: Config = from_json(text)?;
    config.validate()?;
    Ok(config)
}

#[test]
fn schema_five_requires_selection_without_a_legacy_default() {
    let c = Config::default();
    assert_eq!(c.schema_version, SCHEMA_VERSION);
    assert!(c.selected_targets.is_empty());
    let mut wire = serde_json::to_value(&c).unwrap();
    wire.as_object_mut().unwrap().remove("selectedTargets");
    assert!(config_from_json(&wire.to_string()).is_err());
    wire = serde_json::to_value(&c).unwrap();
    wire["schemaVersion"] = json!(3);
    assert!(config_from_json(&wire.to_string()).is_err());
}
#[test]
fn selection_validates_known_unique_targets_without_a_per_kind_limit() {
    let c = Config::default();
    for selected in [&["mic-1", "mic-1"][..], &["unknown"]] {
        assert!(c.validate_selection(&ids(selected)).is_err());
    }
    assert!(c.validate_selection(&[]).is_ok());
    // The connected device, not the generic settings model, determines which
    // paths exist and whether their combined allocation fits its hardware.
    for selected in [
        &["mic-1", "analog-1"][..],
        &["usb-main", "usb-chat", "usb-secondary"],
        &["send-chat", "headphones-1"],
    ] {
        assert!(c.validate_selection(&ids(selected)).is_ok());
    }
    assert_eq!(
        c.canonical_selection(&ids(&["send-chat", "mic-1"]))
            .unwrap(),
        ids(&["mic-1", "send-chat"])
    );
}
