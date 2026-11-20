# Firmware fixtures

Shared test cases read by `desktop/firmware` (Go) and `crates/rc2fx-firmware/tests/shared_fixtures.rs` (Rust). Both sides must agree on every case here, except the USB IDs in `models.json` which only Go uses (the mixer identifies itself from board metadata, not USB descriptors). Neither side picks a profile by executable hash alone; Pro II and Duo share binaries, so selection always requires a known model. The `select` cases verify an image against the catalog's single profile.

- `models.json`: per-model identity strings, USB IDs and lookups, board evidence (`boards`: hostname and `extlinux.conf` text), and model IDs.
- `catalogs.json`: catalog paths relative to the repo root (starting with the real `config/firmware-profiles.json`) and whether `Catalog::parse` accepts them. `error` is either the exact expected message or `*` when only rejection matters (e.g. serde's malformed-JSON messages).
- `select/`: tiny hand-crafted AArch64 ELF images, catalogs pinned to them, launchers, and `cases.json` listing the expected profile or exact error. Since the desktop never selects a runtime itself, Go runs `selectForModel` cases through a test-only port of the Rust selector; both `selectForModel` and `select` exercise Go's `VerifyImage` used by `inspectForModel`.

These fixtures were originally generated from the Go port; add new cases by hand and make sure both Go and Rust tests pass.
