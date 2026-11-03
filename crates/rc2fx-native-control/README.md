# rc2fx-native-control

Message types and a Linux control client for talking to the native library
inside the mixer process. The message types are portable. The client uses a
Unix datagram socket and runs on the mixer. The desktop app talks only to
`rc2fxd`, over USB 1.

The receiving side is in `crates/rc2fx-native`. This crate starts no service,
has no global allocator, and doesn't depend on the DSP or the ELF audit code.
Linking it creates no process, socket or device setting.

## Settings transactions

Protocol version 5 uses Prepare, Commit and Cancel messages. A receipt names
the process session, callback context, target, revision and a random
transaction ID. A receipt from an earlier native process can't be reused after
reconnecting. Receipts carry no addresses or Rust ownership handles.

`Client::save_and_commit` handles one target:

1. Prepare the target and its `PathMode` in the native receiver without
   publishing them.
2. Call the controller's persistence closure once.
3. Commit the prepared audio change if persistence succeeded.

If preparation fails, the save closure isn't called. A persistence error is
returned unchanged together with the cancellation outcome. This includes
uncertain Store errors, and cancelling does not restore an old saved file.

After a successful save the result is always `Saved<T>`. If the commit is
refused or its reply is lost, the result is `Publication::Pending` with the
original receipt. Keep the desired settings and call `commit_saved`, which
retries that receipt without saving again. `Published` confirms publication.
Whether the audio has applied it is reported separately, by the target status
and applied revision.

The receiver limits the number of pending preparations and recent committed
receipts. A repeated message doesn't allocate another candidate or extend its
lease. Abandoned preparations expire during later control requests. There is
no periodic timer.

## Presets

`save_and_commit` covers a single path. The device's `native-pipeline`
profile uses these messages behind its control API for presets: it prepares
every changed active target, saves once, and then commits each path on its
own. A missing commit reply stays pending and is retried without saving again.
A preset change is therefore applied path by path, and the paths can switch at
different times.

## Status and telemetry

Status includes a fingerprint of each target and complete audio windows.
Unchanged paths can have older native publication revisions than the saved
document. The controller confirms the document once every bound path reports
the expected fingerprint and publication and shows callback progress.

Telemetry goes through a fixed single-producer, single-consumer ring. JSON,
RMS and worker snapshots are built on the control side. The controller reuses
its receive buffer and does not register a JACK audio client.

## Path changes

Mode changes and structural replacements apply only to the requested bound
path. `Processed` runs the path's DSP and provider. `Passthrough` uses the
native buffers and adds no delay.

Compatible settings keep the same runtime epoch. A replacement builds a new
runtime outside the callback. Retired worker receipts keep the final counters
after the callback has acknowledged and the control side has joined the
worker. The latest receipt and the cumulative retirement count show whether
any observations were skipped.

Global toggles keep the saved selection, config revision and engine
generation. Native publication revisions advance on their own. This profile
doesn't add unbound ports or recover stock audio automatically.

## Tests

The tests cover malformed and outdated messages, receipt lifetime limits,
refused and lost preparations, persistence cancellation, refused and lost
commits, retry without a second save, and mismatched acknowledgements.

To run only this package's checks:

```sh
cargo test -p rc2fx-native-control --lib
cargo clippy -p rc2fx-native-control --all-targets -- -D warnings
```
