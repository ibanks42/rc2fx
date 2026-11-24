# rc2fx-native-control

Protocol messages and a Linux control client for talking to the native library running inside the mixer process. The message types themselves are platform-independent; the client talks over a Unix datagram socket on the mixer. The desktop app never uses this directly; it only talks to `rc2fxd` over USB 1.

The receiving side lives in `crates/rc2fx-native`. This crate provides the protocol message types and the Unix datagram socket client.

## Settings transactions

Protocol versions 6 and 7 use Prepare, Commit, and Cancel messages. Each receipt carries the process session, callback context, target, revision, and a random transaction ID. Receipts are tied to the live native process, so they can't be reused across reconnects, and they never contain raw pointers or Rust ownership handles.

Updating a target's settings happens in three steps:

1. `Client::transaction` mints a receipt, and a Prepare request stages the target and its `PathMode` on the native side without publishing them yet.
2. The controller persists the settings. If Prepare failed, nothing is written to disk; if saving fails, the controller sends Cancel and drops the staged change.
3. `Client::commit_saved` publishes the staged audio change.

If the commit is refused, the reply gets lost, or the reply names a different receipt, `commit_saved` returns `Publication::Pending` along with the original receipt. Keep the desired settings and call `commit_saved` again to retry that receipt without saving a second time. `Published` means the control side published the update; whether the audio callback has actually picked it up is tracked separately via target status and applied revision.

The receiver caps both pending preparations and recent committed receipts. Retransmitting a message won't allocate a duplicate candidate or refresh its lease, and abandoned preparations are swept lazily on subsequent control requests (there is no background timer).

## Presets

`rc2fxd` uses the same transaction protocol for presets: it prepares every changed active target, writes the saved state once, and then commits each path individually. Any missing commit reply stays pending and retries without re-saving. Because commits happen per path, different paths in a preset can switch over at slightly different times.

## Status and telemetry

Status replies carry a fingerprint of each target along with complete audio windows. Paths that didn't change can legitimately report older native publication revisions than the saved config. The controller considers the document settled once every bound path reports the expected fingerprint and publication and shows callback progress.

Telemetry flows through a fixed single-producer, single-consumer ring buffer, with JSON, RMS, and worker snapshots assembled on the control side. The controller reuses a single receive buffer and never registers a JACK audio client.

## Path changes

Mode switches and structural replacements only affect the targeted bound path. `Processed` runs the path's DSP and suppression provider, while `Passthrough` uses the native buffers directly with zero added delay.

Compatible edits keep the existing runtime epoch. Structural replacements build a new runtime off the audio thread. Once the callback acknowledges retirement and the control side joins the old worker, retired worker receipts preserve the final counters; comparing the latest receipt against the cumulative retirement count reveals whether any observations were missed.

Global toggles preserve the saved selection, config revision, and engine generation, while native publication revisions advance independently. This profile doesn't add unbound ports or automatically recover stock audio.

## Tests

Unit tests cover malformed and stale messages, receipt lifetime limits, and refused or mismatched commit acks. The full prepare/save/commit-retry flow is tested alongside `rc2fx-device`'s native backend.

To run just this crate's checks:

```sh
cargo test -p rc2fx-native-control --lib
cargo clippy -p rc2fx-native-control --all-targets -- -D warnings
```
