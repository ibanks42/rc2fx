# Firmware fixtures

Shared test cases read by `desktop/firmware` (Go) and `crates/rc2fx-firmware/tests/shared_fixtures.rs` (Rust). Both sides must agree on every case here, except the USB IDs in `models.json`, which only Go uses (the mixer identifies itself from board metadata, not USB descriptors). Neither side picks a profile by executable hash alone; Pro II and Duo share binaries, so selection always requires a known model. The `select` cases verify an image against the catalog's single profile.

- `models.json`: per-model identity strings, USB IDs and lookups, board evidence (`boards`: hostname and `extlinux.conf` text), and model IDs.
- `catalogs.json`: `Catalog::parse` cases (starting with the real `config/firmware-profiles.json`) and their expected results in `expect` (`ok`, plus exact `error` or `*` when only rejection matters, such as serde's malformed-JSON messages). Base catalog: `catalogs/base.json` (a frozen copy of `config/firmware-profiles.json` so fixture edits do not shift when the real config gains profiles).
- `select/`: tiny hand-crafted AArch64 ELF images, launchers, base catalog `select/base.json`, the two-profile catalogs `shared.json` and `duo.json`, the single-profile `unqualified.json`, and `cases.json` listing the expected profile or exact error. Since the desktop never selects a runtime itself, Go runs `selectForModel` cases through a test-only port of the Rust selector; both `selectForModel` and `select` exercise Go's `VerifyImage` used by `inspectForModel`.

## Case format

Each case in `catalogs.json` and `select/cases.json` is a single-line JSON object with `name`, `expect`, and exactly one catalog input mode (paths are relative to the manifest's directory):

- `file`: read a committed fixture file directly (used for standalone edge cases, multi-profile catalogs, and `../../config/firmware-profiles.json`).
- `edits`: apply an ordered list of `{"find": "...", "replace": "..."}` text replacements to `base` (or the manifest's `defaultBase` when `base` is omitted). Every `find` string must be non-empty and occur **exactly once** in the current text; both harnesses fail loudly otherwise.
- `generate`: size-only generators applied to `base` (or `defaultBase`):
  - `{"profiles": N}` (for `4 <= N <= 256`): appends synthetic `rodecaster-duo` profiles `x-4` through `x-(N-1)` (each with `mixerSha256` set to the 2-digit lowercase hex byte `i` repeated 32 times, `runtime: null`, and empty `notes`) before the closing `\n  ]\n}\n` of the case's `base` (or `defaultBase` when `base` is omitted), which must hold exactly 4 profiles.
  - `{"padBytes": N}` (for `N > 0`): inserts `N` copies of `x` immediately after the first `"notes": "` in the base catalog.

Both helpers enforce the same input rules: `edits` must be non-empty, each `find` must be non-empty and occur exactly once, `file` cannot be combined with `base`, `edits`, or `generate`, and exactly one of `file`, `edits`, or `generate` is required. Cases in `select/cases.json` also carry `op`, `image`, `model`, `launcher`, and `experimentalDuo`.

## Adding a case

1. Prefer a one-line `edits` entry against the family's `defaultBase` (`catalogs/base.json` or `select/base.json`), using enough context in `find` so it matches exactly once.
2. Add a standalone file under `catalogs/` or `select/` only when the input cannot be expressed cleanly as text edits of a base (for example, empty or non-object JSON, or a multi-profile catalog reused by several cases).
3. Run both Go and Rust tests to confirm both parsers agree on the new case.
