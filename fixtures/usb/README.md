# USB framing fixtures

Read by desktop/usbproto (Go) and crates/rc2fx-usb/tests/shared_fixtures.rs
(Rust); both must give every answer recorded here.

- `cases.json`: hello acknowledgements (`hello4`), HID report descriptors
  (`descriptors`), 256-byte reports and their decoded frames (`frames`, null
  when the report is not a valid frame), OUT transfer lengths (`canonical`),
  request serialization (`requests`: the exact bytes serde writes) and
  request/reply parsing (`replies`, `requestsIn`). Byte strings are hex.
- `messages/<name>.bin`: packed messages, written by the Go `Pack` or cut
  from its output. `cases.json` gives the limit and either the file holding
  the decoded bytes or the exact error (`*`: any error, where zlib itself
  refuses the stream).

The files were generated once from the Go port; add cases by hand and check
both tests agree.
