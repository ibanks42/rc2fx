# Releases

A release consists of a version bump, a changelog entry, a tag, and a GitHub
release with one desktop package per platform. The **Release** workflow does
all of it and is started manually.

## Cutting a release

1. Commit and push everything to `main`.
2. On GitHub, open **Actions › Release › Run workflow** and choose branch
   `main`.
3. Pick **patch**, **minor** or **major**.
4. Run it. Enable **dry-run** to build without publishing.

A successful run creates a GitHub release, a `chore(release): vX.Y.Z` commit
on `main` and a `vX.Y.Z` tag.

## Version selection

The current version is `[workspace.package] version` in `Cargo.toml`. Each
release increases it.

- First release (no `v*` tags): the bump is applied to the version in
  `Cargo.toml`. From `0.1.0` that gives `v0.1.1`, `v0.2.0` or `v1.0.0`.
- Later releases: the bump is applied to the tag matching the version in
  `Cargo.toml`. That tag must exist, be in `main`'s history, and have commits
  after it.

The run stops before building if these conditions aren't met, if the new tag
already exists, or if the checkout has uncommitted changes.

## Release contents

On `main`, `github-actions[bot]` adds one tagged commit. It sets the new
version in every workspace crate and in all three lockfiles (`Cargo.lock` and
the standalone lockfiles in `crates/rc2fx-native` and `crates/rc2fx-bench`),
and adds the release's section to `CHANGELOG.md`.

On GitHub, the release is marked latest and contains:

- `rc2fx-linux-x64.tar.gz`, `rc2fx-windows-x64.zip` and
  `rc2fx-macos-arm64.zip` (GitHub's macOS runners are Apple silicon). Each is
  built by `cargo xtask package` (see [packaging](building.md#3-package-it))
  and includes the documentation and its own `SHA256SUMS`.
- a `SHA256SUMS` file for the archives.
- the release notes.

## Release notes

git-cliff (`cliff.toml`) generates the notes from commit messages since the
last tag. The Conventional Commit prefix sets the section:

| Prefix | Section |
| --- | --- |
| `feat` | Features |
| `fix` | Bug fixes |
| `refactor` | Refactoring |
| `doc` | Documentation |
| `perf` | Performance |
| `test` | Testing |
| `chore`, `ci` | Maintenance |
| anything else | Other |

Release commits are excluded. Commits with no prefix are listed under
**Other**.

## Workflow jobs

`.github/workflows/release.yml` runs three jobs in order:

1. **prepare** (Ubuntu) runs the versioning tests, computes the version,
   rewrites the version files and changelog (`.github/scripts/release.py
   prepare`), and cross-builds the mixer runtime. The later jobs use this
   output, so every platform packages the same version files and runtime.
2. **build** runs on Ubuntu, Windows and macOS in parallel. Each job applies
   the prepare output and runs `cargo xtask package` with
   `RC2FX_RELEASE_BUILD=1`, which selects the launcher's quieter diagnostics.
   Packaging compares the runtime, catalog and scripts embedded in the
   executable with the build inputs, including with `--skip-build`, and
   rejects a stale or development executable.
3. **release** (Ubuntu) checks each archive, writes the checksums, creates the
   release commit and tag (`release.py commit`), and publishes. If `main` has
   moved since the run started, it stops without pushing. Start a new run in
   that case.

A dry run does everything except push and publish. Only one release runs at a
time. Starting another one queues it.

Every push and pull request also runs the **Desktop** workflow, a Linux build
of the app used as a compile check. It doesn't package, test or lint.

## Running the workflows locally

With Docker and [`gh act`](https://github.com/nektos/gh-act):

```bash
gh act push                                    # Desktop workflow
gh act workflow_dispatch -W .github/workflows/release.yml \
  --input bump=patch --input dry-run=true      # Release dry run, Linux only
python3 .github/scripts/test_release.py        # versioning tests; needs git-cliff 2.14.1
```

Under act, the release commit and tag are created inside the container and
nothing is pushed or published. Don't pass `--bind`, which would create the
commit in your own checkout. The Linux archive and checksums are written to
`.local/act-artifacts`.

act can't run the Windows and macOS builds or the publish step, and it builds
your working tree instead of a clean checkout. A passing local run doesn't
guarantee the real run will pass.

## Before the first release

- Add the GitHub repository as the `origin` remote.
- The macOS build is unsigned and not notarized, so users have to allow it in
  Privacy & Security. It is built only for the runner's architecture.
- The Linux build links against the runner's system libraries and may not run
  on very old distributions.

## Per-platform build runners

The desktop app can't be cross-compiled from Linux. GPUI, its UI library,
compiles shaders during the build only when the build machine runs macOS or
Windows. macOS and Windows builds made on Linux lack their shaders and fail to
compile. Both were attempted with cargo-zigbuild and failed this way. Fixing
it would require patching GPUI, so each platform builds on its own GitHub
runner.
