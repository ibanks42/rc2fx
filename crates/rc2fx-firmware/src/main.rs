use rc2fx_firmware::{Catalog, DEFAULT_CATALOG, Model, Profile};
use std::{path::Path, process::ExitCode};

fn read(path: &str) -> Result<Vec<u8>, String> {
    std::fs::read(path).map_err(|e| e.to_string())
}

/// What a profile is good for on its own model, as both listings print it.
fn qualification(profile: &Profile) -> &'static str {
    if profile.model == Model::Duo.id() {
        "experimental; opt-in required, NOT hardware tested"
    } else if profile.runtime.is_some() {
        "qualified"
    } else {
        "NOT qualified (offline-only)"
    }
}

/// Selects the model's runtime profile and checks the launcher, then prints
/// `<profile id> <saved-show path>` for the launcher to read.
fn check(catalog: &str, image: &str, launcher: &str, model: Model) -> Result<(), String> {
    let catalog = Catalog::load(Path::new(catalog))?;
    let image = read(image)?;
    let profile = catalog.select_for_model_with(
        model,
        &image,
        rc2fx_firmware::experimental_duo_from_env(),
    )?;
    profile.verify_launcher(&read(launcher)?)?;
    let runtime = profile
        .runtime
        .as_ref()
        .ok_or("selected profile has no runtime")?;
    println!("{} {}", profile.id, runtime.show_path);
    Ok(())
}

fn run(args: &[String]) -> Result<(), String> {
    match args {
        [command, catalog, image, launcher, model] if command == "check" => {
            check(catalog, image, launcher, Model::parse(model)?)
        }
        [command, catalog, image, launcher] if command == "check-device" => {
            // Read board evidence here as well as in the injected library:
            // refusal must happen before the launcher stops stock audio.
            check(catalog, image, launcher, Model::read_device()?)
        }
        [command, catalog, image, launcher, model] if command == "inspect" => {
            let catalog = Catalog::load(Path::new(catalog))?;
            let profile =
                catalog.inspect_for_model(Model::parse(model)?, &read(image)?, &read(launcher)?)?;
            println!(
                "{}: executable ABI verified; runtime {}",
                profile.id,
                qualification(profile)
            );
            Ok(())
        }
        [] => {
            for profile in Catalog::parse(DEFAULT_CATALOG)?.profiles {
                println!("{}: {}", profile.id, qualification(&profile));
            }
            Ok(())
        }
        _ => Err("usage: rc2fx-firmware [check CATALOG MIXER LAUNCHER MODEL | inspect CATALOG MIXER LAUNCHER MODEL | check-device CATALOG MIXER LAUNCHER]".into()),
    }
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
