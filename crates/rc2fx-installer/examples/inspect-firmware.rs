//! Read-only official firmware audit; no device or extracted rootfs required.
use rc2fx_firmware::Model;
use std::{path::Path, process::ExitCode};

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let [model, archive] = args.as_slice() else {
        return Err("usage: inspect-firmware MODEL OS.tar.gz".into());
    };
    let model = Model::parse(model)?;
    let report = rc2fx_installer::inspection::inspect_archive(model, Path::new(archive))?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("inspection refused: {e}");
            ExitCode::FAILURE
        }
    }
}
