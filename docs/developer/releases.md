# Releases

The desktop app and the RC2FX mixer runtime are versioned independently, but
released together: the mixer runtime is only ever published inside the app
that bundles it. RØDE firmware is a third version, published by RØDE and only
supported by RC2FX. A desktop release therefore names two versions: its own
and the mixer runtime it bundles. The **Release** workflow does all of it and
is started manually.

## The three versions

| Version | Where it lives | Released as |
| --- | --- | --- |
| Desktop app | `[workspace.package] version` in `Cargo.toml` | `vX.Y.Z`, with the app packages |
| RC2FX mixer runtime | `config/mixer-release.json` | Inside each app package only; never released on its own |
| RØDE firmware | the versions pinned in `config/firmware-profiles.json` | RØDE's own releases; RC2FX publishes nothing |

Each desktop build embeds one mixer runtime, built from the same checkout.
`config/mixer-release.json` names the bundled runtime and records the protocol
version the app speaks. **Settings › Versions**
shows the app version, the installed and bundled RC2FX mixer software versions,
and the installed and newest supported RØDE firmware versions; see [Versions
and updates](../user/getting-started.md#versions-and-updates). Mixer software
and firmware are compared only with what the app bundles. For the app itself,
`rc2fx_installer::app_update` asks GitHub's `releases/latest` API for this
repository and, when a newer app is out, pages through the release list for
the notes since the installed version. It accepts only published, stable
`vMAJOR.MINOR.PATCH` tags, and it links
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

A successful run builds the mixer runtime once from that commit, packages it
into the app for every platform, and creates a GitHub release for the app and
a `chore(release): vX.Y.Z` commit on `main` with a `vX.Y.Z` tag.

A dry run doesn't push or publish anything.

## Version selection

### Desktop app

The current version is `[workspace.package] version` in `Cargo.toml`.

- First release (no stable `vX.Y.Z` tag yet): the
  version in `Cargo.toml` is released unchanged, so `0.1.0` becomes `v0.1.0`.
  The bump choice is ignored.
- Later releases: each one increases the version. The bump is applied to the tag matching the version in
  `Cargo.toml`. That tag must exist, be in `main`'s history, and have commits
  after it.

The run stops before building if these conditions aren't met, if the new tag
already exists, or if the checkout has uncommitted changes.

Every mixer runtime change reaches users in a desktop release that bundles it.
Releasing the app doesn't change `config/mixer-release.json`: an app-only
release keeps the mixer version, although the runtime is rebuilt for it.

### Mixer runtime version

The file is strict JSON with three fields:

```json
{ "schemaVersion": 1, "version": "0.1.0", "protocolVersion": 1 }
```

`version` is dotted numbers such as `MAJOR.MINOR.PATCH`, without a
pre-release suffix; the app compares versions numerically and treats anything
else as unknown. Bump it by hand when anything that ends up on the mixer
changes, and leave it alone otherwise: the crates built into the runtime,
`xtask/mixer/`, `config/firmware-profiles.json`, the runtime's dependencies,
and the toolchain and build settings that compile it. When reviewing such a
change, check that it bumps the version. The release run only warns: if
mixer-side paths changed since the previous app release while the version
stayed the same, its prepare step shows a **Mixer version unchanged**
warning and carries on. The check compares paths (the runtime crates,
`xtask/mixer/`, the mixer build code, `config/`, the root Cargo files,
toolchain and `.cargo/`), so shared dependencies and build settings can
trigger it too. It never blocks a release; whether to bump is your call. `protocolVersion` must equal `rc2fx_core::MIXER_PROTOCOL_VERSION`;
bump it only when an older desktop app can no longer drive the mixer.

`cargo xtask mixer-build` copies the file into `target/mixer` and adds it to
`SHA256SUMS`. Building the app with the embedded payload, packaging and
`cargo xtask mixer install` all refuse a runtime whose `mixer-release.json`
differs from `config/mixer-release.json`; rebuild the runtime after a bump. The
installer writes the file to `/opt/rc2fx/mixer-release.json` and covers it with
the bundle's `SHA256SUMS`. The mixer reports the runtime, protocol, firmware
and firmware profile through [`GET api/v1/version`](../reference/api.md#versions),
omitting any value it doesn't know. A runtime too old to answer returns 404, which the app shows
as **Unknown** rather than offering an update or downgrade.

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

- the app packages, named `rc2fx-VERSION-OS-ARCH.EXT`, each built by
  `cargo xtask package` (see [packaging](building.md#3-package-it)): a zip
  for Windows x64, a zip for macOS arm64 (GitHub's macOS runners are Apple
  silicon), and for Linux x64 an AppImage, a `.deb` and an `.rpm`. The macOS
  zip is added after the release is published, by a separate workflow (see
  [Workflow jobs](#workflow-jobs)); until then the release notes say it is
  pending.
  The zips hold the app, with its licence notices built in, and their own
  `SHA256SUMS`. The AppImage is the app and its licence notices in one file.
- `rc2fx-VERSION-appimage-runtime-sources.tar.gz`: the source of the AppImage type 2
  runtime at the start of the AppImage, and of the libraries it links
  statically, including its modified LGPL-2.1 libfuse. See [AppImage runtime
  sources](#appimage-runtime-sources).
- a `SHA256SUMS` file for the archives, rewritten when the macOS zip is
  added.
- the release notes.

Every app package in a release embeds the same mixer payload: the runtime
built once in that run, with the catalog, `mixer-release.json` and scripts
from the same checkout, as one checksummed envelope inside the executable.
The runtime is rebuilt for every release, so two releases with the same mixer
version aren't promised to carry byte-identical runtimes.

## AppImage runtime sources

The AppImage starts with the AppImage type 2 runtime, which statically links a
modified libfuse 3.15.0 (LGPL-2.1) with squashfuse, zstd, zlib, musl and
mimalloc. `.github/scripts/prepare-appimage-notices.py` runs before Linux
packaging. It writes the runtime's licence notices to
`target/appimage-legal/notices.json`, which packaging adds to the app's
notices, and the corresponding source archive to `target/package`, which the
release attaches beside the AppImage as
`rc2fx-VERSION-appimage-runtime-sources.tar.gz`.

The script fetches the runtime at a pinned commit (checked by git), libfuse
and squashfuse at the SHA-256 sums the runtime's own build recipe pins, and
the Alpine build recipes of zstd, zlib, musl and mimalloc at a pinned aports
commit, from Alpine's official GitHub mirror (`alpinelinux/aports`, also
checked by git). Their sources come from Alpine's distfiles mirror, checked
against the recipes' SHA-512 sums. It fails if any download, checksum or
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

Every app release's notes end with a `Bundled RC2FX mixer software: VERSION.`
line naming the mixer version the packages carry, even when it didn't change. The same version is visible in the app's
**Settings › Versions**.

## Workflow jobs

Two workflows make a release. **Release** (`.github/workflows/release.yml`)
publishes it for Linux and Windows; **Release macOS**
(`.github/workflows/macos-release.yml`) then adds the macOS zip, so a
shortage of macOS runners never holds up or fails the release.

**Release** runs three jobs in order:

1. **prepare** (Ubuntu) runs the versioning tests, computes the app version
   and rewrites the version files and changelog (`.github/scripts/release.py
   prepare`). It then cross-builds the mixer runtime from the checkout with
   `cargo xtask mixer-build` and shares `target/mixer` with the later jobs,
   with the version files, so every platform packages the same version files
   and runtime.
2. **build** runs on Ubuntu and Windows in parallel. Each job applies
   the prepare output, stages the shared runtime into `target/mixer`, takes
   the scripts and catalog from its own checkout of the same commit, and runs
   `cargo xtask package` with `RC2FX_RELEASE_BUILD=1`, which selects the
   launcher's quieter diagnostics. Packaging compares the runtime, catalog,
   `mixer-release.json` and scripts embedded in the executable with the build
   inputs, including with `--skip-build`, and rejects a stale or development
   executable.
3. **release** (Ubuntu) checks each archive, writes the checksums, creates the
   release commit and tag (`release.py commit`), and publishes, with a
   pending notice for macOS in the release notes. If `main` has moved since
   the run started, it stops without pushing. Start a new run in that case.

A dry run does everything except push and publish. Only one release runs at a
time. Starting another one queues it. The run's `release-input` artifact (the
version files, release plan and mixer runtime) is kept for 90 days, because
**Release macOS** builds from it.

**Release macOS** starts by itself when a **Release** run succeeds. It uses
`.github/scripts/macos_release.py`:

1. **prepare** (Ubuntu) checks that the run was a successful **Release** run
   on `main`, that its artifact hasn't expired, and that its tag is published
   as the release commit on top of the run's source commit. After a dry run
   nothing was published, so the automatic run skips the build.
2. **build** (macOS) checks out the release's source commit, applies the
   version files from the artifact (only the manifests, lockfiles and
   changelog a version bump writes), stages the run's mixer runtime and runs
   `cargo xtask package`. It builds the same version and runtime as the other
   platforms, without a version bump or rebuilding them.
3. **publish** (Ubuntu) checks the zip, uploads it to the release, rebuilds
   `SHA256SUMS` from the release's actual assets and removes the pending
   notice. Which release is marked latest doesn't change. It never replaces
   a different macOS zip that is already there, and running it again after
   success changes nothing but `SHA256SUMS`.

If no macOS runner is available, only **Release macOS** is cancelled or
fails; the release stays published without the macOS zip. A real build error
shows as a failed **Release macOS** run. To retry, open **Actions › Release
macOS › Run workflow** and enter the ID of the original **Release** run (the
number in its URL). A retry is refused once that run's artifact has expired;
make a new release then.

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

act can't run the Windows or macOS builds, **Release macOS**, or publish the
release. `python3 .github/scripts/test_macos_release.py` tests the macOS
helper offline. act builds
your working tree instead of a clean checkout, so a
passing local run doesn't guarantee the real run will pass.

## Before the first release

- Add the GitHub repository as the `origin` remote.
- Apple signing and notarization remain deferred. The macOS app is unsigned,
  so users have to allow it in Privacy & Security. The macOS download is
  built only for the runner's architecture (`arm64`).
- The Linux build links against the runner's system libraries and may not run
  on very old distributions.

## Per-platform build runners

The desktop app can't be cross-compiled from Linux. GPUI, its UI library,
compiles shaders during the build only when the build machine runs macOS or
Windows. macOS and Windows builds made on Linux lack their shaders and fail to
compile. Both were attempted with cargo-zigbuild and failed this way. Fixing
it would require patching GPUI, so each platform builds on its own GitHub
runner.
