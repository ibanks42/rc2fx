# Changelog

All notable changes to RC2FX are documented here.

## [0.2.1] - 2026-11-21

### Features

- *(app)* Restart into app updates on their own and reconnect at once

### Bug fixes

- *(updates)* Read the update feed from the docs domain and count standalone mixer releases

### Documentation

- Describe the update feed, release landing, and one-click app updates

### Maintenance

- *(release)* Let app and mixer releases land on each other's version commit

### Other

- Update image captions in README.md

## [0.2.0] - 2026-11-21

### Features

- *(app)* Port the desktop app to Go
- *(mixer)* Live editing, exact reset check, and click-free stage switches
- *(app)* Storage fix, guided install tasks, live editing, and one save at a time
- *(installer)* Make partition fix an opt-in experimental option

### Bug fixes

- *(docs)* Change base URL in VitePress
- *(ci)* Go 1.27.4, installer tests off the real helper folder, and manifests for Windows test binaries
- *(desktop)* Link velopack_libc through CGO_LDFLAGS, not a #cgo directive
- *(mixer)* Verify a flash before RØDE's reboot and repair it from the archive
- *(desktop)* Link Velopack beside static gpuikit on Linux; run the fix script's tests on Linux only
- *(ci)* Libxkbcommon 1.7.0 in the AppImage build image
- Installer, update, device, and app bugs
- Resolve release script drift, DiagnosticsKind::Other, and just test
- *(rodecaster)* Read storage-fix reports over USB on macOS and Windows
- *(installer)* Block unsafe second updates, clear the p1 gap on restore, and fix the settings-save hang
- *(mixer)* Set the previous image check result aside before a new check
- *(ci)* Build the GUI pacing shim under fortify and allow pow's last bit in the gate fixture
- Restore gui_launch.py comment anchor and apply rustfmt
- *(licenses)* Accept repeated cargo-about JSON keys
- *(app)* Keep the payload-mode marker in release builds
- *(package)* Pack the universal macOS app as osx-arm64
- *(package)* Leave freetype's libraries out of the AppImage

### Refactoring

- *(device)* Remove the in-process JACK runtime path
- *(core)* Remove the desktop editor logic
- Drop each side's copy of the other side's logic
- Replace flag clusters with explicit state types
- *(desktop)* Clean up after the Go port
- Remove dead code and unneeded tests on the mixer side
- *(desktop)* Remove dead code and unneeded tests
- Remove dead code beside the realtime path
- *(native)* Split pipeline.rs into modules by concern
- Build the Rust side for Linux only
- Remove the realtime path's unused branches and counters
- Move pure-C mixer modules (gui, jackerr) out of crates/
- *(rust)* Deduplicate helpers, normalize test layout, and tidy rc2fx-device
- *(rust)* Split rc2fx-native monoliths and group rc2fx-device by domain
- *(mixer)* Extract xrun-trace and experimental UI runners out of rc2fx.sh
- *(go)* Unify strict serde JSON into desktop/internal/sjson
- *(go)* Decompose installer, extract usbcontrol and appupdate, split large files
- *(app)* Extract state and theme packages, split installer views, and consolidate dev tags
- *(installer)* Share MBR parsing with rodecaster/fix
- *(rodecaster)* Prune dead storage-mode fix, report, and disk code
- *(ext2fs)* Replace vendored e2fsprogs cgo wrapper with pure-Go ext4
- *(desktop)* Simplify sjson, helper IPC, and Go internal helpers

### Documentation

- Describe the code as it is now
- Align installer, Duo, update, and release documentation with current behavior
- Rewrite documentation and code comments to read naturally
- Update EQ and noise suppression screenshots

### Performance

- *(app)* Pace spinners and ambient animations to cut GPU usage

### Testing

- Add shared payload list, gate, EQ, update, and udev fixtures
- *(ci)* Use fixed zip timestamps in test_macos_release
- *(appimage)* Allow the host's libfreetype
- Avoid 108-byte Unix socket path limit under long TMPDIR

### Maintenance

- Choose the mixer version bump when running Mixer release
- Build, lint, and test what ships; trim the release tooling
- *(mixer)* Add unsigned release dry runs

### Other

