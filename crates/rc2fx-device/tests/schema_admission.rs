//! Removed settings must be refused without replacing them with fresh defaults.
#![cfg(unix)]

use rc2fx_core::Config;
use rc2fx_device::store::{Store, StoreError};
use serde_json::{Value, json};
use std::{fs, os::unix::fs::MetadataExt};

#[test]
fn unsupported_saved_settings_are_not_migrated_or_overwritten() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.json");
    let valid = json!({
        "engineGeneration": 7,
        "config": Config::default(),
        "presets": [{
            "id": "kept-preset", "name": "Keep these settings",
            "targets": Config::default().targets,
        }],
    });
    let mut invalid = Vec::<Value>::new();
    for version in [4, 5, 6, 7, 8, 10] {
        let mut old = valid.clone();
        old["config"]["schemaVersion"] = json!(version);
        invalid.push(old);
    }
    for location in ["config", "preset"] {
        for value in ["webrtc", "speex", "removed-strength"] {
            let mut image = valid.clone();
            let target = if location == "config" {
                // Inactive targets must not bypass admission either.
                &mut image["config"]["targets"][0]
            } else {
                &mut image["presets"][0]["targets"][0]
            };
            if value == "removed-strength" {
                target["suppression"]["strength"] = json!(2);
            } else {
                target["suppression"]["algorithm"] = json!(value);
            }
            invalid.push(image);
        }
    }
    let good = serde_json::to_vec_pretty(&valid).unwrap();
    for image in invalid {
        let bytes = serde_json::to_vec_pretty(&image).unwrap();
        fs::write(&path, &bytes).unwrap();
        let before = fs::metadata(&path).unwrap();
        assert!(matches!(
            Store::open(directory.path(), Config::default()),
            Err(StoreError::Invalid(_))
        ));
        assert_eq!(fs::read(&path).unwrap(), bytes);
        let after = fs::metadata(&path).unwrap();
        assert_eq!(after.ino(), before.ino());
        assert_eq!(after.mtime(), before.mtime());
        assert_eq!(after.mtime_nsec(), before.mtime_nsec());

        // Explicit fixture repair proves failed admission released its lock;
        // opening an existing valid state preserves its generation and preset.
        fs::write(&path, &good).unwrap();
        let reopened = Store::open(directory.path(), Config::default()).unwrap();
        assert_eq!(reopened.generation(), 7);
        assert_eq!(reopened.presets()[0].id, "kept-preset");
        assert_eq!(fs::read(&path).unwrap(), good);
        drop(reopened);
    }
}
