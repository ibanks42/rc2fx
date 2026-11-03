use rc2fx_firmware::{Catalog, DEFAULT_CATALOG, Model};
use std::{path::Path, process::ExitCode};

fn run(args: &[String]) -> Result<(), String> {
    match args {
        [command, catalog, image, launcher, model] if command == "check" || command == "inspect" => {
            let catalog = Catalog::load(Path::new(catalog))?;
            let model = Model::parse(model)?;
            let image = std::fs::read(image).map_err(|e| e.to_string())?;
            let launcher = std::fs::read(launcher).map_err(|e| e.to_string())?;
            if command == "inspect" {
                let profile = catalog.inspect_for_model(model, &image, &launcher)?;
                println!("{}: executable ABI verified; runtime {}", profile.id,
                    if model == Model::Duo { "experimental; NOT hardware tested" } else if profile.runtime.is_some() { "qualified" } else { "NOT qualified (offline-only)" });
            } else {
                let profile = catalog.select_for_model_with(model, &image, rc2fx_firmware::experimental_duo_from_env())?;
                profile.verify_launcher(&launcher)?;
                println!("{} {}", profile.id, profile.runtime.as_ref().unwrap().show_path);
            }
        }
        [command, catalog, image, launcher] if command == "check-device" => {
            // Read board evidence here as well as in the injected library:
            // refusal must happen before the launcher stops stock audio.
            let model = Model::read_device()?;
            run(&["check".into(), catalog.clone(), image.clone(), launcher.clone(), model.id().into()])?;
        }
        [command, catalog, image, launcher] if command == "check" => {
            let catalog = Catalog::load(Path::new(catalog))?;
            let image = std::fs::read(image).map_err(|e| e.to_string())?;
            let profile = catalog.select(&image)?;
            profile.verify_launcher(&std::fs::read(launcher).map_err(|e| e.to_string())?)?;
            println!(
                "{} {}",
                profile.id,
                profile.runtime.as_ref().unwrap().show_path
            );
        }
        [] => {
            let catalog = Catalog::parse(DEFAULT_CATALOG)?;
            for profile in catalog.profiles {
                println!(
                    "{}: {}",
                    profile.id,
                    if profile.model == Model::Duo.id() {
                        "experimental; opt-in required, NOT hardware tested"
                    } else if profile.runtime.is_some() {
                        "qualified"
                    } else {
                        "unqualified"
                    }
                );
            }
        }
        _ => return Err("usage: rc2fx-firmware [check CATALOG MIXER LAUNCHER [MODEL] | inspect CATALOG MIXER LAUNCHER MODEL | check-device CATALOG MIXER LAUNCHER]".into()),
    }
    Ok(())
}

fn main() -> ExitCode {
    match run(&std::env::args().skip(1).collect::<Vec<_>>()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("firmware profile refused: {error}");
            ExitCode::FAILURE
        }
    }
}
