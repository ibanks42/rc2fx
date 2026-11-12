//! `mixer xruns [SECONDS]`: what the JACK error recorder saw while you watched.
//!
//! The recorder (`crates/rc2fx-native/jackerr`) is preloaded into the mixer's
//! jackd at every RC2FX start and keeps a ring of every `jack_error` call in
//! RAM. This command checks that it is the live server's, takes a snapshot of
//! the ring with the bundle's `rc2fx_jackerr_snapshot`, waits SECONDS, takes
//! another, and prints the records that arrived in between. `0` prints
//! everything the ring still holds for this jackd instead. Nothing on the
//! mixer is changed; the snapshots live in a private /run directory that is
//! removed afterwards. It runs a program there, so it arms power recovery.
use crate::{Result, mixer::Mixer};
use std::{collections::BTreeMap, io::Read};

pub const DEFAULT_SECONDS: u32 = 60;
const MAX_SECONDS: u32 = 3600;

pub fn run(mixer: &Mixer, args: &[&str]) -> Result {
    const USAGE: &str = "usage: mixer xruns [SECONDS] (0: everything recorded so far)";
    let seconds = match args {
        [] => DEFAULT_SECONDS,
        [value] => value.parse().map_err(|_| USAGE.to_string())?,
        _ => return Err(USAGE.into()),
    };
    if seconds > MAX_SECONDS {
        return Err(format!("SECONDS must be 0–{MAX_SECONDS}"));
    }
    let mixer = mixer.arm_power_recovery("mixer xruns")?;
    if seconds > 0 {
        eprintln!("Watching the JACK recorder for {seconds} s…");
    }
    let archive = mixer.capture_script(&remote_script(seconds))?;
    let mut snapshots = BTreeMap::new();
    let mut tar = tar::Archive::new(archive.as_slice());
    for entry in tar.entries().map_err(|e| e.to_string())? {
        let mut entry = entry.map_err(|e| e.to_string())?;
        let name = entry
            .path()
            .map_err(|e| e.to_string())?
            .display()
            .to_string();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
        snapshots.insert(name, bytes);
    }
    let after = Snapshot::parse(snapshots.get("after").ok_or("the mixer sent no snapshot")?)?;
    let before = match snapshots.get("before") {
        Some(bytes) => Some(Snapshot::parse(bytes)?),
        None => None,
    };
    print!("{}", report(before.as_ref(), &after, seconds));
    Ok(())
}

