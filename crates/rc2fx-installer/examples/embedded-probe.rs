//! Host check of the installer payload embedded in this build: no window, no
//! USB, no network.
//!
//! This is how the embedded payload is qualified on a host: copy this
//! executable into an empty folder and run it there, so nothing beside it can
//! be read. `identity` prints what it carries; `verify <work>` checks a work
//! folder's prepared firmware against it.

use std::path::PathBuf;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("identity") => identity(),
        Some("verify") => verify(args.get(1)),
        Some("duplicate") => duplicate(),
        _ => Err("usage: embedded-probe identity | verify <work> | duplicate".to_owned()),
    };
    if let Err(error) = result {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn identity() -> Result<(), String> {
    let identity = rc2fx_installer::embed::identity().map_err(|e| e.0)?;
    // What this executable carries, found in its own bytes: exactly one
    // envelope, so a payload embedded twice (or not at all) cannot pass.
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let bytes = std::fs::read(&exe).map_err(|e| e.to_string())?;
    let envelope = rc2fx_installer::Envelope::find(&bytes)?;
    let files = envelope.files()?;
    let report = serde_json::json!({
        "identity": identity,
        "executable": exe.file_name().map(|n| n.to_string_lossy().into_owned()),
        "envelope": {
            "valid": 1,
            "bytes": envelope.as_bytes().len(),
            "files": files.len(),
        },
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
    );
    Ok(())
}

/// Negative control: an executable carrying its payload twice must be refused,
/// not resolved to whichever copy is found first.
fn duplicate() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let bytes = std::fs::read(&exe).map_err(|e| e.to_string())?;
    let envelope = rc2fx_installer::Envelope::find(&bytes)?;
    let mut twice = bytes.clone();
    twice.extend_from_slice(envelope.as_bytes());
    match rc2fx_installer::Envelope::find(&twice) {
        Ok(_) => Err("a payload embedded twice was accepted".to_owned()),
        Err(why) => {
            println!("refused as expected: {why}");
            Ok(())
        }
    }
}

fn verify(work: Option<&String>) -> Result<(), String> {
    let work = work.ok_or("usage: embedded-probe verify <work>")?;
    let mut engine = rc2fx_installer::Engine::embedded(PathBuf::from(work));
    let reply = engine
        .handle(rc2fx_installer::Request::Verify)
        .map_err(|e| e.0)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&reply).map_err(|e| e.to_string())?
    );
    Ok(())
}
