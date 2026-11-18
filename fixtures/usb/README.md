# USB framing fixtures

Read by desktop/usbproto (Go) and crates/rc2fx-usb/tests/shared_fixtures.rs
(Rust); both must give every answer recorded here. Only Go reads `hello4`
and `descriptors`: parsing the mixer's hello acknowledgement and checking a
HID report descriptor for RC2FX's reports are client-side jobs, and the
mixer has no Rust twin of either.

- `cases.json`: hello acknowledgements (`hello4`), HID report descriptors
  (`descriptors`), 256-byte reports and their decoded frames (`frames`, null
  when the report is not a valid frame), OUT transfer lengths (`canonical`, read only by
  Rust: the OUT transfer is the mixer's side), request serialization (`requests`: the exact bytes serde writes) and
  request/reply parsing (`replies`, `requestsIn`). Byte strings are hex.
- `messages/<name>.bin`: packed messages, written by the Go `Pack` or cut
  from its output. `cases.json` gives the limit and either the file holding
  the decoded bytes or the exact error (`*`: any error, where zlib itself
  refuses the stream).

The files were generated once from the Go port; add cases by hand and check
both tests agree.
