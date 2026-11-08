#!/usr/bin/env python3
"""Host-only tests of rc2fx.sh's gui-launch and the GUI shim (no device, no GUI).

python3 xtask/mixer/tests/gui_launch.py

Extracts gui_say/gui_bundle/gui_starts/gui_launch and gui_status from the
script, replaces `exec` with a recorder and stamp() with a fixed prefix, and
drives every fallback reason, the crash guard, the quiet-log rotation and the
shim's selection with a fake root (RC2FX_DATA, RC2FX_GUI_HOME,
RC2FX_STOCK_GUI) in a temporary directory.
"""
import os
import re
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

SCRIPT = (Path(__file__).parents[1] / 'rc2fx.sh').read_text()
SHIM = (Path(__file__).parents[1] / 'rc2fx-gui-shim.sh').read_text()
GUI_SHA = 'aa92a92d384bdb24bc1cdb01615ac280d4d4867e661158df5c7f86ff98b605ca'

WAIT_START = SCRIPT.index('# display-start.sh starts the GUI as soon as')
START = SCRIPT.index('# Why the stock GUI is starting')
END = SCRIPT.index('\ncase "${1:-}" in', START)
# The GUI block, then the mixer-wait helpers it calls (they sit just above it
# in the script and use gui_say, so they come after it here).
BLOCK = SCRIPT[START:END] + SCRIPT[WAIT_START:START]
# Only the two command forms are recorded: `exec 9>lock` must still apply
# its redirection to this shell.
BLOCK = BLOCK.replace('exec env LD_AUDIT', 'record env LD_AUDIT')
BLOCK = BLOCK.replace('exec "$stock_gui"', 'record "$stock_gui"')
BLOCK = BLOCK.replace('  exec "$@"', '  record "$@"')

# The block as the tests run it: recorded exec, fixed stamp, quiet status.
HARNESS = '''set -euo pipefail
say() { printf 'REASON %s\\n' "$*"; }
stamp() { printf 'STAMP'; }
gui_home=$RC2FX_GUI_HOME
rc2fx_build=${RC2FX_TEST_BUILD:-development}
data=$RC2FX_DATA
home=$RC2FX_BUNDLE
stock_gui=$RC2FX_STOCK_GUI
gui_shim=$RC2FX_GUI_SHIM
gui_sha=$GUI_SHA
run=$RC2FX_RUN
boot=$RC2FX_BOOT
stock_mixer=$RC2FX_STOCK_MIXER
audio_shim=$RC2FX_AUDIO_SHIM
record() { printf 'EXEC %s\n' "$*"; exit 0; }
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
      if test "$n" -ge 2; then printf '%s\n' "$RC2FX_TEST_MIXER_PID" > "$RC2FX_RUN/native.pid"; fi
      printf '{"callbacks":%d}' $(( 4 + n )) > "$RC2FX_RUN/audit.json" ;;
  esac
}
flock() { "$RC2FX_FLOCK" "$@"; }
uptime_s() { cat "$RC2FX_ROOT/uptime"; }
'''


def bash(body, root, env=None):
    full = dict(os.environ, RC2FX_ROOT=str(root), **(env or {}))
    script = HARNESS.replace('$GUI_SHA', f'"{GUI_SHA}"') + BLOCK + '\n' + body
    return subprocess.run(['bash', '-c', script], capture_output=True, text=True, cwd=root, env=full,
                          timeout=60)


