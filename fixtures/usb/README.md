# USB framing fixtures

Read by desktop/usbproto (Go) and crates/rc2fx-usb/tests/shared_fixtures.rs
(Rust); each checks its own direction. Only Go reads `hello4`,
`descriptors`, `requests`, and `replies`: parsing the mixer's hello
acknowledgement, checking a HID report descriptor for RC2FX's reports,
encoding requests, and decoding replies are client-side jobs.

- `cases.json`: hello acknowledgements (`hello4`), HID report descriptors
  (`descriptors`), 256-byte reports and their decoded frames (`frames`, null
  when the report is not a valid frame), OUT transfer lengths (`canonical`,
  read only by Rust: the OUT transfer is the mixer's side), request
  serialization (`requests`: the exact bytes to write), reply parsing
  (`replies`), and request parsing (`requestsIn`). Byte strings are hex.
- `messages/<name>.bin`: packed messages, written by the Go `Pack` or cut
  from its output. `cases.json` gives the limit and either the file holding
  the decoded bytes or the exact error (`*`: any error, where zlib itself
  refuses the stream).

The files were generated once from the Go port; add cases by hand and check
both tests agree.
