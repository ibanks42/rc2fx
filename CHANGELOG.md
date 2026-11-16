# Changelog

All notable changes to RC2FX are documented here.

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
- *(suppression)* Vectorise the denoiser's remaining loops and process blocks
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
- *(deps)* Move to gpui 0.9, RustCrypto 0.11 and reqwest 0.13

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
- *(diagnostics)* Add bench-only USB ring, monitor job and resampler gauges
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
- Report app, installed mixer, control-protocol and RØDE firmware versions, with explicit unknown states.
- Download signed mixer builds and firmware profiles independently of desktop releases, using the existing USB firmware installer.
- Check payload signatures, hashes and compatibility before installation; require an app update when a mixer build or firmware profile needs newer support.
- Offer an optional embedded mixer payload for offline installation.
- Show separate app and mixer update histories, reviewed mixer selection, download cancellation and downgrade confirmation.
- Manage developer SSH over USB and display consolidated third-party licenses in the app.
- Reduce audio deadline interference from mixer screen redraws and simplify desktop background animations.
- Correct suppression timing totals after lookahead changes and retain native performance samples and recent audio faults for diagnosis.
- Provide versioned Linux DEB, RPM and AppImage packages, Windows packages and macOS packages, with checksums and platform license notices.
- Validate Linux package dependencies, installation and removal before release; preserve draft release identity while completing platform builds.
- Include app screenshots, setup instructions and independent mixer-release documentation.

<!-- generated by git-cliff -->
