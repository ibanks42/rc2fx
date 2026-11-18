# Release metadata signed by the Go xtask

TUF metadata written by the Go release tooling (`desktop/internal/xtask`,
`./xtask mixer-sign`, and `./xtask mixer-refresh`) for one packaged mixer
release, `targets/mixer-v1.2.3/` (a synthetic payload with stand-in ARM64
binaries, as `./xtask mixer-package` lays it out). The mixer's verifier must
accept every repository here; the Go signer must keep writing the same
bytes.

| folder | what |
|---|---|
| `targets/mixer-v1.2.3/` | the payload, its catalog, compatibility, and notices |
| `ed25519/` | fixed Ed25519 keys and expiries: reproducible byte for byte |
| `rsa/` | a `./xtask mixer-keys` root (RSA-3072, RSA-PSS/SHA-256, the production key type) |

Each repository has `root.json` (its trusted root), `metadata/` (mixer-sign,
sequence 1), and `refreshed/` (mixer-refresh, sequence 2). The keys are
test-only and trusted nowhere; no private key is kept.

- Go: `desktop/internal/xtask/release/fixture_test.go` re-signs `ed25519/`
  and compares, and verifies all four with the tough-equivalent loader.
- Rust: `crates/rc2fx-device/tests/xtask_tuf_fixtures.rs` verifies all four
  with the in-place update verifier (sigstore-tuf), as the mixer does.

Regenerate (after a deliberate change to the signer or the payload format),
from `desktop/`:

    RC2FX_WRITE_XTASK_FIXTURES=1 go test ./internal/xtask/release -run TestXtaskFixtures
