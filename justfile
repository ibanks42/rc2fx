# RC2FX tasks. They behave the same on Windows, Linux and macOS: the work lives
# in the Go tool in desktop/cmd/xtask, which ./xtask runs (`./xtask help`).
set windows-shell := ["powershell.exe", "-NoLogo", "-NoProfile", "-Command"]

# PowerShell cannot run the POSIX sh launcher, so on Windows the recipes run
# the tool as ./xtask does: without cgo, from desktop/, with the repository
# root and the directory paths are relative to (recipes run at the root).
xtask := if os_family() == "windows" { "$env:CGO_ENABLED='0'; $env:RC2FX_XTASK_ROOT=$PWD.Path; $env:RC2FX_XTASK_CWD=$PWD.Path; go -C desktop run ./cmd/xtask" } else { "./xtask" }

# List the recipes
default:
    @just --list --unsorted

# One-time setup: the mixer build (Rust target, cargo-zigbuild, zig check) and the pre-commit formatting hook
setup:
    {{ xtask }} setup
    git config core.hooksPath .githooks

# Run the desktop app (development build; needs desktop/third_party/gpuikit)
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

# Build the desktop app and its packages into target/package (needs the gpuikit download and Velopack's C library; Windows and macOS need vpk)
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
    bash .github/scripts/package-check.sh

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

# Tests and lints: the desktop app (Go; the UI packages only when the gpuikit download is linked) and the Rust workspace
[unix]
test: mixer-build
    cd desktop && test -z "$(gofmt -l $(go list -e -f '{{ "{{" }}.Dir{{ "}}" }}' ./...))"
    cd desktop && CGO_ENABLED=0 go vet $(go list ./... | grep -v -e '/app$' -e '/app/uitest$' -e '/cmd/rc2fx$')
    cd desktop && if [ -e third_party/gpuikit ]; then CGO_LDFLAGS="-L$PWD/third_party/gpuikit/lib" go test ./...; else go test $(go list ./... | grep -v -e '/app$' -e '/app/uitest$' -e '/cmd/rc2fx$'); fi
    go -C desktop test -tags mockmixer ./transport/...
    cargo test --locked --workspace
    cargo clippy --locked --workspace --all-targets -- -D warnings

# Tests and lints: the desktop app (Go; the UI packages only when the gpuikit download is linked) and the Rust workspace
[windows]
test: mixer-build
    $ui = '/app$|/app/uitest$|/cmd/rc2fx$'; $all = go -C desktop list ./...; $nocgo = $all | Where-Object { $_ -notmatch $ui }; $pkgs = if (Test-Path desktop/third_party/gpuikit) { $all } else { $nocgo }; $env:CGO_ENABLED='0'; go -C desktop vet $nocgo; if (-not $?) { exit 1 }; Remove-Item Env:CGO_ENABLED; if (Test-Path desktop/third_party/gpuikit) { $env:CGO_LDFLAGS = "-L$((Resolve-Path desktop/third_party/gpuikit/lib).Path)" }; go -C desktop test $pkgs; if (-not $?) { exit 1 }
    go -C desktop test -tags mockmixer ./transport/...
    cargo test --locked --workspace
    cargo clippy --locked --workspace --all-targets -- -D warnings

# Tests of the mixer side: the native library, launcher scripts and C modules (Linux or macOS, with python3 and cc)
test-mixer:
    cargo test --locked --manifest-path crates/rc2fx-native/Cargo.toml
    cargo clippy --locked --manifest-path crates/rc2fx-native/Cargo.toml --all-targets -- -D warnings
    for t in mixer/scripts/tests/*.py; do python3 "$t" || exit 1; done
    python3 -m unittest discover -s crates/rc2fx-native/jackerr/tests -p 'test_*.py'
    python3 crates/rc2fx-gui/test_gui_pacing.py

# Cross-build the suppression benchmark
bench-build:
    {{ xtask }} bench-build

# Time every suppression model on the mixer over a 48 kHz recording and fetch the denoised audio
bench recording *flags: bench-build
    {{ xtask }} bench {{ recording }} {{ flags }}

# Build the documentation site into target/docs (needs Node.js)
docs:
    cd docs && npm ci --no-audit --no-fund && npm run build

# Serve the documentation site on localhost, rebuilding on change
docs-serve:
    cd docs && npm ci --no-audit --no-fund && npm run dev -- --open
