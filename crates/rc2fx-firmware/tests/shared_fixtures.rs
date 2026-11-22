//! Shared firmware fixtures also run by the Go desktop port (`desktop/firmware`).
//! USB identities in `models.json` are desktop-only (the mixer identifies itself
//! from board files), so only Go checks those.
mod common;

use rc2fx_firmware::{Catalog, Model};
use serde_json::Value;
use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn fixtures() -> PathBuf {
    root().join("fixtures/firmware")
}

fn json(path: PathBuf) -> Value {
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
}

fn list<'a>(value: &'a Value, key: &str) -> &'a Vec<Value> {
    let list = value[key].as_array().unwrap();
    assert!(!list.is_empty(), "no {key} cases");
    list
}

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap()
}

#[test]
fn models_match() {
    let fixtures = json(fixtures().join("models.json"));
    let models = list(&fixtures, "models");
    assert_eq!(models.len(), Model::ALL.len());
    for (model, want) in Model::ALL.into_iter().zip(models) {
        assert_eq!(model.id(), text(want, "id"));
        assert_eq!(model.hostname(), text(want, "hostname"));
        assert_eq!(model.board_dtb(), text(want, "boardDtb"));
        assert_eq!(model.route_layout(), text(want, "routeLayout"));
        assert_eq!(model.source_enum(), text(want, "sourceEnum"));
        assert_eq!(
            Some(model.runtime_enabled()),
            want["runtimeEnabled"].as_bool()
        );
    }
    for case in list(&fixtures, "boards") {
        let (hostname, extlinux) = (text(case, "hostname"), text(case, "extlinux"));
        let got = Model::from_board(hostname, extlinux);
        match case["model"].as_str() {
            Some(model) => assert_eq!(got.map(Model::id), Ok(model), "{hostname:?} {extlinux:?}"),
            None => assert_eq!(
                got.unwrap_err(),
                text(case, "error"),
                "{hostname:?} {extlinux:?}"
            ),
        }
    }
    for case in list(&fixtures, "parse") {
        let id = text(case, "id");
        assert_eq!(
            Model::parse(id).ok().map(Model::id),
            case["model"].as_str(),
            "{id:?}"
        );
    }
}

#[test]
fn catalog_fixture_base_parses_independently() {
    // fixtures/firmware/catalogs/base.json is intentionally committed as a
    // frozen copy separate from config/firmware-profiles.json so fixture edits
    // do not change when config/firmware-profiles.json gains a profile.
    let base = std::fs::read_to_string(fixtures().join("catalogs/base.json")).unwrap();
    Catalog::parse(&base).expect("catalogs/base.json must parse as a valid catalog");
}

#[test]
fn catalog_fixture_bases_parse_equally() {
    let base_text = std::fs::read_to_string(fixtures().join("catalogs/base.json")).unwrap();
    let compact_text =
        std::fs::read_to_string(fixtures().join("catalogs/compact-reencoded.json")).unwrap();
    let base = Catalog::parse(&base_text).expect("catalogs/base.json must parse");
    let compact =
        Catalog::parse(&compact_text).expect("catalogs/compact-reencoded.json must parse");
    assert_eq!(
        format!("{base:?}"),
        format!("{compact:?}"),
        "catalogs/base.json and catalogs/compact-reencoded.json must parse identically"
    );
}

#[test]
fn catalogs_parse_the_same_way() {
    let dir = fixtures();
    let fixtures = json(dir.join("catalogs.json"));
    let default_base = text(&fixtures, "defaultBase");
    for case in list(&fixtures, "cases") {
        let name = text(case, "name");
        let catalog = common::resolve_case_text(&dir, default_base, case);
        let parsed = Catalog::parse(&catalog);
        let expect = &case["expect"];
        if expect["ok"].as_bool().unwrap() {
            assert!(parsed.is_ok(), "{name}: {:?}", parsed.err());
        } else {
            let error = parsed
                .err()
                .unwrap_or_else(|| panic!("{name} was accepted"));
            let want = text(expect, "error");
            if want != "*" {
                assert_eq!(error, want, "{name}");
            }
        }
    }
}

#[test]
fn executables_are_selected_the_same_way() {
    let dir = fixtures().join("select");
    let fixtures = json(dir.join("cases.json"));
    let default_base = text(&fixtures, "defaultBase");
    let read = |name: &str| std::fs::read(dir.join(name)).unwrap();
    for case in list(&fixtures, "cases") {
        let name = text(case, "name");
        let catalog_text = common::resolve_case_text(&dir, default_base, case);
        let catalog = Catalog::parse(&catalog_text).unwrap_or_else(|e| panic!("{name}: {e}"));
        let image = read(text(case, "image"));
        let model = case["model"].as_str().map(|id| Model::parse(id).unwrap());
        let experimental = case["experimentalDuo"].as_bool().unwrap_or(false);
        let op = text(case, "op");
        let result = match op {
            // The image verification every selection ends with.
            "select" => {
                assert_eq!(catalog.profiles.len(), 1, "{name}");
                catalog.profiles[0]
                    .verify_image(&image)
                    .map(|()| catalog.profiles[0].id.clone())
            }
            "selectForModel" => catalog
                .select_for_model_with(model.unwrap(), &image, experimental)
                .map(|p| p.id.clone()),
            "inspectForModel" => catalog
                .inspect_for_model(model.unwrap(), &image, &read(text(case, "launcher")))
                .map(|p| p.id.clone()),
            "verifyLauncher" => catalog.profiles[0]
                .verify_launcher(&read(text(case, "launcher")))
                .map(|()| catalog.profiles[0].id.clone()),
            other => panic!("unknown op {other}"),
        };
        let expect = &case["expect"];
        match expect["profile"].as_str() {
            Some(profile) => assert_eq!(result.as_deref(), Ok(profile), "{name}"),
            None => assert_eq!(result.unwrap_err(), text(expect, "error"), "{name}"),
        }
    }
}
