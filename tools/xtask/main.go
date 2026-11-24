// Command xtask implements cross-platform project tasks, invoked via `just` or
// `go -C tools/xtask run . <command>` from the repo root.
package main

import (
	"fmt"
	"os"

	"github.com/ibanks42/rc2fx/tools/xtask/internal/deps"
	"github.com/ibanks42/rc2fx/tools/xtask/internal/elfcheck"
	"github.com/ibanks42/rc2fx/tools/xtask/internal/licenses"
	"github.com/ibanks42/rc2fx/tools/xtask/internal/lint"
	"github.com/ibanks42/rc2fx/tools/xtask/internal/mixer"
	"github.com/ibanks42/rc2fx/tools/xtask/internal/packaging"
	"github.com/ibanks42/rc2fx/tools/xtask/internal/pkgcheck"
	"github.com/ibanks42/rc2fx/tools/xtask/internal/release"
	"github.com/ibanks42/rc2fx/tools/xtask/internal/schema"
	"github.com/ibanks42/rc2fx/tools/xtask/internal/setup"
	"github.com/ibanks42/rc2fx/tools/xtask/internal/signing"
	"github.com/ibanks42/rc2fx/tools/xtask/internal/task"
)

const help = `go -C tools/xtask run . <command>

  setup                 Fetch goui-kit and Velopack's C library, install the Rust
                        target, cargo-zigbuild, cargo-about, and vpk (needs the
                        .NET SDK), check for zig, and on Linux install linuxdeploy,
                        patchelf, mksquashfs, and the AppImage runtime notices
  fetch-goui-kit [TARGET]
                        Download the pinned goui-kit library (GOOS-GOARCH or
                        darwin-universal; default: this machine)
  fetch-velopack [TARGET...]
                        Download the pinned velopack_libc for each target (default: this machine)
  fetch-vpk             Install the pinned vpk into desktop/third_party/tools (needs the .NET SDK)
  mixer-build           Cross-build the mixer binaries and profile checker into target/mixer
  mixer-package DIR     Package a standalone release payload and notices in a new directory
  mixer-keys DIR        Create signing keys and a public trust root outside the checkout
  mixer-sign ROOT PACKAGE OUT SEQUENCE EXPIRES TARGETS_KEY SNAPSHOT_KEY TIMESTAMP_KEY
                        Sign mixer update metadata with existing release keys (never root keys)
  mixer-refresh ROOT METADATA OUT SEQUENCE EXPIRES SNAPSHOT_KEY TIMESTAMP_KEY
                        Renew discovery metadata without changing approved targets
  app-sign ROOT ASSETS VERSION OUT SEQUENCE EXPIRES TARGETS_KEY SNAPSHOT_KEY TIMESTAMP_KEY
                        Sign app update metadata for a downloaded, published app release
  app-refresh ROOT METADATA OUT SEQUENCE EXPIRES SNAPSHOT_KEY TIMESTAMP_KEY
                        Renew app discovery metadata without changing approved targets
  mixer <action>        Manage RC2FX on the mixer over SSH:
      run               run this build from RAM (a reboot returns to the installed build or stock)
      install           install to /opt/rc2fx and start at every boot
      uninstall [--purge]  remove the install (--purge also deletes settings)
      stop              stop RC2FX and hand audio back to stock
      status | logs     show what is running / recent logs
      xruns [SECONDS]   what the JACK error recorder records while you watch
                        (default 60 s; 0: everything since jackd started)
      api METHOD PATH [BODY|@file]  one controller API request over USB 1 (docs/reference/api.md)
      enable | disable  allow or skip starting at boot
  bench-build           Cross-build the suppression benchmark into target/bench
  bench WAV [--unpaced] [--policy production|inherited] [--cpus LIST] [--priority N]
                        [--pause-us N] [--spin] [--kernel tiled|rows|compare] [--weights FILE]
                        Time every suppression model on the mixer over a 48 kHz
                        recording and fetch the denoised audio
  check-elf FILE [--shared]  Check a mixer binary against the mixer ABI baseline
  licenses [--base] FILE  Write consolidated license notices HTML (or --base JSON for release jobs)
  non-ui-packages       List desktop Go packages that do not depend on goui-kit
  schema [--check]      Generate Rust and Go wire types from schema/ (--check fails if stale)
  check-ui-link [DIR]   Link ./cmd/rc2fx with release Velopack and goui-kit flags
  app [--release] [--mock] [--embedded] [--build-only] [-- ARGS...]
                        Build the desktop app for this machine into target/debug (a development
                        build), target/release (--release) or target/mock (--mock: the simulated
                        mixer) and run it; --embedded compiles in the mixer payload from target/mixer
  package-check         Build the AppImage in the AlmaLinux 8 container and test it on other distributions
  test-linux-packages [DIR] [--artifacts DIR] [--version VERSION] [--runtime auto|podman|docker]
                        Check the Linux release artifacts, then inspect the AppImage in containers
  build-image-tag       Print the tag of the package-check build image
  device-panel build | check | preview --restart-gui [SECONDS | --keep-running]
                        Build, unit test, or preview from RAM the experimental touchscreen UI
  lint [--fix] [--staged]  Check formatting and lints for Go, Rust, Python, C, shell, and workflows
                        (--fix applies them; --staged checks only staged files)
  package [--target TRIPLE] [--skip-build] [--embedded]
                        Build the download-only desktop app (or embed the mixer with --embedded) and
                        package it with Velopack into target/package

Mixer connection: .rc2fx-mixer.conf (see .rc2fx-mixer.conf.example) or the
RC2FX_MIXER_IP, RC2FX_MIXER_USER, RC2FX_MIXER_KEY, RC2FX_MIXER_KNOWN_HOSTS,
RC2FX_MIXER_HOST_KEY_ALIAS and RC2FX_MIXER_PRE_COMMAND environment variables.

If pre_command is configured, bench and mutating mixer actions run it before
touching the mixer over SSH and abort if it fails. status, logs and api do not
run pre_command (api uses USB 1 only).`