- Main (v0.1.7, mixer 0.2.1 prep, and the mixer version bump workflow) into the Go port
- *(desktop)* Gpuikit 0.0.2, linked into the app as a static library
- Fold rc2fx-native into root Cargo workspace and tidy manifests
- *(release)* Move app version to config, pin velopack config, and streamline CI
- Merge pull request #4 from ibanks42/port/desktop-go

feat: Go desktop app, live editing, and codebase cleanup

## [0.1.7] - 2026-11-16

### Features

- *(installer)* List RØDE's files in the image, check root space and the mixer's serial before flashing
- *(mixer)* Check the flashed image at boot and restore damaged RØDE files from the other root
- *(app)* Repair mixer, for a mixer a flash left unable to keep its setup

### Documentation

- Recovering a mixer that forgets its setup

### Maintenance

- *(mixer)* Prepare the 0.2.1 mixer release

### Other

- Vendor libext2fs from e2fsprogs 1.47.4
- *(installer)* Build entry_name only where the repair uses it

## [0.1.6] - 2026-11-15

### Features

- *(app)* Update in place through Velopack, checked against signed release metadata
- *(gui)* Count conversion plans by shape and size, and skip unused paint text
- *(bench)* Compare the tiled and row-major GRU kernels on the mixer
- *(native)* Bench-only timing of RØDE's worker-join waits
- *(mixer)* Report recent timing peaks and engine running time; native protocol 7
- *(installer)* Read the RØDE firmware version from the mixer's USB release number
- *(app)* Rework the telemetry panel around what a person can act on
- *(mixer)* Install signed RC2FX releases in place, without a flash
- *(app)* Offer RC2FX updates in place, and a notice to update older mixer software
- *(usb)* Control protocol 4, and a hook that keeps a transfer flowing
- *(update)* Send only what changed in an in-place update
- *(update)* Verify releases with sigstore-tuf on the mixer and in the app
- *(update)* Apply controller-only updates without restarting the mixer

### Bug fixes

- *(device)* Stop polling fast once the app has gone away
- *(xtask)* Arm power recovery before developer mixer commands
- *(xtask)* Remove the RAM upload once an install is in place
- *(native)* Count a callback start as late only past 3/5 of the period
- *(mixer)* Refuse in-place installs of releases that can't commit them
- *(mixer)* Give in-place updates their own pending marker
- *(mixer)* Let a new in-place upload replace an abandoned one
- *(mixer)* Fingerprint signed targets by their bytes, not a re-serialization
- *(app)* Keep in-place update progress, and show the mixer's refusals
- *(mixer)* Free the rollback copy's space once an update commits

### Refactoring

- Get thread IDs with the gettid syscall

### Documentation

- Put downloads first on the front page and document the new packaging
- Describe the RØDECaster's built-in processing accurately

### Performance

- *(native)* Resample USB channels through a shared bit-exact plan
- *(dsp)* Check and meter blocks at a time, and skip unchanged preparation
- *(suppression)* Vectorize the denoiser's remaining loops and process blocks
- *(native)* Trim the callback's bookkeeping and speed up the resampler
- *(dfn)* Store the GRU weights in tiles and multiply four rows at a time
- *(dsp)* Lay out each EQ bank's hot state together and prefetch it
- *(native)* Prefetch the callback's own state before each phase
- *(native)* Let the helper wake itself instead of the callback waking it
- *(gui)* Copy screen strips out of the framebuffer with 32-byte loads
- *(dfn)* Let the denoiser wait out the audio callback
- *(native)* Mark the callback for the denoiser hold; wake the helper 50 us after its handoff
- *(native)* Wake one idle vendor worker per job instead of all three
- *(native)* Reuse JACK port buffer pointers within a graph generation

### Testing

- *(suppression)* Keep the slowest-frame test from racing under load
- *(mixer)* Keep the stand-in update driver a shell on bash systems

### Maintenance

- Check Linux in the AlmaLinux 8 build image, and on pull requests
- Run the Linux check container with an init process
- Make the Linux check faster and keep pull requests on main's cache
- *(mixer)* Prepare the 0.2.0 mixer release
- Name the build user in the package container so vpk's .NET runs

### Other

