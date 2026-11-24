#!/usr/bin/env python3
"""Host-side tests for rc2fx.sh gui-launch and rc2fx-gui-shim.sh.

  python3 mixer/scripts/tests/gui_launch.py

Sources the real rc2fx.sh, intercepts exec via fake stock-gui/env binaries on
PATH, stubs stamp/sleep/uptime_s/bundle_home, and exercises fallback paths, the
crash-loop guard, log rotation, and shim selection in a temp dir.
"""

import os
import shutil
import socket
import subprocess
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from _common import (
    SCRIPTS_DIR,
    TempDirTestCase,
    sha256_bytes,
    source_rc2fx,
    write_executable,
    write_fake_bin,
)

SHIM = (SCRIPTS_DIR / "rc2fx-gui-shim.sh").read_text()

HARNESS = """say() { printf 'REASON %s\\n' "$*"; }
stamp() { printf 'STAMP'; }
rc2fx_build=${RC2FX_TEST_BUILD:-development}
# The real bundle_home() picks a RAM build over the install; the tests point
# it at one directory.
bundle_home() { printf '%s\\n' "$RC2FX_BUNDLE"; }
# The mixer wait's polls are counted, not timed; a test says what each poll
# sees in audit.json (advancing, constant, or nothing).
sleep() {
  case "${RC2FX_TEST_SLEEP_AUDIT:-}" in
    advance) n=$(( ${n:-0} + 1 )); printf '{"callbacks":%d}' $(( 10 + n )) > "$RC2FX_RUN/audit.json" ;;
    constant) printf '{"callbacks":7}' > "$RC2FX_RUN/audit.json" ;;
    late-audit)
      n=$(( ${n:-0} + 1 ))
      if test "$n" -ge 2; then printf '{"callbacks":%d}' $(( 4 + n )) > "$RC2FX_RUN/audit.json"; fi ;;
    appear)
      # The mixer writes native.pid one poll after the GUI has started.
      n=$(( ${n:-0} + 1 ))
      if test "$n" -ge 2; then printf '%s\\n' "$RC2FX_TEST_MIXER_PID" > "$RC2FX_RUN/native.pid"; fi
      printf '{"callbacks":%d}' $(( 4 + n )) > "$RC2FX_RUN/audit.json" ;;
  esac
}
uptime_s() { cat "$RC2FX_ROOT/uptime"; }
"""


