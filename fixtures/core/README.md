# Core JSON fixtures

Shared accept/reject JSON fixtures for `crates/rc2fx-core/tests/shared_fixtures.rs` (Rust) and `desktop/core/shared_fixtures_test.go` (Go). `cases.json` indexes every input file, expected verdict, and golden snapshot. `.gitattributes` marks every file `-text` so CRLF line endings and non-UTF-8 test inputs stay byte-exact. Most rejection, device-state, and config-update cases use a four-path target catalog to keep files small, while `config/default.json` covers the full 25-target catalog.

## Contract

- **Rust (`rc2fx-core`)** owns the `canonical` (compact) and `pretty` (`serde_json::to_string_pretty`) golden bytes and the exact `error` wording listed on rejected cases. Accepted documents must re-encode byte for byte to their `canonical` and `pretty` files.
- **Go (`desktop/core`)** must reach the same accept or reject verdict on every case. Its encoding of an accepted document must parse strictly to the same JSON as `canonical`: the same members and values with object key order ignored, and every number written with the same characters as in `canonical`. `EncodePretty` output must parse strictly to the same JSON as `Encode` output. Go does not assert on error wording, field order, whitespace, string escaping, or the `pretty` files.

Number text is part of the shared contract in both directions: both sides must read the same value from the same number text, including the decimals the mixer's `serde_json` (built without `float_roundtrip`) does not round to the nearest double, and Go must write each number with the characters Rust writes. The `json` cases (including `json long-integer`, `json inexact-float`, `json many-digits`, `json negative-zero`, and the integer-boundary and exponent cases), the `meter with performance` case, and `edited config, hand-written style` cover this, and the Go test compares number text against `canonical` so a parser or writer that disagrees with the mixer fails instead of matching itself.

## Cases Rust skips for rejection

Rust skips rejected cases for document types written by the mixer and only parsed by the desktop app (`WRITTEN_ONLY` in `crates/rc2fx-core/tests/shared_fixtures.rs`): `deviceState`, `deviceVersions`, `sshStatus`, `meter`, `diagnosticsEvents`, and `presetSummaries`. Rust still checks encoding for their accepted cases, and Go verifies both accept and reject verdicts for every type.

## Adding a case

1. Create or edit the input JSON file by hand in the matching subdirectory (for example, `config/`, `meter/`, or `json/`).
2. Add an entry to `cases.json` with `name`, `type`, `file`, `accept`, and `why`, plus `canonical` and `pretty` as needed. `error` is required on every rejected case Rust reads.
3. Regenerate any new or updated golden files with the Rust test, then run both the Rust and Go fixture tests.

## Regenerating goldens

Run the Rust fixture test with `RC2FX_UPDATE_FIXTURES=1`:

```sh
RC2FX_UPDATE_FIXTURES=1 cargo test -p rc2fx-core --test shared_fixtures
```

That rewrites only `canonical` and `pretty` files whose bytes changed and prints each updated path to stderr. Update mode never writes a golden that is also an input `file` (`config/default.json`, `config/default-pretty.json`, `config-update/update.json`, `device-state/jack-running.json`, `device-versions/empty.json`, `inventory/show.json`, and `ssh-status/minimal.json`); a difference there fails the test and must be fixed by hand. Expected `error` strings in `cases.json` are not rewritten automatically; on a mismatch, the Rust test prints the actual message so it can be pasted into `cases.json`. The run passes after rewriting; check `git diff fixtures/core`.
