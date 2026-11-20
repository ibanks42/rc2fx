//! Device entry point; see `supervisor.rs`.
#[cfg(not(target_os = "linux"))]
compile_error!("rc2fx's mixer crates build for Linux only");
mod supervisor;

mod arguments;

fn main() -> std::process::ExitCode {
    match arguments::parse(std::env::args_os().skip(1)) {
        Ok(arguments::Command::Version) => {
            println!("rc2fx-supervisor {}", env!("CARGO_PKG_VERSION"));
            std::process::ExitCode::SUCCESS
        }
        Ok(arguments::Command::Supervise(options)) => {
            supervisor::run(options);
            std::process::ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("rc2fx-supervisor: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}
