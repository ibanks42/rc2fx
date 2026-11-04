# Releases

The desktop app and the RC2FX mixer runtime are versioned independently. RØDE
firmware is a third version, published by RØDE and only supported by RC2FX. A
desktop release therefore names two versions: its own and the mixer runtime it
bundles. The **Release** workflow does all of it and is started manually.

## The three versions

| Version | Where it lives | Released as |
| --- | --- | --- |
| Desktop app | `[workspace.package] version` in `Cargo.toml` | `vX.Y.Z`, with the app packages |
| RC2FX mixer runtime | `config/mixer-release.json` | `mixer-vVERSION`, with `rc2fx-mixer-VERSION.tar.gz` |
| RØDE firmware | the versions pinned in `config/firmware-profiles.json` | RØDE's own releases; RC2FX publishes nothing |

Each desktop build embeds one mixer runtime. `config/mixer-release.json` names
the bundled runtime and records the protocol version the app speaks, so an
app-only release can reuse the same mixer build byte for byte. **Settings › Versions**
shows the app version, the installed and bundled RC2FX mixer software versions,
and the installed and newest supported RØDE firmware versions; see [Versions
and updates](../user/getting-started.md#versions-and-updates). Mixer software
and firmware are compared only with what the app bundles. For the app itself,
`rc2fx_installer::app_update` asks GitHub's `releases/latest` API for this
repository and, when a newer app is out, pages through the release list for
the notes since the installed version. It accepts only published, stable
`vMAJOR.MINOR.PATCH` tags, so mixer releases are never offered, and it links
to release pages built from the validated tag. It never downloads anything.
The repository is a fixed constant in that module, so builds from forks also
check the official releases.

## Cutting a release

1. If the mixer runtime changed, bump its version in
   `config/mixer-release.json` and commit the change (see [Mixer runtime
   version](#mixer-runtime-version)).
2. Commit and push everything to `main`.
3. On GitHub, open **Actions › Release › Run workflow** and choose branch
   `main`.
4. Pick **patch**, **minor** or **major** for the app. The first release
   ignores this choice (see [Desktop app](#desktop-app)).
5. Run it. Enable **dry-run** to build without publishing.

A successful run reuses the mixer runtime release named by
`config/mixer-release.json`, creates a GitHub release for the app and a
`chore(release): vX.Y.Z` commit on `main` with a `vX.Y.Z` tag.

If there is no `mixer-vVERSION` release yet, the run builds the runtime from
that commit, publishes it as a separate release before the app release, and
packages the app with exactly those bytes. That mixer release is never marked
latest. Later app releases reuse it until the mixer version changes.

A dry run doesn't push or publish either release.

## Version selection

### Desktop app

The current version is `[workspace.package] version` in `Cargo.toml`.

- First release (no stable `vX.Y.Z` tag yet; `mixer-v` tags don't count): the
  version in `Cargo.toml` is released unchanged, so `0.1.0` becomes `v0.1.0`.
  The bump choice is ignored.
- Later releases: each one increases the version. The bump is applied to the tag matching the version in
  `Cargo.toml`. That tag must exist, be in `main`'s history, and have commits
  after it.

The run stops before building if these conditions aren't met, if the new tag
already exists, or if the checkout has uncommitted changes.

Every mixer runtime change reaches users in a desktop release that bundles it.
An app-only release leaves the runtime version and its archive unchanged, and
releasing the app does not rewrite `config/mixer-release.json`.

### Mixer runtime version

The file is strict JSON with three fields:

```json
{ "schemaVersion": 1, "version": "0.1.0", "protocolVersion": 1 }
```

`version` is dotted numbers such as `MAJOR.MINOR.PATCH`, without a
pre-release suffix; the app compares versions numerically and treats anything
else as unknown. Bump it by hand when the mixer runtime or anything else in its
archive changes, and leave it alone otherwise. When reviewing a change to
mixer-side sources, check that it bumps the version; the release run refuses
to reuse a version whose sources changed. Mixer-side means everything the
source fingerprint covers: the crates built into the runtime, `xtask/mixer/`,
`config/`, the licence inputs, the `Cargo.lock` entries in the runtime's
dependency closure, the root Cargo profiles, toolchain and `.cargo/`
configuration, and the mixer build code in `xtask` (`setup.rs`, `elf.rs`). The desktop app, installer, transport and bench crates, and
lockfile entries only they use, are excluded, so app-only changes and
dependency updates don't need a bump. `protocolVersion` must equal
`rc2fx_core::MIXER_PROTOCOL_VERSION`; bump it only when an older desktop app
can no longer drive the mixer.

`cargo xtask mixer-build` copies the file into `target/mixer` and adds it to
`SHA256SUMS`. Building the app with the embedded payload, packaging and
`cargo xtask mixer install` all refuse a runtime whose `mixer-release.json`
differs from `config/mixer-release.json`; rebuild the runtime after a bump. The
installer writes the file to `/opt/rc2fx/mixer-release.json` and covers it with
the bundle's `SHA256SUMS`. The mixer reports the runtime, protocol, firmware
and firmware profile through [`GET api/v1/version`](../reference/api.md#versions),
omitting any value it doesn't know. A runtime too old to answer returns 404, which the app shows
as **Unknown** rather than offering an update or downgrade.

When the `mixer-vVERSION` release exists, prepare computes a fingerprint of the
mixer-side sources and compares it with the one recorded with that release. If
they differ, the run fails until the version in `config/mixer-release.json` is
bumped, so two different runtimes can't share a version.

A published `mixer-vVERSION` release is immutable. To change the runtime,
bump the version; don't replace its assets. Only the release workflow publishes
it, so don't create the release or upload the archive from a local machine.

### RØDE firmware

RØDE firmware versions are not released from this repository. The installer
offers the versions pinned in `config/firmware-profiles.json`, newest first,
and only after they have been checked. [Firmware support](../reference/firmware.md)
describes adding a version.

## Release contents

On `main`, `github-actions[bot]` adds one tagged commit. It sets the app
version in every workspace crate and in all three lockfiles (`Cargo.lock` and
the standalone lockfiles in `crates/rc2fx-native` and `crates/rc2fx-bench`),
and adds the release's section to `CHANGELOG.md`.

On GitHub, the app release is marked latest and contains:

- `rc2fx-linux-x64.AppImage`, `rc2fx-windows-x64.zip` and
  `rc2fx-macos-arm64.zip` (GitHub's macOS runners are Apple silicon), each
  built by `cargo xtask package` (see [packaging](building.md#3-package-it)).
  The zips hold the app, with its licence notices built in, and their own
  `SHA256SUMS`. The AppImage is the app and its licence notices in one file.
- `rc2fx-appimage-runtime-sources.tar.gz`: the source of the AppImage type 2
  runtime at the start of the AppImage, and of the libraries it links
  statically, including its modified LGPL-2.1 libfuse. See [AppImage runtime
  sources](#appimage-runtime-sources).
- a `SHA256SUMS` file for the archives.
- the release notes.

Each app package embeds the same mixer payload: the runtime files, catalog,
`mixer-release.json` and scripts from that archive, as one checksummed
envelope inside the executable. The `.tar.gz` itself isn't shipped in the app.
An app-only release therefore ships new app files with an unchanged runtime.

The separate `mixer-vVERSION` release contains:

- `rc2fx-mixer-VERSION.tar.gz`, holding `runtime/`, `xtask/mixer/`, the
  `firmware-profiles.json` and `mixer-release.json` files it runs with, and
  `LICENSES.html`, the consolidated third-party notices (`cargo xtask licenses
  FILE` writes the same file).
- the archive's SHA-256, so the app release can be checked against the
  immutable asset it reused.

## AppImage runtime sources

The AppImage starts with the AppImage type 2 runtime, which statically links a
modified libfuse 3.15.0 (LGPL-2.1) with squashfuse, zstd, zlib, musl and
mimalloc. `.github/scripts/prepare-appimage-notices.py` runs before Linux
packaging. It writes the runtime's licence notices to
`target/appimage-legal/notices.json`, which packaging adds to the app's
notices, and the corresponding source to
`target/package/rc2fx-appimage-runtime-sources.tar.gz`, which the release
attaches beside the AppImage.

The script fetches the runtime at a pinned commit (checked by git), libfuse
and squashfuse at the SHA-256 sums the runtime's own build recipe pins, and
the Alpine build recipes and sources of zstd, zlib, musl and mimalloc,
checked against Alpine's SHA-512 sums. It fails if any download, checksum or
licence text is missing or wrong. The runtime's recipe doesn't pin those four
Alpine packages, so the script uses the aports commit current when the runtime
was built; the archive's README and `SOURCES.json` mark those revisions as
inferred. The archive supports rebuilding the runtime with a modified libfuse;
it isn't claimed to rebuild a byte-identical runtime.

When the runtime pinned in the AppImage tooling changes, update
`RUNTIME_COMMIT` and `APORTS_COMMIT` in the script together. Its offline tests
are `python3 .github/scripts/test_prepare_appimage_notices.py`.

## Release notes

git-cliff (`cliff.toml`) generates the notes from commit messages since the
last tag. The Conventional Commit prefix sets the section:

| Prefix | Section |
| --- | --- |
| `feat` | Features |
| `fix` | Bug fixes |
| `refactor` | Refactoring |
| `docs` | Documentation |
| `perf` | Performance |
| `test` | Testing |
| `chore`, `ci` | Maintenance |
| anything else | Other |

Release commits are excluded. Commits with no prefix are listed under
**Other**.

### Writing commit messages

Every commit's subject becomes a changelog line, so write it for users:

```text
feat(mixer): report the installed mixer software version

Why the change was needed and how it was validated (tests run, checks on
hardware or none).
```

- Start the subject with one of the prefixes above: `feat`, `fix`,
  `refactor`, `docs`, `perf`, `test`, `chore` or `ci`. An optional scope in
  parentheses is shown in the changelog, and `!` before the colon
  (`feat!:`) marks a breaking change.
- After the colon, write a short, human-readable summary of what changed,
  in the imperative and without a trailing period. The changelog shows this
  text, capitalised, under the prefix's section.
- Put the reasoning and how the change was validated in the body. The body
  isn't included in the changelog.
- Don't use `chore(release)`: it is reserved for the release workflow's own
  commit, which the changelog skips.

An app-only release reuses the mixer runtime unchanged, so its notes and
changelog describe app changes only. Every app release's notes include a
`Bundled RC2FX mixer software VERSION` line, naming the runtime the package
carries even when it didn't change. The same version is visible in the app's
**Settings › Versions**.

## Workflow jobs

`.github/workflows/release.yml` runs three jobs in order:

1. **prepare** (Ubuntu) runs the versioning tests, computes the app version,
   reads `config/mixer-release.json`, and resolves the matching mixer runtime
   release. It verifies the archive's SHA-256 and checks the source fingerprint
   against the recorded one. If the release doesn't exist, it cross-builds the
   runtime and prepares the `mixer-vVERSION` release. It then rewrites the
   version files and changelog (`.github/scripts/release.py prepare`). The
   later jobs use this output, so every platform packages the same version
   files and runtime bytes.
2. **build** runs on Ubuntu, Windows and macOS in parallel. Each job applies
   the prepare output, stages the verified runtime from the reused archive and
   runs `cargo xtask package` with `RC2FX_RELEASE_BUILD=1`, which selects the
   launcher's quieter diagnostics. Packaging compares the runtime, catalog,
   `mixer-release.json` and scripts embedded in the executable with the build inputs, including with
   `--skip-build`, and rejects a stale or development executable.
3. **release** (Ubuntu) checks each archive, writes the checksums, creates the
   release commit and tag (`release.py commit`), and publishes. A pending
   mixer release is published first and is never marked latest. If `main` has
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

act can't run the Windows and macOS builds, publish either release, or fetch
the immutable mixer asset from GitHub. Use a real workflow run to exercise
payload reuse. act builds your working tree instead of a clean checkout, so a
passing local run doesn't guarantee the real run will pass.

## Before the first release

- Add the GitHub repository as the `origin` remote.
- Apple signing and notarization remain deferred. The macOS app is unsigned,
  so users have to allow it in Privacy & Security. The macOS download is
  `rc2fx-macos-arm64.zip`, built only for the runner's architecture.
- The Linux build links against the runner's system libraries and may not run
  on very old distributions.

## Per-platform build runners

The desktop app can't be cross-compiled from Linux. GPUI, its UI library,
compiles shaders during the build only when the build machine runs macOS or
Windows. macOS and Windows builds made on Linux lack their shaders and fail to
compile. Both were attempted with cargo-zigbuild and failed this way. Fixing
it would require patching GPUI, so each platform builds on its own GitHub
runner.
