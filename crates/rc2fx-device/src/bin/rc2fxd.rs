//! Device entry point. No native work, storage mutation, or threads on --help.
#[cfg(target_os = "linux")]
#[path = "rc2fxd/native.rs"]
mod linux;

fn main() -> std::process::ExitCode {
    #[cfg(target_os = "linux")]
    match linux::run(std::env::args_os().skip(1)) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("rc2fxd: {error}");
            std::process::ExitCode::FAILURE
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        eprintln!("rc2fxd runs on the Linux mixer; use rc2fx for the desktop controller");
        std::process::ExitCode::FAILURE
    }
}
