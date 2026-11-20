// Command xtask implements cross-platform project tasks, invoked via `just` or
// `./xtask <command>` from the repo root.
package main

import (
	"fmt"
	"os"

	"github.com/ibanks42/rc2fx/desktop/internal/xtask/elfcheck"
	"github.com/ibanks42/rc2fx/desktop/internal/xtask/licenses"
	"github.com/ibanks42/rc2fx/desktop/internal/xtask/mixer"
	"github.com/ibanks42/rc2fx/desktop/internal/xtask/packaging"
	"github.com/ibanks42/rc2fx/desktop/internal/xtask/release"
	"github.com/ibanks42/rc2fx/desktop/internal/xtask/setup"
)

const help = `./xtask <command>

  setup                 Install the Rust target and cargo-zigbuild, and check
                        for zig
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
  non-ui-packages       List desktop Go packages that do not depend on gpuikit
  check-ui-link [DIR]   Link ./cmd/rc2fx with release Velopack and gpuikit flags
  app [--release] [--mock] [--embedded] [--build-only] [-- ARGS...]
                        Build the desktop app for this machine into target/debug (a development
                        build), target/release (--release) or target/mock (--mock: the simulated
                        mixer) and run it; --embedded compiles in the mixer payload from target/mixer
  package [--target TRIPLE] [--skip-build] [--embedded]
                        Build the download-only desktop app (or embed the mixer with --embedded) and bundle
                        documentation, licenses and SHA256SUMS in target/package

Mixer connection: .rc2fx-mixer.conf (see .rc2fx-mixer.conf.example) or the
RC2FX_MIXER_IP, RC2FX_MIXER_USER, RC2FX_MIXER_KEY and RC2FX_MIXER_KNOWN_HOSTS
environment variables.

bench and every mixer action except status, logs and api first arm the mixer's
one-shot power recovery over SSH and refuse to run if they cannot
(docs/developer/mixer-workflow.md). api uses USB 1 only and does not arm.`

// Launcher working directory so relative path arguments resolve as expected.
const cwdVariable = "RC2FX_XTASK_CWD"

func main() {
	if dir := os.Getenv(cwdVariable); dir != "" {
		if err := os.Chdir(dir); err != nil {
			fmt.Fprintf(os.Stderr, "error: %s: %s\n", dir, err)
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
	return fmt.Errorf("unknown command; see `./xtask help`\n\n%s", help)
}