func main() {
	// Relative paths are relative to the repository root, wherever `go -C
	// tools/xtask run .` was started.
	// check-appimage runs alone in a throwaway container, without the
	// repository.
	if len(os.Args) < 2 || os.Args[1] != "check-appimage" {
		if err := os.Chdir(task.Root()); err != nil {
			fmt.Fprintf(os.Stderr, "error: %s\n", err)
			os.Exit(1)
		}
	}
	if err := dispatch(os.Args[1:]); err != nil {
		fmt.Fprintf(os.Stderr, "error: %s\n", err)
		os.Exit(1)
	}
}

func dispatch(args []string) error {
	command := ""
	if len(args) > 0 {
		command = args[0]
	}
	rest := []string{}
	if len(args) > 1 {
		rest = args[1:]
	}
	switch {
	case command == "setup" && len(rest) == 0:
		return setup.Run()
	case command == "fetch-goui-kit" && len(rest) <= 1:
		target := ""
		if len(rest) == 1 {
			target = rest[0]
		}
		return deps.FetchGouiKit(target)
	case command == "fetch-velopack":
		return deps.FetchVelopack(rest)
	case command == "fetch-vpk" && len(rest) == 0:
		_, err := deps.FetchVPK("")
		return err
	case command == "package-check" && len(rest) == 0:
		return pkgcheck.Check()
	case command == "package-check-build" && len(rest) == 0:
		return pkgcheck.Build()
	case command == "test-linux-packages":
		return pkgcheck.TestLinuxPackages(rest)
	case command == "check-appimage" && len(rest) == 2:
		return pkgcheck.CheckAppImage(rest[0], rest[1])
	case command == "build-image-tag" && len(rest) == 0:
		return pkgcheck.PrintImageTag()
	case command == "macos-signing" && len(rest) == 0:
		return signing.MacOS()
	case command == "windows-signing" && len(rest) == 0:
		return signing.Windows()
	case command == "device-panel":
		return mixer.DevicePanel(rest)
	case command == "tuf-test-repos" && (len(rest) == 1 || len(rest) == 2 && rest[1] == "--no-rsa"):
		return release.GenerateTestRepos(rest[0], len(rest) == 1)
	case command == "lint":
		return lint.Run(rest)
	case command == "mixer-build" && len(rest) == 0:
		return setup.BuildMixer()
	case command == "mixer-package" && len(rest) == 1:
		return release.Package(rest[0])
	case command == "mixer-keys" && len(rest) == 1:
		return release.Keys(rest[0])
	case command == "mixer-sign":
		return release.Sign(rest)
	case command == "mixer-refresh":
		return release.Refresh(rest)
	case command == "app-sign":
		return release.AppSign(rest)
	case command == "app-refresh":
		return release.AppRefresh(rest)
	case command == "mixer" && len(rest) >= 1:
		return mixer.Run(rest[0], rest[1:])
	case command == "bench-build" && len(rest) == 0:
		return mixer.BenchBuild()
	case command == "bench" && len(rest) >= 1:
		return mixer.Bench(rest[0], rest[1:])
	case command == "check-elf" && len(rest) == 1:
		return elfcheck.Check(rest[0], false)
	case command == "check-elf" && len(rest) == 2 && rest[1] == "--shared":
		return elfcheck.Check(rest[0], true)
	case command == "licenses" && len(rest) == 1:
		return licenses.WriteLicenses(rest[0])
	case command == "licenses" && len(rest) == 2 && rest[0] == "--base":
		return licenses.WriteBaseLicenses(rest[1])
	case command == "non-ui-packages" && len(rest) == 0:
		return packaging.NonUIPackages()
	case command == "schema" && len(rest) == 0:
		return schema.Run(false)
	case command == "schema" && len(rest) == 1 && rest[0] == "--check":
		return schema.Run(true)
	case command == "check-ui-link" && len(rest) == 0:
		return packaging.CheckUILink("")
	case command == "check-ui-link" && len(rest) == 1:
		return packaging.CheckUILink(rest[0])
	case command == "app":
		return packaging.RunApp(rest)
	case command == "package":
		return packaging.Run(rest)
	case len(args) == 0 || (len(rest) == 0 && (command == "help" || command == "--help" || command == "-h")):
		fmt.Println(help)
		return nil
	}
	return fmt.Errorf("unknown command; see `go -C tools/xtask run . help`\n\n%s", help)
}
