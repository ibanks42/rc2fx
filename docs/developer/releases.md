# Releases

The desktop app and the RC2FX mixer runtime are versioned and released
independently: **Release** publishes the app, and **Mixer release** publishes
the signed mixer runtime, which the app downloads. RØDE firmware is a third
version, published by RØDE and only supported by RC2FX. Both workflows are
started manually.

## The three versions

| Version | Where it lives | Released as |
| --- | --- | --- |
| Desktop app | `[workspace.package] version` in `Cargo.toml`, with its Go copy `AppVersion` in `desktop/installer/installer.go`, which release preparation bumps with it | `vX.Y.Z`, with the app packages |
| RC2FX mixer runtime | `config/mixer-release.json` | `mixer-vX.Y.Z`, with a signed standalone payload |
| RØDE firmware | the versions pinned in `config/firmware-profiles.json` | RØDE's own releases; RC2FX publishes nothing |

Desktop builds download signed mixer payloads and firmware profiles by default.
The `embedded` build tag (`./xtask package --embedded`) provides an optional
bundled payload for offline use.
`config/mixer-release.json` names the mixer runtime and records its control
protocol. **Settings › Versions** shows installed versions and
compatible signed offers, including when a newer app is required. See
[Mixer releases](mixer-releases.md) for signing, publication, and renewal, and
[Versions and updates](../user/getting-started.md#versions-and-updates).
For the app itself,
`installer.CheckForUpdates` (`desktop/installer/app_update.go`) asks GitHub's `releases/latest` API for this
repository and, when a newer app is out, pages through the release list for
the notes since the installed version. It accepts only published, stable
`vMAJOR.MINOR.PATCH` tags, and it links
to release pages built from the validated tag. It never downloads anything.
The repository is a fixed constant (`AppRepository`) in that file, so builds from forks also
check the official releases.

Copies installed by Velopack (the Windows and macOS installers and portable
zips, and the AppImage) can also update in place:
`desktop/app/selfupdate`, through Velopack's C library
(`desktop/app/selfupdate/vpkc`), reads `releases.win.json`,
`releases.osx.json`, or `releases.linux.json` from `releases/latest/download`,
downloads the package the feed names when the user presses **Download
update**, and Velopack checks it against the feed's SHA-256. The feed isn't
signed, so the app also checks the package against the signed app release
metadata (see [Signed app updates](#signed-app-updates)). Its update source
(`SignedFeed`) does that as the download finishes, before Velopack renames
the `.partial` file into place: on Windows, Velopack replaces its own updater
with the package's right after that rename, so a later check would be too
late. The feed offers only full packages, never deltas, because the release
signs whole packages. A package already on disk, which Velopack doesn't
fetch again, and a package left from an earlier run, found at startup, are
checked before **Restart to update** is offered. A package the signed
metadata refuses is deleted. When the check can't be made (no network, or
the metadata can't be fetched), a package already on disk is kept and the
button reads **Check again**, which checks it again without downloading.
The update is applied only when the user chooses
**Restart to update**, never at the next launch by itself, and that restart
waits while the installer has a run going or the mixer may still be
updating after a flash. On Linux the AppImage keeps downloaded packages in
`$XDG_CACHE_HOME/rc2fx/velopack-packages` (or
`~/.cache/rc2fx/velopack-packages`), created with mode 0700, instead of Velopack's shared default under `/var/tmp`, where
another account could swap a package between its check and its use.

Copies Velopack doesn't manage, such as builds from source and development
builds, offer **Download**, which opens the release page.

## Cutting a release

1. If the mixer runtime changed, release it with **Mixer release**, choosing
   the bump; the workflow writes `config/mixer-release.json` and commits it
   (see [Mixer runtime version](#mixer-runtime-version)).
2. On x86-64 Linux, run `just package-check` before spending a GitHub release
   run. It builds the AppImage from the current working tree in the same
   AlmaLinux 8 container as a release and checks it in disposable containers.
   See
   [local package checks](building.md#check-the-linux-packages-before-a-release).
3. Commit and push everything to `main`.
4. On GitHub, open **Actions › Release › Run workflow** and choose branch
   `main`.
5. Pick **patch**, **minor**, or **major** for the app (see
   [Desktop app](#desktop-app)).
6. Run it. Enable **dry-run** to build without publishing.

A successful run packages the app for Linux, Windows, and macOS, pushes a
`chore(release): vX.Y.Z` commit on `main` with a `vX.Y.Z` tag, creates the
GitHub release as a draft, and, once every package is verified, publishes it
(see [Workflow jobs](#workflow-jobs)).

A dry run doesn't push or publish anything.

## Version selection

### Desktop app

The current version is `[workspace.package] version` in `Cargo.toml`.

Each release increases the version. The bump is applied to the tag matching
the version in `Cargo.toml`. That tag must exist, be in `main`'s history, and
have commits after it.

The run stops before building if these conditions aren't met, if the new tag
already exists, or if the checkout has uncommitted changes.

Releasing the app doesn't change `config/mixer-release.json`: release
packages don't carry the mixer runtime, and mixer changes reach users through
**Mixer release**.

### Mixer runtime version

The file is strict JSON with three fields:

```json
{ "schemaVersion": 1, "version": "1.0.0", "protocolVersion": 1 }
```

`version` is dotted numbers such as `MAJOR.MINOR.PATCH`, without a
pre-release suffix; the app compares versions numerically and treats anything
else as unknown. **Mixer release** sets it: its bump choice (patch, minor, or
major) applies to the latest published mixer release, and the workflow
commits the result. Release the mixer when anything that ends up on it
changes: the crates built into the runtime, `mixer/scripts/`,
`config/firmware-profiles.json`, the runtime's dependencies, and the
toolchain and build settings that compile it. A version bumped by hand is
kept when it equals the workflow's result, and stops the run otherwise. Mixer versions are reviewed and published independently of desktop releases;
a desktop-only release does not require a mixer bump or mixer notes.
`protocolVersion` must equal `rc2fx_core::MIXER_PROTOCOL_VERSION`;
bump it only when an older desktop app can no longer drive the mixer.

`./xtask mixer-build` copies the file into `target/mixer` and adds it to
`SHA256SUMS`. Building the app with the embedded payload, packaging, and
`./xtask mixer install` all refuse a runtime whose `mixer-release.json`
differs from `config/mixer-release.json`; rebuild the runtime after a bump. The
installer writes the file to `/opt/rc2fx/mixer-release.json` and covers it with
the bundle's `SHA256SUMS`. The mixer reports the runtime, protocol, firmware,
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

On GitHub, the published app release is marked latest and contains:

- the app packages, each built by `./xtask package` (see
  [packaging](building.md#3-package-it)). Every package carries the license
  notices in the app.

  | Platform | Files |
  | --- | --- |
  | Windows x64 | `rc2fx-windows-x64-setup.exe`, `rc2fx-windows-x64.zip` |
  | macOS (universal) | `rc2fx-macos-universal.pkg`, `rc2fx-macos-universal.zip`, and the same files as `rc2fx-macos-arm64.pkg` and `rc2fx-macos-arm64.zip` |
  | Linux x64 | `rc2fx-linux-x64.AppImage` |

  All of them are made by Velopack's `vpk`. Their names have no version, so
  `releases/latest/download/NAME` always names the newest one, and an AppImage
  update replaces the file in place. The `-setup.exe`
  installs per user, without an administrator prompt, into
  `%LOCALAPPDATA%\io.github.ibanks42.rc2fx`, with a Start menu shortcut and an
  **Installed apps** entry. The `.pkg` installs `RC2FX.app` into
  `/Applications`, or `~/Applications` with **Install for me only**. The zips are portable copies that can update themselves
  too. The macOS app is universal (Apple silicon and Intel, macOS 13 or
  later), built on an Apple silicon runner. Releases up to 0.1.7 shipped an
  Apple silicon-only app as `rc2fx-macos-arm64.*`, and links to
  `releases/latest/download/` under those names are published, so the Release
  workflow also uploads the universal files under those names;
  `macos_release.py` refuses them unless they are byte-identical. Drop the
  copies (`LEGACY_ALIASES` in `macos_release.py` and the copy step in
  `release.yml`) once those links no longer matter.
- the update packages and feeds the app updates itself from:
  `io.github.ibanks42.rc2fx-VERSION-full.nupkg` and `releases.win.json` for
  Windows, `io.github.ibanks42.rc2fx-VERSION-osx-full.nupkg` and
  `releases.osx.json` for macOS, `io.github.ibanks42.rc2fx-VERSION-linux-full.nupkg`
  and `releases.linux.json` for the AppImage. They keep vpk's names, because
  each feed names its package. The app reads them from the release GitHub marks latest,
  so only app releases may be latest.
- `rc2fx-VERSION-appimage-runtime-sources.tar.gz`: the source of the AppImage type 2
  runtime at the start of the AppImage, and of the libraries it links
  statically, including its modified LGPL-2.1 libfuse. See [AppImage runtime
  sources](#appimage-runtime-sources).
- a `SHA256SUMS` file for all of the above.
- the release notes.

That is twelve packages and `SHA256SUMS`. A release is only published once all
of them are there.

App packages carry no mixer payload. Release builds are download-only: the
app fetches signed mixer software when it installs RC2FX on a mixer. See
[mixer releases](mixer-releases.md).

## AppImage runtime sources

The AppImage starts with the AppImage type 2 runtime, which statically links a
modified libfuse 3.15.0 (LGPL-2.1) with squashfuse, zstd, zlib, musl, and
mimalloc. vpk puts its own copy of the runtime there
(`vendor/appimagekit-runtime-x86_64` in the vpk NuGet package). That copy is a
continuous build of type2-runtime commit `3d17002` from 2026-03-07; no tagged
release holds that binary, and the commit differs from the 20251108 release
only in CI files. `.github/scripts/prepare-appimage-notices.py` runs before
Linux packaging. It writes the runtime's license notices to
`target/appimage-legal/notices.json`, which packaging adds to the app's
notices, and the corresponding source archive to `target/package`, which the
release attaches beside the AppImage as
`rc2fx-VERSION-appimage-runtime-sources.tar.gz`.

The script takes the runtime from the vpk package, checked against the
package's pinned SHA-256 (the one `setup-vpk.sh` installs), the runtime's own
SHA-256, and the commit the binary reports. It fetches the runtime source at
that commit (checked by git), libfuse
and squashfuse at the SHA-256 sums the runtime's own build recipe pins, and
the Alpine build recipes of zstd, zlib, musl, and mimalloc at a pinned aports
commit, from Alpine's official GitHub mirror (`alpinelinux/aports`, also
checked by git). Their sources come from Alpine's distfiles mirror, checked
against the recipes' SHA-512 sums. It fails if any download, checksum, or
license text is missing or wrong. The runtime's recipe doesn't pin those four
Alpine packages, so the script uses the last aports 3.21-stable commit before
the runtime was built; the archive's README and `SOURCES.json` mark those revisions as
inferred. The archive supports rebuilding the runtime with a modified libfuse;
it isn't claimed to rebuild a byte-identical runtime. The notices record the
runtime's SHA-256 and size, and packaging refuses an AppImage that doesn't
start with exactly that runtime.

When the vpk version changes, update `VPK_VERSION` and `VPK_SHA256` in the
script with `setup-vpk.sh`, and check whether its runtime changed: if it did,
update `RUNTIME_SHA256`, `RUNTIME_COMMIT`, and `APORTS_COMMIT` together. Its offline tests
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
  `refactor`, `docs`, `perf`, `test`, `chore`, or `ci`. An optional scope in
  parentheses is shown in the changelog, and `!` before the colon
  (`feat!:`) marks a breaking change.
- After the colon, write a short, human-readable summary of what changed,
  in the imperative and without a trailing period. The changelog shows this
  text, capitalized, under the prefix's section.
- Put the reasoning and how the change was validated in the body. The body
  isn't included in the changelog.
- Don't use `chore(release)`: it is reserved for the release workflow's own
  commit, which the changelog skips.

Every app release's notes end with a `Mixer software is downloaded separately
by the firmware installer.` line. The installed mixer version is visible in
the app's **Settings › Versions**.

## Workflow jobs

**Release** (`.github/workflows/release.yml`) runs these jobs. Nothing is
public until the release is complete.

1. **prepare** (Ubuntu) computes the app version and rewrites the version
   files and changelog (`.github/scripts/release.py prepare`). It validates the
   pending app notes and builds the documentation without deploying it. The
   site build still includes only published history. It shares the prepared
   version files with later jobs; mixer payloads are built by the separate
   **Mixer release** workflow.
2. **build** (Windows), **linux**, and **macos** run in parallel. **build**
   and **macos** apply the prepare output, set up the Go version
   `desktop/go.mod` names, fetch the gpuikit library
   (`desktop/scripts/fetch-gpuikit.sh`, reading the bindings repository's
   releases with the `GPUIKIT_TOKEN` secret, or the workflow's own token
   without one) and Velopack's C library (`desktop/scripts/fetch-velopack.sh`),
   both checked against pinned SHA-256s, install .NET and a checked vpk
   (`.github/scripts/setup-vpk.sh` verifies the NuGet package's SHA-256), and
   run `./xtask package` in download-only mode; **build** also uses MinGW-w64
   gcc for cgo and its `windres` for the icon; **macos** also signs and
   notarizes the macOS packages. **build** builds with `--build-only`, signs
   in to Azure only then, so the session is fresh when signing starts, and
   packages with `--skip-build`.
   With signing configured it then checks that `RC2FX.exe`, `Update.exe`, and
   `current/rc2fx.exe` in the zip, and the `-setup.exe`, have valid,
   timestamped Authenticode signatures. See [Signing](#signing).
   Packaging verifies the executable's payload mode, including with
   `--skip-build`, and rejects a mismatched executable. Explicit `--embedded`
   packaging also compares the embedded payload with the build inputs and
   rejects a stale executable.

   **linux** applies the prepare output and runs
   `bash .github/scripts/package-check.sh` with Docker, as
   `just package-check` does (see
   [local package checks](building.md#check-the-linux-packages-before-a-release)),
   in the published build image (below) instead of building one.
   It builds the AppImage in that AlmaLinux 8 image, so the AppImage needs only
   glibc 2.28, whatever the runner has. The container installs the same vpk,
   prepares the AppImage runtime's notices and sources from that vpk package,
   and packages. The AppImage is then extracted and checked in clean
   containers of AlmaLinux 8, Ubuntu 22.04, Debian 12, Ubuntu 24.04, Debian 13,
   and Fedora 44. The checks cover libraries, metadata, permissions,
   payload-mode checks, and a headless executable smoke test. They do not
   exercise a desktop session, FUSE mounting, or mixer USB access.
3. **release** (Ubuntu) checks the Linux and Windows files, writes
   checksums for them, creates and pushes the release commit and tag
   (`release.py commit`), and creates a draft release with these packages and
   a pending notice for macOS in its notes. The checks require exactly the
   expected files. On Linux, the AppImage must start with an AppImage type 2
   header, the Linux `.nupkg` must hold exactly this AppImage, and
   `releases.linux.json` must list this version's package with its actual
   SHA-256. On Windows, the `-setup.exe` must start with `MZ`, the zip
   must hold `RC2FX.exe`, `Update.exe`, and `current/rc2fx.exe`, the `.nupkg`
   must hold `lib/app/rc2fx.exe`, and `releases.win.json` must list exactly
   this version's package with its actual SHA-256. If `main` has moved since
   the run started, it stops without pushing. Start a new run in that case.
4. **publish** (Ubuntu) waits for **release** and **macos**, then finishes the
   draft with `.github/scripts/macos_release.py publish`: it checks the macOS
   files, uploads them, rebuilds `SHA256SUMS` from the draft's actual assets,
   removes the pending notice, checks the whole draft again, and only then
   publishes it as the latest release, as its very last step. It re-reads the
   release before every change. A failed upload, a digest that doesn't match,
   or a missing or unexpected package leaves the draft unpublished. It never
   replaces a different macOS file already on the draft. Run again on a
   published release, it only checks that the release is complete and holds
   the same macOS files, and changes nothing. Publication jobs are serialized,
   and an older draft cannot become latest after a newer stable release has
   been published. After publication, a separate step dispatches **Docs**
   with `source=latest-release`, which builds the site from the new
   release's tag.
5. **app-metadata** calls **Sign app release metadata** once **publish** has
   finished, to sign the published release for in-app updates (see
   [Signed app updates](#signed-app-updates)). It passes
   `expect-artifacts: true`, so only the packages this run built are signed.
   It needs a reviewer's approval in the `mixer-signing` environment, and is
   skipped while `APP_UPDATES_ENABLED` isn't `true`.

A dry run builds every package but doesn't push, create the draft, publish, or
sign.
Only one release runs at a time. Starting another one queues it.

If **macos** fails, or no macOS runner is available, the draft stays
unpublished and nothing is lost. Use **Re-run failed jobs** on the **Release**
run: it reruns **macos** and then **publish**.

If you publish the draft by hand before **publish** finishes, the release is
public but incomplete until it does. **publish** then adds what's missing
without publishing it again or changing which release is latest. Don't do this
once immutable releases are on in the repository settings: a published release
that is immutable and incomplete can't be finished.

Immutable releases can be turned on once mixer metadata has moved to its branch
(see [Where metadata lives](mixer-releases.md#where-metadata-lives)). Nothing else
writes to a published release: the app release stays an editable draft until
**publish** has attached the macOS files, and mixer payload releases are published
once. Correcting either one takes a new version.

The **Check** workflow lints, tests, and audits the code on pushes to `main`
and on pull requests to it, excluding documentation-only changes, and runs
the release tooling's tests. Its **desktop** job checks the Go app on Ubuntu,
Windows, and macOS: `gofmt` on Linux, then `go vet` and `go test` for every
package but the UI (the packages that depend on gpuikit: `app`, `app/uitest`,
and `cmd/rc2fx`), the transport's tests with the `mockmixer` tag, a vet with
the `rc2fxdev` and `embedded` tags (against an empty stand-in payload), and,
on Linux, `govulncheck` over those packages' Go dependencies. Its
**desktop-ui** job fetches the gpuikit download and Velopack's C library on
all three systems, vets the UI with and without `mockmixer` and with the
development and embedded tags, builds it with `-tags velopack` as releases
do (with the release link flags), and runs `go test ./app/... ./cmd/rc2fx`;
on Linux it also runs `govulncheck` over `cmd/rc2fx`, the shipped app with its
UI. The offscreen UI tests in `app/uitest` need gpuikit's diagnostic library,
which the downloads do not have, so they skip there. A system whose gpuikit SHA-256 is not yet pinned in
`desktop/scripts/fetch-gpuikit.sh` is skipped with a warning (Release's
**prepare** refuses to start until all three are pinned). The desktop job
also fetches Velopack's C library and vets its bindings
(`app/selfupdate/...` with `-tags velopack` and cgo), so they are checked on
every system without gpuikit. The
**desktop-alma** job builds the app and tests the installer (with the
vendored libext2fs) in the AlmaLinux 8 image releases build the AppImage
in. Both jobs read the gpuikit bindings release with the `GPUIKIT_TOKEN`
secret, falling back to the workflow's own token; a pull request from a
fork (which gets no secrets) that cannot fetch it skips the UI build with a
warning, and any other run that cannot fetch it fails. For the Rust crates, on Linux it formats, lints, and tests in that same
image. The Linux job builds without debug info and, through
`.github/cargo-ci.toml`, with unoptimized dependencies, which more than
halves a cold build. Only runs on `main` save build caches, Rust and Go
alike; pull requests read them, and a twice-weekly scheduled run keeps them
from expiring.

The **Build image** workflow publishes that image,
`.github/scripts/package-check.Containerfile`, as
`ghcr.io/ibanks42/rc2fx-build`. Its tag is a digest of the Containerfile and
`rust-toolchain.toml` (`.github/scripts/build-image-tag.sh`), so **Check**
and **Release** build and push it only when one of those changed, and reuse
it otherwise. A weekly run, or running the workflow by hand, rebuilds the
current tag so its packages, including the libraries the AppImage bundles,
pick up updates.
Release preparation checks the docs before publication, so removing automatic
docs deployment does not remove that check.

## Signing

vpk signs the Windows and macOS packages when the signing settings exist.
Without them, the release still builds and the run shows a warning: the
Windows packages are unsigned and Windows users see an unknown-publisher
warning; the macOS app is signed ad hoc only, its `.pkg` is unsigned, and
macOS users have to allow the app in Privacy & Security.
vpk signs nothing on Linux; the AppImage's updates are checked against the
signed app release metadata (see [Signed app updates](#signed-app-updates)).

### Windows

Windows packages are signed with Azure Artifact Signing. vpk signs
`rc2fx.exe`, its launcher, the updater, and the `-setup.exe`. The **build** job
signs in through GitHub's OIDC token with `azure/login`, so there is no
secret. It needs these repository variables:

| Variable | Value |
| --- | --- |
| `AZURE_TENANT_ID` | The Microsoft Entra tenant ID |
| `AZURE_CLIENT_ID` | The client ID of the app registration with a federated credential for this repository. It needs a signing role on the certificate profile only, not a subscription. |
| `ARTIFACT_SIGNING_ENDPOINT` | The Artifact Signing account's endpoint URL |
| `ARTIFACT_SIGNING_ACCOUNT` | The Artifact Signing account name |
| `ARTIFACT_SIGNING_PROFILE` | The certificate profile name |

Set all five or none. `.github/scripts/windows-signing.ps1` writes the
Artifact Signing metadata file vpk signs with, limited to the Azure CLI
session `azure/login` leaves behind. To sign a local build, write a metadata
file like it, sign in with `az login`, and set
`RC2FX_ARTIFACT_SIGNING_METADATA` to the file's path.

### macOS

`./xtask package` builds `RC2FX.app`. vpk signs it with the Developer ID
Application certificate, signs the `.pkg` with the Developer ID Installer
certificate, and notarizes and staples both, so users can open them without
visiting Privacy & Security. The **macos** job needs these repository secrets:

| Secret | Value |
| --- | --- |
| `MACOS_CERTIFICATE` | The Developer ID Application certificate and key, as a base64 `.p12` |
| `MACOS_CERTIFICATE_PASSWORD` | That `.p12`'s password |
| `MACOS_INSTALLER_CERTIFICATE` | The Developer ID Installer certificate and key, as a base64 `.p12` |
| `MACOS_INSTALLER_CERTIFICATE_PASSWORD` | That `.p12`'s password |
| `MACOS_NOTARY_KEY` | The App Store Connect API key (`.p8` contents) |
| `MACOS_NOTARY_KEY_ID` | That key's ID |
| `MACOS_NOTARY_ISSUER` | The App Store Connect issuer ID |

Set all seven or none. `.github/scripts/macos-signing.sh` loads the
certificates into a temporary keychain and stores the API key there as a
notarytool profile. A rejected notarization fails the **macos** job and leaves
the draft unpublished. Local builds use the same steps through
`RC2FX_MACOS_IDENTITY` and `RC2FX_MACOS_INSTALLER_IDENTITY` (the two
certificates' full names; set both or neither), and `RC2FX_NOTARY_PROFILE` and
`RC2FX_MACOS_KEYCHAIN` (a notarytool profile and the keychain that holds it;
set both or neither, to notarize). The app is signed with
`desktop/packaging/rc2fx.entitlements`, which grants no hardened-runtime exceptions.

## Signed app updates

Before applying a downloaded update package, the app checks it against signed
TUF metadata (`desktop/installer/app_release.go`). This is a separate TUF
repository from the mixer's, with the same trust root
(`config/mixer-update-root.json`, compiled into the app) and the same release
keys, so nothing about mixer releases or older apps changes. The package must
be the target `vVERSION/ASSET`, with the custom field `{"version": VERSION}`,
and match its signed length and SHA-256. A package that doesn't is deleted.
This applies to every package Velopack downloads: Windows, macOS, and the
AppImage.

### The `app-metadata` branch

Release assets never change, but the metadata is re-signed at every renewal, so
it lives on the orphan branch `app-metadata`. The branch holds only the
numbered root files, `targets.json`, `snapshot.json`, `timestamp.json`, and a
short README. The app reads it from
`https://raw.githubusercontent.com/ibanks42/rc2fx/app-metadata/` and the
packages from `https://github.com/ibanks42/rc2fx/releases/download/`, and
refuses any other host. Every asset of the latest app release except
`SHA256SUMS` is one target; only the latest release is listed. Each check
starts from the compiled-in root with no saved state, in a temporary folder
under the installer work folder that is removed afterwards, and refuses
expired metadata. Saved state would let mixer metadata replayed at the app's
address raise the app's rollback floor and stop its updates. Without it a
replay can only offer older metadata, which can't vouch for a release
published after it, and expiry (seven days) limits how long it can hide one.

The first signing creates the branch. Until it exists, **Renew app metadata**
fails at its fetch step. Afterwards, protect `app-metadata` in the branch
rules as for `mixer-metadata` (see
[Where metadata lives](mixer-releases.md#where-metadata-lives)): against
deletion, letting GitHub Actions and the maintainer push to it, with no
required pull requests or status checks.

### Signing after publication

When **Release** has published an app release, its **app-metadata** job calls
**Sign app release metadata** (`app-metadata-sign.yml`) in the
`mixer-signing` environment, which needs a reviewer's approval. That workflow
downloads every asset of the published release
(`publish_app.py prepare`) and checks each one against GitHub's digest and the
release's `SHA256SUMS`, which must list exactly the other assets. Called by
**Release** (`expect-artifacts: true`), it also downloads that run's
`release-assets` and `macos-package` artifacts and passes them as `--built`:
the release, apart from `SHA256SUMS`, must hold exactly those files, with the
same size and SHA-256, or nothing is signed. So it signs only what that run
built, not files changed on the release in between. It then signs
targets, snapshot, and timestamp with `./xtask app-sign`, and adds them to
the branch in one commit (`publish_app.py publish`). Targets expire with the
root, as for mixer releases, and snapshot and timestamp after seven days.
Until a release is signed, apps that check updates refuse its packages with
"This update has not been signed yet. Try again later."

It can also be run by hand (**Run workflow**, with the published tag), for
example to sign a release whose automatic signing was skipped. The same gates
apply: `APP_UPDATES_ENABLED`, `main` only, the `mixer-signing` reviewers, and
the shared concurrency group. No build vouches for a manual run, so it checks
the release against GitHub's digests and its `SHA256SUMS` only.

App metadata versions are the UTC Unix time less 10⁹, set in both app
workflows. Both repositories share one root, so each one's metadata verifies
at the other's address. These versions stay below every mixer metadata
version (Unix times), so app metadata replayed at the mixer's address reads
as a rollback and is refused. `publish_app.py` refuses app metadata whose
version isn't below 1,700,000,000, so a workflow that slips back to plain
Unix time fails. The app keeps no saved state (see
[The `app-metadata` branch](#the-app-metadata-branch)), so the other
direction is bounded by expiry instead.

### Kill switch

The repository variable `APP_UPDATES_ENABLED` must be `true` for signing and
renewal to run. Set it to `false` to stop both, for example if a release key
leaks (see [If the targets key leaks](mixer-releases.md#if-the-targets-key-leaks);
the same keys sign both repositories, so rotate them for both). With it off,
new releases are not signed. Within seven days the existing metadata expires,
and apps then refuse every update: they fail closed. Turn it on before the
first release that follows an app version that checks updates.

### Renewal

**Renew app metadata** (`app-metadata.yml`) runs twice a week in the
`mixer-renewal` environment, and can be run by hand. It re-signs only snapshot
and timestamp (seven days) with `./xtask app-refresh`, keeping the exact
`targets.json`. It can't authorize other assets or outlive the root. Targets
last until the root expires. If renewal fails, investigate before the
metadata expires. Both workflows run only from `main` and share one
concurrency group, so they never write the branch at the same time.

When the root is rotated, both the `mixer-metadata` and `app-metadata` branches
need each new numbered root. Each signing adds the root it signs under; the
signer never replaces a published numbered root. There is no `--reauthorize`
for app metadata: after a key rotation, release a new app version, which is
signed with the new keys.

### Failure and recovery

- **Rerun.** Rerunning **Sign app release metadata** for the same tag is safe.
  If the branch already holds that release with the same assets, it changes
  nothing. If the run stopped before its push, it signs and publishes again
  with a newer sequence. A push that names a stale tip fails rather than
  overwriting a concurrent writer; just run it again.
- **Slow cache.** raw.githubusercontent.com caches each file for about five
  minutes. The publisher waits up to seven. If it gives up, the push has still
  happened, and apps may briefly see no update.
- **Refusals.** It refuses to sign a release older than the one already signed
  (a rollback), and the same version with different assets: publish a new
  version instead. It also refuses a draft or prerelease, an asset without a
  GitHub digest, any mismatch with `SHA256SUMS`, and, when **Release** calls
  it, a release whose files aren't exactly the ones that run built.

## Publishing documentation

Documentation deploys only after release publication or when you manually run
**Docs**. Pushing changes to `docs/` stages them for a later deployment; it
does not publish the site.

Run **Docs** from `main`, choosing:

- **main** (the default) to publish the current documentation and note
  corrections without releasing an app.
- **latest-release** to publish the tagged source of the current latest
  published app release. **Release** selects this automatically after
  publication. The workflow runs
  on `main` for the Pages environment, then checks out the selected tag to
  build the site.

Each run resolves its source when it starts, even when retrying an old run.
Build and deployment run in one job, so retrying a failed deployment also
resolves the source and rebuilds the site. The job uses one Pages concurrency
group. The regular
docs, [update histories](update-history.md), and `updates.json` deploy together.
Only published stable releases enter the history; tags on drafts do not count.

A release snapshot can replace documentation that you manually deployed from
newer `main` during the draft window. Run **Docs** with **main** again to restore
those changes.

A failed docs deployment does not undo a published app release. Retry **Docs**
independently; there is no need to rebuild packages. If publication succeeds
but its docs dispatch fails, manually dispatch **Docs** with
**latest-release**. Publishing or deleting
a release by hand also requires a manual docs deployment. During the gap,
the app can discover the release while hosted notes remain unavailable.

## Testing the release tooling locally

The release scripts have offline tests, which **Check** also runs:

```bash
python3 .github/scripts/test_release.py          # versioning; needs git-cliff 2.14.1
python3 .github/scripts/test_macos_release.py    # finishing and publishing the draft
python3 .github/scripts/test_app_release.py      # signing app releases for in-app updates
python3 .github/scripts/test_docs_source.py
python3 .github/scripts/test_update_history.py
```

The workflows themselves only run on GitHub. A **Release** dry run builds every
package without pushing, publishing, or signing anything.

## Before the first release

- Add the GitHub repository as the `origin` remote.
- Set the repository variable `APP_UPDATES_ENABLED` to `true` before
  releasing the version after the first app that checks updates (see
  [Kill switch](#kill-switch)), then protect the `app-metadata` branch once
  the first signing has created it.
- The AppImage needs glibc 2.28 or newer, the version in its AlmaLinux 8
  build container.

## Per-platform build runners

The desktop app links the gpuikit library for its platform through cgo, and
that library is built on each platform: GPUI, underneath it, compiles its
shaders only when the build machine runs macOS or Windows. Packaging is
per-platform too: vpk, Authenticode signing with Azure, and Developer ID
signing and notarization each run on their own system. So each platform
builds on its own GitHub runner.