/// Runs on the mixer. Writes a tar of `before` (unless SECONDS is 0) and
/// `after` to stdout; everything else goes to stderr.
fn remote_script(seconds: u32) -> String {
    format!(
        r#"set -e
root=/run/rc2fx-recorder
started_of() {{ local record rest; record=$(cat "/proc/$1/stat") || return 1; rest=${{record##*) }}; set -- $rest; printf '%s\n' "${{20}}"; }}
if ! read -r attempt pid started < "$root/active" 2>/dev/null; then
  echo "The JACK recorder is not active: $(head -n 1 "$root/status" 2>/dev/null || echo 'RC2FX has not started it since boot')" >&2
  exit 3
fi
if test "$(started_of "$pid" 2>/dev/null)" != "$started"; then
  echo "The JACK recorder's jackd ($pid) is no longer running; the next RC2FX start records again" >&2
  exit 3
fi
ring=$(cat "$attempt/jackerr-dir")
snapshot=
for d in /run/rc2fx-dev /opt/rc2fx; do
  if test -x "$d/rc2fx_jackerr_snapshot"; then snapshot=$d/rc2fx_jackerr_snapshot; break; fi
done
test -n "$snapshot" || {{ echo 'No RC2FX bundle on this mixer has rc2fx_jackerr_snapshot' >&2; exit 3; }}
out=$(mktemp -d /run/rc2fx-xruns.XXXXXX)
trap 'rm -rf "$out"' EXIT
echo "JACK recorder: jackd $pid, ring $ring" >&2
if test {seconds} -gt 0; then
  "$snapshot" "$ring" "$out/before" >&2
  sleep {seconds}
  if test "$(started_of "$pid" 2>/dev/null)" != "$started"; then
    echo "jackd ($pid) restarted while watching; its records up to then are in the snapshot below" >&2
  fi
fi
"$snapshot" "$ring" "$out/after" >&2
cd "$out"
if test {seconds} -gt 0; then tar -cf - before after; else tar -cf - after; fi
"#
    )
}

const MAGIC: &[u8; 8] = b"RC2FXJS1";
const HEADER: usize = 128;
const RECORD: usize = 96;

/// A snapshot written by `rc2fx_jackerr_snapshot` (its header comment has
/// the layout; `jackerr/tests/jackerr_ring.py` is the reference decoder).
#[derive(Debug, PartialEq)]
pub struct Snapshot {
    pub pid: u64,
    pub created_ns: u64,
    pub dropped: u64,
    /// 0 when the mixer could not read its clock.
    pub snapshot_ns: u64,
    pub first_index: u64,
    pub end_index: u64,
    pub unpublished: u64,
    pub overwritten: u64,
    pub torn: u64,
    pub records: Vec<Record>,
}

#[derive(Debug, PartialEq)]
pub struct Record {
    pub index: u64,
    /// CLOCK_MONOTONIC on the mixer; None when the hook could not read it.
    pub t_ns: Option<u64>,
    pub kind: Kind,
    pub value1: i64,
    pub value2: i64,
    pub string1: Option<String>,
    pub string2: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    ClientNotFinished,
    ClientFinishedLate,
    TimedDriverXrun,
    AlsaXrun,
    AlsaWaitStatus,
    InternalXrun,
    PollFailed,
    PollTimeoutExiting,
    PollTimeoutRetrying,
    PollRecoveryFailed,
    PollInterrupt,
    UnknownFormat,
    Other(u16),
}

impl Kind {
    fn from_code(code: u16) -> Self {
        match code {
            1 => Self::ClientNotFinished,
            2 => Self::ClientFinishedLate,
            3 => Self::TimedDriverXrun,
            4 => Self::AlsaXrun,
            5 => Self::AlsaWaitStatus,
            6 => Self::InternalXrun,
            7 => Self::PollFailed,
            8 => Self::PollTimeoutExiting,
            9 => Self::PollTimeoutRetrying,
            10 => Self::PollRecoveryFailed,
            11 => Self::PollInterrupt,
            255 => Self::UnknownFormat,
            other => Self::Other(other),
        }
    }
    fn name(self) -> String {
        match self {
            Self::ClientNotFinished => "clientNotFinished".into(),
            Self::ClientFinishedLate => "clientFinishedLate".into(),
            Self::TimedDriverXrun => "timedDriverXrun".into(),
            Self::AlsaXrun => "alsaXrun".into(),
            Self::AlsaWaitStatus => "alsaWaitStatus".into(),
            Self::InternalXrun => "internalXrun".into(),
            Self::PollFailed => "pollFailed".into(),
            Self::PollTimeoutExiting => "pollTimeoutExiting".into(),
            Self::PollTimeoutRetrying => "pollTimeoutRetrying".into(),
            Self::PollRecoveryFailed => "pollRecoveryFailed".into(),
            Self::PollInterrupt => "pollInterrupt".into(),
            Self::UnknownFormat => "unknownFormat".into(),
            Self::Other(code) => format!("kind{code}"),
        }
    }
    /// The driver's own xrun reports, as opposed to a late client.
    fn is_xrun(self) -> bool {
        matches!(
            self,
            Self::TimedDriverXrun | Self::AlsaXrun | Self::InternalXrun
        )
    }
    /// What its two numbers and two strings mean (jackd's format strings).
    fn fields(self) -> (&'static [&'static str], &'static [&'static str]) {
        match self {
            Self::ClientNotFinished => (&[], &["client", "state"]),
            Self::ClientFinishedLate => (&[], &["client"]),
            Self::TimedDriverXrun | Self::AlsaXrun => (&["usecs"], &[]),
            Self::AlsaWaitStatus | Self::PollRecoveryFailed => (&["status"], &[]),
            Self::InternalXrun => (&["frameOffset"], &[]),
            Self::PollTimeoutExiting | Self::PollTimeoutRetrying => (&["usecs", "retries"], &[]),
            Self::PollFailed => (&[], &["reason"]),
            Self::UnknownFormat => (&[], &["format", "formatContinued"]),
            Self::PollInterrupt | Self::Other(_) => (&[], &[]),
        }
    }
}

const STRING1_TRUNCATED: u64 = 1;
const STRING2_TRUNCATED: u64 = 2;
const STRING1_NULL: u64 = 4;
const STRING2_NULL: u64 = 8;
const CLOCK_FAILED: u64 = 16;

impl Snapshot {
    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() < HEADER || &data[..8] != MAGIC {
            return Err("not an RC2FX jackd error ring snapshot".into());
        }
        let u32_at = |at: usize| u32::from_le_bytes(data[at..at + 4].try_into().unwrap());
        let u64_at = |at: usize| u64::from_le_bytes(data[at..at + 8].try_into().unwrap());
        let (version, record_size) = (u32_at(8), u32_at(12));
        if version != 1 || record_size as usize != RECORD {
            return Err(format!(
                "unsupported snapshot version {version} / record size {record_size}"
            ));
        }
        // pid, created, claimed, dropped, unknown, calls, snapshot time,
        // first index, end index, stable, unpublished, overwritten, torn.
        let field = |n: usize| u64_at(16 + 8 * n);
        let (pid, created_ns, dropped) = (field(0), field(1), field(3));
        let (snapshot_ns, first_index, end_index) = (field(6), field(7), field(8));
        let (stable, unpublished, overwritten, torn) = (field(9), field(10), field(11), field(12));
        let records_len = usize::try_from(stable)
            .ok()
            .and_then(|n| n.checked_mul(RECORD))
            .filter(|&n| data.len() == HEADER + n)
            .ok_or("snapshot length does not match its record count")?;
        let accounted = [stable, unpublished, overwritten, torn]
            .iter()
            .try_fold(0u64, |sum, &n| sum.checked_add(n));
        if end_index.checked_sub(first_index) != accounted {
            return Err("snapshot accounting does not add up".into());
        }
        let records = data[HEADER..HEADER + records_len]
            .as_chunks::<RECORD>()
            .0
            .iter()
            .map(|bytes| record(bytes))
            .collect();
        Ok(Self {
            pid,
            created_ns,
            dropped,
            snapshot_ns,
            first_index,
            end_index,
            unpublished,
            overwritten,
            torn,
            records,
        })
    }
}

fn record(bytes: &[u8]) -> Record {
    let u64_at = |at: usize| u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap());
    let kind_flags = u64_at(16);
    let flags = (kind_flags >> 16) & 0xffff;
    let lengths = [
        ((kind_flags >> 32) & 0xff) as usize,
        ((kind_flags >> 40) & 0xff) as usize,
    ];
    let text = |raw: &[u8], length: usize, null: u64| {
        (flags & null == 0)
            .then(|| String::from_utf8_lossy(&raw[..length.min(raw.len())]).into_owned())
    };
    Record {
        index: u64_at(0),
        t_ns: (flags & CLOCK_FAILED == 0).then(|| u64_at(8)),
        kind: Kind::from_code((kind_flags & 0xffff) as u16),
        value1: u64_at(24) as i64,
        value2: u64_at(32) as i64,
        string1: text(&bytes[40..80], lengths[0], STRING1_NULL),
        string2: text(&bytes[80..96], lengths[1], STRING2_NULL),
    }
    .with_truncation(flags)
}

