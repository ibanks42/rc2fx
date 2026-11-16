# Delta fixtures

Written by `go generate ./delta` in desktop/ (desktop/delta/internal/genfixtures);
do not edit by hand. Read by desktop/delta (Go) and
crates/rc2fx-delta/tests/shared_fixtures.rs (Rust), so the mixer's Rust
decoder keeps accepting what the desktop's Go encoder writes.

- `accept/<case>/delta` decodes to `accept/<case>/envelope` with a limit of the
  envelope's own length. `installed/<name>` are the files on the mixer; a file
  missing there is not installed.
- `accept/large-patch` stores only the delta; its envelope and installed files are
  rebuilt by both tests from the recipe in desktop/delta/internal/fixture
  (`Large`), which keeps a patch whose matches reach over 1 MiB out of the repo.
- `reject/<case>/delta` must be refused. `max` is the envelope limit (none: no
  limit). `error` is the exact message both decoders give; without it any
  refusal will do (the zstd libraries word their own errors differently).