class GuiLaunch(TempDirTestCase):
    def setUp(self):
        super().setUp()
        (self.root / "uptime").write_text("1000\n")
        self.gui_home = self.root / "run-gui"
        self.data = self.root / "data"
        self.data.mkdir()
        self.bundle = self.root / "bundle"
        (self.bundle / "bin").mkdir(parents=True)
        # Fake stock GUI and env binaries record the exec'd command instead of
        # loading the dummy LD_AUDIT library.
        self.bin = write_fake_bin(
            self.root / "bin",
            {"env": "printf 'EXEC env %s\\n' \"$*\""},
            shebang="#!/bin/sh",
        )
        self.stock = write_executable(
            self.root / "stock-gui",
            'printf \'EXEC %s%s\\n\' "$0" "${*:+ $*}"',
            shebang="#!/bin/sh",
        )
        self.make_bundle_module()
        self.run_dir = self.root / "run"
        self.boot_dir = self.root / "boot"
        self.run_dir.mkdir()
        self.boot_dir.mkdir()
        # Copy bash as a fake rc_audio_mixer so /proc/<pid>/exe resolves to this
        # path (multicall coreutils like Ubuntu's sleep reject unknown argv[0]).
        self.fake_mixer = self.root / "rc_audio_mixer"
        shutil.copy(shutil.which("bash"), self.fake_mixer)
        self.env = {
            "PATH": f"{self.bin}:{os.environ['PATH']}",
            "RC2FX_ROOT": str(self.root),
            "RC2FX_HOME": str(self.bundle / "bin"),
            "RC2FX_GUI_HOME": str(self.gui_home),
            "RC2FX_DATA": str(self.data),
            "RC2FX_BUNDLE": str(self.bundle / "bin"),
            "RC2FX_STOCK_GUI": str(self.stock),
            "RC2FX_GUI_SHIM": str(self.root / "installed-gui-shim"),
            "RC2FX_RUN": str(self.run_dir),
            "RC2FX_BOOT": str(self.boot_dir),
            "RC2FX_STOCK_MIXER": str(self.fake_mixer),
            "RC2FX_GUI_WAIT_MAX": "1",  # two polls with stubbed sleep()
            "RC2FX_AUDIO_SHIM": str(self.root / "audio-shim"),
            "RC2FX_GUI_BOOT_WINDOW": "180",
        }
        (self.root / "installed-gui-shim").write_text("#!/bin/sh\n")
        self.stock_hash = sha256_bytes(self.stock.read_bytes())
        self.gui_hash = self.stock_hash

    def make_bundle_module(self):
        module = self.bundle / "bin" / "librc2fx_gui.so"
        data = b"ELF-module"
        module.write_bytes(data)
        (self.bundle / "bin" / "SHA256SUMS").write_text(
            f"{sha256_bytes(data)}  librc2fx_gui.so\n"
        )

    def run_gui(self, *args, env=None, body='gui_launch "$@"', hash_override=True):
        e = dict(self.env, **(env or {}))
        script = HARNESS
        if hash_override:
            script += f"gui_sha={self.gui_hash}\n"
        script += f"set -- {' '.join(args) if args else ''}\n" + body
        return source_rc2fx(script, env=e, cwd=self.root, timeout=60)

    def logged(self):
        log = self.gui_home / "gui.log"
        return log.read_text() if log.exists() else ""

    def last_logged(self):
        return self.logged().splitlines()[-1] if self.logged() else ""

    def assert_stock(self, result, reason):
        self.assertIn(f"EXEC {self.stock}", result.stdout, result.stderr)
        self.assertNotIn("LD_AUDIT", result.stdout)
        self.assertIn(reason, self.logged())

    def test_each_fallback_reason(self):
        cases = {
            "first boot": ("/usr/bin/first-boot", "first boot after a firmware update"),
            "production boot": ("/usr/bin/production-boot", "production boot"),
        }
        for name, (marker, reason) in cases.items():
            with self.subTest(name):
                if not Path(marker).exists():
                    self.skipTest(f"{marker} is not on this host")
        self.addCleanup(
            lambda: (self.data / "gui-partial-disabled").unlink(missing_ok=True)
        )
        for name, setup, reason in (
            (
                "disabled in settings",
                lambda: self.data.joinpath("gui-partial-disabled").write_text(""),
                "partial conversion is disabled in settings",
            ),
            (
                "off for this boot",
                lambda: (
                    self.gui_home.mkdir(exist_ok=True)
                    or self.gui_home.joinpath("off").write_text("")
                ),
                "partial conversion is off for this boot",
            ),
            (
                "crashed three times",
                lambda: (
                    self.gui_home.mkdir(exist_ok=True)
                    or self.gui_home.joinpath("stock").write_text(
                        "the GUI module crashed it 3 times within 30 s\n"
                    )
                ),
                "the GUI module crashed it",
            ),
            (
                "gui not qualified",
                lambda: setattr(self, "gui_hash", "0" * 64),
                "this GUI is not the qualified one",
            ),
            (
                "no bundle",
                lambda: (self.bundle / "bin" / "librc2fx_gui.so").unlink(),
                "no bundle module",
            ),
        ):
            with self.subTest(name):
                for marker in (
                    self.data / "gui-partial-disabled",
                    self.gui_home / "off",
                    self.gui_home / "stock",
                ):
                    marker.unlink(missing_ok=True)
                setup()
                r = self.run_gui("--x")
                self.gui_hash = self.stock_hash
                self.assert_stock(r, reason)

    def test_a_tampered_module_falls_back(self):
        (self.bundle / "bin" / "librc2fx_gui.so").write_bytes(b"changed")
        r = self.run_gui("--x")
        self.assert_stock(r, "fails its checksum")

    def test_the_module_starts_with_the_production_environment(self):
        r = self.run_gui("--x", env={"RC2FX_TEST_BUILD": "release"})
        self.assertEqual(r.returncode, 0, r.stderr)
        line = next(l for l in r.stdout.splitlines() if l.startswith("EXEC "))
        self.assertIn(f"LD_AUDIT={self.bundle}/bin/librc2fx_gui.so", line)
        self.assertIn("RC2FX_GUI_PACING=partial-cw", line)
        self.assertIn("RC2FX_GUI_PACING_QUIET=1", line)
        self.assertIn(f"RC2FX_GUI_PACING_LOG={self.gui_home}/pacing.log", line)
        self.assertTrue(line.endswith(f" {self.stock} --x"))
        self.assertIn("GUI module starting from", self.logged())
        self.assertIn("STAMP", self.logged())

    def test_the_build_kind_picks_the_default_diagnostics(self):
        r = self.run_gui("--x")
        self.assertIn("LD_AUDIT=", r.stdout)
        self.assertNotIn("RC2FX_GUI_PACING_QUIET", r.stdout)
        self.assertIn("GUI module starting from", self.logged())
        self.assertIn("(verbose diagnostics)", self.last_logged())
        (self.root / "uptime").write_text("2000\n")  # outside the 30s crash-loop window
        r = self.run_gui("--x", env={"RC2FX_TEST_BUILD": "release"})
        self.assertIn("RC2FX_GUI_PACING_QUIET=1", r.stdout)
        self.assertNotIn("(verbose diagnostics)", self.last_logged())

    def start_mixer(self):
        """Start a dummy process whose /proc/<pid>/exe points to fake_mixer."""
        if not Path("/proc/self/exe").exists():
            self.skipTest("/proc/<pid>/exe is not on this host")
        # Block in bash's builtin read so no child process is spawned.
        process = subprocess.Popen(
            [str(self.fake_mixer), "-c", "read -r -t 30 _ || :"], stdin=subprocess.PIPE
        )
        self.addCleanup(process.wait)
        self.addCleanup(process.stdin.close)
        self.addCleanup(process.kill)
        (self.run_dir / "native.pid").write_text(f"{process.pid}\n")
        return process

    def control_socket(self):
        """Bind a real Unix socket at $run/control.sock."""
        server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        server.bind(str(self.run_dir / "control.sock"))
        server.listen(1)
        self.addCleanup(server.close)

    def boot_fixture(self):
        """Set up a fresh boot state with the audio shim present and low uptime."""
        write_executable(self.root / "audio-shim", "", shebang="#!/bin/sh")
        (self.root / "uptime").write_text("5\n")
        self.control_socket()

    def test_the_boot_gui_waits_for_the_mixer_to_appear_then_for_readiness(self):
        self.boot_fixture()
        mixer = self.start_mixer()
        self.run_dir.joinpath("native.pid").unlink()  # not written yet
        r = self.run_gui(
            "--x",
            env={
                "RC2FX_TEST_SLEEP_AUDIT": "appear",
                "RC2FX_TEST_MIXER_PID": str(mixer.pid),
                "RC2FX_GUI_WAIT_MAX": "5",
            },
        )
        self.assertIn("EXEC ", r.stdout)
        # native.pid appears on poll 1, callbacks advance on poll 2 (1s total).
        self.assertIn("Waited 1 s for the mixer before starting the GUI", self.logged())

    def test_no_audit_json_yet_does_not_abort_the_wait(self):
        # At boot the socket can appear before audit.json; make sure set -e doesn't abort.
        self.boot_fixture()
        self.start_mixer()
        r = self.run_gui(
            "--x",
            env={"RC2FX_TEST_SLEEP_AUDIT": "late-audit", "RC2FX_GUI_WAIT_MAX": "4"},
        )
        self.assertIn("RC2FX_GUI_PACING", r.stdout, "the module started, not stock")
        self.assertIn("LD_AUDIT=", r.stdout)
        self.assertNotIn("reported ready", self.logged())
        self.assertIn("Waited", self.logged())

    def test_an_unexpected_abort_logs_before_starting_stock(self):
        # Make `cat $gui_home/stock` fail under set -e to exercise the EXIT trap.
        self.gui_home.mkdir()
        (self.gui_home / "stock").mkdir()
        r = self.run_gui("--x")
        self.assertIn(f"EXEC {self.stock}", r.stdout)
        self.assertNotIn("LD_AUDIT", r.stdout)
        self.assertIn(
            "GUI launch failed unexpectedly; stock GUI started", self.logged()
        )

    def test_a_mixer_that_never_appears_times_out_and_the_gui_starts(self):
        self.boot_fixture()
        r = self.run_gui(
            "--x", env={"RC2FX_TEST_SLEEP_AUDIT": "constant", "RC2FX_GUI_WAIT_MAX": "1"}
        )
        self.assertIn("EXEC ", r.stdout)
        self.assertIn(
            "GUI started before the mixer reported ready (1 s)", self.logged()
        )

    def test_a_later_gui_start_with_no_mixer_does_not_wait(self):
        (self.root / "uptime").write_text("1000\n")
        r = self.run_gui(
            "--x", env={"RC2FX_TEST_SLEEP_AUDIT": "constant", "RC2FX_GUI_WAIT_MAX": "5"}
        )
        self.assertIn("EXEC ", r.stdout)
        self.assertNotIn("Waited", self.logged())
        self.assertNotIn("reported ready", self.logged())
        # Even with the shim installed, late uptime isn't treated as a boot start.
        write_executable(self.root / "audio-shim", "", shebang="#!/bin/sh")
        r = self.run_gui(
            "--x", env={"RC2FX_TEST_SLEEP_AUDIT": "constant", "RC2FX_GUI_WAIT_MAX": "5"}
        )
        self.assertIn("EXEC ", r.stdout)
        self.assertNotIn("reported ready", self.logged())

    def test_no_wait_without_an_rc2fx_mixer_in_charge(self):
        r = self.run_gui("--x")
        self.assertIn("EXEC ", r.stdout)
        self.assertNotIn("Waited", self.logged())
        self.assertNotIn("reported ready", self.logged())
        # Stale PID pointing to a non-mixer process shouldn't trigger a wait either.
        self.run_dir.joinpath("native.pid").write_text(f"{os.getpid()}\n")
        r = self.run_gui("--x")
        self.assertIn("EXEC ", r.stdout)
        self.assertNotIn("Waited", self.logged())

    def test_waits_then_execs_once_the_callbacks_advance(self):
        self.start_mixer()
        self.control_socket()
        r = self.run_gui(
            "--x", env={"RC2FX_TEST_SLEEP_AUDIT": "advance", "RC2FX_GUI_WAIT_MAX": "5"}
        )
        self.assertIn("EXEC ", r.stdout)
        self.assertIn("Waited 1 s for the mixer before starting the GUI", self.logged())

    def test_a_mixer_that_never_reports_still_starts_the_gui(self):
        self.start_mixer()
        self.control_socket()
        r = self.run_gui(
            "--x", env={"RC2FX_TEST_SLEEP_AUDIT": "constant", "RC2FX_GUI_WAIT_MAX": "1"}
        )
        self.assertIn("EXEC ", r.stdout)
        self.assertIn(
            "GUI started before the mixer reported ready (1 s)", self.logged()
        )

    def test_no_wait_before_the_socket_exists(self):
        # Callbacks advancing without control.sock shouldn't count as ready.
        self.start_mixer()
        r = self.run_gui(
            "--x", env={"RC2FX_TEST_SLEEP_AUDIT": "advance", "RC2FX_GUI_WAIT_MAX": "1"}
        )
        self.assertIn("EXEC ", r.stdout)
        self.assertIn(
            "GUI started before the mixer reported ready (1 s)", self.logged()
        )

    def test_the_stock_marker_skips_the_wait(self):
        self.start_mixer()
        self.control_socket()
        (self.boot_dir / "stock").write_text('"a reason"\n')
        r = self.run_gui(
            "--x", env={"RC2FX_TEST_SLEEP_AUDIT": "advance", "RC2FX_GUI_WAIT_MAX": "5"}
        )
        self.assertIn("EXEC ", r.stdout)
        self.assertNotIn("Waited", self.logged())

    def test_status_names_the_diagnostics_of_the_build_kind(self):
        self.gui_home.mkdir()
        for build, expected in (
            ("development", "verbose (development build)"),
            ("release", "quiet (release build)"),
        ):
            with self.subTest(build=build):
                r = self.run_gui(body="gui_status", env={"RC2FX_TEST_BUILD": build})
                self.assertIn(f"GUI diagnostics: {expected}", r.stdout, r.stderr)
                lines = [
                    x for x in r.stdout.splitlines() if x.startswith("GUI diagnostics:")
                ]
                self.assertEqual(len(lines), 1, lines)

    def test_status_has_no_screen_conversion_setting(self):
        self.gui_home.mkdir()
        (self.gui_home / "screen-conversion").write_text("hardware\n")
        r = self.run_gui(body="gui_status")
        self.assertNotIn("Screen conversion:", r.stdout)

    def test_the_previous_pacing_log_is_rotated(self):
        self.gui_home.mkdir()
        (self.gui_home / "pacing.log").write_text("old\n")
        self.run_gui()
        self.assertEqual((self.gui_home / "pacing.prev.log").read_text(), "old\n")
        self.assertFalse((self.gui_home / "pacing.log").exists())

    def test_three_starts_within_thirty_seconds_drop_to_stock_for_the_boot(self):
        r = self.run_gui()
        self.assertIn("LD_AUDIT=", r.stdout)
        (self.root / "uptime").write_text("1020\n")
        r = self.run_gui()
        self.assertIn("LD_AUDIT=", r.stdout)
        (self.root / "uptime").write_text("1029\n")
        r = self.run_gui()
        self.assert_stock(r, "crashed it 3 times within 30 s")
        self.assertEqual(
            (self.gui_home / "stock").read_text().strip(),
            "the GUI module crashed it 3 times within 30 s",
        )
        # Stays on stock for the rest of the boot even after the 30s window passes.
        (self.root / "uptime").write_text("2000\n")
        self.assert_stock(self.run_gui(), "crashed it 3 times within 30 s")

    def test_starts_spread_out_are_not_a_crash_loop(self):
        for uptime in ("1000", "1031", "1062", "1093"):
            (self.root / "uptime").write_text(uptime + "\n")
            r = self.run_gui()
            self.assertIn("LD_AUDIT=", r.stdout, f"uptime {uptime}")
        self.assertFalse((self.gui_home / "stock").exists())

    def test_status_reports_module_and_counters(self):
        self.gui_home.mkdir()
        (self.gui_home / "pacing.log").write_text(
            "start pid=1 t=2 merge_px=80000\ncounters pid=1 t=3 conversions=9\n"
        )
        (self.gui_home / "gui.log").write_text("GUI module starting from x\n")
        r = self.run_gui(body="gui_status")
        self.assertIn("GUI: partial conversion, module", r.stdout)
        self.assertIn("GUI counters: counters pid=1 t=3 conversions=9", r.stdout)

    def test_status_reasons_match_the_fallback_ones(self):
        self.data.joinpath("gui-partial-disabled").write_text("")
        r = self.run_gui(body="gui_status")
        self.assertIn(
            "GUI: stock (partial conversion is disabled in settings)", r.stdout
        )
        self.data.joinpath("gui-partial-disabled").unlink()
        self.gui_home.mkdir()
        (self.gui_home / "stock").write_text(
            "the GUI module crashed it 3 times within 30 s\n"
        )
        r = self.run_gui(body="gui_status")
        self.assertIn(
            "GUI: stock until the next boot (the GUI module crashed it 3 times within 30 s)",
            r.stdout,
        )


