//! The command line `rc2fx.sh launch` starts the supervisor with.
use crate::supervisor::{Options, READY_SECONDS};
use std::{ffi::OsString, path::PathBuf};

pub const USAGE: &str = "rc2fx-supervisor --mixer PID --mixer-start TICKS --script FILE \
--controller FILE --run DIRECTORY --boot DIRECTORY [--ready-seconds N] -- CONTROLLER_ARGUMENTS";

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    /// Also how `launch` proves this binary runs, before it commits to RC2FX.
    Version,
    Supervise(Options),
}

fn number<T: TryFrom<u64>>(value: &OsString, key: &str) -> Result<T, String> {
    value
        .to_str()
        .and_then(|text| text.parse::<u64>().ok())
        .and_then(|number| T::try_from(number).ok())
        .ok_or_else(|| format!("{key} needs a number"))
}

pub fn parse(args: impl Iterator<Item = OsString>) -> Result<Command, String> {
    let mut args = args.peekable();
    if args.peek().and_then(|first| first.to_str()) == Some("--version") {
        return Ok(Command::Version);
    }
    let (mut mixer, mut mixer_start, mut ready_seconds) = (None, None, None);
    let (mut script, mut controller, mut run, mut boot) = (None, None, None, None);
    while let Some(key) = args.next() {
        let key = key.to_str().ok_or("option is not UTF-8")?;
        if key == "--" {
            break;
        }
        let value = args
            .next()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| format!("missing value for {key}"))?;
        match key {
            "--mixer" => mixer = Some(number(&value, key)?),
            "--mixer-start" => mixer_start = Some(number(&value, key)?),
            "--ready-seconds" => ready_seconds = Some(number(&value, key)?),
            "--script" => script = Some(PathBuf::from(value)),
            "--controller" => controller = Some(PathBuf::from(value)),
            "--run" => run = Some(PathBuf::from(value)),
            "--boot" => boot = Some(PathBuf::from(value)),
            _ => return Err(format!("unknown option {key}\n{USAGE}")),
        }
    }
    let required = |name: &str| format!("{name} is required\n{USAGE}");
    let options = Options {
        mixer: mixer.ok_or_else(|| required("--mixer"))?,
        mixer_start: mixer_start.ok_or_else(|| required("--mixer-start"))?,
        script: script.ok_or_else(|| required("--script"))?,
        controller: controller.ok_or_else(|| required("--controller"))?,
        run: run.ok_or_else(|| required("--run"))?,
        boot: boot.ok_or_else(|| required("--boot"))?,
        controller_args: args.collect(),
        ready_seconds: ready_seconds.unwrap_or(READY_SECONDS),
    };
    let paths = [
        &options.script,
        &options.controller,
        &options.run,
        &options.boot,
    ];
    if paths.iter().any(|path| !path.is_absolute()) {
        return Err("paths must be absolute".into());
    }
    Ok(Command::Supervise(options))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(args: &[&str]) -> Result<Command, String> {
        parse(args.iter().map(OsString::from))
    }

    const FULL: [&str; 12] = [
        "--mixer",
        "12",
        "--mixer-start",
        "34",
        "--script",
        "/s",
        "--controller",
        "/c",
        "--run",
        "/r",
        "--boot",
        "/b",
    ];

    #[test]
    fn everything_after_the_separator_belongs_to_the_controller() {
        let args = [&FULL[..], &["--", "--mixer", "--data", "/x y"]].concat();
        let Ok(Command::Supervise(options)) = parsed(&args) else {
            panic!("did not parse");
        };
        assert_eq!((options.mixer, options.mixer_start), (12, 34));
        assert_eq!(options.controller_args, ["--mixer", "--data", "/x y"]);
        assert_eq!(options.ready_seconds, READY_SECONDS);
    }

    #[test]
    fn a_missing_or_unknown_option_is_refused() {
        assert!(
            parsed(&FULL[2..])
                .unwrap_err()
                .contains("--mixer is required")
        );
        let args = [&FULL[..], &["--what", "1"]].concat();
        assert!(parsed(&args).unwrap_err().contains("unknown option --what"));
    }

    #[test]
    fn numbers_and_paths_are_checked() {
        let mut args = FULL;
        args[1] = "99999999999";
        assert!(
            parsed(&args)
                .unwrap_err()
                .contains("--mixer needs a number")
        );
        let mut args = FULL;
        args[5] = "relative";
        assert!(parsed(&args).unwrap_err().contains("absolute"));
    }

    #[test]
    fn version_needs_nothing_else() {
        assert_eq!(parsed(&["--version"]), Ok(Command::Version));
    }
}
