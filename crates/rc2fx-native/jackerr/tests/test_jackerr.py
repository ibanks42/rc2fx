"""Host-only tests: python3 -m unittest discover -s crates/rc2fx-native/jackerr/tests -p 'test_jackerr.py'.

Builds the jackd error recorder (with its test hooks), its on-device snapshot
reader and a harness with the host C compiler. The harness reaches
jack_error() through a stand-in vendor library whose own jack_error() is a
no-op, and gets the recorder by LD_PRELOAD, the way jackd would. Every ring is
read through the snapshot reader, as on the device.
"""
import os
import shutil
import stat
import subprocess
import tempfile
import unittest
from pathlib import Path

import jackerr_ring as m

HERE = Path(__file__).resolve().parent
SOURCES = HERE.parent
HARNESS = HERE / 'jackerr_harness.c'
CC = os.environ.get('CC') or shutil.which('cc') or 'cc'
FLAGS = ['-O2', '-Wall', '-Wextra', '-Werror']
SLOTS = 4096
VENDOR = '''
#include <stdarg.h>
/* The vendor's: a no-op, called through this library's PLT. */
__attribute__((visibility("default"))) void jack_error(const char *format, ...) { (void)format; }
/* As JackEngine::CheckXRun: the library calls its own exported jack_error. */
__attribute__((visibility("default"))) void vendor_check_xrun(const char *client) {
    jack_error("JackEngine::XRun: client %s finished after current callback", client);
}
'''


def setUpModule():
    global BUILD
    BUILD = tempfile.TemporaryDirectory()
    out = Path(BUILD.name)
    (out / 'vendor.c').write_text(VENDOR)
    subprocess.run([CC, *FLAGS, '-shared', '-fPIC', str(out / 'vendor.c'), '-o', str(out / 'libvendor.so')], check=True)
    for name, extra in (('librc2fx_jackerr.so', ['-DRC2FX_JACKERR_TESTING']), ('librc2fx_jackerr-production.so', [])):
        subprocess.run([CC, *FLAGS, *extra, '-shared', '-fPIC', '-fvisibility=hidden', str(SOURCES / 'rc2fx_jackerr.c'),
                        '-o', str(out / name)], check=True)
    for name, extra in (('snapshot', []), ('snapshot-testing', ['-DRC2FX_JACKERR_TESTING'])):
        subprocess.run([CC, *FLAGS, *extra, str(SOURCES / 'rc2fx_jackerr_snapshot.c'), '-o', str(out / name)], check=True)
    subprocess.run([CC, *FLAGS, str(HARNESS), '-L', str(out), '-lvendor', '-lpthread', '-ldl',
                    f'-Wl,-rpath,{out}', '-o', str(out / 'harness')], check=True)


def tearDownModule():
    BUILD.cleanup()


class Run:
    """One harness run with its own ring directory."""
    count = 0

    def __init__(self, *args, expected_exe=True, preload='librc2fx_jackerr.so', background=False):
        out = Path(BUILD.name)
        Run.count += 1
        self.directory = out / f'rings-{Run.count}'
        env = {k: v for k, v in os.environ.items() if not k.startswith('RC2FX_JACKERR')}
        env['RC2FX_JACKERR_DIR'] = str(self.directory)
        if expected_exe:
            env['RC2FX_JACKERR_EXE'] = str((out / 'harness').resolve())
        if preload:
            env['LD_PRELOAD'] = str(out / preload)
        command = [str(out / 'harness'), *args]
        if background:
            self.process = subprocess.Popen(command, env=env)
        else:
            self.code = subprocess.run(command, env=env, timeout=60).returncode

    def rings(self):
        return sorted(self.directory.glob('ring-*.ring')) if self.directory.exists() else []

    def snapshot(self):
        out = self.directory.parent / f'snapshot-{Run.count}-{len(list(self.directory.parent.iterdir()))}.bin'
        subprocess.run([str(Path(BUILD.name) / 'snapshot'), str(self.directory), str(out)], check=True,
                       capture_output=True, timeout=30)
        return m.read(out.read_bytes())


