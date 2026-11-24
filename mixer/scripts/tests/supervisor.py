#!/usr/bin/env python3
"""Host tests for the rc2fx.sh helpers invoked by rc2fx-supervisor."""

import os
import subprocess
import sys
import time
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from _common import (
    RC2FX_SH,
    TempDirTestCase,
    function_body,
    run_rc2fx,
    source_rc2fx,
    write_executable,
)


@unittest.skipUnless(
    Path("/proc/self/stat").exists(), "/proc/<pid>/stat is not on this host"
)
class Tests(TempDirTestCase):
    def setUp(self):
        super().setUp()
        self.env = {"RC2FX_LOGS": str(self.root), "RC2FX_DATA": str(self.root)}

    def script(self, *args):
        return run_rc2fx(*args, env=self.env)

    def arguments(self, setup):
        body = (
            f"home={self.root}\ndata=/d\nrun=/r\nshow=\nprofile_id=\n{setup}\n"
            'controller_arguments\nprintf "%s\\n" "${controller[@]}"\n'
        )
        r = source_rc2fx(body)
        self.assertEqual(r.returncode, 0, r.stderr)
        return r.stdout.split("\n")[:-1]

    def test_exit_writes_the_last_exit_log_and_succeeds_with_nothing_to_read(self):
        r = self.script("supervise-exit")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("Supervisor stopping", r.stdout)
        self.assertIn("--- kernel", (self.root / "last-exit.log").read_text())

    def test_exit_stops_a_controller_that_is_still_running(self):
        child = subprocess.Popen(["sleep", "60"])
        self.addCleanup(child.kill)
        # The child stays a zombie until reaped, which the script counts as gone.
        began = time.monotonic()
        r = self.script("supervise-exit", str(child.pid))
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(child.wait(timeout=5), -15)
        self.assertLess(time.monotonic() - began, 5)

    def test_restart_keeps_the_gadget_only_when_asked(self):
        stubs = 'mixer_start=\nretire_usb_node() { echo "retire $*"; }\nstop_owned() { echo "stop $*"; }\n'
        for keep, expected in [
            ("0", "retire 10 20\nstop 20\nstop 10 99\n"),
            ("1", "stop 20\nstop 10 99\n"),
        ]:
            r = source_rc2fx(stubs + f"supervise_restart {keep} 10 99 20\n")
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertEqual(r.stdout, expected)

    def test_a_refused_usb_retirement_fails_the_restart_and_stops_nothing(self):
        stubs = "mixer_start=\nretire_usb_node() { return 1; }\nstop_owned() { echo stopped; }\n"
        r = source_rc2fx(stubs + "supervise_restart 0 10 99 20\n", strict=False)
        self.assertNotEqual(r.returncode, 0)
        self.assertEqual(r.stdout, "")

    def test_start_restart_and_stop_refuse_during_first_boot_or_production_boot(self):
        marker = self.root / "boot-marker"
        marker.touch()
        for var in ("RC2FX_FIRST_BOOT", "RC2FX_PRODUCTION_BOOT"):
            for action in ("start", "restart", "stop"):
                r = run_rc2fx(action, env={**self.env, var: str(marker)})
                self.assertEqual(r.returncode, 4, f"{var} {action}: {r.stderr}")
                self.assertIn("Refusing:", r.stderr)

    def test_the_controller_always_gets_its_sockets_and_developer_ssh(self):
        self.assertEqual(
            self.arguments(""),
            [
                "--data",
                "/d",
                "--native-socket",
                "/r/control.sock",
                "--usb-socket",
                "/r/usb.sock",
                "--developer-ssh",
            ],
        )
        self.assertEqual(
            self.arguments("RC2FX_UI_SOCKET=1"),
            [
                "--data",
                "/d",
                "--native-socket",
                "/r/control.sock",
                "--usb-socket",
                "/r/usb.sock",
                "--developer-ssh",
                "--ui-socket",
                "/r/ui.sock",
            ],
        )

    def test_optional_controller_arguments_follow_what_exists(self):
        (self.root / "mixer-release.json").write_text("{}")
        show = self.root / "show"
        show.write_text("{}")
        args = self.arguments(f"show={show}\nprofile_id=p1")
        self.assertEqual(
            args[7:],
            [
                "--stock-config",
                str(show),
                "--release",
                f"{self.root}/mixer-release.json",
                "--firmware-catalog",
                f"{self.root}/firmware-profiles.json",
                "--firmware-profile",
                "p1",
            ],
        )

    def test_the_supervisor_gets_a_script_that_outlives_its_bundle(self):
        # An install removes a RAM bundle, then asks the running supervisor to
        # stop. Its exit steps must work with nothing left of that bundle.
        launch = function_body("launch")
        self.assertIn('cp "$home/rc2fx.sh" "$run/rc2fx.sh"', launch)
        self.assertIn('--script "$run/rc2fx.sh"', launch)
        self.assertLess(launch.index('cp "$home/rc2fx.sh"'), launch.index("setsid env"))
        run = self.root / "run"
        run.mkdir()
        (run / "rc2fx.sh").write_text(RC2FX_SH.read_text())
        r = subprocess.run(
            ["bash", str(run / "rc2fx.sh"), "supervise-exit"],
            capture_output=True,
            text=True,
            env=dict(os.environ, **self.env),
            cwd=run,
            timeout=30,
            check=False,
        )
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("--- kernel", (self.root / "last-exit.log").read_text())

    def test_launch_records_the_supervisors_own_pid(self):
        # The supervisor keeps the PID launch starts it with through setsid and
        # env, and gets the controller's arguments after `--`.
        own, args = self.root / "supervisor-own-pid", self.root / "supervisor-args"
        code, log = self.injected_launch(
            f'test "${{1:-}}" = --version && exit 0\necho $$ > {own}\nprintf "%s\\n" "$@" > {args}\nexit 0',
            "sleep 1\nexit 7",
        )
        self.assertEqual(code, 7, log)
        self.assertEqual(
            (self.root / "run/supervisor.pid").read_text(), own.read_text()
        )
        passed = args.read_text().splitlines()
        self.assertEqual(passed[0], "--mixer")
        self.assertEqual(
            passed[passed.index("--") + 1 :][:2], ["--data", f"{self.root}/data"]
        )

    def stop_owned(self, target, started):
        return source_rc2fx(f"stop_owned {target} {started}\n")

    def test_a_pid_that_has_moved_on_is_not_stopped(self):
        child = subprocess.Popen(["sleep", "60"])
        self.addCleanup(child.kill)
        self.assertEqual(self.stop_owned(child.pid, 1).returncode, 0)
        self.assertIsNone(child.poll())

    def test_a_pid_with_the_right_start_time_is_stopped(self):
        child = subprocess.Popen(["sleep", "60"])
        self.addCleanup(child.kill)
        stat = Path(f"/proc/{child.pid}/stat").read_text()
        started = stat.rsplit(") ", 1)[1].split()[19]
        self.assertEqual(self.stop_owned(child.pid, started).returncode, 0)
        self.assertEqual(child.wait(timeout=5), -15)

    def injected_launch(self, supervisor, mixer="exec sleep 30", full_log=False):
        """Runs the real launch with its system steps stubbed, as far as the end
        of the mixer. Returns the exit code and the log. With full_log, the log
        is /dev/full and the real-time throttle steps run against a file."""
        home, run, boot = self.root / "home", self.root / "run", self.root / "boot"
        home.mkdir()
        for name, text in {
            "rc2fx-supervisor": supervisor,
            "rc2fxd": "exit 0",
            "rc2fx-firmware": "echo profile show",
            "stock": mixer,
        }.items():
            write_executable(home / name, text)
        (home / "rc2fx.sh").write_text(RC2FX_SH.read_text())
        subprocess.run(
            "sha256sum rc2fxd > SHA256SUMS", shell=True, cwd=home, check=True
        )
        stubbed = [
            "image_check_start",
            "flash_watch_start",
            "prepare_usb",
            "jack_recorder",
            "retire_usb_node",
        ]
        if full_log:
            (self.root / "rc2fx.log").symlink_to("/dev/full")
            (self.root / "rt").write_text("950000\n")
        else:
            stubbed += ["relax_rt_throttle", "restore_rt_throttle"]
        stubs = "".join(f"{name}() {{ :; }}\n" for name in stubbed)
        body = (
            f"show=\nprofile_id=\nvendor_log=/nonexistent\n"
            f"rt_throttle={self.root}/rt\nrt_throttle_default=950000\n"
            "keep_usb=0\nmixer_start=\nredact() { cat; }\npgrep() { return 1; }\n"
            + stubs
            + "launch\n"
        )
        env = {
            "RC2FX_HOME": str(home),
            "RC2FX_DATA": str(self.root / "data"),
            "RC2FX_LOGS": str(self.root),
            "RC2FX_RUN": str(run),
            "RC2FX_BOOT": str(boot),
            "RC2FX_STOCK_MIXER": str(home / "stock"),
        }
        r = source_rc2fx(body, env=env)
        if full_log:
            return r.returncode, ""
        return r.returncode, (self.root / "rc2fx.log").read_text()

    def test_a_supervisor_that_crashes_at_once_leaves_stock_behind(self):
        code, log = self.injected_launch('test "${1:-}" = --version || exit 2')
        self.assertIn(
            "Supervisor died with status 2", (self.root / "boot/stock").read_text()
        )
        self.assertIn("Supervisor died with status 2", log)
        self.assertIn("--- kernel", (self.root / "last-exit.log").read_text())
        self.assertEqual(code, 143)

    def test_a_supervisor_killed_later_leaves_stock_behind(self):
        code, _log = self.injected_launch(
            'test "${1:-}" = --version && exit 0\nsleep 1\nkill -KILL $$'
        )
        self.assertIn(
            "Supervisor died with status 137", (self.root / "boot/stock").read_text()
        )
        self.assertEqual(code, 143)

    def test_a_supervisor_that_hands_the_mixer_back_is_not_a_crash(self):
        # It exits 0 when it leaves the mixer running on purpose.
        code, log = self.injected_launch("exit 0", "sleep 1\nexit 7")
        self.assertFalse((self.root / "boot/stock").exists())
        self.assertNotIn("Supervisor died", log)
        self.assertIn("Mixer exited with status 7", log)
        self.assertEqual(code, 7)

    def test_a_signal_to_the_launch_shell_still_ends_the_mixer(self):
        # A signal ends the wait early, with no process to name.
        code, log = self.injected_launch(
            'test "${1:-}" = --version && exit 0\nsleep 1\nkill -TERM $PPID\nexec sleep 3'
        )
        self.assertIn("Mixer exited with status 143", log)
        self.assertFalse((self.root / "boot/stock").exists())
        self.assertEqual(code, 143)

    def test_the_mixers_own_status_survives_the_watch_on_the_supervisor(self):
        code, log = self.injected_launch(
            'test "${1:-}" = --version && exit 0\nsleep 5', "sleep 1\nexit 7"
        )
        self.assertFalse((self.root / "boot/stock").exists())
        self.assertIn("Mixer exited with status 7", log)
        self.assertEqual(code, 7)

    @unittest.skipUnless(Path("/dev/full").exists(), "/dev/full is not on this host")
    def test_a_full_log_after_the_stock_fallback_is_gone_still_runs_the_mixer(self):
        # Past `trap - EXIT` nothing falls back to stock: a log write that fails
        # (a full root) must not end the launch shell before or after the mixer.
        ran, during = self.root / "mixer-ran", self.root / "rt-during"
        code, _ = self.injected_launch(
            "exit 0",
            f"cp {self.root}/rt {during}\ntouch {ran}\nsleep 1\nexit 7",
            full_log=True,
        )
        self.assertTrue(ran.exists())
        self.assertEqual(during.read_text(), "-1\n")
        self.assertEqual((self.root / "rt").read_text(), "950000\n")
        self.assertEqual(code, 7)

    def launch_with(self, supervisor):
        """Runs the real launch against a temporary bundle with its paths in the fixture."""
        home = self.root / "home"
        home.mkdir()
        stubs = {
            "rc2fx-firmware": "echo profile show",
            "rc2fx-supervisor": supervisor,
            "stock": f'printf "%s\\n" "${{LD_AUDIT:-none}}" "$*" > {self.root}/stock-ran',
        }
        for name, text in stubs.items():
            write_executable(home / name, text)
        subprocess.run(
            "sha256sum rc2fx-firmware > SHA256SUMS", shell=True, cwd=home, check=True
        )
        env = dict(
            self.env,
            RC2FX_HOME=str(home),
            RC2FX_RUN=str(self.root / "run"),
            RC2FX_BOOT=str(self.root / "boot"),
            RC2FX_STOCK_MIXER=str(home / "stock"),
        )
        r = run_rc2fx("launch", "--arg", env=env)
        self.assertEqual(r.returncode, 0, r.stderr)
        return (self.root / "stock-ran").read_text().splitlines()

    def test_a_supervisor_that_cannot_run_starts_the_stock_mixer_without_rc2fx(self):
        self.assertEqual(self.launch_with("exit 1"), ["none", "--arg"])
        self.assertIn(
            "Stock mixer started: the supervisor cannot run",
            (self.root / "rc2fx.log").read_text(),
        )

    def test_stop_supervisor_fails_immediately_when_touch_fails(self):
        body = f"""
run={self.root}/missing-run
sleeps=0
alive() {{ return 0; }}
sleep() {{ sleeps=$((sleeps + 1)); }}
rc=0
stop_supervisor 4242 || rc=$?
printf 'rc=%d sleeps=%d\\n' "$rc" "$sleeps"
"""
        r = source_rc2fx(body)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("sleeps=0", r.stdout)
        self.assertNotIn("rc=0 ", r.stdout)
        self.assertTrue(r.stderr.strip(), "touch error message must be preserved")

    def test_update_reload_matches_manifest_lines(self):
        home = self.root / "home"
        run = self.root / "run"
        logs = self.root / "logs"
        for d in (home, run, logs):
            d.mkdir()
        (home / "rc2fx.sh").write_text("# same\n")
        (run / "rc2fx.sh").write_text("# same\n")
        (run / "home").write_text(f"{home}\n")
        (run / "supervisor.features").write_text("reload\n")
        names = [
            "librc2fx_native.so",
            "rc2fx-supervisor",
            "librc2fx_gui.so",
            "librc2fx_jackerr.so",
            "rc2fx_jackerr_snapshot",
        ]
        good = "".join(f"{'a' * 64}  {n}\n" for n in names)

        def run_reload(bundle_sums, home_sums):
            (run / "bundle.sums").write_text(bundle_sums)
            (home / "SHA256SUMS").write_text(home_sums)
            (run / "reload").unlink(missing_ok=True)
            body = """
supervisor() { echo 123; }
uptime_s() { echo 100; }
update_ready() { return 0; }
sleep() { :; }
update_reload 1.2.3 200
"""
            return source_rc2fx(
                body,
                env={
                    "RC2FX_HOME": str(home),
                    "RC2FX_RUN": str(run),
                    "RC2FX_LOGS": str(logs),
                },
                strict=False,
            )

        self.assertEqual(run_reload(good, good).returncode, 0)
        star = good.replace("  librc2fx_native.so", " *librc2fx_native.so")
        self.assertNotEqual(run_reload(good, star).returncode, 0)
        dup = good + f"{'b' * 64}  librc2fx_native.so\n"
        self.assertNotEqual(run_reload(good, dup).returncode, 0)