class GuiLaunch(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / 'uptime').write_text('1000\n')
        self.gui_home = self.root / 'run-gui'
        self.data = self.root / 'data'
        self.data.mkdir()
        self.bundle = self.root / 'bundle'
        (self.bundle / 'bin').mkdir(parents=True)
        self.stock = self.root / 'stock-gui'
        self.stock.write_text('#!/bin/sh\nexit 0\n')
        self.stock.chmod(0o755)
        self.make_bundle_module()
        self.run_dir = self.root / 'run'
        self.boot_dir = self.root / 'boot'
        self.run_dir.mkdir()
        self.boot_dir.mkdir()
        # A copy of a real program, so /proc/<pid>/exe names this path.
        self.fake_mixer = self.root / 'rc_audio_mixer'
        shutil.copy(shutil.which('sleep'), self.fake_mixer)
        self.env = {
            'RC2FX_GUI_HOME': str(self.gui_home),
            'RC2FX_DATA': str(self.data),
            'RC2FX_BUNDLE': str(self.bundle / 'bin'),
            'RC2FX_STOCK_GUI': str(self.stock),
            'RC2FX_FLOCK': '/usr/bin/flock',
            'RC2FX_GUI_SHIM': str(self.root / 'installed-gui-shim'),
            'RC2FX_RUN': str(self.run_dir),
            'RC2FX_BOOT': str(self.boot_dir),
            'RC2FX_STOCK_MIXER': str(self.root / 'rc_audio_mixer'),
            'RC2FX_GUI_WAIT_MAX': '1',  # two polls, with sleep() stubbed
            'RC2FX_AUDIO_SHIM': str(self.root / 'audio-shim'),
            'RC2FX_GUI_BOOT_WINDOW': '180',
        }
        # The qualified GUI: the fake stock executable has whatever hash, so
        # the test overrides the expected hash with its own.
        (self.root / 'installed-gui-shim').write_text('#!/bin/sh\n')
        self.stock_hash = subprocess.run(['sha256sum', str(self.stock)], capture_output=True,
                                         text=True).stdout.split()[0]
        self.gui_hash = self.stock_hash

    def make_bundle_module(self):
        module = self.bundle / 'bin' / 'librc2fx_gui.so'
        module.write_bytes(b'ELF-module')
        sums = subprocess.run(['sha256sum', 'librc2fx_gui.so'], cwd=self.bundle / 'bin',
                              capture_output=True, text=True).stdout
        (self.bundle / 'bin' / 'SHA256SUMS').write_text(sums)

    def run_gui(self, *args, env=None, body='gui_launch "$@"', hash_override=True):
        e = dict(self.env, **(env or {}))
        script = f'set -- {" ".join(args) if args else ""}\n'
        if hash_override:
            script = f'gui_sha={self.gui_hash}\n' + script
        return bash(script + body, self.root, e)

    def logged(self):
        log = self.gui_home / 'gui.log'
        return log.read_text() if log.exists() else ''

    def last_logged(self):
        return self.logged().splitlines()[-1] if self.logged() else ''

    def assert_stock(self, result, reason):
        self.assertIn(f'EXEC {self.stock}', result.stdout, result.stderr)
        self.assertNotIn('LD_AUDIT', result.stdout)
        self.assertIn(reason, self.logged())

    # Every reason, in the order gui_launch checks them.
    def test_each_fallback_reason(self):
        cases = {
            'first boot': ('/usr/bin/first-boot', 'first boot after a firmware update'),
            'production boot': ('/usr/bin/production-boot', 'production boot'),
        }
        for name, (marker, reason) in cases.items():
            with self.subTest(name):
                if not Path(marker).exists():
                    self.skipTest(f'{marker} is not on this host')
        self.addCleanup(lambda: (self.data / 'gui-partial-disabled').unlink(missing_ok=True))
        for name, setup, reason in (
            ('disabled in settings', lambda: self.data.joinpath('gui-partial-disabled').write_text(''),
             'partial conversion is disabled in settings'),
            ('off for this boot', lambda: self.gui_home.mkdir(exist_ok=True) or
             self.gui_home.joinpath('off').write_text(''), 'partial conversion is off for this boot'),
            ('crashed three times', lambda: self.gui_home.mkdir(exist_ok=True) or
             self.gui_home.joinpath('stock').write_text('the GUI module crashed it 3 times within 30 s\n'),
             'the GUI module crashed it'),
            ('gui not qualified', lambda: setattr(self, 'gui_hash', '0' * 64), 'this GUI is not the qualified one'),
            ('no bundle', lambda: (self.bundle / 'bin' / 'librc2fx_gui.so').unlink(), 'no bundle module'),
        ):
            with self.subTest(name):
                for marker in (self.data / 'gui-partial-disabled', self.gui_home / 'off',
                               self.gui_home / 'stock'):
                    marker.unlink(missing_ok=True)
                setup()
                r = self.run_gui('--x')
                self.gui_hash = self.stock_hash
                self.assert_stock(r, reason)

    def test_a_tampered_module_falls_back(self):
        (self.bundle / 'bin' / 'librc2fx_gui.so').write_bytes(b'changed')
        r = self.run_gui('--x')
        self.assert_stock(r, 'fails its checksum')

    def test_the_module_starts_with_the_production_environment(self):
        # A release build, the only kind that starts the module quietly.
        r = self.run_gui('--x', env={'RC2FX_TEST_BUILD': 'release'})
        self.assertEqual(r.returncode, 0, r.stderr)
        line = [l for l in r.stdout.splitlines() if l.startswith('EXEC ')][0]
        self.assertIn(f'LD_AUDIT={self.bundle}/bin/librc2fx_gui.so', line)
        self.assertIn('RC2FX_GUI_PACING=partial-cw', line)
        self.assertIn('RC2FX_GUI_PACING_QUIET=1', line)
        self.assertIn(f'RC2FX_GUI_PACING_LOG={self.gui_home}/pacing.log', line)
        self.assertTrue(line.endswith(f' {self.stock} --x'))
        self.assertIn('GUI module starting from', self.logged())
        self.assertIn('STAMP', self.logged())

    def test_the_build_kind_picks_the_default_diagnostics(self):
        # A development build logs verbosely...
        r = self.run_gui('--x')
        self.assertIn('LD_AUDIT=', r.stdout)
        self.assertNotIn('RC2FX_GUI_PACING_QUIET', r.stdout)
        self.assertIn('GUI module starting from', self.logged())
        self.assertIn('(verbose diagnostics)', self.last_logged())
        # ...and a release build (the packaged script says `release`) is quiet.
        (self.root / 'uptime').write_text('2000\n')  # a later start: not a crash loop
        r = self.run_gui('--x', env={'RC2FX_TEST_BUILD': 'release'})
        self.assertIn('RC2FX_GUI_PACING_QUIET=1', r.stdout)
        self.assertNotIn('(verbose diagnostics)', self.last_logged())

    def start_mixer(self):
        """A live process whose /proc/<pid>/exe is the fake stock mixer."""
        import subprocess
        if not Path('/proc/self/exe').exists():
            self.skipTest('/proc/<pid>/exe is not on this host')
        process = subprocess.Popen([str(self.fake_mixer), '30'])
        self.addCleanup(process.kill)
        (self.run_dir / 'native.pid').write_text(f'{process.pid}\n')
        return process

    def control_socket(self):
        """A real unix socket where the mixer's control socket appears."""
        import socket
        server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        server.bind(str(self.run_dir / 'control.sock'))
        server.listen(1)
        self.addCleanup(server.close)

    def audit(self, callbacks):
        (self.run_dir / 'audit.json').write_text('{"callbacks":%d}' % callbacks)

    def boot_fixture(self):
        """Expected this boot: an audio shim, no marker, a young uptime."""
        (self.root / 'audio-shim').write_text('#!/bin/sh\n')
        (self.root / 'audio-shim').chmod(0o755)
        (self.root / 'uptime').write_text('5\n')
        self.control_socket()

    def test_the_boot_gui_waits_for_the_mixer_to_appear_then_for_readiness(self):
        self.boot_fixture()
        mixer = self.start_mixer()
        self.run_dir.joinpath('native.pid').unlink()  # not written yet
        r = self.run_gui('--x', env={'RC2FX_TEST_SLEEP_AUDIT': 'appear',
                                     'RC2FX_TEST_MIXER_PID': str(mixer.pid),
                                     'RC2FX_GUI_WAIT_MAX': '5'})
        self.assertIn('EXEC ', r.stdout)
        # native.pid on the first poll, readiness on the second: one second.
        self.assertIn('Waited 1 s for the mixer before starting the GUI', self.logged())

    def test_no_audit_json_yet_does_not_abort_the_wait(self):
        # The socket appears before audit.json, as at boot: the wait must
        # survive that under set -e and still start the module.
        self.boot_fixture()
        self.start_mixer()
        r = self.run_gui('--x', env={'RC2FX_TEST_SLEEP_AUDIT': 'late-audit', 'RC2FX_GUI_WAIT_MAX': '4'})
        self.assertIn('RC2FX_GUI_PACING', r.stdout, 'the module started, not stock')
        self.assertIn('LD_AUDIT=', r.stdout)
        self.assertNotIn('reported ready', self.logged())
        self.assertIn('Waited', self.logged())

    def test_an_unexpected_abort_logs_before_starting_stock(self):
        # A directory where the stock reason is read from: `cat` fails under
        # set -e, so the EXIT trap must log and start the stock GUI.
        self.gui_home.mkdir()
        (self.gui_home / 'stock').mkdir()
        r = self.run_gui('--x')
        self.assertIn(f'EXEC {self.stock}', r.stdout)
        self.assertNotIn('LD_AUDIT', r.stdout)
        self.assertIn('GUI launch failed unexpectedly; stock GUI started', self.logged())

    def test_a_mixer_that_never_appears_times_out_and_the_gui_starts(self):
        self.boot_fixture()
        r = self.run_gui('--x', env={'RC2FX_TEST_SLEEP_AUDIT': 'constant', 'RC2FX_GUI_WAIT_MAX': '1'})
        self.assertIn('EXEC ', r.stdout)
        self.assertIn('GUI started before the mixer reported ready (1 s)', self.logged())

    def test_a_later_gui_start_with_no_mixer_does_not_wait(self):
        # Late uptime, no audio shim: RC2FX is not expected, so exec at once.
        (self.root / 'uptime').write_text('1000\n')
        r = self.run_gui('--x', env={'RC2FX_TEST_SLEEP_AUDIT': 'constant', 'RC2FX_GUI_WAIT_MAX': '5'})
        self.assertIn('EXEC ', r.stdout)
        self.assertNotIn('Waited', self.logged())
        self.assertNotIn('reported ready', self.logged())
        # An installed shim but a late uptime: still not a boot start.
        (self.root / 'audio-shim').write_text('#!/bin/sh\n')
        (self.root / 'audio-shim').chmod(0o755)
        r = self.run_gui('--x', env={'RC2FX_TEST_SLEEP_AUDIT': 'constant', 'RC2FX_GUI_WAIT_MAX': '5'})
        self.assertIn('EXEC ', r.stdout)
        self.assertNotIn('reported ready', self.logged())

    def test_no_wait_without_an_rc2fx_mixer_in_charge(self):
        # No native.pid: nothing to wait for.
        r = self.run_gui('--x')
        self.assertIn('EXEC ', r.stdout)
        self.assertNotIn('Waited', self.logged())
        self.assertNotIn('reported ready', self.logged())
        # A pid that is not the mixer: the same.
        self.run_dir.joinpath('native.pid').write_text(f'{os.getpid()}\n')
        r = self.run_gui('--x')
        self.assertIn('EXEC ', r.stdout)
        self.assertNotIn('Waited', self.logged())

    def test_waits_then_execs_once_the_callbacks_advance(self):
        self.start_mixer()
        self.control_socket()
        r = self.run_gui('--x', env={'RC2FX_TEST_SLEEP_AUDIT': 'advance', 'RC2FX_GUI_WAIT_MAX': '5'})
        self.assertIn('EXEC ', r.stdout)
        self.assertIn('Waited 1 s for the mixer before starting the GUI', self.logged())

    def test_a_mixer_that_never_reports_still_starts_the_gui(self):
        self.start_mixer()
        self.control_socket()
        r = self.run_gui('--x', env={'RC2FX_TEST_SLEEP_AUDIT': 'constant', 'RC2FX_GUI_WAIT_MAX': '1'})
        self.assertIn('EXEC ', r.stdout)
        self.assertIn('GUI started before the mixer reported ready (1 s)', self.logged())

    def test_no_wait_before_the_socket_exists(self):
        # Callbacks advancing but no control socket: the wait still runs to its
        # timeout, never treating callbacks alone as readiness.
        self.start_mixer()
        r = self.run_gui('--x', env={'RC2FX_TEST_SLEEP_AUDIT': 'advance', 'RC2FX_GUI_WAIT_MAX': '1'})
        self.assertIn('EXEC ', r.stdout)
        self.assertIn('GUI started before the mixer reported ready (1 s)', self.logged())

    def test_the_stock_marker_skips_the_wait(self):
        self.start_mixer()
        self.control_socket()
        (self.boot_dir / 'stock').write_text('"a reason"\n')
        r = self.run_gui('--x', env={'RC2FX_TEST_SLEEP_AUDIT': 'advance', 'RC2FX_GUI_WAIT_MAX': '5'})
        self.assertIn('EXEC ', r.stdout)
        self.assertNotIn('Waited', self.logged())

    def test_status_names_the_diagnostics_of_the_build_kind(self):
        self.gui_home.mkdir()
        for build, expected in (('development', 'verbose (development build)'),
                                ('release', 'quiet (release build)')):
            with self.subTest(build=build):
                r = self.run_gui(body='gui_status', env={'RC2FX_TEST_BUILD': build})
                self.assertIn(f'GUI diagnostics: {expected}', r.stdout, r.stderr)
                lines = [x for x in r.stdout.splitlines() if x.startswith('GUI diagnostics:')]
                self.assertEqual(len(lines), 1, lines)
    def test_status_has_no_screen_conversion_setting(self):
        self.gui_home.mkdir()
        (self.gui_home / 'screen-conversion').write_text('hardware\n')
        r = self.run_gui(body='gui_status')
        self.assertNotIn('Screen conversion:', r.stdout)

    def test_the_previous_pacing_log_is_rotated(self):
        self.gui_home.mkdir()
        (self.gui_home / 'pacing.log').write_text('old\n')
        self.run_gui()
        self.assertEqual((self.gui_home / 'pacing.prev.log').read_text(), 'old\n')
        self.assertFalse((self.gui_home / 'pacing.log').exists())

    def test_three_starts_within_thirty_seconds_drop_to_stock_for_the_boot(self):
        r = self.run_gui()
        self.assertIn('LD_AUDIT=', r.stdout)
        (self.root / 'uptime').write_text('1020\n')
        r = self.run_gui()
        self.assertIn('LD_AUDIT=', r.stdout)
        (self.root / 'uptime').write_text('1029\n')
        r = self.run_gui()
        self.assert_stock(r, 'crashed it 3 times within 30 s')
        self.assertEqual((self.gui_home / 'stock').read_text().strip(),
                         'the GUI module crashed it 3 times within 30 s')
        # Stock for the rest of the boot, even long after the window.
        (self.root / 'uptime').write_text('2000\n')
        self.assert_stock(self.run_gui(), 'crashed it 3 times within 30 s')

    def test_starts_spread_out_are_not_a_crash_loop(self):
        for uptime in ('1000', '1031', '1062', '1093'):
            (self.root / 'uptime').write_text(uptime + '\n')
            r = self.run_gui()
            self.assertIn('LD_AUDIT=', r.stdout, f'uptime {uptime}')
        self.assertFalse((self.gui_home / 'stock').exists())

    def test_status_reports_module_and_counters(self):
        self.gui_home.mkdir()
        (self.gui_home / 'pacing.log').write_text(
            'start pid=1 t=2 merge_px=80000\ncounters pid=1 t=3 conversions=9\n')
        (self.gui_home / 'gui.log').write_text('GUI module starting from x\n')
        r = self.run_gui(body='gui_status')
        self.assertIn('GUI: partial conversion, module', r.stdout)
        self.assertIn('GUI counters: counters pid=1 t=3 conversions=9', r.stdout)

    def test_status_reasons_match_the_fallback_ones(self):
        self.data.joinpath('gui-partial-disabled').write_text('')
        r = self.run_gui(body='gui_status')
        self.assertIn('GUI: stock (partial conversion is disabled in settings)', r.stdout)
        self.data.joinpath('gui-partial-disabled').unlink()
        self.gui_home.mkdir()
        (self.gui_home / 'stock').write_text('the GUI module crashed it 3 times within 30 s\n')
        r = self.run_gui(body='gui_status')
        self.assertIn('GUI: stock until the next boot (the GUI module crashed it 3 times within 30 s)', r.stdout)


