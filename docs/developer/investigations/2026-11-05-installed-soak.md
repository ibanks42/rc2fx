# Installed runtime qualification: failed zero-xrun gate

The requested two-hour qualification did **not** pass. One new xrun notification
occurred approximately 80 minutes into the observation. The 15-minute poll at
90.78 minutes detected it and ended the test. No release was pushed or published.

## Installation and baseline

`just mixer-install` rebuilt and copied the candidate from source commit
`538019e` into `/opt/rc2fx`, with mixer manifest version `1.0.1`. Native, GUI,
controller and other payload hashes matched on readback. The audio restart lost
SSH connectivity, so the authorized Home Assistant `Rodecaster Switch` power
cycle recovered the device. Once callbacks advanced and the controller was
running, the installer's existing commit procedure completed the transaction.
The one-shot power-recovery flag was re-armed and verified.

Settings remained at revision 606, with `mic-1` and `headphones-1` selected.
The optional client timing probe was off. The installed development launcher
used verbose GUI diagnostics; release packaging normally uses quiet diagnostics.
This measurement is therefore not a qualification of a separately packaged
release or subsequent updater changes.

Observation began at **2026-11-05 11:36:32 UTC** (06:36:32 Eastern), after
startup. Mixer PID 1411 remained unchanged. The baseline already contained
83 startup xrun notifications; those are recorded separately and were not reset.
The gate required at least 7,200 seconds, continuous callbacks, no restart,
fresh telemetry and zero counter increments for xruns or monitored native faults.

## Playback and monitoring

At 12:07:38 UTC, six simultaneous 48 kHz stereo streams began on the host's
System, Game, Music, Virtual A, Virtual B and Chat USB routes. Each played a
distinct modulated harmonic tone with a peak bounded by -40 dBFS. All six
PipeWire sinks were verified running. Playback ended automatically on the
failed gate at approximately 13:07:19 UTC; the mixer itself stayed running.

Snapshots initially arrived about every 30 seconds. At the user's request,
polling changed to 15 minutes while preserving the original baseline, prior
snapshot and elapsed time. The counter increase between polls remains a failure;
it was not discarded or re-baselined.

## Failure evidence

- Final observation duration: **5,446.525 seconds**; 150 retained snapshots.
- Xrun notifications: **83 -> 84**, one new notification.
- Audio callbacks advanced by **2,334,289** with the same boot and mixer PID.
- Native event time: **4,926,669,595,415 ns** on the device monotonic clock.
- JACK recorder: **4926.669 s, `clientFinishedLate`, `usb1out`**.
- Helper late/stall counters did not increase.
- Suppression reported no failed worker, missed deadlines, input drops or output
  drops. The worker's lifetime maxima include startup and do not by themselves
  identify the cause of this later event.

The JACK record identifies the late client, not the root cause. This evidence
does not establish that lookahead switching, the suppression worker, playback
startup or a GUI redraw caused this fault. Playback was already running long
before it occurred. A later attempt needs a reproducible trigger or more precise
attribution before another performance change is justified.

## Source validation and release hold

The clean committed checkout passed 117 native tests (2 ignored), 63 GUI tests,
24 native diagnostic-history tests (1 ignored), 33 xtask tests (1 ignored), and
11 launcher/release test scripts. Native and host tooling Clippy checks passed.
The host test-module ordering fix in `1929797` did not change installed code.

During the run the user withdrew release publication authorization and requested
a future initial `v0.1.1` with consolidated history, alongside a decoupled mixer
updater. The `1.0.1` label above identifies this historical installed candidate;
it is not the planned new release version.

Measured counters and payload identities are retained in
[`2026-11-05-installed-soak-evidence.json`](2026-11-05-installed-soak-evidence.json).
Full snapshots, playback scripts, install/recovery receipts and JACK output are
under `/home/ibanks/.local/state/rc2fx-qualification/2026-11-05-1.0.1/`.
