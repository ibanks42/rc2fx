//! The native library's launch plan: every mixer path it binds, with the saved
//! settings for each. Generated from the store at every launch, so the bound
//! paths always match the controller's saved configuration.
use crate::catalog::{PhysicalKind, mixer_port, physical_paths_for};
use rc2fx_core::Config;
use rc2fx_firmware::Model;
use serde_json::{Value, json};

/// The plan for `config`. Binding is fixed for the life of the mixer process,
/// so every catalog path is bound: selected paths are processed and the rest
/// pass audio through untouched until enabled. The worker block is omitted, so
/// the library applies its production suppression policy (`PRODUCTION_WORKER`:
/// SCHED_RR 5, pinned to CPU 0).
pub fn plan(config: &Config) -> Result<Value, String> {
    plan_for_model(config, Model::ProIi)
}

pub fn plan_for_model(config: &Config, model: Model) -> Result<Value, String> {
    let catalog = physical_paths_for(model);
    if (model == Model::Duo
        && (config.targets.len() != catalog.len()
            || config
                .targets
                .iter()
                .any(|t| !catalog.iter().any(|p| p.id == t.id))))
        || (model == Model::ProIi && config.targets.iter().any(|t| t.id.starts_with("duo-")))
    {
        return Err("saved settings belong to a different device model".into());
    }
    let mut paths = Vec::with_capacity(catalog.len());
    for path in catalog {
        let target = config
            .targets
            .iter()
            .find(|target| target.id == path.id)
            .ok_or_else(|| format!("saved settings have no {} path", path.id))?;
        if usize::from(target.channels) != path.channels.len() {
            return Err(format!("{} has {} channels", path.id, target.channels));
        }
        let ports: Vec<_> = path
            .channels
            .iter()
            .map(|channel| mixer_port(path, channel))
            .collect();
        let processed = config.selected_targets.iter().any(|id| id == path.id);
        // Heard only on the mixer's hardware outputs, which jackd writes at
        // the start of the next cycle: processed off the callback, one frame
        // later.
        let hardware = path.kind == PhysicalKind::Output
            && path
                .channels
                .iter()
                .all(|channel| channel[1].starts_with("system:playback_"));
        paths.push(json!({
            "direction": if path.kind == PhysicalKind::Output { "output" } else { "input" },
            "ports": ports,
            "target": target,
            "mode": if processed { "processed" } else { "passthrough" },
            "hardware": hardware,
        }));
    }
    Ok(json!({
        "version": 1,
        "sampleRate": 48_000,
        "frames": 112,
        "paths": paths,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binds_every_path_and_processes_only_selected_ones() {
        let config = Config {
            selected_targets: vec!["mic-1".into(), "headphones-2".into()],
            ..Config::default()
        };
        let plan = plan(&config).unwrap();
        let paths = plan["paths"].as_array().unwrap();
        assert_eq!(paths.len(), 25);
        assert!(plan.get("worker").is_none());
        for path in paths {
            let id = path["target"]["id"].as_str().unwrap();
            let expected = if config.selected_targets.iter().any(|s| s == id) {
                "processed"
            } else {
                "passthrough"
            };
            assert_eq!(path["mode"], expected, "{id}");
        }
        let port =
            |id: &str| paths.iter().find(|p| p["target"]["id"] == id).unwrap()["ports"].clone();
        assert_eq!(port("mic-1"), json!(["in_4"]));
        assert_eq!(port("headphones-1"), json!(["out_3", "out_4"]));
        assert_eq!(port("send-main"), json!(["out_15", "out_16"]));
        assert_eq!(port("send-secondary"), json!(["out_19", "out_20"]));
        assert_eq!(port("usb-game"), json!(["in_13", "in_14"]));
        assert_eq!(port("usb-music"), json!(["in_15", "in_16"]));
        assert_eq!(port("usb-a"), json!(["in_17", "in_18"]));
        assert_eq!(port("usb-b"), json!(["in_19", "in_20"]));
        let hardware =
            |id: &str| paths.iter().find(|p| p["target"]["id"] == id).unwrap()["hardware"].clone();
        assert_eq!(hardware("headphones-1"), json!(true));
        assert_eq!(hardware("monitors"), json!(true));
        assert_eq!(hardware("send-chat"), json!(false));
        assert_eq!(hardware("send-main"), json!(false));
        assert_eq!(hardware("send-secondary"), json!(false));
        assert_eq!(hardware("usb-game"), json!(false));
        assert_eq!(hardware("mic-1"), json!(false));
        assert_eq!(port("bluetooth"), json!(["in_5", "in_6"]));
        let ports: Vec<_> = paths
            .iter()
            .flat_map(|path| path["ports"].as_array().unwrap().clone())
            .collect();
        let mut unique = ports.clone();
        unique.sort_by_key(|port| port.as_str().unwrap().to_owned());
        unique.dedup();
        assert_eq!(unique.len(), ports.len(), "a buffer is bound twice");
    }

    #[test]
    fn the_plan_carries_each_paths_lookahead() {
        // The worker is built from this field, so it must survive the plan.
        let mut config = Config::default();
        config.targets[0].suppression.enabled = true;
        config.targets[0].suppression.select_lookahead(1);
        config.validate().unwrap();
        let value = plan(&config).unwrap();
        let target = &value["paths"][0]["target"];
        assert_eq!(target["id"], "mic-1");
        assert_eq!(target["suppression"]["lookahead"], 1);
        // And the reverse: what the plan says is what the settings mean.
        let travelled: rc2fx_core::Target = serde_json::from_value(target.clone()).unwrap();
        assert_eq!(travelled, config.targets[0]);
        // A path that does not set it keeps LA0.
        config.targets[1].suppression.lookahead = 0;
        let value = plan(&config).unwrap();
        assert_eq!(value["paths"][1]["target"]["suppression"]["lookahead"], 0);
    }

    #[test]
    fn duo_plan_uses_a_separate_map_and_defaults_to_passthrough() {
        let config = crate::catalog::default_config(Model::Duo);
        config.validate().unwrap();
        let value = plan_for_model(&config, Model::Duo).unwrap();
        let paths = value["paths"].as_array().unwrap();
        assert_eq!(paths.len(), 23);
        let ports =
            |id: &str| paths.iter().find(|p| p["target"]["id"] == id).unwrap()["ports"].clone();
        assert_eq!(ports("duo-analog-1"), json!(["in_1"]));
        assert_eq!(ports("duo-analog-2"), json!(["in_2"]));
        assert_eq!(ports("duo-headset-mic"), json!(["in_3"]));
        assert_eq!(ports("duo-headphones-1"), json!(["out_3", "out_4"]));
        assert_eq!(ports("duo-headphones-2"), json!(["out_1", "out_2"]));
        assert_eq!(ports("duo-headset"), json!(["out_8", "out_7"]));
        assert_eq!(ports("duo-monitors"), json!(["out_5", "out_6"]));
        assert_eq!(ports("duo-send-main"), json!(["out_15", "out_16"]));
        assert_eq!(ports("duo-send-chat"), json!(["out_17", "out_18"]));
        assert_eq!(ports("duo-send-secondary"), json!(["out_19", "out_20"]));
        assert_eq!(ports("duo-usb-game"), json!(["in_13", "in_14"]));
        assert_eq!(ports("duo-usb-music"), json!(["in_15", "in_16"]));
        assert_eq!(ports("duo-usb-a"), json!(["in_17", "in_18"]));
        assert_eq!(ports("duo-usb-b"), json!(["in_19", "in_20"]));
        for path in paths {
            let id = path["target"]["id"].as_str().unwrap();
            if id.starts_with("duo-send-") {
                assert_eq!(path["hardware"], false);
            }
            if id == "duo-monitors" {
                assert_eq!(path["hardware"], true);
            }
        }
        assert!(paths.iter().all(|p| p["mode"] == "passthrough"));
        let mut used = std::collections::HashSet::new();
        for path in paths {
            for port in path["ports"].as_array().unwrap() {
                assert!(used.insert(port.as_str().unwrap()));
            }
        }
        assert!(plan_for_model(&Config::default(), Model::Duo).is_err());
        assert!(plan_for_model(&config, Model::ProIi).is_err());
        let mut foreign = config;
        foreign.targets.push(Config::default().targets[0].clone());
        assert!(plan_for_model(&foreign, Model::Duo).is_err());
    }

    #[test]
    fn default_plans_bind_every_unique_catalog_path_and_stay_passthrough() {
        for (model, config) in [
            (Model::ProIi, Config::default()),
            (Model::Duo, crate::catalog::default_config(Model::Duo)),
        ] {
            let catalog = crate::catalog::physical_paths_for(model);
            let mut ids = std::collections::HashSet::new();
            let mut ports = std::collections::HashSet::new();
            for path in catalog {
                assert!(ids.insert(path.id), "duplicate path {}", path.id);
                for channel in path.channels {
                    assert!(
                        ports.insert(crate::catalog::mixer_port(path, channel)),
                        "duplicate mixer port in {}",
                        path.id
                    );
                }
            }
            let virtual_ids = if model == Model::ProIi {
                ["usb-game", "usb-music", "usb-a", "usb-b"]
            } else {
                ["duo-usb-game", "duo-usb-music", "duo-usb-a", "duo-usb-b"]
            };
            for id in virtual_ids {
                assert_eq!(
                    catalog.iter().find(|path| path.id == id).unwrap().kind,
                    PhysicalKind::Playback,
                    "{id}"
                );
            }
            if model == Model::ProIi {
                for id in ["send-main", "send-secondary"] {
                    assert_eq!(
                        catalog.iter().find(|path| path.id == id).unwrap().kind,
                        PhysicalKind::Output,
                        "{id}"
                    );
                }
            }

            let value = plan_for_model(&config, model).unwrap();
            let paths = value["paths"].as_array().unwrap();
            assert_eq!(paths.len(), catalog.len());
            assert_eq!(paths.len(), config.targets.len());
            for path in paths {
                assert_eq!(path["mode"], "passthrough");
                if path["target"]["kind"] == "playback" {
                    assert_eq!(path["target"]["suppression"]["enabled"], false);
                }
            }

            let mut catalog_ids: Vec<_> = catalog.iter().map(|path| path.id).collect();
            let mut target_ids: Vec<_> = config
                .targets
                .iter()
                .map(|target| target.id.as_str())
                .collect();
            catalog_ids.sort_unstable();
            target_ids.sort_unstable();
            assert_eq!(target_ids, catalog_ids);
        }

        let duo = crate::catalog::default_config(Model::Duo);
        for (id, label) in [
            ("duo-usb-game", "Game"),
            ("duo-usb-music", "Music"),
            ("duo-usb-a", "Virtual A"),
            ("duo-usb-b", "Virtual B"),
        ] {
            assert_eq!(
                duo.targets
                    .iter()
                    .find(|target| target.id == id)
                    .unwrap()
                    .name,
                label
            );
        }
    }

    #[test]
    fn refuses_settings_missing_a_bound_path() {
        let mut config = Config::default();
        config.targets.retain(|target| target.id != "bluetooth");
        assert!(plan(&config).unwrap_err().contains("bluetooth"));
    }
}
