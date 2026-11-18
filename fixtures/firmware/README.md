# Firmware fixtures

Read by desktop/firmware (Go) and crates/rc2fx-firmware/tests/shared_fixtures.rs
(Rust); both must give every answer recorded here, except the USB identities
in `models.json`, which only Go reads (the mixer identifies itself by its
board, not by USB IDs). Neither side selects a profile by executable hash
alone: the Pro II and Duo share executables, so a selection is always for a
known model. The `select` cases are image verification against the
catalog's one profile.

- `models.json`: each model's identity strings and USB IDs, USB lookups,
  board evidence (`boards`: hostname and extlinux.conf text), and model IDs.
- `catalogs.json`: catalog texts (paths from the repository root; the first
  is the real `config/firmware-profiles.json`) and whether `Catalog::parse`
  accepts them. `error` is the exact message, or `*` where only the refusal is
  shared (serde's own wording for malformed JSON).
- `select/`: small hand-built AArch64 ELF images, catalogs pinned to them,
  launchers, and `cases.json` with the profile each selection picks or its
  exact error. The desktop never selects a runtime, so the Go test runs the
  `selectForModel` cases through a test-only copy of the Rust selection; it
  and the `select` cases still exercise the Go `VerifyImage` that
  `inspectForModel` uses.

The files were generated once from the Go port; add cases by hand and check
both tests agree.
