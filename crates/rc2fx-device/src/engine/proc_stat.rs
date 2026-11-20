//! Shared Linux `/proc/<pid>/stat` parsing for process identity and resource
//! accounting within `rc2fx-device`.
//!
//! `rc2fx-supervisor` intentionally keeps its own independent parser so the
//! recovery watchdog shares zero code with `rc2fxd`.

use std::{fs, io};

/// Split `/proc/<pid>/stat` into `(pid, fields_after_comm)`.
///
/// The `comm` field is enclosed in parentheses and may itself contain spaces or
/// parentheses, so we locate the first `(` and the last `)`.
pub(crate) fn split_stat(bytes: &[u8]) -> io::Result<(u32, Vec<&str>)> {
    let invalid = || io::Error::other("invalid process stat");
    let text = std::str::from_utf8(bytes).map_err(|_| invalid())?;
    let open = text.find('(').ok_or_else(invalid)?;
    let close = text
        .rfind(')')
        .filter(|close| *close > open)
        .ok_or_else(invalid)?;
    let pid = text[..open].trim().parse::<u32>().map_err(|_| invalid())?;
    let fields: Vec<_> = text[close + 1..].split_whitespace().collect();
    Ok((pid, fields))
}

/// Process start time in clock ticks (field 22 of `/proc/<pid>/stat`, index 19
/// after `comm`), as text, for PID reuse checks.
pub(crate) fn process_start(pid: u32) -> Option<String> {
    let bytes = fs::read(format!("/proc/{pid}/stat")).ok()?;
    let (parsed_pid, fields) = split_stat(&bytes).ok()?;
    if parsed_pid != pid {
        return None;
    }
    fields.get(19).map(|s| (*s).to_owned())
}
