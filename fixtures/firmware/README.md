# Firmware fixtures

Read by desktop/firmware (Go) and crates/rc2fx-firmware/tests/shared_fixtures.rs
(Rust); both must give every answer recorded here, with two exceptions that
only Go reads: the USB identities in `models.json` (the mixer identifies
itself by its board, not by USB IDs), and the `select` cases that pick a
profile by executable hash alone (`unrecognized` and `ambiguous` answers). The
Pro II and Duo share executables, so the mixer always selects for its board's
model; Rust checks the other `select` cases as plain image verification.

- `models.json`: each model's identity strings and USB IDs, USB lookups,
  board evidence (`boards`: hostname and extlinux.conf text) and model IDs.
- `catalogs.json`: catalog texts (paths from the repository root; the first
  is the real `config/firmware-profiles.json`) and whether `Catalog::parse`
  accepts them. `error` is the exact message, or `*` where only the refusal is
  shared (serde's own wording for malformed JSON).
- `select/`: small AArch64 ELF images built the way the Rust unit tests build
  theirs, catalogs pinned to them, launchers, and `cases.json` with the
  profile each selection picks or its exact error. The desktop never selects
  a runtime, so the Go test runs the `select` and `selectForModel` cases
  through test-only copies of the Rust selection; they still exercise the Go
  `VerifyImage` that `inspectForModel` uses.

The files were generated once from the Go port; add cases by hand and check
both tests agree.
