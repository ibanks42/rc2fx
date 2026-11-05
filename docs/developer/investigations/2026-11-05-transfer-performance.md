# Bulk SSH transfer interference, 2026-11-05

Follow-up to the redraw/timing/diagnostics fixes committed in `991c802`.
Before new stress tests, the installed mixer's xrun counter remained at 130,
unchanged from the previous check roughly three hours earlier. Tests below
kept mixer PID 26344 running, with the optional callback timing probe off.
No firmware flash, runtime replacement, or audio restart was needed.

## Reproduction and intervention

Each transfer was bracketed by `/run/rc2fx/audit.json` snapshots, with a
two-second observation tail and ten seconds between trials. PID equality
was checked. Counts below are per-trial deltas, not lifetime totals.

| Transfer | Plain xruns | Gzip xruns |
| --- | --- | --- |
| Approximately 4 MiB pacing log download | 26, 17, 29 | 0, 0, 0 |
| Three runtime files as a tar download | 36, 19, 24 | 0, 0, 0 |
| Same three files uploaded to an unused RAM directory | 0, 0, 0 | 4, 0, 18 |

Archive trials alternated transfer formats. Downloaded archives were checked
for the exact member set and each file's SHA-256 against the host originals;
uploads were also hash-verified. Uploaded files were never launched or installed.
All compressed downloads had zero helper-late and helper-stall increments.

The plain download archive was 9,734,656 bytes; gzip was 6,519,755 bytes.
Device uptime brackets measured approximately 2.8–3.1 seconds for plain
downloads and 17.7–19.1 seconds for gzip downloads. Log downloads also became
slower. This is an audio-interference mitigation with a throughput cost.
It does not establish whether pacing, encryption load, or another scheduling
effect causes the underlying interference.

Requesting SSH compression with `ssh -C` was ineffective: negotiation still
reported `compression: none`. Explicit BusyBox `gzip -c` and `tar -czf` were
tested instead. Server configuration was not changed.

## Change and limits

`xtask bench` now retrieves its bulk WAV/JSON/log results using `tar -czf`.
Host decoding validates the gzip stream before extracting the tar. Uploads
remain uncompressed because the upload experiment did not support changing
them. Small control requests remain unchanged.

This fixes the tested transfer pattern in the benchmark download path; it
does not make arbitrary bulk SSH downloads safe, nor prove all possible WAV
payloads or concurrent workloads free of xruns. No full audio benchmark was
rerun alongside live audio solely to test result retrieval.

Regression tests cover binary audio/report round trips and rejection of
truncated or CRC-corrupt gzip before extraction. The round-trip test failed
against the original raw-tar reader and passed with decoding enabled.
`cargo test -p xtask`: 33 passed, 1 ignored (requires Zig).

Trial measurements are preserved in
[`2026-11-05-transfer-evidence.json`](2026-11-05-transfer-evidence.json).
Raw snapshots, archives, scripts, and test logs remain locally under
`/tmp/rc2fx-investigation`; these temporary files are not durable repository
artifacts. The deliberately adverse plain-download and compressed-upload
tests increased the cumulative xrun counter.

Final quiet observation: 45 seconds, same PID 26344, zero additional xruns,
helper-late events, or helper stalls.