class RecorderTests(unittest.TestCase):
    def test_every_known_format_is_recorded_with_its_arguments(self):
        run = Run()
        self.assertEqual(run.code, 0)
        result = run.snapshot()
        self.assertEqual((result['calls'], result['unknownFormats'], result['dropped']), (14, 2, 0))
        records = result['records']
        self.assertEqual([r['kind'] for r in records], [
            'clientNotFinished', 'clientFinishedLate', 'timedDriverXrun', 'alsaXrun', 'alsaWaitStatus',
            'internalXrun', 'pollFailed', 'pollTimeoutExiting', 'pollTimeoutRetrying', 'pollRecoveryFailed',
            'pollInterrupt', 'clientNotFinished', 'unknownFormat'])
        self.assertEqual((records[0]['client'], records[0]['state']), ('usb1out', 'Running'))
        self.assertEqual(records[1]['client'], 'usb1-chat-out')
        self.assertEqual((records[2]['usecs'], records[3]['usecs'], records[4]['status'], records[5]['frameOffset']),
                         (4321, 1250, -32, 77))
        self.assertEqual(records[6]['reason'], 'Interrupted system call')
        self.assertEqual((records[7]['usecs'], records[7]['retries'], records[8]['usecs'], records[8]['retries']),
                         (5_000_000, 5, 2_000_000, 2))
        self.assertEqual(records[9]['status'], -5)
        times = [r['tNs'] for r in records]
        self.assertTrue(all(times) and times == sorted(times))

    def test_long_and_null_strings_and_unknown_formats_are_bounded(self):
        records = Run().snapshot()['records']
        edge = records[11]
        self.assertEqual(edge['client'], 'a-client-name-that-is-much-longer-than-f')  # 40 bytes
        self.assertIsNone(edge['state'])
        self.assertEqual(set(edge['flags']), {'string1Truncated', 'string2Null'})
        unknown = records[12]
        self.assertEqual(unknown['format'] + unknown['formatContinued'],
                         'Some other message %d with %s that must not be formatted'[:56])

    def test_the_vendor_librarys_own_call_reaches_the_recorder(self):
        records = Run('vendor').snapshot()['records']
        self.assertEqual([(r['kind'], r['client']) for r in records], [('clientFinishedLate', 'sip-audio-out')])

    def test_a_stalled_writer_makes_the_next_lap_drop_not_overwrite(self):
        run = Run('stall')
        self.assertEqual(run.code, 0, 'the stalled slot was written over')
        result = run.snapshot()
        self.assertEqual(result['dropped'], 1)
        self.assertEqual(result['claimed'], SLOTS + 10)
        self.assertEqual(result['unpublished'], 1, 'index 4101: claimed, dropped, never published')
        self.assertEqual(result['stable'], SLOTS - 1)
        self.assertNotIn(SLOTS + 5, [r['index'] for r in result['records']])

    def test_a_reservation_older_than_the_slot_drops(self):
        run = Run('newer')
        self.assertEqual(run.code, 0, 'a newer record was written over')
        result = run.snapshot()
        self.assertEqual(result['dropped'], 1)
        self.assertEqual(result['overwritten'], 1, 'index 4101: the slot holds a later lap')

    def test_concurrent_writers_lose_nothing_below_capacity(self):
        result = Run('threads', '2').snapshot()
        self.assertEqual((result['claimed'], result['stable'], result['dropped']), (4000, 4000, 0))
        self.assertTrue(all(r['client'] == 'usb1out' and r['state'] == 'Running' for r in result['records']))

    def test_overflow_is_reported_not_hidden(self):
        result = Run('threads', '4').snapshot()
        self.assertEqual(result['claimed'], 8000)
        self.assertEqual(result['overwrittenBeforeWindow'], 8000 - SLOTS)
        self.assertEqual(result['stable'] + result['unpublished'] + result['overwritten'] + result['torn'], SLOTS)

    def test_snapshots_of_a_live_ring_are_never_torn(self):
        import time
        run = Run('live', '1500', background=True)
        try:
            deadline = time.monotonic() + 10
            while not run.rings() and time.monotonic() < deadline:
                time.sleep(0.01)
            seen, snapshots = 0, 0
            while run.process.poll() is None and snapshots < 40:
                result = run.snapshot()
                snapshots += 1
                for record in result['records']:
                    self.assertIn((record['client'], record['state']), {('usb1out', 'Running'), ('usb1-chat-out', 'Running')})
                indices = [r['index'] for r in result['records']]
                self.assertEqual(indices, sorted(indices))
                seen += result['stable']
        finally:
            self.assertEqual(run.process.wait(timeout=30), 0)
        self.assertGreater(snapshots, 1, 'snapshots taken while writing')
        self.assertGreater(seen, 0)

    def test_errno_is_left_as_it_was(self):
        self.assertEqual(Run('errno').code, 0)

    def test_a_failed_clock_read_is_flagged_and_kept(self):
        (record,) = Run('clockfail').snapshot()['records']
        self.assertIsNone(record['tNs'])
        self.assertIn('clockFailed', record['flags'])
        self.assertEqual(record['client'], 'usb1out')

    def test_the_ring_is_private_and_new_for_each_process(self):
        first, second = Run(), Run()
        for run in (first, second):
            (ring,) = run.rings()
            self.assertEqual(stat.S_IMODE(run.directory.stat().st_mode), 0o700)
            self.assertEqual(stat.S_IMODE(ring.stat().st_mode), 0o600)
        self.assertNotEqual(first.rings()[0].name, second.rings()[0].name)
        # A second generation in the same directory is a new file, not a truncation.
        shared = Path(BUILD.name) / 'shared-rings'
        for _ in range(2):
            env = {k: v for k, v in os.environ.items() if not k.startswith('RC2FX_JACKERR')}
            env.update(RC2FX_JACKERR_DIR=str(shared), RC2FX_JACKERR_EXE=str((Path(BUILD.name) / 'harness').resolve()),
                       LD_PRELOAD=str(Path(BUILD.name) / 'librc2fx_jackerr.so'))
            subprocess.run([str(Path(BUILD.name) / 'harness')], env=env, check=True)
        self.assertEqual(len(list(shared.glob('ring-*.ring'))), 2)

    def test_a_shared_or_foreign_directory_is_refused(self):
        loose = Path(BUILD.name) / 'loose'
        loose.mkdir(mode=0o755)
        loose.chmod(0o755)
        env = {k: v for k, v in os.environ.items() if not k.startswith('RC2FX_JACKERR')}
        env.update(RC2FX_JACKERR_DIR=str(loose), RC2FX_JACKERR_EXE=str((Path(BUILD.name) / 'harness').resolve()),
                   LD_PRELOAD=str(Path(BUILD.name) / 'librc2fx_jackerr.so'))
        subprocess.run([str(Path(BUILD.name) / 'harness')], env=env, check=True)
        self.assertEqual(list(loose.iterdir()), [], 'no ring in a directory others can reach')

    def test_another_executable_or_no_preload_records_nothing(self):
        self.assertEqual(Run(expected_exe=False).rings(), [])
        self.assertEqual(Run(preload=None).rings(), [])

    def test_the_production_build_has_no_test_hooks_and_still_records(self):
        run = Run('stall', preload='librc2fx_jackerr-production.so')
        self.assertEqual(run.code, 3, 'no test hooks in the production build')
        self.assertEqual(run.snapshot()['claimed'], 10)
        self.assertEqual(Run('errno', preload='librc2fx_jackerr-production.so').code, 0)

    def test_a_snapshot_whose_clock_failed_has_no_time_not_a_garbage_one(self):
        run = Run()
        good = run.snapshot()
        self.assertTrue(good['snapshotClockValid'])
        self.assertGreater(good['snapshotNs'], 0)
        out = Path(BUILD.name) / 'no-clock.bin'
        subprocess.run([str(Path(BUILD.name) / 'snapshot-testing'), str(run.directory), str(out)], check=True,
                       capture_output=True, env=dict(os.environ, RC2FX_SNAPSHOT_TEST_FAIL_CLOCK='1'))
        failed = m.read(out.read_bytes())
        self.assertEqual((failed['snapshotNs'], failed['snapshotClockValid']), (None, False))
        self.assertEqual(failed['records'], good['records'], 'the records are unaffected')
        # The production reader has no such hook.
        subprocess.run([str(Path(BUILD.name) / 'snapshot'), str(run.directory), str(out)], check=True,
                       capture_output=True, env=dict(os.environ, RC2FX_SNAPSHOT_TEST_FAIL_CLOCK='1'))
        self.assertTrue(m.read(out.read_bytes())['snapshotClockValid'])

    def test_damaged_snapshots_are_refused(self):
        run = Run()
        out = Path(BUILD.name) / 'damaged.bin'
        subprocess.run([str(Path(BUILD.name) / 'snapshot'), str(run.directory), str(out)], check=True, capture_output=True)
        data = out.read_bytes()
        with self.assertRaises(m.RingError):
            m.read(b'X' + data[1:])
        with self.assertRaises(m.RingError):
            m.read(data[:-5])
        (ring,) = run.rings()
        with self.assertRaises(m.RingError):
            m.read(ring.read_bytes())  # a raw live ring is never decoded


if __name__ == '__main__':
    unittest.main()
