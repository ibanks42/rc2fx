# Firmware fixtures

Read by desktop/firmware (Go) and crates/rc2fx-firmware/tests/shared_fixtures.rs
(Rust); both must give every answer recorded here.

- `models.json`: each model's identity strings and USB IDs, USB lookups,
  board evidence (`boards`: hostname and extlinux.conf text) and model IDs.
- `catalogs.json`: catalog texts (paths from the repository root; the first
  is the real `config/firmware-profiles.json`) and whether `Catalog::parse`
  accepts them. `error` is the exact message, or `*` where only the refusal is
  shared (serde's own wording for malformed JSON).
- `select/`: small AArch64 ELF images built the way the Rust unit tests build
  theirs, catalogs pinned to them, launchers, and `cases.json` with the
  profile each selection picks or its exact error.

The files were generated once from the Go port; add cases by hand and check
both tests agree.