impl Record {
    fn with_truncation(mut self, flags: u64) -> Self {
        for (string, bit) in [
            (&mut self.string1, STRING1_TRUNCATED),
            (&mut self.string2, STRING2_TRUNCATED),
        ] {
            if flags & bit != 0
                && let Some(text) = string
            {
                text.push('…');
            }
        }
        self
    }

    fn describe(&self) -> String {
        let (values, strings) = self.kind.fields();
        let mut parts = vec![self.kind.name()];
        for (name, value) in values.iter().zip([self.value1, self.value2]) {
            parts.push(format!("{name}={value}"));
        }
        for (name, value) in strings.iter().zip([&self.string1, &self.string2]) {
            parts.push(format!("{name}={}", value.as_deref().unwrap_or("(null)")));
        }
        parts.join("  ")
    }

    fn client(&self) -> Option<&str> {
        matches!(
            self.kind,
            Kind::ClientNotFinished | Kind::ClientFinishedLate
        )
        .then_some(self.string1.as_deref())
        .flatten()
    }
}

/// The records of `after` that arrived while watching, and how complete the
/// ring's view of that window is.
pub fn report(before: Option<&Snapshot>, after: &Snapshot, seconds: u32) -> String {
    let same_server = before.is_none_or(|b| (b.pid, b.created_ns) == (after.pid, after.created_ns));
    let from = match before {
        Some(b) if same_server => b.end_index,
        _ => after.first_index,
    };
    let origin = match before {
        Some(b) if same_server && b.snapshot_ns != 0 => Some(b.snapshot_ns),
        _ => None,
    };
    let records: Vec<&Record> = after.records.iter().filter(|r| r.index >= from).collect();
    let mut out = String::new();
    if before.is_some() && !same_server {
        out += "jackd was replaced while watching: showing everything the new server recorded\n";
    }
    let xruns = records.iter().filter(|r| r.kind.is_xrun()).count();
    let late = records.iter().filter(|r| r.client().is_some()).count();
    out += &match before {
        Some(_) => format!("Watched {seconds} s: "),
        None => "Recorded since this jackd started: ".into(),
    };
    out += &format!(
        "{} event(s): {xruns} driver xrun(s), {late} late client(s)\n",
        records.len()
    );
    for r in &records {
        let at = match (r.t_ns, origin) {
            (Some(t), Some(o)) => format!("{:>+10.3} s", (t as f64 - o as f64) / 1e9),
            (Some(t), None) => format!("{:>10.3} s", t as f64 / 1e9),
            (None, _) => format!("{:>12}", "(no clock)"),
        };
        out += &format!("  {at}  {}\n", r.describe());
    }
    if !records.is_empty() {
        let mut kinds = BTreeMap::new();
        let mut clients = BTreeMap::new();
        for r in &records {
            *kinds.entry(r.kind.name()).or_insert(0) += 1;
            if let Some(client) = r.client() {
                *clients.entry(client.to_owned()).or_insert(0) += 1;
            }
        }
        let list = |map: &BTreeMap<String, usize>| {
            map.iter()
                .map(|(name, n)| format!("{name} {n}"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        out += &format!("By kind: {}\n", list(&kinds));
        if !clients.is_empty() {
            out += &format!("Late clients: {}\n", list(&clients));
        }
    }
    // Records of the window the ring could not show.
    let missed = after.first_index.saturating_sub(from);
    let dropped = match before {
        Some(b) if same_server => after.dropped.saturating_sub(b.dropped),
        _ => after.dropped,
    };
    let unreadable = after.unpublished + after.torn;
    if missed + dropped + unreadable > 0 {
        out += &format!(
            "Incomplete: {missed} overwritten before the snapshot, {dropped} dropped by the recorder, \
             {unreadable} unreadable at snapshot time\n"
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(snapshot: &Snapshot, stable: u64) -> Vec<u8> {
        let mut bytes = MAGIC.to_vec();
        bytes.extend(1u32.to_le_bytes());
        bytes.extend((RECORD as u32).to_le_bytes());
        for value in [
            snapshot.pid,
            snapshot.created_ns,
            0,
            snapshot.dropped,
            0,
            0,
            snapshot.snapshot_ns,
            snapshot.first_index,
            snapshot.end_index,
            stable,
            snapshot.unpublished,
            snapshot.overwritten,
            snapshot.torn,
            0,
        ] {
            bytes.extend(value.to_le_bytes());
        }
        bytes
    }

    fn encode(
        kind: u16,
        flags: u64,
        index: u64,
        t: u64,
        values: [i64; 2],
        strings: [&str; 2],
    ) -> Vec<u8> {
        let mut bytes = Vec::new();
        let kind_flags = u64::from(kind)
            | (flags << 16)
            | ((strings[0].len() as u64) << 32)
            | ((strings[1].len() as u64) << 40);
        for value in [index, t, kind_flags, values[0] as u64, values[1] as u64] {
            bytes.extend(value.to_le_bytes());
        }
        let mut s1 = strings[0].as_bytes().to_vec();
        s1.resize(40, 0);
        let mut s2 = strings[1].as_bytes().to_vec();
        s2.resize(16, 0);
        bytes.extend(s1);
        bytes.extend(s2);
        bytes
    }

    fn snapshot(first: u64, end: u64, snapshot_ns: u64, records: &[Vec<u8>]) -> Vec<u8> {
        let meta = Snapshot {
            pid: 16643,
            created_ns: 5,
            dropped: 0,
            snapshot_ns,
            first_index: first,
            end_index: end,
            unpublished: 0,
            overwritten: 0,
            torn: 0,
            records: Vec::new(),
        };
        let mut bytes = header(&meta, records.len() as u64);
        for r in records {
            bytes.extend(r);
        }
        bytes
    }

    #[test]
    fn a_snapshot_decodes_as_the_reference_decoder_does() {
        let bytes = snapshot(
            0,
            2,
            9_000_000_000,
            &[
                encode(2, 0, 0, 1_000_000_000, [0, 0], ["rc2fxd", ""]),
                encode(4, 0, 1, 2_000_000_000, [1234, 0], ["", ""]),
            ],
        );
        let parsed = Snapshot::parse(&bytes).unwrap();
        assert_eq!(parsed.pid, 16643);
        assert_eq!(parsed.records.len(), 2);
        assert_eq!(parsed.records[0].kind, Kind::ClientFinishedLate);
        assert_eq!(parsed.records[0].string1.as_deref(), Some("rc2fxd"));
        assert_eq!(parsed.records[1].kind, Kind::AlsaXrun);
        assert_eq!(parsed.records[1].value1, 1234);
        assert_eq!(parsed.records[1].describe(), "alsaXrun  usecs=1234");
    }

    #[test]
    fn flags_mark_null_truncated_strings_and_a_failed_clock() {
        let bytes = snapshot(
            0,
            1,
            0,
            &[encode(
                1,
                STRING2_NULL | STRING1_TRUNCATED | CLOCK_FAILED,
                0,
                7,
                [0, 0],
                ["jackd-cl", ""],
            )],
        );
        let r = &Snapshot::parse(&bytes).unwrap().records[0];
        assert_eq!(r.t_ns, None);
        assert_eq!(r.string1.as_deref(), Some("jackd-cl…"));
        assert_eq!(r.string2, None);
        assert_eq!(
            r.describe(),
            "clientNotFinished  client=jackd-cl…  state=(null)"
        );
    }

    #[test]
    fn damaged_snapshots_are_refused() {
        let good = snapshot(0, 1, 1, &[encode(4, 0, 0, 1, [1, 0], ["", ""])]);
        assert!(
            Snapshot::parse(&good[..good.len() - 1]).is_err(),
            "truncated"
        );
        let mut magic = good.clone();
        magic[0] = b'X';
        assert!(Snapshot::parse(&magic).is_err());
        // One stable record claimed for a window of two.
        let mut accounting = good.clone();
        accounting[16 + 8 * 8..16 + 8 * 9].copy_from_slice(&2u64.to_le_bytes());
        assert!(Snapshot::parse(&accounting).is_err());
    }

    #[test]
    fn the_report_shows_only_what_arrived_while_watching() {
        let before = Snapshot::parse(&snapshot(
            0,
            1,
            10_000_000_000,
            &[encode(4, 0, 0, 1_000_000_000, [99, 0], ["", ""])],
        ))
        .unwrap();
        let after = Snapshot::parse(&snapshot(
            0,
            3,
            70_000_000_000,
            &[
                encode(4, 0, 0, 1_000_000_000, [99, 0], ["", ""]),
                encode(2, 0, 1, 12_500_000_000, [0, 0], ["rc2fxd", ""]),
                encode(3, 0, 2, 13_000_000_000, [250, 0], ["", ""]),
            ],
        ))
        .unwrap();
        let text = report(Some(&before), &after, 60);
        assert!(
            text.starts_with("Watched 60 s: 2 event(s): 1 driver xrun(s), 1 late client(s)\n"),
            "{text}"
        );
        assert!(
            text.contains("+2.500 s  clientFinishedLate  client=rc2fxd"),
            "{text}"
        );
        assert!(
            text.contains("+3.000 s  timedDriverXrun  usecs=250"),
            "{text}"
        );
        assert!(
            !text.contains("usecs=99"),
            "an earlier record is not in the window: {text}"
        );
        assert!(text.contains("Late clients: rc2fxd 1"), "{text}");
        assert!(!text.contains("Incomplete"), "{text}");
    }

    #[test]
    fn the_report_says_when_the_ring_lost_part_of_the_window() {
        let before = Snapshot::parse(&snapshot(
            0,
            2,
            1,
            &[
                encode(4, 0, 0, 1, [1, 0], ["", ""]),
                encode(4, 0, 1, 1, [1, 0], ["", ""]),
            ],
        ))
        .unwrap();
        // The window began at index 2; the ring now starts at 5.
        let mut lapped = header(
            &Snapshot {
                pid: 16643,
                created_ns: 5,
                dropped: 0,
                snapshot_ns: 2,
                first_index: 5,
                end_index: 6,
                unpublished: 0,
                overwritten: 0,
                torn: 0,
                records: Vec::new(),
            },
            1,
        );
        lapped.extend(encode(4, 0, 5, 2, [1, 0], ["", ""]));
        let after = Snapshot::parse(&lapped).unwrap();
        let text = report(Some(&before), &after, 5);
        assert!(
            text.contains("Incomplete: 3 overwritten before the snapshot"),
            "{text}"
        );
    }

    #[test]
    fn zero_seconds_lists_the_whole_ring() {
        let after = Snapshot::parse(&snapshot(
            0,
            1,
            5,
            &[encode(6, 0, 0, 3_000_000_000, [12, 0], ["", ""])],
        ))
        .unwrap();
        let text = report(None, &after, 0);
        assert!(
            text.starts_with("Recorded since this jackd started: 1 event(s): 1 driver xrun(s)"),
            "{text}"
        );
        assert!(
            text.contains("3.000 s  internalXrun  frameOffset=12"),
            "{text}"
        );
    }

    #[test]
    fn the_remote_script_is_valid_shell() {
        for seconds in [0, 60] {
            let status = std::process::Command::new("bash")
                .args(["-n", "-c", &remote_script(seconds)])
                .status()
                .unwrap();
            assert!(status.success());
        }
    }
}