class StopOwnedTests(TempDirTestCase):
    """Tests for stop_owned() in both normal and strict modes against a fake /proc."""

    def run_stop(self, *, mode="", setup="", target="4242", started="100"):
        proc = self.root / "proc"
        proc.mkdir(exist_ok=True)
        body = f"""
write_stat() {{
  local p=$1 st=$2 t=$3
  mkdir -p "$proc/$p"
  printf '%s (fake) %s 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 %s 0 0\\n' "$p" "$st" "$t" > "$proc/$p/stat"
}}
sleeps=0
kills=()
logs=()
say() {{ logs+=("$*"); }}
on_kill() {{ return 0; }}
on_sleep() {{ :; }}
kill() {{
  kills+=("$1 $2")
  on_kill "$1" "$2"
}}
sleep() {{
  sleeps=$((sleeps + 1))
  on_sleep "$sleeps"
}}
{setup}
rc=0
stop_owned {target} {started} {mode} || rc=$?
printf 'RC=%d\\n' "$rc"
printf 'SLEEPS=%d\\n' "$sleeps"
for k in "${{kills[@]}}"; do printf 'KILL=%s\\n' "$k"; done
for l in "${{logs[@]}}"; do printf 'LOG=%s\\n' "$l"; done
"""
        r = source_rc2fx(body, env={"RC2FX_PROC": str(proc)})
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        parsed = {"rc": None, "sleeps": None, "kills": [], "logs": []}
        for line in r.stdout.splitlines():
            key, val = line.split("=", 1)
            if key == "RC":
                parsed["rc"] = int(val)
            elif key == "SLEEPS":
                parsed["sleeps"] = int(val)
            elif key == "KILL":
                parsed["kills"].append(val)
            elif key == "LOG":
                parsed["logs"].append(val)
        return parsed

    def test_process_gone_before_call(self):
        normal = self.run_stop()
        self.assertEqual((normal["rc"], normal["kills"], normal["sleeps"]), (0, [], 0))
        strict = self.run_stop(mode="strict")
        self.assertEqual((strict["rc"], strict["kills"], strict["sleeps"]), (1, [], 0))

    def test_zombie_with_matching_start_time_counts_as_stopped(self):
        setup = "write_stat 4242 Z 100"
        for mode in ("", "strict"):
            with self.subTest(mode=mode or "normal"):
                out = self.run_stop(mode=mode, setup=setup)
                self.assertEqual((out["rc"], out["kills"], out["sleeps"]), (0, [], 0))

    def test_term_fails(self):
        setup = """
write_stat 4242 S 100
on_kill() {
  if test "$1" = -TERM; then return 1; fi
  rm -f "$proc/$2/stat"
  return 0
}
"""
        normal = self.run_stop(setup=setup)
        self.assertEqual(normal["rc"], 0)
        self.assertEqual(normal["kills"], ["-TERM 4242", "-KILL 4242"])
        self.assertEqual(normal["sleeps"], 50)
        strict = self.run_stop(mode="strict", setup=setup)
        self.assertEqual(
            (strict["rc"], strict["kills"], strict["sleeps"]), (1, ["-TERM 4242"], 0)
        )

    def test_dies_during_term_wait(self):
        for final_state in ("gone", "zombie"):
            die = (
                'rm -f "$proc/4242/stat"'
                if final_state == "gone"
                else "write_stat 4242 Z 100"
            )
            setup = f"""
write_stat 4242 S 100
on_sleep() {{ if test "$1" -eq 3; then {die}; fi; }}
"""
            for mode in ("", "strict"):
                with self.subTest(mode=mode or "normal", final_state=final_state):
                    out = self.run_stop(mode=mode, setup=setup)
                    self.assertEqual(
                        (out["rc"], out["kills"], out["sleeps"]), (0, ["-TERM 4242"], 3)
                    )

    def test_dies_during_kill_wait_and_strict_waits_twenty_iterations(self):
        # Dies at iteration 15 after KILL (sleep 65): both modes succeed.
        setup_15 = """
write_stat 4242 S 100
on_sleep() { if test "$1" -eq 65; then rm -f "$proc/4242/stat"; fi; }
"""
        for mode in ("", "strict"):
            with self.subTest(when="iter15", mode=mode or "normal"):
                out = self.run_stop(mode=mode, setup=setup_15)
                self.assertEqual(out["rc"], 0)
                self.assertEqual(out["kills"], ["-TERM 4242", "-KILL 4242"])
                self.assertEqual(out["sleeps"], 65)
        # Dies at iteration 25 after KILL (sleep 75): normal succeeds, strict times out at 20.
        setup_25 = """
write_stat 4242 S 100
on_sleep() { if test "$1" -eq 75; then rm -f "$proc/4242/stat"; fi; }
"""
        normal = self.run_stop(setup=setup_25)
        self.assertEqual((normal["rc"], normal["sleeps"]), (0, 75))
        strict = self.run_stop(mode="strict", setup=setup_25)
        self.assertEqual((strict["rc"], strict["sleeps"], strict["logs"]), (1, 70, []))

    def test_never_dies(self):
        setup = "write_stat 4242 S 100"
        normal = self.run_stop(setup=setup)
        self.assertEqual(
            (normal["rc"], normal["kills"], normal["sleeps"]),
            (1, ["-TERM 4242", "-KILL 4242"], 80),
        )
        self.assertTrue(
            any("Process 4242 would not exit" in line for line in normal["logs"])
        )
        strict = self.run_stop(mode="strict", setup=setup)
        self.assertEqual(
            (strict["rc"], strict["kills"], strict["sleeps"], strict["logs"]),
            (1, ["-TERM 4242", "-KILL 4242"], 70, []),
        )

    def test_pid_reused(self):
        # Reused before the call: normal returns 0, strict returns 1; neither sends signals.
        before = "write_stat 4242 S 200"
        normal = self.run_stop(setup=before)
        self.assertEqual((normal["rc"], normal["kills"], normal["sleeps"]), (0, [], 0))
        strict = self.run_stop(mode="strict", setup=before)
        self.assertEqual((strict["rc"], strict["kills"], strict["sleeps"]), (1, [], 0))
        # Reused during TERM wait: both return 0 immediately without sending KILL.
        during_term = """
write_stat 4242 S 100
on_sleep() { if test "$1" -eq 2; then write_stat 4242 S 200; fi; }
"""
        for mode in ("", "strict"):
            with self.subTest(when="term", mode=mode or "normal"):
                out = self.run_stop(mode=mode, setup=during_term)
                self.assertEqual(
                    (out["rc"], out["kills"], out["sleeps"]), (0, ["-TERM 4242"], 2)
                )
        # Reused during KILL wait: both return 0 immediately.
        during_kill = """
write_stat 4242 S 100
on_sleep() { if test "$1" -eq 52; then write_stat 4242 S 200; fi; }
"""
        for mode in ("", "strict"):
            with self.subTest(when="kill", mode=mode or "normal"):
                out = self.run_stop(mode=mode, setup=during_kill)
                self.assertEqual(
                    (out["rc"], out["kills"], out["sleeps"]),
                    (0, ["-TERM 4242", "-KILL 4242"], 52),
                )


if __name__ == "__main__":
    unittest.main()