- *(release)* Sign app releases into the app-metadata branch
- *(package)* Package every platform with Velopack, with unversioned file names
- *(linux)* Build the AppImage on AlmaLinux 8 (glibc 2.28)
- *(rust)* Move the toolchain to 1.100.0
- *(app)* Move to gpui-kit 0.9.1 and gpui-fast 0.1.15
- Declare platform-only dependencies for the platforms that use them
- *(device)* Depend on libc only on Unix

## [0.1.5] - 2026-11-08

### Features

- *(installer)* Offer stock firmware from RØDE's manifest for each model
- *(app)* Put RC2FX and stock firmware on tabs on the Firmware page
- *(app)* Show when RØDE has firmware newer than RC2FX supports

### Bug fixes

- *(app)* Quit cleanly on macOS and add a Quit menu with Cmd+Q
- *(mixer)* Read the monotonic clocks on any Unix so host tests time frames

### Testing

- *(mixer)* Run the mixer script tests on macOS hosts and in Linux CI
- *(mixer)* Start the fake mixer from a copy of bash

### Maintenance

- *(just)* Run debug builds by default, with release variants
- Format the workspace and fix stale `just app` docs
- *(rust)* Move to Rust 1.99.1
- Lint on every platform, test the workspace on Linux, check formatting
- Publish releases from the Release workflow and drop dead CI code
- *(deps)* Move to gpui 0.9, RustCrypto 0.11, and reqwest 0.13

### Other

- *(app)* Use gpui-kit's gpui-fast backend and test against it

## [0.1.4] - 2026-11-07

### Features

- *(release)* Serve mixer metadata from a branch and keep targets valid until the root expires
- *(supervisor)* Watch the mixer and controller from a Rust binary
- *(app)* Compact layout, telemetry redesign, installer card, versions on Settings

### Bug fixes

- *(installer)* Harden the desktop side after the security review
- *(mixer)* Bound logs and clean up sockets after the security review
- *(device)* Report the true callback peak until diagnostics are reset

### Maintenance

- *(mixer-release)* Clear release outputs restored from the Rust cache

## [0.1.3] - 2026-11-07

### Features

- *(bench)* Report slow callbacks and callback means at the lightest level
- *(diagnostics)* Add bench-only USB ring, monitor job, and resampler gauges
- *(macos)* Ship the app as RC2FX.app so it no longer opens in Terminal

### Bug fixes

- *(native)* Keep frames contiguous when a mode is seen before it is counted
- *(installer)* Find the update drive on macOS

### Documentation

- *(macos)* Drop the Privacy & Security step now that the app is notarized

### Performance

- *(supervisor)* Read each thread status file once
- *(control)* Skip background work when nothing changed or nobody watches
- *(native)* Visit only the paths with work this cycle

### Maintenance

- *(release)* Build macOS beside the other platforms and drop zip checksums

## [0.1.2] - 2026-11-06

### Bug fixes

- *(release)* Resume macOS builds for already-versioned tags
- *(installer)* Recognize update drives smaller than the usual size

## [0.1.1] - 2026-11-05

### Initial release

- Process audio for the RØDECaster Pro II and control mixer settings from the desktop app.
- Report app, installed mixer, control-protocol, and RØDE firmware versions, with explicit unknown states.
- Download signed mixer builds and firmware profiles independently of desktop releases, using the existing USB firmware installer.
- Check payload signatures, hashes, and compatibility before installation; require an app update when a mixer build or firmware profile needs newer support.
- Offer an optional embedded mixer payload for offline installation.
- Show separate app and mixer update histories, reviewed mixer selection, download cancellation, and downgrade confirmation.
- Manage developer SSH over USB and display consolidated third-party licenses in the app.
- Reduce audio deadline interference from mixer screen redraws and simplify desktop background animations.
- Correct suppression timing totals after lookahead changes and retain native performance samples and recent audio faults for diagnosis.
- Provide versioned Linux DEB, RPM, and AppImage packages, Windows packages, and macOS packages, with checksums and platform license notices.
- Validate Linux package dependencies, installation, and removal before release; preserve draft release identity while completing platform builds.
- Include app screenshots, setup instructions, and independent mixer-release documentation.

<!-- generated by git-cliff -->