class Shim(unittest.TestCase):
    """The installed shim hands over to rc2fx.sh gui-launch when it can."""

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.shim = self.root / 'rc_gui_juce'
        text = SHIM.replace('/run/rc2fx-dev/rc2fx.sh', str(self.root / 'dev/rc2fx.sh'))
        text = text.replace('/opt/rc2fx/rc2fx.sh', str(self.root / 'opt/rc2fx.sh'))
        text = text.replace('/usr/bin/rc_gui_juce', str(self.root / 'stock-gui'))
        self.shim.write_text(text)
        self.shim.chmod(0o755)
        (self.root / 'stock-gui').write_text('#!/bin/sh\nprintf "STOCK %s\\n" "$*"\n')
        (self.root / 'stock-gui').chmod(0o755)

    def test_hands_over_to_the_rc2fx_script_that_has_gui_launch(self):
        (self.root / 'opt').mkdir(exist_ok=True)
        script = self.root / 'opt/rc2fx.sh'
        script.write_text('#!/bin/bash\ncase "${1:-}" in\n  gui-launch) shift; printf "LAUNCH %s\\n" "$*"; exit 7;;\nesac\nexit 9\n')
        r = subprocess.run([str(self.shim), '--a', 'b'], capture_output=True, text=True, timeout=30)
        self.assertEqual((r.returncode, r.stdout), (7, 'LAUNCH --a b\n'))

    def test_a_build_in_ram_wins(self):
        dev = self.root / 'dev/rc2fx.sh'
        dev.parent.mkdir()
        dev.write_text('#!/bin/bash\ncase "${1:-}" in\n  gui-launch) shift; printf "RAM %s\\n" "$*"; exit 5;;\nesac\n')
        (self.root / 'opt').mkdir(exist_ok=True)
        (self.root / 'opt/rc2fx.sh').write_text('#!/bin/bash\ncase "${1:-}" in\n  gui-launch) printf "INSTALLED\\n"; exit 6;;\nesac\n')
        r = subprocess.run([str(self.shim), 'x'], capture_output=True, text=True, timeout=30)
        self.assertEqual((r.returncode, r.stdout), (5, 'RAM x\n'))

    def test_without_a_script_that_has_gui_launch_the_stock_gui_runs(self):
        (self.root / 'opt').mkdir(exist_ok=True)
        (self.root / 'opt/rc2fx.sh').write_text('#!/bin/bash\nexit 9\n')  # no gui-launch case
        r = subprocess.run([str(self.shim), '--q'], capture_output=True, text=True, timeout=30)
        self.assertEqual(r.stdout, 'STOCK --q\n')


if __name__ == '__main__':
    unittest.main()
