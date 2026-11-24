# RC2FX build and dev tasks (implemented in tools/xtask; see `go -C tools/xtask run . help`).
set windows-shell := ["powershell.exe", "-NoLogo", "-NoProfile", "-Command"]

# Run xtask from its own Go module so tasks work before goui-kit is downloaded.
xtask := "go -C tools/xtask run ."

# List the recipes
default:
    @just --list --unsorted

# One-time setup for building, packaging, and running the app and building the mixer: the goui-kit and Velopack libraries, the Rust target, cargo-zigbuild, cargo-about, vpk, the Linux AppImage tools and notices, a zig check, and the pre-commit formatting hook
setup:
    {{ xtask }} setup
    git config core.hooksPath .githooks

# Run the desktop app (development build; needs desktop/third_party/goui-kit)
app:
    {{ xtask }} app

# Run the desktop app (release)
app-release:
    {{ xtask }} app --release

# Run the desktop app against a simulated mixer: no hardware, development build only
app-mock:
    {{ xtask }} app --mock

# Build the desktop app into target/release
desktop:
    {{ xtask }} app --release --build-only

# Build the desktop app against a simulated mixer into target/mock
desktop-mock:
    {{ xtask }} app --mock --build-only

# Build the desktop app and its packages into target/package (run `just setup` first)
package:
    {{ xtask }} package

# Run desktop app with bundled mixer payload (debug)
app-embedded: mixer-build
    {{ xtask }} app --embedded

# Run desktop with bundled mixer payload (release)
app-embedded-release: mixer-build
    {{ xtask }} app --release --embedded

# Build desktop app with bundled mixer payload (release)
desktop-embedded: mixer-build
    {{ xtask }} app --release --embedded --build-only

# Build the AppImage from the working tree in the release container (glibc 2.28), then check it on several distributions (Linux, needs podman or docker)
package-check:
    {{ xtask }} package-check

# Cross-build the mixer side into target/mixer (unchanged files are left as they are)
mixer-build:
    {{ xtask }} mixer-build

# Run from RAM; reboot returns to the installed build or stock, mixer-stop to stock
mixer-run: mixer-build
    {{ xtask }} mixer run

# Build and install on the mixer, starting at every boot
mixer-install: mixer-build
    {{ xtask }} mixer install

# Remove RC2FX from the mixer (--purge also deletes its settings)
mixer-uninstall *flags:
    {{ xtask }} mixer uninstall {{ flags }}

# Stop RC2FX and hand audio back to stock
mixer-stop:
    {{ xtask }} mixer stop

# Show what is running on the mixer
mixer-status:
    {{ xtask }} mixer status

# Print recent RC2FX, controller and mixer logs
mixer-logs:
    {{ xtask }} mixer logs

# What the JACK error recorder records over SECONDS (0: everything since jackd started)
mixer-xruns seconds="60":
    {{ xtask }} mixer xruns {{ seconds }}

# Let an installed RC2FX start at boot
mixer-enable:
    {{ xtask }} mixer enable

# Keep an installed RC2FX from starting at boot
mixer-disable:
    {{ xtask }} mixer disable

# Regenerate shared Rust and Go wire types from schema/
schema:
    {{ xtask }} schema

# Tests and lints: the desktop app and tools/xtask (Go; the UI packages only when the goui-kit download is linked) and, on Linux, the Rust workspace
[unix]
test:
    {{ xtask }} schema --check
    cd desktop && test -z "$(gofmt -l $(go list -e -f '{{ "{{" }}.Dir{{ "}}" }}' ./...))"
    pkgs=$({{ xtask }} non-ui-packages) && cd desktop && CGO_ENABLED=0 go vet $pkgs
    pkgs=$({{ xtask }} non-ui-packages) && cd desktop && if [ -e third_party/goui-kit ]; then CGO_LDFLAGS="-L$PWD/third_party/goui-kit/lib" go test -tags gkstatic ./...; else go test $pkgs; fi
    go -C desktop test -tags mockmixer ./transport/...
    cd tools/xtask && test -z "$(gofmt -l .)" && CGO_ENABLED=0 go vet ./... && CGO_ENABLED=0 go test ./...
    if [ "$(uname -s)" = Linux ]; then {{ just_executable() }} test-rust; else echo "Rust tests skipped: the Rust crates build for Linux only"; fi

# Tests and lints of the Rust workspace, warnings as errors (the Rust crates build for Linux only)
[linux]
test-rust:
    cargo test --locked --workspace
    cargo clippy --locked --workspace --all-targets -- -D warnings
    cargo clippy --locked -p rc2fx-dfn --features bench-kernels --all-targets -- -D warnings

# Tests and lints: the desktop app and tools/xtask (Go; the UI packages only when the goui-kit download is linked). The Rust crates build for Linux only.
[windows]
test:
    $nocgo = & { {{ xtask }} non-ui-packages }; $pkgs = if (Test-Path desktop/third_party/goui-kit) { './...' } else { $nocgo }; $env:CGO_ENABLED='0'; go -C desktop vet $nocgo; if (-not $?) { exit 1 }; Remove-Item Env:CGO_ENABLED; if (Test-Path desktop/third_party/goui-kit) { $env:CGO_LDFLAGS = "-L$((Resolve-Path desktop/third_party/goui-kit/lib).Path)" }; go -C desktop test -tags gkstatic $pkgs; if (-not $?) { exit 1 }
    go -C desktop test -tags mockmixer ./transport/...
    $env:CGO_ENABLED='0'; go -C tools/xtask vet ./...; if (-not $?) { exit 1 }; go -C tools/xtask test ./...; if (-not $?) { exit 1 }

# Tests of the mixer side: the native library binding check, launcher scripts and C modules (Linux, with python3 and cc)
test-mixer:
    cargo fmt --all --manifest-path crates/rc2fx-bench/Cargo.toml -- --check
    cargo fmt --all --manifest-path tools/src_fast_check/Cargo.toml -- --check
    cargo check --locked --manifest-path crates/rc2fx-bench/Cargo.toml --all-targets
    cargo build --locked -p rc2fx-native
    for t in mixer/scripts/tests/*.py; do python3 "$t" || exit 1; done
    python3 -m unittest discover -s mixer/jackerr/tests -p 'test_*.py'
    python3 mixer/gui/test_gui_pacing.py

# Cross-build the suppression benchmark
bench-build:
    {{ xtask }} bench-build

# Time the suppression model on the mixer over a 48 kHz recording and fetch the denoised audio
bench recording *flags: bench-build
    {{ xtask }} bench {{ recording }} {{ flags }}

# Build the documentation site into target/docs (needs Node.js)
docs:
    cd docs && npm ci --no-audit --no-fund && npm run build

# Serve the documentation site on localhost, rebuilding on change
docs-serve:
    cd docs && npm ci --no-audit --no-fund && npm run dev -- --open
