# RC2FX tasks. They behave the same on Windows, Linux and macOS: every recipe is
# a cargo command, and the work lives in xtask/ (`cargo xtask help`).
set windows-shell := ["powershell.exe", "-NoLogo", "-NoProfile", "-Command"]

# List the recipes
default:
    @just --list --unsorted

# One-time setup for the mixer build: Rust target, cargo-zigbuild, zig check
setup:
    cargo xtask setup

# Run the desktop app with authenticated mixer downloads
app:
    cargo run --locked --release -p rc2fx-app

# Build the desktop app into target/release
desktop:
    cargo build --locked --release -p rc2fx-app

# Build the desktop app and bundle it with its documentation into target/package
package:
    cargo xtask package

# Optional desktop with a bundled mixer payload
app-embedded: mixer-build
    cargo run --locked --release -p rc2fx-app --features embedded

desktop-embedded: mixer-build
    cargo build --locked --release -p rc2fx-app --features embedded

# Build the Linux packages from the working tree in a container, then test installing them (Linux, needs podman or docker)
package-check:
    bash .github/scripts/package-check.sh

# Cross-build the mixer side into target/mixer (unchanged files are left as they are)
mixer-build:
    cargo xtask mixer-build

# Run from RAM; reboot returns to the installed build or stock, mixer-stop to stock
mixer-run: mixer-build
    cargo xtask mixer run

# Build and install on the mixer, starting at every boot
mixer-install: mixer-build
    cargo xtask mixer install

# Remove RC2FX from the mixer (--purge also deletes its settings)
mixer-uninstall *flags:
    cargo xtask mixer uninstall {{flags}}

# Stop RC2FX and hand audio back to stock
mixer-stop:
    cargo xtask mixer stop

# Show what is running on the mixer
mixer-status:
    cargo xtask mixer status

# Print recent RC2FX, controller and mixer logs
mixer-logs:
    cargo xtask mixer logs

# What the JACK error recorder records over SECONDS (0: everything since jackd started)
mixer-xruns seconds="60":
    cargo xtask mixer xruns {{seconds}}

# Let an installed RC2FX start at boot
mixer-enable:
    cargo xtask mixer enable

# Keep an installed RC2FX from starting at boot
mixer-disable:
    cargo xtask mixer disable

# Tests and lints for the desktop and shared crates
test: mixer-build
    cargo test --locked --workspace
    cargo clippy --locked --workspace --all-targets -- -D warnings

# Tests of the mixer side: the native library, launcher scripts and C modules (Linux or macOS, with python3 and cc)
test-mixer:
    cargo test --locked --manifest-path crates/rc2fx-native/Cargo.toml
    cargo clippy --locked --manifest-path crates/rc2fx-native/Cargo.toml --all-targets -- -D warnings
    for t in xtask/mixer/tests/*.py; do python3 "$t" || exit 1; done
    python3 -m unittest discover -s crates/rc2fx-native/jackerr/tests -p 'test_*.py'
    python3 crates/rc2fx-gui/test_gui_pacing.py

# Cross-build the suppression benchmark
bench-build:
    cargo xtask bench-build

# Time every suppression model on the mixer over a 48 kHz recording and fetch the denoised audio
bench recording *flags: bench-build
    cargo xtask bench {{recording}} {{flags}}

# Build the documentation site into target/docs (needs Node.js)
docs:
    cd docs && npm ci --no-audit --no-fund && npm run build

# Serve the documentation site on localhost, rebuilding on change
docs-serve:
    cd docs && npm ci --no-audit --no-fund && npm run dev -- --open
