//! Device entry point; see `supervisor.rs`.
#[cfg(target_os = "linux")]
mod supervisor;

#[cfg(target_os = "linux")]
mod arguments;

fn main() -> std::process::ExitCode {
    #[cfg(target_os = "linux")]
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
    #[cfg(not(target_os = "linux"))]
    {
        eprintln!("rc2fx-supervisor runs on the Linux mixer");
        std::process::ExitCode::FAILURE
    }
}
