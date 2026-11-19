//! Device entry point. No native work, storage mutation, or threads on --help.
#[path = "rc2fxd/native.rs"]
mod native;

fn main() -> std::process::ExitCode {
    match native::run(std::env::args_os().skip(1)) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("rc2fxd: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}