class Shim(TempDirTestCase):
    """Tests for rc2fx-gui-shim.sh dispatching to rc2fx.sh gui-launch."""

    def setUp(self):
        super().setUp()
        self.shim = self.root / "rc_gui_juce"
        text = SHIM.replace("/run/rc2fx-dev/rc2fx.sh", str(self.root / "dev/rc2fx.sh"))
        text = text.replace("/opt/rc2fx/rc2fx.sh", str(self.root / "opt/rc2fx.sh"))
        text = text.replace("/usr/bin/rc_gui_juce", str(self.root / "stock-gui"))
        self.shim.write_text(text)
        self.shim.chmod(0o755)
        write_executable(
            self.root / "stock-gui", 'printf "STOCK %s\\n" "$*"', shebang="#!/bin/sh"
        )

    def test_hands_over_to_the_rc2fx_script_that_has_gui_launch(self):
        (self.root / "opt").mkdir(exist_ok=True)
        (self.root / "opt/rc2fx.sh").write_text(
            '#!/bin/bash\ncase "${1:-}" in\n  gui-launch) shift; printf "LAUNCH %s\\n" "$*"; exit 7;;\nesac\nexit 9\n'
        )
        r = subprocess.run(
            [str(self.shim), "--a", "b"],
            capture_output=True,
            text=True,
            timeout=30,
            check=False,
        )
        self.assertEqual((r.returncode, r.stdout), (7, "LAUNCH --a b\n"))

    def test_a_build_in_ram_wins(self):
        dev = self.root / "dev/rc2fx.sh"
        dev.parent.mkdir()
        dev.write_text(
            '#!/bin/bash\ncase "${1:-}" in\n  gui-launch) shift; printf "RAM %s\\n" "$*"; exit 5;;\nesac\n'
        )
        (self.root / "opt").mkdir(exist_ok=True)
        (self.root / "opt/rc2fx.sh").write_text(
            '#!/bin/bash\ncase "${1:-}" in\n  gui-launch) printf "INSTALLED\\n"; exit 6;;\nesac\n'
        )
        r = subprocess.run(
            [str(self.shim), "x"],
            capture_output=True,
            text=True,
            timeout=30,
            check=False,
        )
        self.assertEqual((r.returncode, r.stdout), (5, "RAM x\n"))

    def test_without_a_script_that_has_gui_launch_the_stock_gui_runs(self):
        (self.root / "opt").mkdir(exist_ok=True)
        (self.root / "opt/rc2fx.sh").write_text(
            "#!/bin/bash\nexit 9\n"
        )  # no gui-launch case
        r = subprocess.run(
            [str(self.shim), "--q"],
            capture_output=True,
            text=True,
            timeout=30,
            check=False,
        )
        self.assertEqual(r.stdout, "STOCK --q\n")


if __name__ == "__main__":
    unittest.main()
