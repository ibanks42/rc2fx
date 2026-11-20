# Releases

The desktop app and the RC2FX mixer runtime are versioned and released
separately: the **Release** workflow publishes the desktop app, while **Mixer
release** publishes the signed mixer runtime that the app downloads. RØDE
firmware is a third independent version published by RØDE and qualified by
RC2FX. Both GitHub workflows are triggered manually.

## The three versions

| Version | Where it lives | Released as |
| --- | --- | --- |
| Desktop app | `config/app-release.json`, with its Go copy `AppVersion` in `desktop/installer/installer.go` (updated alongside it during release preparation) | `vX.Y.Z`, with the app packages |
| RC2FX mixer runtime | `config/mixer-release.json` | `mixer-vX.Y.Z`, with a signed standalone payload |
| RØDE firmware | Versions pinned in `config/firmware-profiles.json` | RØDE's own releases; RC2FX does not publish firmware |

Desktop release builds download signed mixer payloads and firmware profiles by
default. The `embedded` build tag (`./xtask package --embedded`) adds an
optional bundled payload for offline use. `config/mixer-release.json` names the
mixer runtime version and its control protocol version. **Settings › Versions**
displays installed versions and compatible signed offers, including when a
newer desktop app is required. See [Mixer releases](mixer-releases.md) for
signing, publishing, and renewal, and
[Versions and updates](../user/getting-started.md#versions-and-updates) for the
user-facing behavior.

For desktop app updates, `installer.CheckForUpdates`
(`desktop/installer/app_update.go`) queries GitHub's `releases/latest` API for
this repository and, when a newer version is available, pages through the
release list to gather notes since the installed version. It only accepts
published, stable `vMAJOR.MINOR.PATCH` tags, constructs release page links from
the validated tag, and never downloads binaries on its own. Because the
repository URL is hardcoded in that file's constants, builds from forks still
check the official releases.

Copies managed by Velopack (the Windows and macOS installers, portable zips,
and the Linux AppImage) can also self-update in place. `desktop/app/selfupdate`
calls Velopack's C library (`desktop/app/selfupdate/vpkc`) to fetch
`releases.win.json`, `releases.osx.json`, or `releases.linux.json` from
`releases/latest/download`, downloads the referenced package when the user
clicks **Download update**, and lets Velopack check the feed's SHA-256. Because
the feed itself is unsigned, the app also verifies the package against signed
app release metadata (see [Signed app updates](#signed-app-updates)). Its
custom update source (`SignedFeed`) runs that verification as the download
completes, before Velopack renames the `.partial` file into place. (On Windows,
Velopack replaces its own updater with the package's copy right after that
rename, so checking any later would be too late.) The feed only serves full
packages, never deltas, since the release signs whole packages. Any package
already on disk (which Velopack won't redownload) or left over from a previous
session is re-verified before **Restart to update** appears. If the signed
metadata rejects a package, it is deleted. If verification can't run (for
instance, offline or metadata unreachable), an already-downloaded package is
kept and the button switches to **Check again** so the user can retry without
redownloading. Updates are only applied when the user clicks **Restart to
update**, never silently on next launch, and restarting is blocked while the
installer is running or the mixer may still be finishing a flash. On Linux, the
AppImage stores downloaded packages in
`$XDG_CACHE_HOME/rc2fx/velopack-packages` (or
`~/.cache/rc2fx/velopack-packages`), created with mode `0700`, instead of
Velopack's shared `/var/tmp` default where another local user could swap a
package between verification and install.

Builds not managed by Velopack (such as source builds and development builds)
show **Download**, which opens the GitHub release page.

## Cutting a release

1. If the mixer runtime changed, release it first via **Mixer release** and
   pick the version bump; that workflow updates `config/mixer-release.json` and
   commits it (see [Mixer runtime version](#mixer-runtime-version)).
2. On x86-64 Linux, run `just package-check` before kicking off a GitHub
   release. It builds the AppImage from your working tree in the same
   AlmaLinux 8 container used in CI and checks it in clean distro containers.
   See [local package checks](building.md#check-the-linux-packages-before-a-release).
3. Commit and push everything to `main`.
4. On GitHub, open **Actions › Release › Run workflow** targeting `main`.
5. Choose **patch**, **minor**, or **major** for the app (see
   [Desktop app](#desktop-app)).
6. Run the workflow (enable **dry-run** to test packaging without publishing).

A full run packages the app for Linux, Windows, and macOS, pushes a
`chore(release): vX.Y.Z` commit and `vX.Y.Z` tag to `main`, creates a draft
GitHub release, and publishes it once every artifact passes verification (see
[Workflow jobs](#workflow-jobs)).

Dry runs build everything without pushing commits, creating tags, or publishing
anything.

## Version selection

### Desktop app

The current app version lives in `config/app-release.json`.

Every release must move the version forward. The chosen bump is applied to the
tag matching `config/app-release.json`, which must already exist in `main`'s
history with at least one commit after it.

The workflow aborts before building if those conditions aren't met, if the
target tag already exists, or if the checkout has uncommitted changes.

Releasing the desktop app does not touch `config/mixer-release.json`. Shipped
app packages don't embed the mixer runtime; mixer changes go out through
**Mixer release**.

### Mixer runtime version

`config/mixer-release.json` is strict JSON with three fields:

```json
{ "schemaVersion": 1, "version": "1.0.0", "protocolVersion": 1 }
```

`version` is dotted integers like `MAJOR.MINOR.PATCH` with no pre-release
suffix; the app compares versions numerically and treats anything else as
unknown. **Mixer release** calculates the new version by applying your chosen
bump (patch, minor, or major) to the latest published mixer release and commits
the update. Cut a mixer release whenever anything deployed to the mixer
changes: the runtime crates, `mixer/scripts/`, `config/firmware-profiles.json`,
runtime dependencies, or the toolchain and build settings that compile it. If
you bumped the version manually, the workflow accepts it if it matches the
computed bump and aborts otherwise. Mixer versions are reviewed and released
independently of desktop releases; a desktop-only release needs no mixer bump
or mixer notes. `protocolVersion` must match `rc2fx_core::MIXER_PROTOCOL_VERSION`;
only bump it when older desktop apps can no longer talk to the mixer.

`./xtask mixer-build` copies `config/mixer-release.json` into `target/mixer`
and includes it in `SHA256SUMS`. Embedded builds, packaging, and
`./xtask mixer install` all reject a runtime whose `mixer-release.json` doesn't
match `config/mixer-release.json`, so rebuild the runtime after any bump. The
installer writes this file to `/opt/rc2fx/mixer-release.json` (covered by the
bundle's `SHA256SUMS`). The mixer reports its runtime, protocol, firmware, and
firmware profile via [`GET api/v1/version`](../reference/api.md#versions),
omitting unknown fields. Older runtimes that don't implement that endpoint
return 404, which the app shows as **Unknown** rather than proposing an update
or downgrade.

### RØDE firmware

RØDE firmware isn't released from this repository. The installer offers the
versions pinned in `config/firmware-profiles.json`, newest first, only after
they have been audited. See [Firmware support](../reference/firmware.md) for
how to qualify a new version.

## Release contents

On `main`, `github-actions[bot]` pushes a single tagged commit that updates the
app version across all workspace crates and all three lockfiles (`Cargo.lock`,
`crates/rc2fx-native/Cargo.lock`, and `crates/rc2fx-bench/Cargo.lock`), and
prepends the new release section to `CHANGELOG.md`.

On GitHub, the published app release is marked latest and includes:

- The app packages built by `./xtask package` (see
  [packaging](building.md#3-package-it)), each with license notices embedded in
  the app:

  | Platform | Files |
  | --- | --- |
  | Windows x64 | `rc2fx-windows-x64-setup.exe`, `rc2fx-windows-x64.zip` |
  | macOS (universal) | `rc2fx-macos-universal.pkg`, `rc2fx-macos-universal.zip`, plus identical copies named `rc2fx-macos-arm64.pkg` and `rc2fx-macos-arm64.zip` |
  | Linux x64 | `rc2fx-linux-x64.AppImage` |

  All packages are built with Velopack's `vpk`. Their names have no version
  number so `releases/latest/download/NAME` always resolves to the newest build
  and AppImage updates can replace the file in place. `-setup.exe` installs per
  user without UAC into `%LOCALAPPDATA%\io.github.ibanks42.rc2fx`, adding a
  Start menu shortcut and an **Installed apps** entry. The `.pkg` installs
  `RC2FX.app` into `/Applications` (or `~/Applications` with **Install for me
  only**). The zips are portable copies that still support self-updates. The
  macOS app is universal (Apple silicon and Intel, macOS 13+), built on an
  Apple silicon runner. Releases through 0.1.7 shipped Apple silicon-only
  assets as `rc2fx-macos-arm64.*`, so the Release workflow also uploads
  byte-identical copies under those names so existing
  `releases/latest/download/` links keep working (`macos_release.py` verifies
  they are byte-identical). Remove `LEGACY_ALIASES` in `macos_release.py` and
  the copy step in `release.yml` once those old links are no longer needed.
- The Velopack update packages and feeds:
  `io.github.ibanks42.rc2fx-VERSION-full.nupkg` and `releases.win.json` for
  Windows, `io.github.ibanks42.rc2fx-VERSION-osx-full.nupkg` and
  `releases.osx.json` for macOS, and
  `io.github.ibanks42.rc2fx-VERSION-linux-full.nupkg` and `releases.linux.json`
  for Linux. These keep vpk's filenames because each feed references its
  package by name. Because the app fetches feeds from GitHub's latest release,
  only app releases should ever be marked latest.
- `rc2fx-VERSION-appimage-runtime-sources.tar.gz`: source code for the
  AppImage type 2 runtime prepended to the AppImage and its statically linked
  libraries, including its modified LGPL-2.1 libfuse. See
  [AppImage runtime sources](#appimage-runtime-sources).
- `SHA256SUMS` covering every asset above.
- The release notes.

That totals twelve packages plus `SHA256SUMS`. A release is never published
until every single file is present and verified.

App packages do not embed a mixer payload. Release builds are download-only and
fetch signed mixer software during installation. See
[mixer releases](mixer-releases.md).

## AppImage runtime sources

The AppImage begins with the AppImage type 2 runtime, which statically links a
modified libfuse 3.15.0 (LGPL-2.1) alongside squashfuse, zstd, zlib, musl, and
mimalloc. `vpk` embeds its own copy of that runtime
(`vendor/appimagekit-runtime-x86_64` inside the vpk NuGet package), which is a
continuous build of `type2-runtime` commit `3d17002` from 2026-03-07 (no tagged
release holds that binary; the commit only differs from release `20251108` in
CI files). Before Linux packaging,
`.github/scripts/prepare-appimage-notices.py` writes the runtime's license
notices to `target/appimage-legal/notices.json` (which packaging merges into
the app's notices) and creates `rc2fx-VERSION-appimage-runtime-sources.tar.gz`
in `target/package` to upload alongside the AppImage.

The script extracts the runtime from the vpk package and verifies the package's
pinned SHA-256 (matching `setup-vpk.sh`), the runtime binary's SHA-256, and the
commit string reported by the binary. It then fetches the runtime source at
that git commit, libfuse and squashfuse at the SHA-256 hashes pinned in the
runtime's own build recipe, and Alpine build recipes for zstd, zlib, musl, and
mimalloc from a pinned commit on `alpinelinux/aports` (verified by git), with
their tarballs fetched from Alpine's distfiles mirror and verified against the
recipes' SHA-512 hashes. Any missing or mismatched download, checksum, or
license file fails the script. Because the runtime's build recipe doesn't pin
those four Alpine packages, the script pins the last `3.21-stable` aports
commit before the runtime's build date; the archive's README and `SOURCES.json`
explicitly mark those revisions as inferred. The archive is meant to allow
rebuilding the runtime with a modified libfuse, not to guarantee a
byte-identical binary. The generated notices record the runtime's exact SHA-256
and size, and packaging rejects any AppImage that doesn't start with that exact
runtime.

Whenever vpk is updated, update `VPK_VERSION` and `VPK_SHA256` in
`prepare-appimage-notices.py` alongside `setup-vpk.sh`, and check if the
embedded runtime changed; if so, update `RUNTIME_SHA256`, `RUNTIME_COMMIT`, and
`APORTS_COMMIT` together. Run its offline tests with
`python3 .github/scripts/test_prepare_appimage_notices.py`.

## Release notes

`git-cliff` (`cliff.toml`) builds the release notes from commits since the last
tag, grouping entries by Conventional Commit prefix:

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

Release commits are skipped. Commits without a recognized prefix fall under
**Other**.

### Writing commit messages

Because each commit's subject becomes a line in the changelog, write it with
users in mind:

```text
feat(mixer): report the installed mixer software version

Why the change was needed and how it was validated (tests run, checks on
hardware or none).
```

- Start the subject with one of the prefixes above (`feat`, `fix`, `refactor`,
  `docs`, `perf`, `test`, `chore`, or `ci`). An optional scope in parentheses
  appears in the changelog, and `!` before the colon (`feat!:`) flags a
  breaking change.
- Follow the colon with a concise, imperative summary with no trailing period.
  The changelog capitalizes it under the matching section.
- Put motivation and validation details in the commit body (bodies are omitted
  from the changelog).
- Don't use `chore(release)`; that prefix is reserved for the release workflow's
  automated commit, which the changelog filters out.

Every app release's notes end with `Mixer software is downloaded separately by
the firmware installer.` Users can check their installed mixer version in
**Settings › Versions**.

## Workflow jobs

**Release** (`.github/workflows/release.yml`) executes five jobs. Nothing
becomes public until every package is verified.

1. **prepare** (Ubuntu) calculates the next app version and updates the version
   files and changelog via `.github/scripts/release.py prepare`. It validates
   pending app notes and test-builds the documentation without deploying it
   (only published history enters the site build). It passes the updated
   version files to downstream jobs; mixer payloads are handled separately by
   **Mixer release**.
2. **build** (Windows), **linux**, and **macos** run concurrently. **build**
   and **macos** apply the prepare output, install the Go version from
   `desktop/go.mod`, fetch gpuikit (`desktop/scripts/fetch-gpuikit.sh`, using
   `GPUIKIT_TOKEN` or falling back to the workflow token) and Velopack's C
   library (`desktop/scripts/fetch-velopack.sh`) verified against pinned
   SHA-256s, install .NET and a checksummed vpk (`.github/scripts/setup-vpk.sh`),
   and run `./xtask package` in download-only mode. **build** also sets up
   MinGW-w64 gcc for cgo and `windres` for the icon, running `--build-only`
   first, authenticating to Azure only right before signing so the token is
   fresh, and finishing with `--skip-build`. When signing is configured, it
   verifies that `RC2FX.exe`, `Update.exe`, `current/rc2fx.exe` inside the zip,
   and `-setup.exe` all carry valid, timestamped Authenticode signatures (see
   [Signing](#signing)). **macos** signs and notarizes the macOS artifacts.
   Packaging verifies the binary's payload mode even with `--skip-build` and
   rejects mismatches; explicit `--embedded` packaging also checks the embedded
   payload against current inputs and rejects stale binaries.

   **linux** applies the prepare output and runs
   `bash .github/scripts/package-check.sh` under Docker (the same script
   `just package-check` runs, see
   [local package checks](building.md#check-the-linux-packages-before-a-release)),
   pulling the prebuilt AlmaLinux 8 build image instead of building it locally.
   Building inside AlmaLinux 8 keeps the glibc requirement at 2.28 regardless
   of the GitHub runner's OS. The container installs the same vpk, generates
   AppImage runtime notices and sources, and packages the AppImage. It then
   extracts and tests the AppImage inside clean containers of AlmaLinux 8,
   Ubuntu 22.04, Debian 12, Ubuntu 24.04, Debian 13, and Fedora 44, checking
   shared libraries, metadata, permissions, payload mode, and a headless smoke
   test (desktop sessions, FUSE mounts, and USB access aren't exercised here).
3. **release** (Ubuntu) validates the Linux and Windows artifacts, generates
   their checksums, creates and pushes the release commit and tag
   (`release.py commit`), and opens a draft GitHub release containing those
   packages plus a temporary macOS-pending notice in the notes. It enforces
   strict asset checks: on Linux, the AppImage must begin with a type 2 header,
   the `.nupkg` must contain that exact AppImage, and `releases.linux.json`
   must reference this version's package with its real SHA-256; on Windows,
   `-setup.exe` must begin with `MZ`, the zip must contain `RC2FX.exe`,
   `Update.exe`, and `current/rc2fx.exe`, the `.nupkg` must contain
   `lib/app/rc2fx.exe`, and `releases.win.json` must list this version's
   package and actual SHA-256. If `main` advanced while the workflow was
   running, this job aborts without pushing; kick off a new run.
4. **publish** (Ubuntu) waits for both **release** and **macos**, then runs
   `.github/scripts/macos_release.py publish`: it verifies the macOS files,
   uploads them to the draft, rebuilds `SHA256SUMS` from the draft's actual
   assets, strips the pending notice, re-verifies the complete draft, and
   publishes it as latest as its final step. It re-reads the release before
   every mutation. Any upload error, digest mismatch, or missing/extra asset
   leaves the draft unpublished, and it never overwrites a different macOS
   asset already on the draft. Re-running it on an already-published release
   merely verifies completeness and makes no changes. Publish jobs are
   serialized, and an older draft cannot replace a newer stable release as
   latest. Once published, a final step dispatches **Docs** with
   `source=latest-release` to build the docs site from the new release tag.
5. **app-metadata** invokes **Sign app release metadata** after **publish**
   succeeds, signing the release for in-app updates (see
   [Signed app updates](#signed-app-updates)). It passes
   `expect-artifacts: true` so only the exact packages built by this run can be
   signed. It requires reviewer approval in the `mixer-signing` environment and
   is skipped unless `APP_UPDATES_ENABLED` is `true`.

Dry runs build all packages without pushing, creating a draft, publishing, or
signing. Only one Release workflow runs at a time; concurrent runs queue.

If **macos** fails or no macOS runner is available, the draft remains safely
unpublished. Click **Re-run failed jobs** on the **Release** run to retry
**macos** and **publish**.

If someone manually publishes the draft before **publish** runs, the release
will be live but incomplete until **publish** finishes adding the missing
assets (without re-publishing or changing latest). Never publish drafts by hand
once GitHub immutable releases are enabled, because an incomplete immutable
release can't be edited afterward.

Immutable releases can be enabled once mixer metadata has moved to its own
branch (see [Where metadata lives](mixer-releases.md#where-metadata-lives)).
Nothing else mutates a published release: app releases stay editable drafts
until **publish** finishes attaching macOS assets, and mixer payload releases
are published once. Fixing either one requires cutting a new version.

The **Check** workflow lints, tests, and audits pushes and pull requests to
`main` (skipping docs-only changes) and runs the release script tests:

- **desktop** checks the Go app on Ubuntu, Windows, and macOS: `gofmt` on
  Linux, `go vet` and `go test` on all non-UI packages (everything except
  `app`, `app/uitest`, and `cmd/rc2fx`), `transport` tests with `mockmixer`, a
  vet pass with `rc2fxdev` and `embedded` (using an empty stand-in payload),
  and `govulncheck` on Linux. It also fetches Velopack's C library and vets
  `app/selfupdate/...` with `-tags velopack` and cgo on every OS without
  needing gpuikit.
- **desktop-ui** fetches gpuikit and Velopack's C library on all three OSes,
  vets the UI with and without `mockmixer` and with `rc2fxdev` and `embedded`,
  builds with `-tags velopack` and release link flags, runs
  `go test ./app/... ./cmd/rc2fx`, and runs `govulncheck` over `cmd/rc2fx` on
  Linux. Offscreen UI tests in `app/uitest` skip here because prebuilt gpuikit
  downloads don't include the diagnostic library. If an OS's gpuikit SHA-256
  isn't pinned in `desktop/scripts/fetch-gpuikit.sh` yet, it skips with a
  warning (though Release's **prepare** refuses to run until all three are
  pinned).
- **desktop-alma** builds the app and tests the installer (with vendored
  libext2fs) inside the same AlmaLinux 8 container used for AppImage builds.

Both UI jobs fetch gpuikit using `GPUIKIT_TOKEN`, falling back to the workflow
token; a fork PR without secrets that can't fetch it skips the UI build with a
warning, while any other run fails. Rust formatting, linting, and tests also
run on Linux inside the AlmaLinux 8 image. The Linux CI job builds without
debug info and with unoptimized dependencies (`.github/cargo-ci.toml`), more
than halving cold build times. Only `main` runs save Rust and Go build caches;
PRs read from them, and a twice-weekly scheduled run keeps caches warm.

The **Build image** workflow builds and pushes
`.github/scripts/package-check.Containerfile` to
`ghcr.io/ibanks42/rc2fx-build`. Its tag is derived from a digest of the
Containerfile and `rust-toolchain.toml` (`.github/scripts/build-image-tag.sh`),
so **Check** and **Release** only rebuild it when one of those changes. A
weekly scheduled run (or manual dispatch) refreshes the current tag so system
packages and bundled AppImage libraries pick up updates. Release preparation
also test-builds the docs before publishing, so disabling automatic docs
deployment on push never skips docs validation.

## Signing

`vpk` signs Windows and macOS packages whenever signing credentials are
configured. Without them, builds still succeed with a warning: Windows packages
are unsigned (triggering SmartScreen / unknown-publisher warnings), and on
macOS the app is ad-hoc signed with an unsigned `.pkg` (requiring users to
allow it in Privacy & Security). `vpk` does not sign Linux AppImages; instead,
AppImage self-updates are verified against signed app release metadata (see
[Signed app updates](#signed-app-updates)).

### Windows

Windows packages are signed via Azure Artifact Signing, covering `rc2fx.exe`,
its launcher, the updater, and `-setup.exe`. The **build** job authenticates
using GitHub OIDC via `azure/login` (no stored secret required) and reads five
repository variables:

| Variable | Value |
| --- | --- |
| `AZURE_TENANT_ID` | Microsoft Entra tenant ID |
| `AZURE_CLIENT_ID` | Client ID of the app registration with a federated credential for this repository (needs only a signing role on the certificate profile, not a subscription role) |
| `ARTIFACT_SIGNING_ENDPOINT` | Artifact Signing account endpoint URL |
| `ARTIFACT_SIGNING_ACCOUNT` | Artifact Signing account name |
| `ARTIFACT_SIGNING_PROFILE` | Certificate profile name |

Set all five variables or none. `.github/scripts/windows-signing.ps1` generates
the Artifact Signing metadata JSON for `vpk`, scoped to the Azure CLI session
created by `azure/login`. To sign a local build, create a matching metadata
file, sign in with `az login`, and point `RC2FX_ARTIFACT_SIGNING_METADATA` at
the file.

### macOS

`./xtask package` builds `RC2FX.app`, then `vpk` signs the app bundle with the
Developer ID Application certificate, signs the `.pkg` with the Developer ID
Installer certificate, and notarizes and staples both so Gatekeeper opens them
cleanly. The **macos** job expects seven repository secrets:

| Secret | Value |
| --- | --- |
| `MACOS_CERTIFICATE` | Developer ID Application certificate and key, base64-encoded `.p12` |
| `MACOS_CERTIFICATE_PASSWORD` | Password for that `.p12` |
| `MACOS_INSTALLER_CERTIFICATE` | Developer ID Installer certificate and key, base64-encoded `.p12` |
| `MACOS_INSTALLER_CERTIFICATE_PASSWORD` | Password for that `.p12` |
| `MACOS_NOTARY_KEY` | App Store Connect API key (`.p8` contents) |
| `MACOS_NOTARY_KEY_ID` | Key ID |
| `MACOS_NOTARY_ISSUER` | App Store Connect issuer ID |

Set all seven secrets or none. `.github/scripts/macos-signing.sh` imports the
certificates into a temporary keychain and saves the API key as a `notarytool`
profile. A failed notarization fails the **macos** job and keeps the release in
draft. For local signing, set `RC2FX_MACOS_IDENTITY` and
`RC2FX_MACOS_INSTALLER_IDENTITY` (full certificate names, both or neither), and
optionally `RC2FX_NOTARY_PROFILE` and `RC2FX_MACOS_KEYCHAIN` (both or neither)
to enable notarization. The app is signed with
`desktop/packaging/rc2fx.entitlements`, which requests no hardened-runtime
exceptions.

## Signed app updates

Before applying any downloaded update package, the desktop app verifies it
against signed TUF metadata (`desktop/installer/app_release.go`). This uses a
separate TUF repository from the mixer's, sharing the same compiled-in trust
root (`config/mixer-update-root.json`) and release keys so mixer releases and
older apps remain unaffected. Each package must match target `vVERSION/ASSET`
with custom metadata `{"version": VERSION}` as well as its signed length and
SHA-256. Any package that fails verification is deleted. This covers all
Velopack-managed platforms: Windows, macOS, and the Linux AppImage.

### The `app-metadata` branch

Because release assets are immutable while TUF metadata is re-signed on every
renewal, app metadata lives on the orphan branch `app-metadata`. The branch
contains only numbered root files, `targets.json`, `snapshot.json`,
`timestamp.json`, and a short README. The app fetches metadata only from
`https://raw.githubusercontent.com/ibanks42/rc2fx/app-metadata/` and packages
only from `https://github.com/ibanks42/rc2fx/releases/download/`, rejecting any
other host. Every asset of the latest app release except `SHA256SUMS` is a
target, and only the latest release is listed.

Each verification run starts fresh from the compiled-in root with no persisted
TUF state (using a temporary folder inside the installer work directory that is
deleted afterward) and rejects expired metadata. Persisting TUF state would let
someone replay mixer metadata at the app's URL to bump the app's rollback floor
and stall updates. Without persisted state, a replay can only present older
metadata that can't vouch for newer releases, and the seven-day expiry bounds
how long an attacker can hide a new release.

The first signing run creates the `app-metadata` branch (until then, **Renew
app metadata** fails at its fetch step). Once created, protect `app-metadata`
with the same branch rules as `mixer-metadata` (see
[Where metadata lives](mixer-releases.md#where-metadata-lives)): block
deletion, allow GitHub Actions and the maintainer to push, and don't require
pull requests or status checks.

### Signing after publication

Once **Release** publishes an app release, its **app-metadata** job calls
**Sign app release metadata** (`app-metadata-sign.yml`) in the `mixer-signing`
environment, which pauses for reviewer approval. The workflow downloads every
asset of the published release (`publish_app.py prepare`) and checks each
against GitHub's asset digest and the release's `SHA256SUMS` (which must list
exactly the other assets). When triggered by **Release**
(`expect-artifacts: true`), it also pulls that run's `release-assets` and
`macos-package` artifacts and passes them via `--built`, refusing to sign
unless the published release (excluding `SHA256SUMS`) contains those exact
files with identical sizes and SHA-256s. That guarantees it only signs what
that workflow run built, not anything modified on the release afterward. It
then signs targets, snapshot, and timestamp with `./xtask app-sign` and pushes
them to `app-metadata` in a single commit (`publish_app.py publish`). Targets
expire with the root (matching mixer releases), while snapshot and timestamp
expire after seven days. Until signing completes, apps checking for updates
refuse the new release with "This update has not been signed yet. Try again
later."

You can also trigger **Sign app release metadata** manually (**Run workflow**
with a published tag) if automatic signing was skipped. The same gates apply
(`APP_UPDATES_ENABLED`, `main` only, `mixer-signing` reviewer approval, and the
shared concurrency group), except manual runs have no build artifacts to cross-check
and verify only against GitHub's digests and `SHA256SUMS`.

Both app workflows set metadata versions to UTC Unix time minus 10⁹. Because
app and mixer metadata share one root, either repository's metadata is
cryptographically valid at the other's URL. Offsetting app metadata versions
keeps them strictly below mixer metadata versions (plain Unix timestamps), so
replaying app metadata at the mixer URL is rejected as a rollback.
`publish_app.py` refuses any app metadata version at or above `1,700,000,000`
to catch a workflow accidentally reverting to plain Unix time. In the reverse
direction, since the app persists no TUF state (see
[The `app-metadata` branch](#the-app-metadata-branch)), replay impact is bounded
by expiry.

### Kill switch

Both signing and renewal require the repository variable `APP_UPDATES_ENABLED`
to be `true`. Setting it to `false` halts both immediately (for example, if a
release key leaks; see [If the targets key leaks](mixer-releases.md#if-the-targets-key-leaks),
and remember both repositories share keys so rotate them for both). With the
switch off, new releases go unsigned and existing metadata expires within seven
days, causing apps to fail closed and reject all updates. Turn it on before
shipping the first release after an app version that checks signed updates.

### Renewal

**Renew app metadata** (`app-metadata.yml`) runs twice weekly in the
`mixer-renewal` environment and can also be triggered manually. It runs
`./xtask app-refresh` to re-sign only snapshot and timestamp (for seven days)
while leaving `targets.json` untouched, so it cannot authorize new assets or
extend past the root's expiry. If renewal fails, investigate before the
seven-day window runs out. Both signing and renewal run only from `main` and
share one concurrency group so they never race on `app-metadata`.

During root rotation, both `mixer-metadata` and `app-metadata` need every new
numbered root. Each signing run adds the root it signs under without replacing
previously published numbered roots. There is no `--reauthorize` flag for app
metadata; after rotating keys, publish a new app release so it gets signed with
the new keys.

### Failure and recovery

- **Rerun:** Re-running **Sign app release metadata** for the same tag is
  idempotent. If the branch already has that release with identical assets, it
  does nothing. If an earlier run failed before pushing, it re-signs and pushes
  with a newer version number. A push against a stale branch tip fails instead
  of clobbering a concurrent writer; simply re-run it.
- **Slow cache:** `raw.githubusercontent.com` caches files for roughly five
  minutes. The publisher polls for up to seven minutes; if it times out, the
  push still succeeded and apps will see the update once caches expire.
- **Refusals:** Signing refuses older versions than what's already signed
  (rollbacks), the same version with different assets (cut a new version
  instead), drafts or prereleases, assets lacking a GitHub digest, any
  `SHA256SUMS` mismatch, and (when called by **Release**) any discrepancy from
  the run's built artifacts.

## Publishing documentation

The docs site only deploys after a release is published or when **Docs** is
triggered manually. Pushing edits to `docs/` stages them in `main` without
deploying immediately.

Run **Docs** from `main` with one of two sources:

- **main** (default): deploys current docs and release-note fixes without
  cutting an app release.
- **latest-release**: checks out the tag of the latest published app release
  and builds the site from it (**Release** dispatches this automatically after
  publishing). The workflow runs on `main` for the Pages environment, then
  checks out the resolved tag for the build.

Every run resolves its source at start time, including retries. Because build
and deploy happen in a single job under one Pages concurrency group, retrying a
failed deployment re-resolves the source and rebuilds cleanly. Regular docs,
[update histories](update-history.md), and `updates.json` all deploy together.
Only published stable releases appear in the history; draft tags are excluded.

If you manually deployed docs from newer `main` while a release draft was open,
publishing that release will overwrite the site with the tagged snapshot; just
run **Docs** with **main** again to restore your newer changes.

A failed docs deployment never affects a published app release; retry **Docs**
independently without rebuilding packages. If release publication succeeds but
dispatching **Docs** fails, trigger **Docs** manually with **latest-release**.
Manually publishing or deleting a release on GitHub also requires a manual docs
deployment; until it runs, the app may see the release before hosted notes are
live.

## Testing the release tooling locally

All release scripts have offline test suites that **Check** runs in CI:

```bash
python3 .github/scripts/test_release.py          # versioning; needs git-cliff 2.14.1
python3 .github/scripts/test_macos_release.py    # finishing and publishing the draft
python3 .github/scripts/test_app_release.py      # signing app releases for in-app updates
python3 .github/scripts/test_docs_source.py
python3 .github/scripts/test_update_history.py
```

The GitHub Actions workflows themselves only execute on GitHub. Use a
**Release** dry run to exercise full packaging without pushing, publishing, or
signing.

## Before the first release

- Make sure the GitHub repository is configured as the `origin` remote.
- Set the repository variable `APP_UPDATES_ENABLED` to `true` before releasing
  the first version after an app that verifies signed updates (see
  [Kill switch](#kill-switch)), and protect the `app-metadata` branch as soon
  as the first signing run creates it.
- Keep the Linux AppImage compatible with glibc 2.28 or newer by building in
  its AlmaLinux 8 container.

## Per-platform build runners

The desktop app links the platform-specific gpuikit static library via cgo, and
gpuikit itself has to be compiled on each OS because GPUI only compiles its
shaders natively on macOS and Windows. Packaging is platform-specific as well:
`vpk`, Azure Authenticode signing, and macOS Developer ID signing and
notarization each require their native OS. As a result, each platform builds on
its own GitHub runner.
