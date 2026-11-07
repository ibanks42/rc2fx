#!/usr/bin/env python3
"""Host-only tests of the steps rc2fx.sh keeps for rc2fx-supervisor.

python3 xtask/mixer/tests/supervisor.py

The supervisor itself is tested in crates/rc2fx-supervisor. These run the real
script's entry points with RC2FX_LOGS in a temporary directory, and its
functions with stubs, against no mixer and no USB device.
"""
import os
import subprocess
import tempfile
import time
import unittest
from pathlib import Path

PATH = Path(__file__).parents[1] / 'rc2fx.sh'
SCRIPT = PATH.read_text()


def function(name):
    start = SCRIPT.index(f'\n{name}() {{\n') + 1
    return SCRIPT[start:SCRIPT.index('\n}\n', start) + 3]


class Tests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        self.env = dict(os.environ, RC2FX_LOGS=str(self.root), RC2FX_DATA=str(self.root))

    def script(self, *args):
        return subprocess.run(['bash', str(PATH), *args], capture_output=True, text=True,
                              env=self.env, timeout=30)

    def arguments(self, setup):
        body = (f'home={self.root}\ndata=/d\nrun=/r\nshow=\nprofile_id=\n{setup}\n'
                + function('controller_arguments')
                + 'controller_arguments\nprintf "%s\\n" "${controller[@]}"\n')
        r = subprocess.run(['bash', '-euc', body], capture_output=True, text=True, timeout=30)
        self.assertEqual(r.returncode, 0, r.stderr)
        return r.stdout.split('\n')[:-1]

    def test_exit_writes_the_last_exit_log_and_succeeds_with_nothing_to_read(self):
        r = self.script('supervise-exit')
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn('Supervisor stopping', r.stdout)
        self.assertIn('--- kernel', (self.root / 'last-exit.log').read_text())

    def test_exit_stops_a_controller_that_is_still_running(self):
        child = subprocess.Popen(['sleep', '60'])
        self.addCleanup(child.kill)
        # The child stays a zombie until reaped, which the script counts as gone.
        began = time.monotonic()
        r = self.script('supervise-exit', str(child.pid))
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(child.wait(timeout=5), -15)
        self.assertLess(time.monotonic() - began, 5)

    def test_restart_keeps_the_gadget_only_when_asked(self):
        body = ('mixer_start=\nretire_usb_node() { echo "retire $*"; }\nstop_owned() { echo "stop $*"; }\n'
                + function('restart_mixer') + function('supervise_restart'))
        for keep, expected in [('0', 'retire 10 20\nstop 20\nstop 10 99\n'), ('1', 'stop 20\nstop 10 99\n')]:
            r = subprocess.run(['bash', '-euc', body + f'supervise_restart {keep} 10 99 20\n'],
                               capture_output=True, text=True, timeout=30)
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertEqual(r.stdout, expected)

    def test_a_refused_usb_retirement_fails_the_restart_and_stops_nothing(self):
        body = ('retire_usb_node() { return 1; }\nstop_owned() { echo stopped; }\n'
                + function('restart_mixer') + function('supervise_restart'))
        r = subprocess.run(['bash', '-uc', 'mixer_start=\n' + body + 'supervise_restart 0 10 99 20\n'],
                           capture_output=True, text=True, timeout=30)
        self.assertNotEqual(r.returncode, 0)
        self.assertEqual(r.stdout, '')

    def test_the_controller_always_gets_its_sockets_and_developer_ssh(self):
        self.assertEqual(self.arguments(''), [
            '--data', '/d', '--native-socket', '/r/control.sock', '--usb-socket', '/r/usb.sock',
            '--ui-socket', '/r/ui.sock', '--developer-ssh', f'{self.root}/rc2fx.sh'])

    def test_optional_controller_arguments_follow_what_exists(self):
        (self.root / 'mixer-release.json').write_text('{}')
        show = self.root / 'show'
        show.write_text('{}')
        args = self.arguments(f'show={show}\nprofile_id=p1')
        self.assertEqual(args[10:], [
            '--stock-config', str(show), '--release', f'{self.root}/mixer-release.json',
            '--firmware-catalog', f'{self.root}/firmware-profiles.json', '--firmware-profile', 'p1'])

    def test_the_supervisor_gets_a_script_that_outlives_its_bundle(self):
        # An install removes a RAM bundle, then asks the running supervisor to
        # stop. Its exit steps must work with nothing left of that bundle.
        launch = function('launch')
        self.assertIn('cp "$home/rc2fx.sh" "$run/rc2fx.sh"', launch)
        self.assertIn('--script "$run/rc2fx.sh"', launch)
        self.assertLess(launch.index('cp "$home/rc2fx.sh"'), launch.index('setsid env'))
        run = self.root / 'run'
        run.mkdir()
        (run / 'rc2fx.sh').write_text(SCRIPT)
        r = subprocess.run(['bash', str(run / 'rc2fx.sh'), 'supervise-exit'], capture_output=True,
                           text=True, env=self.env, cwd=run, timeout=30)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn('--- kernel', (self.root / 'last-exit.log').read_text())

    def test_launch_records_the_supervisors_own_pid(self):
        launch = function('launch')
        start = launch.index('setsid env')
        self.assertIn('"$home/rc2fx-supervisor" --mixer', launch[start:])
        self.assertIn('local watcher=$!\n  printf \'%s\\n\' "$watcher" > "$run/supervisor.pid"', launch[start:])
        self.assertLess(launch.index('controller_arguments'), launch.index('"$stock_mixer" "$@" 2>>'))

    def stop_owned(self, target, started):
        body = ''.join(line + '\n' for line in SCRIPT.split('\n')
                       if line.startswith(('start_time() ', 'alive() ', 'alive_as() ', 'stamp() ', 'say() ')))
        body += function('stop_owned') + f'stop_owned {target} {started}\n'
        return subprocess.run(['bash', '-euc', body], capture_output=True, text=True, timeout=30)

    def test_a_pid_that_has_moved_on_is_not_stopped(self):
        child = subprocess.Popen(['sleep', '60'])
        self.addCleanup(child.kill)
        self.assertEqual(self.stop_owned(child.pid, 1).returncode, 0)
        self.assertIsNone(child.poll())

    def test_a_pid_with_the_right_start_time_is_stopped(self):
        child = subprocess.Popen(['sleep', '60'])
        self.addCleanup(child.kill)
        stat = Path(f'/proc/{child.pid}/stat').read_text()
        started = stat.rsplit(') ', 1)[1].split()[19]
        self.assertEqual(self.stop_owned(child.pid, started).returncode, 0)
        self.assertEqual(child.wait(timeout=5), -15)

    def injected_launch(self, supervisor, mixer='exec sleep 30'):
        """Runs the real launch with its system steps stubbed, as far as the end
        of the mixer. Returns the exit code and the log."""
        home, run, boot = self.root / 'home', self.root / 'run', self.root / 'boot'
        home.mkdir()
        for name, text in {'rc2fx-supervisor': supervisor, 'rc2fxd': 'exit 0',
                           'rc2fx-firmware': 'echo profile show', 'stock': mixer}.items():
            (home / name).write_text(f'#!/bin/bash\n{text}\n')
            (home / name).chmod(0o755)
        (home / 'rc2fx.sh').write_text(SCRIPT)
        subprocess.run('sha256sum rc2fxd > SHA256SUMS', shell=True, cwd=home, check=True)
        helpers = ''.join(line + '\n' for line in SCRIPT.split('\n') if line.startswith(
            ('stamp() ', 'say() ', 'start_time() ', 'alive() ', 'alive_as() ', 'uptime_s() ', 'supervisor() ')))
        stubs = ''.join(f'{name}() {{ :; }}\n' for name in (
            'prepare_usb', 'relax_rt_throttle', 'restore_rt_throttle', 'jack_recorder', 'retire_usb_node'))
        body = (f'home={home}\ndata={self.root}/data\nlogs={self.root}\nrun={run}\nboot={boot}\n'
                f'stock_mixer={home}/stock\nshow=\nprofile_id=\nvendor_log=/nonexistent\n'
                'keep_usb=0\nmixer_start=\nredact() { cat; }\n'
                'pgrep() { return 1; }\n'
                + helpers + stubs
                + ''.join(function(name) for name in (
                    'stop_owned', 'restart_mixer', 'supervise_exit', 'controller_arguments',
                    'supervisor_lost', 'launch'))
                + 'launch\n')
        r = subprocess.run(['bash', '-euc', body], capture_output=True, text=True, timeout=30)
        return r.returncode, (self.root / 'rc2fx.log').read_text()

    def test_a_supervisor_that_crashes_at_once_leaves_stock_behind(self):
        code, log = self.injected_launch('test "${1:-}" = --version || exit 2')
        self.assertIn('Supervisor died with status 2', (self.root / 'boot/stock').read_text())
        self.assertIn('Supervisor died with status 2', log)
        self.assertIn('--- kernel', (self.root / 'last-exit.log').read_text())
        self.assertEqual(code, 143)

    def test_a_supervisor_killed_later_leaves_stock_behind(self):
        code, log = self.injected_launch('test "${1:-}" = --version && exit 0\nsleep 1\nkill -KILL $$')
        self.assertIn('Supervisor died with status 137', (self.root / 'boot/stock').read_text())
        self.assertEqual(code, 143)

    def test_a_supervisor_that_hands_the_mixer_back_is_not_a_crash(self):
        # It exits 0 when it leaves the mixer running on purpose.
        code, log = self.injected_launch('exit 0', 'sleep 1\nexit 7')
        self.assertFalse((self.root / 'boot/stock').exists())
        self.assertNotIn('Supervisor died', log)
        self.assertIn('Mixer exited with status 7', log)
        self.assertEqual(code, 7)

    def test_a_signal_to_the_launch_shell_still_ends_the_mixer(self):
        # A signal ends the wait early, with no process to name.
        code, log = self.injected_launch(
            'test "${1:-}" = --version && exit 0\nsleep 1\nkill -TERM $PPID\nexec sleep 3')
        self.assertIn('Mixer exited with status 143', log)
        self.assertFalse((self.root / 'boot/stock').exists())
        self.assertEqual(code, 143)

    def test_the_mixers_own_status_survives_the_watch_on_the_supervisor(self):
        code, log = self.injected_launch('test "${1:-}" = --version && exit 0\nsleep 5', 'sleep 1\nexit 7')
        self.assertFalse((self.root / 'boot/stock').exists())
        self.assertIn('Mixer exited with status 7', log)
        self.assertEqual(code, 7)

    def launch_with(self, supervisor):
        """Runs the real launch from a copied bundle with its paths in the fixture."""
        home = self.root / 'home'
        home.mkdir()
        copy = SCRIPT.replace('run=/run/rc2fx\n', f'run={self.root}/run\n')
        copy = copy.replace('boot=/run/rc2fx-boot\n', f'boot={self.root}/boot\n')
        (home / 'rc2fx.sh').write_text(copy)
        stubs = {'rc2fx-firmware': 'echo profile show', 'rc2fx-supervisor': supervisor,
                 'stock': f'printf "%s\\n" "${{LD_AUDIT:-none}}" "$*" > {self.root}/stock-ran'}
        for name, text in stubs.items():
            (home / name).write_text(f'#!/bin/bash\n{text}\n')
            (home / name).chmod(0o755)
        subprocess.run('sha256sum rc2fx-firmware > SHA256SUMS', shell=True, cwd=home, check=True)
        env = dict(self.env, RC2FX_STOCK_MIXER=str(home / 'stock'))
        r = subprocess.run(['bash', str(home / 'rc2fx.sh'), 'launch', '--arg'],
                           capture_output=True, text=True, env=env, timeout=30)
        self.assertEqual(r.returncode, 0, r.stderr)
        return (self.root / 'stock-ran').read_text().splitlines()

    def test_a_supervisor_that_cannot_run_starts_the_stock_mixer_without_rc2fx(self):
        self.assertEqual(self.launch_with('exit 1'), ['none', '--arg'])
        self.assertIn('Stock mixer started: the supervisor cannot run',
                      (self.root / 'rc2fx.log').read_text())


if __name__ == '__main__':
    unittest.main()
