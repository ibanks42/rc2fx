"""Host tests of gui_pacing.c: python3 crates/rc2fx-gui/test_gui_pacing.py

Builds the module with -DRC2FX_PACING_TEST (sites matched by symbol name in
gui_pacing_test.cpp instead of the pinned GUI offsets; no executable hash) and
runs the test program under LD_AUDIT. No device.
"""
import os
import platform
import re
import shutil
import subprocess
import tempfile
import time
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
CC = os.environ.get('CC') or shutil.which('cc') or 'cc'
CXX = os.environ.get('CXX') or shutil.which('c++') or 'c++'


def setUpModule():
    global BUILD
    BUILD = tempfile.TemporaryDirectory()
    out = Path(BUILD.name)
    subprocess.run([CC, '-O2', '-g', '-fPIC', '-shared', '-fno-omit-frame-pointer', '-Wall', '-Wextra', '-Werror',
                    '-DRC2FX_PACING_TEST', str(HERE / 'gui_pacing.c'), '-o', str(out / 'pacing.so'), '-ldl'],
                   check=True)
    # The production build: pinned offsets and the GUI hash.
    subprocess.run([CC, '-O2', '-fPIC', '-shared', '-fno-omit-frame-pointer', '-Wall', '-Wextra', '-Werror',
                    str(HERE / 'gui_pacing.c'), '-o', str(out / 'production.so'), '-ldl'], check=True)
    # The observe-only dirty-region build, test sites and production sites.
    subprocess.run([CC, '-O2', '-g', '-fPIC', '-shared', '-fno-omit-frame-pointer', '-Wall', '-Wextra', '-Werror',
                    '-DRC2FX_PACING_TEST', '-DRC2FX_PACING_DIRTY', str(HERE / 'gui_pacing.c'),
                    '-o', str(out / 'dirty.so'), '-ldl'], check=True)
    subprocess.run([CC, '-O2', '-fPIC', '-shared', '-fno-omit-frame-pointer', '-Wall', '-Wextra', '-Werror',
                    '-DRC2FX_PACING_DIRTY', f'-DGUI_PATH="{out / "program"}"', str(HERE / 'gui_pacing.c'),
                    '-o', str(out / 'dirty-as-gui.so'), '-ldl'], check=True)
    # The same, told the test program is "the GUI" (so its hash must fail).
    subprocess.run([CC, '-O2', '-fPIC', '-shared', '-fno-omit-frame-pointer', '-Wall', '-Wextra', '-Werror',
                    f'-DGUI_PATH="{out / "program"}"', str(HERE / 'gui_pacing.c'), '-o', str(out / 'as-gui.so'),
                    '-ldl'], check=True)
    # The partial-conversion build (test sites).
    subprocess.run([CC, '-O2', '-g', '-fPIC', '-shared', '-fno-omit-frame-pointer', '-Wall', '-Wextra', '-Werror',
                    '-DRC2FX_PACING_TEST', '-DRC2FX_PACING_DIRTY', '-DRC2FX_PACING_PARTIAL',
                    str(HERE / 'gui_pacing.c'), '-o', str(out / 'partial.so'), '-ldl'], check=True)
    # GPU-only compact three-job planner, separately opt-in.
    subprocess.run([CC, '-O2', '-g', '-fPIC', '-shared', '-fno-omit-frame-pointer', '-Wall', '-Wextra', '-Werror',
                    '-DRC2FX_PACING_TEST', '-DRC2FX_PACING_DIRTY', '-DRC2FX_PACING_PARTIAL',
                    '-DRC2FX_GUI_RECT_PLAN3', str(HERE / 'gui_pacing.c'), '-o', str(out / 'partial-plan3.so'),
                    '-ldl'], check=True)
    subprocess.run([CC, '-O2', '-fPIC', '-shared', '-fno-omit-frame-pointer', '-Wall', '-Wextra', '-Werror',
                    '-DRC2FX_PACING_DIRTY', '-DRC2FX_PACING_PARTIAL', '-DRC2FX_GUI_RECT_PLAN3',
                    str(HERE / 'gui_pacing.c'), '-o', str(out / 'partial-plan3-production.so'), '-ldl'], check=True)
    # A short conversion-line cap, only in this dedicated test build.
    subprocess.run([CC, '-O2', '-g', '-fPIC', '-shared', '-fno-omit-frame-pointer', '-Wall', '-Wextra', '-Werror',
                    '-DRC2FX_PACING_TEST', '-DRC2FX_PACING_DIRTY', '-DRC2FX_PACING_PARTIAL',
                    '-DRC2FX_TEST_PARTIAL_LINES=8', str(HERE / 'gui_pacing.c'),
                    '-o', str(out / 'partial-short-log.so'), '-ldl'], check=True)
    # Stand-ins for libdrm's commit and the GE2D V4L2 device: imported calls,
    # as in the GUI.
    subprocess.run([CC, '-O2', '-fPIC', '-shared', '-Wall', '-Wextra', '-Werror', str(HERE / 'fake_v4l2.c'),
                    '-o', str(out / 'libfakedrm.so')], check=True)
    subprocess.run([CXX, '-O1', '-g', '-rdynamic', '-fno-omit-frame-pointer', '-pthread', '-Wall', '-Werror',
                    str(HERE / 'gui_pacing_test.cpp'), '-o', str(out / 'program'), f'-L{out}', '-lfakedrm',
                    f'-Wl,-rpath,{out}'], check=True)


def tearDownModule():
    BUILD.cleanup()


def run(mode, *args, extra=None, module='pacing.so'):
    log = Path(BUILD.name) / f'log-{os.getpid()}-{run.n}'
    run.n += 1
    env = dict(os.environ, LD_AUDIT=str(Path(BUILD.name) / module), RC2FX_GUI_PACING_TEST_LOG=str(log),
               **(extra or {}))
    if mode is None:
        env.pop('RC2FX_GUI_PACING', None)
    else:
        env['RC2FX_GUI_PACING'] = mode
    r = subprocess.run([str(Path(BUILD.name) / 'program'), *args], env=env, capture_output=True, text=True,
                       timeout=60)
    assert r.returncode == 0, r.stderr
    stats = []
    if log.exists():
        for line in log.read_text().splitlines():
            fields = dict(f.split('=', 1) for f in line.split()[1:])
            stats.append((line.split()[0], fields))
    return r.stdout, stats


run.n = 0


def passes(stdout):
    return [int(x) for x in re.findall(r'^pass (\d+)$', stdout, re.M)]


class PacingTests(unittest.TestCase):
    def test_observe_counts_passes_and_never_waits(self):
        out, stats = run('observe', 'pump', '20', '0')
        t = passes(out)
        self.assertLess((t[-1] - t[0]) / 1e6, 30, 'back-to-back passes stay back to back')
        last = stats[-1][1]
        self.assertEqual((stats[-1][0], last['passes'], last['paced']), ('exit', '20', '0'))
        self.assertEqual(last['interval_ns'], '0')
        lines = [x for x in stats if x[0] != 'sha-ok']
        self.assertEqual([x[0] for x in lines][:2], ['start', 'first-pass'])
        self.assertEqual(lines[1][1]['passes'], '1', 'written at the first pass')
        self.assertTrue(lines[0][1]['pid'].isdigit())

    def test_each_cap_paces_back_to_back_passes(self):
        # mode, interval, first histogram bucket the paced intervals may use
        for mode, floor_ms, band in (('30', 33.3, 1), ('15', 66.6, 3), ('10', 100.0, 4), ('5', 200.0, 5),
                                     ('2', 500.0, 6)):
            with self.subTest(mode):
                out, stats = run(mode, 'pump', '8', '0')
                gaps = [(b - a) / 1e6 for a, b in zip(passes(out), passes(out)[1:])]
                self.assertTrue(all(g >= floor_ms - 1.0 for g in gaps), gaps)
                self.assertTrue(all(g < floor_ms + 15 for g in gaps), gaps)
                last = stats[-1][1]
                self.assertEqual((last['passes'], last['paced']), ('8', '7'), 'the first pass never waits')
                # The histogram holds the ACTUAL pass intervals (after the wait).
                hist = [int(x) for x in last['pass_intervals'].split(',')]
                # 66.7 ms sits at the 67 ms edge (buckets 50-67 / 67-100 ms) and
                # 33.3 ms at the 34 ms edge (25-34 / 34-50 ms): jitter picks either.
                # 100 and 200 ms are edges themselves, and a paced interval is
                # never shorter: 100-200 ms and 200 ms-1 s. 500 ms sits inside
                # 200 ms-1 s, never reaching the 1 s bucket.
                self.assertEqual(hist[band] + hist[band + 1], 7, hist)
                if mode in ('10', '5'):
                    self.assertEqual(hist[band], 0, hist)
                if mode == '2':
                    self.assertEqual(hist[band], 7, hist)
                self.assertEqual(sum(hist[:band]), 0, hist)
                self.assertIn('other_ns', out)
                other_ms = int(re.search(r'other_ns (\d+)', out).group(1)) / 1e6
                self.assertLess(other_ms, 20, 'other lock sites are never paced')

    def test_passes_slower_than_the_cap_are_not_delayed(self):
        for mode, gap in (('15', '100'), ('5', '250'), ('2', '550')):
            with self.subTest(mode):
                out, stats = run(mode, 'pump', '5', gap)
                self.assertEqual(stats[-1][1]['paced'], '0')

    def test_the_message_thread_waits_are_counted_only_from_repaint(self):
        out, stats = run('observe', 'wait')
        last = stats[-1][1]
        # repaint_a (~30 ms) and repaint_b (~10 ms) blocked; the already-
        # signalled repaint does not reach the condition wait at all; the
        # other caller's wait is seen at the site but not as a repaint wait.
        self.assertEqual((last['repaint_entered'], last['repaint_completed']), ('2', '2'))
        self.assertGreaterEqual(int(last['repaint_wait_max_ns']), 25_000_000)
        self.assertGreaterEqual(int(last['repaint_wait_ns']), 35_000_000)
        self.assertEqual((last['other_entered'], last['unclassified_entered']), ('1', '0'))
        self.assertEqual(last['clock_errors'], '0')

    def test_paint_waits_for_a_paced_pump_stay_within_one_interval(self):
        # The message thread's paint waits for the pump's pass (1:1): pacing
        # lengthens those waits up to one interval, never longer, and the
        # handshake always completes.
        for mode, interval_ms in (('15', 66.7), ('2', 500.0)):
            with self.subTest(mode):
                out, stats = run(mode, 'handshake', '4')
                self.assertIn('handshakes 4', out)
                last = stats[-1][1]
                self.assertEqual((last['repaint_entered'], last['repaint_completed']), ('4', '4'))
                self.assertEqual(last['paced'], '3', 'the first pass never waits')
                wait_max_ms = int(last['repaint_wait_max_ns']) / 1e6
                self.assertGreater(wait_max_ms, interval_ms - 30)
                self.assertLess(wait_max_ms, interval_ms + 30)

    def test_the_programs_errno_is_unchanged(self):
        out, _ = run('15', 'errno')
        self.assertIn('errno 4321', out)
        self.assertIn('errno_after_wait 777', out)

    def test_no_cancellation_point_is_added_to_the_pump(self):
        for mode, interval_ms in (('15', 66.7), ('5', 200.0), ('2', 500.0)):
            with self.subTest(mode):
                out, _ = run(mode, 'cancel')
                m = re.search(r'cancelled (\d) locked_after_cancel_ms (-?[\d.]+)', out)
                self.assertEqual(m.group(1), '1')
                # Cancelled 10 ms into a ~66 (~200) ms wait: the wait completed and
                # the lock was taken before the cancel took effect at pthread_testcancel.
                after = float(m.group(2))
                self.assertGreater(after, interval_ms - 30)
                self.assertLess(after, interval_ms + 30, 'the wait stays within one interval')

    def test_each_frame_is_logged_with_its_stage_times(self):
        # In every capped mode, and when observing (never paced).
        for mode, interval_ms in (('15', 66.7), ('10', 100.0), ('5', 200.0), ('2', 500.0), ('observe', 0)):
            with self.subTest(mode):
                out, stats = run(mode, 'frames', '4')
                self.assertEqual(out.count('commit 0 errno 99'), 4, 'commit result and errno unchanged')
                log = (Path(BUILD.name) / f'log-{os.getpid()}-{run.n - 1}').read_text().splitlines()
                frames = [dict(f.split('=', 1) for f in line.split()[1:]) for line in log
                          if line.startswith('frame ')]
                self.assertEqual(len(frames), 4)
                for i, f in enumerate(frames):
                    t = [int(f[k]) for k in ('request', 'locked', 'converted', 'commit', 'committed')]
                    self.assertEqual(t, sorted(t), f)
                    self.assertGreaterEqual(t[2] - t[1], 3_000_000, 'the conversion took ~3 ms')
                    self.assertGreaterEqual(t[4] - t[3], 5_000_000, 'the commit took ~5 ms')
                    self.assertEqual(f['result'], '0')
                    if not i:
                        continue
                    since = int(f['request']) + int(f['paced_ns']) - int(frames[i - 1]['request'])
                    if interval_ms:  # back to back: every later frame was paced to the interval
                        self.assertGreater(int(f['paced_ns']), (interval_ms - 25) * 1e6)
                        self.assertGreaterEqual(since, (interval_ms - 7) * 1e6)
                    else:
                        self.assertEqual(f['paced_ns'], '0')

    def test_a_clock_failure_is_counted_and_never_paces(self):
        out, stats = run('15', 'pump', '6', '0', extra={'RC2FX_GUI_PACING_TEST_FAIL_CLOCK': '1'})
        t = passes(out)
        self.assertLess((t[-1] - t[0]) / 1e6, 30, 'no wait on an invalid time')
        last = stats[-1][1]
        self.assertEqual((last['passes'], last['paced']), ('6', '0'))
        self.assertGreaterEqual(int(last['clock_errors']), 6)
        self.assertEqual(last['pass_intervals'], '0,0,0,0,0,0,0,0')

    def test_a_pending_cancel_does_not_act_inside_a_log_write(self):
        out, stats = run('observe', 'cancel-write')
        self.assertIn('cancelled 1 reached 1', out)
        self.assertIn('first-pass', [x[0] for x in stats], 'the write happened with the cancel pending')

    def test_a_forked_child_never_acts(self):
        out, stats = run('15', 'fork')
        self.assertIn('child_ms', out)
        child_ms = float(re.search(r'child_ms ([\d.]+)', out).group(1))
        self.assertLess(child_ms, 30, 'the child is not paced')
        self.assertEqual([s[0] for s in stats].count('exit'), 1, 'only the GUI writes its exit line')

    def test_the_admission_hash_is_sha256(self):
        _, stats = run('observe', 'pump', '1', '0')
        self.assertEqual(stats[0][0], 'sha-ok')

    def test_other_executables_stay_silent_and_inactive(self):
        # e.g. the GUI's helper processes, which inherit LD_AUDIT
        env = dict(os.environ, LD_AUDIT=str(Path(BUILD.name) / 'production.so'), RC2FX_GUI_PACING='15')
        r = subprocess.run([str(Path(BUILD.name) / 'program'), 'pump', '6', '0'], env=env, capture_output=True,
                           text=True, timeout=60)
        self.assertEqual(r.returncode, 0)
        self.assertNotIn('RC2FX GUI pacing', r.stderr)
        t = passes(r.stdout)
        self.assertLess((t[-1] - t[0]) / 1e6, 30, 'not paced')

    def test_a_gui_path_executable_that_fails_admission_says_so_with_its_pid(self):
        env = dict(os.environ, LD_AUDIT=str(Path(BUILD.name) / 'as-gui.so'), RC2FX_GUI_PACING='15')
        r = subprocess.run([str(Path(BUILD.name) / 'program'), 'pump', '6', '0'], env=env, capture_output=True,
                           text=True, timeout=60)
        self.assertEqual(r.returncode, 0)
        self.assertRegex(r.stderr, r'^RC2FX GUI pacing: pid \d+ executable not admitted; inactive$')
        t = passes(r.stdout)
        self.assertLess((t[-1] - t[0]) / 1e6, 30, 'not paced')

    @unittest.skipUnless(platform.machine() == 'x86_64', 'the paint frames are x86-64 stand-ins')
    def test_the_dirty_build_logs_each_paint_and_destination(self):
        out, stats = run('observe', 'dirty', module='dirty.so')
        self.assertIn('dirty done', out)
        self.assertIn('errno_after_paint 55', out, 'the GUI errno is unchanged')
        log = (Path(BUILD.name) / f'log-{os.getpid()}-{run.n - 1}').read_text().splitlines()
        paints = [dict(f.split('=', 1) for f in line.split()[1:]) for line in log if line.startswith('paint ')]
        frames = [dict(f.split('=', 1) for f in line.split()[1:]) for line in log if line.startswith('frame ')]
        self.assertEqual(len(paints), 6, paints)  # 4 + the long list + the errno check; 3 rejected
        a = paints[0]
        self.assertEqual((a['path'], a['n'], a['bbox'], a['area'], a['outside']),
                         ('A', '3', '10,20,1280,710', str(30 * 40 + 50 * 60 + 20 * 30), '1'))
        self.assertEqual(a['rects'], '10:20:30:40;100:200:50:60;1270:700:20:30')
        b = paints[3]
        self.assertEqual((b['path'], b['n'], b['bbox'], b['area'], b['outside']),
                         ('B', '1', '0,0,1280,720', str(1280 * 720), '0'))
        many = paints[4]
        self.assertEqual((many['n'], many['area']), ('40', str(40 * 12)))
        self.assertEqual(many['rects'].count(';'), 15, 'at most 16 rectangles are listed')
        self.assertEqual([(f['dest_fd'], f['dest_fb']) for f in frames],
                         [('41', '7'), ('42', '8'), ('41', '7'), ('42', '8')])
        exit_line = [dict(f.split('=', 1) for f in line.split()[1:]) for line in log
                     if line.startswith('dirty-exit ')][-1]
        self.assertEqual((exit_line['paints'], exit_line['paint_rejects'], exit_line['paint_outside']),
                         ('9', '3', '3'))  # the three path-A paints include 1270:700:20:30
        last = [fields for kind, fields in stats if kind == 'exit'][-1]
        self.assertEqual(last['paced'], '0', 'observe only: nothing waits')

    @unittest.skipUnless(platform.machine() == 'x86_64', 'the paint frames are x86-64 stand-ins')
    def test_the_dirty_build_never_paces_and_the_default_build_logs_no_paints(self):
        for mode in ('30', '15', '10', '5', '2'):
            with self.subTest(mode):
                out, stats = run(mode, 'pump', '4', '0', module='dirty.so')
                t = passes(out)
                self.assertLess((t[-1] - t[0]) / 1e6, 30, 'not paced')
                self.assertEqual(stats, [], 'inactive: only observe is accepted')
        out, _ = run('observe', 'dirty')
        log = (Path(BUILD.name) / f'log-{os.getpid()}-{run.n - 1}').read_text()
        self.assertNotIn('paint ', log)
        self.assertNotIn('dest_fd', log)

    def test_the_dirty_build_admits_only_the_gui(self):
        env = dict(os.environ, LD_AUDIT=str(Path(BUILD.name) / 'dirty-as-gui.so'), RC2FX_GUI_PACING='observe')
        r = subprocess.run([str(Path(BUILD.name) / 'program'), 'pump', '3', '0'], env=env, capture_output=True,
                           text=True, timeout=60)
        self.assertEqual(r.returncode, 0)
        self.assertRegex(r.stderr, r'^RC2FX GUI pacing: pid \d+ executable not admitted; inactive$')

    def partial(self, mode, frames=40, module='partial.so', **env):
        out, stats = run(mode, 'partial', str(frames), module=module, extra=env)
        log = (Path(BUILD.name) / f'log-{os.getpid()}-{run.n - 1}').read_text().splitlines()
        converts = [dict(f.split('=', 1) for f in line.split()[1:]) for line in log if line.startswith('convert ')]
        verifies = [dict(f.split('=', 1) for f in line.split()[1:]) for line in log if line.startswith('verify ')]
        frames_out = [line.split() for line in out.splitlines() if line.startswith('frame ')]
        summary = [line.split() for line in out.splitlines() if line.startswith('partial frames')][0]
        return frames_out, converts, verifies, dict(zip(summary[1::2], summary[2::2]))

    @unittest.skipUnless(platform.machine() == 'x86_64', 'the paint frames are x86-64 stand-ins')
    def test_partial_conversion_keeps_every_destination_identical_to_a_full_one(self):
        for direction in ('cw', 'ccw'):
            with self.subTest(direction):
                frames, converts, _, summary = self.partial(f'partial-{direction}', FAKE_GE2D_DIR=direction)
                self.assertEqual(summary['bad_frames'], '0')
                self.assertTrue(all(f[5] == '0' for f in frames), 'every conversion returned success')
                self.assertEqual([(c['kind'], c['reason']) for c in converts[:2]], [('full', 'forced')] * 2,
                                 'the first conversion into each destination is full')
                partial = [c for c in converts if c['kind'] == 'partial']
                self.assertEqual(len(partial), len(converts) - 2)
                self.assertTrue(all(1 <= int(c['jobs']) <= 4 for c in partial))
                self.assertTrue(all(len(c['job_us'].split(',')) == int(c['jobs']) for c in partial))
                self.assertLess(int(summary['pixels']), 3 * 921600, 'far fewer pixels than full frames')

    @unittest.skipUnless(platform.machine() == 'x86_64', 'the paint frames are x86-64 stand-ins')
    def test_plan3_exact_notification_event_plans_and_baseline_totals(self):
        cases = {
            'A': (1216, 36112, 2020, {'167:341:28:27', '411:444:28:28', '1010:45:16:30'}),
            'B': (34534, 104136, 34686, {'131:0:274:120', '411:444:28:42', '968:45:21:30'}),
        }
        for event, (union, baseline_px, candidate_px, expected) in cases.items():
            for module, pixels, jobs in [('partial.so', baseline_px, 2), ('partial-plan3.so', candidate_px, 3)]:
                with self.subTest(event=event, module=module):
                    out, lines = run('partial-cw', 'partial', '4', module=module,
                                     extra={'PARTIAL_EVENT_CASE': event, 'RC2FX_TEST_PLAN_LOG': '1'})
                    self.assertIn('bad_frames 0', out)
                    self.assertIn(f'pending_union {union}', out)
                    start = next(f for k, f in lines if k == 'start')
                    self.assertEqual(start['merge_px'], '20000' if module == 'partial-plan3.so' else '80000')
                    converts = [f for k, f in lines if k == 'convert']
                    plans = [f for k, f in lines if k == 'test-plan']
                    self.assertEqual(len(plans), 2)
                    self.assertEqual([(int(p['jobs']), int(p['px'])) for p in plans], [(jobs, pixels)] * 2)
                    if module == 'partial-plan3.so':
                        self.assertTrue(all(set(p['rects'].split(';')) == expected for p in plans), plans)
                    self.assertEqual([(c['kind'], c['reason']) for c in converts[:2]], [('full', 'forced')] * 2)

    @unittest.skipUnless(platform.machine() == 'x86_64', 'the paint frames are x86-64 stand-ins')
    def test_plan3_preserves_explicit_merge_override(self):
        for threshold, jobs, pixels in [('0', '3', '2020'), ('20000', '3', '2020'), ('80000', '2', '36112')]:
            with self.subTest(threshold=threshold):
                out, lines = run('partial-cw', 'partial', '4', module='partial-plan3.so', extra={
                    'PARTIAL_EVENT_CASE': 'A', 'RC2FX_TEST_PLAN_LOG': '1',
                    'RC2FX_GUI_PARTIAL_MERGE_PX': threshold})
                self.assertIn('bad_frames 0', out)
                self.assertEqual(next(f for k, f in lines if k == 'start')['merge_px'], threshold)
                plans = [f for k, f in lines if k == 'test-plan']
                self.assertEqual([(p['jobs'], p['px']) for p in plans], [(jobs, pixels)] * 2)

    @unittest.skipUnless(platform.machine() == 'x86_64', 'the paint frames are x86-64 stand-ins')
    def test_plan3_random_pixels_capacity_and_soft_budget(self):
        for direction in ('cw', 'ccw'):
            for pattern in ('PARTIAL_RANDOM', 'PARTIAL_CAPACITY'):
                with self.subTest(direction=direction, pattern=pattern):
                    _, converts, verifies, summary = self.partial(
                        f'partial-verify-{direction}', frames=30, module='partial-plan3.so',
                        FAKE_GE2D_DIR=direction, **{pattern: '1'})
                    self.assertEqual(summary['bad_frames'], '0')
                    self.assertTrue(all(v['mismatches'] == '0' for v in verifies))
                    self.assertTrue(all(int(c['jobs']) <= 3 for c in converts if c['kind'] == 'partial'))
                    self.assertEqual(int(summary['jobs']), sum(int(c['jobs']) for c in converts))
                    self.assertEqual(int(summary['pixels']), sum(int(c['px']) for c in converts))
                    if pattern == 'PARTIAL_CAPACITY':
                        self.assertTrue(any(c['kind'] == 'full' and c['reason'] == 'area' for c in converts))
        _, converts, _, summary = self.partial('partial-cw', frames=6, module='partial-plan3.so',
            PARTIAL_EVENT_CASE='A', FAKE_GE2D_DELAY_US='2000', RC2FX_TEST_PARTIAL_BUDGET_US='1000',
            RC2FX_TEST_VERBOSE_EVERY_MS='1')
        self.assertEqual(summary['bad_frames'], '0')
        partial = [c for c in converts if c['kind'] == 'partial' and c['jobs'] == '3']
        self.assertEqual([(c['jobs'], c['px'], c['reason']) for c in partial], [('3', '2020', '-')] * 2)
        counters = [f for k, f in log_lines() if k == 'counters']
        self.assertTrue(counters)
        self.assertEqual(counters[-1]['planner_policy'], 'plan3')
        self.assertEqual(int(counters[-1]['soft_budget_exceeded']), 2, 'one count per over-budget conversion')
        self.assertEqual(counters[-1]['budget_merged'], '0')

    @unittest.skipUnless(platform.machine() == 'x86_64', 'the paint frames are x86-64 stand-ins')
    def test_plan3_rejected_paint_forces_full_and_keeps_pixels(self):
        _, converts, _, summary = self.partial('partial-cw', frames=8, module='partial-plan3.so',
                                              PARTIAL_INVALID='1')
        self.assertEqual(summary['bad_frames'], '0')
        self.assertEqual((converts[4]['kind'], converts[4]['reason']), ('full', 'forced'))
        self.assertEqual((converts[5]['kind'], converts[5]['reason']), ('full', 'forced'))

    @unittest.skipUnless(platform.machine() == 'x86_64', 'the paint frames are x86-64 stand-ins')
    def test_plan3_forced_full_dose_still_uses_full_hardware_job(self):
        with tempfile.TemporaryDirectory() as tmp:
            force = Path(tmp) / 'force-full'
            force.touch()
            _, converts, _, summary = self.partial('partial-cw', frames=8, module='partial-plan3.so',
                RC2FX_TEST_FORCE_FULL_FILE=str(force))
            self.assertEqual(summary['bad_frames'], '0')
            forced = [c for c in converts if c['reason'] == 'forced-full']
            self.assertTrue(forced)
            self.assertTrue(all(c['kind'] == 'full' and c['px'] == '921600' and c['jobs'] == '1' for c in forced))

    @unittest.skipUnless(platform.machine() == 'x86_64', 'the paint frames are x86-64 stand-ins')
    def test_plan3_preserves_cpu_large_for_forced_full(self):
        with tempfile.TemporaryDirectory() as tmp:
            force = Path(tmp) / 'force-full'
            force.touch()
            _, converts, _, summary = self.partial('partial-cw', frames=8, module='partial-plan3.so',
                RC2FX_TEST_CPU_ROTATE='1', RC2FX_TEST_FORCE_FULL_FILE=str(force))
            self.assertEqual(summary['bad_frames'], '0')
            self.assertTrue(any(c['kind'] == 'cpu' and c['reason'] == 'forced-full' for c in converts))

    @unittest.skipUnless(platform.machine() == 'x86_64', 'the paint frames are x86-64 stand-ins')
    def test_plan3_second_extra_job_failures_keep_existing_fallbacks(self):
        for fault, reason in [('FAKE_GE2D_FAIL_QBUF', 'selection-or-qbuf'), ('FAKE_GE2D_FAIL_DQBUF', 'job-failed')]:
            with self.subTest(fault=fault):
                _, converts, _, summary = self.partial('partial-cw', frames=4, module='partial-plan3.so',
                    PARTIAL_EVENT_CASE='A', FAKE_GE2D_DELAY_US='2000', RC2FX_TEST_PARTIAL_BUDGET_US='1000',
                    RC2FX_TEST_VERBOSE_EVERY_MS='1', **{fault: '4'})
                self.assertEqual(summary['bad_frames'], '0')
                self.assertEqual((converts[2]['kind'], converts[2]['reason']), ('full', reason))
                counters = [f for k, f in log_lines() if k == 'counters']
                self.assertTrue(counters)
                self.assertGreater(int(counters[-1]['soft_budget_exceeded']), 0)

    @unittest.skipUnless(platform.machine() == 'x86_64', 'the paint frames are x86-64 stand-ins')
    def test_the_wrong_orientation_is_caught_by_the_identity_check(self):
        _, _, _, summary = self.partial('partial-ccw', frames=6, FAKE_GE2D_DIR='cw')
        self.assertNotEqual(summary['bad_frames'], '0')

    @unittest.skipUnless(platform.machine() == 'x86_64', 'the paint frames are x86-64 stand-ins')
    def test_no_periodic_full_frames_over_a_long_run(self):
        # 200 conversions, more than two seconds of fake time each with two
        # jobs of 6 ms; identity is checked after every one of them.
        frames, converts, _, summary = self.partial('partial-cw', frames=200, FAKE_GE2D_DELAY_US='6000')
        self.assertEqual(summary['bad_frames'], '0')
        self.assertEqual((converts[0]['reason'], converts[1]['reason']), ('forced', 'forced'))
        self.assertTrue(all(c['kind'] == 'partial' for c in converts[2:]), 'only the first two are full')
        self.assertNotIn('periodic', {c['reason'] for c in converts})
        span = int(converts[-1]['t']) - int(converts[0]['t'])
        self.assertGreater(span, 2_000_000_000, 'more than 2 s of run time')

    @unittest.skipUnless(platform.machine() == 'x86_64', 'the paint frames are x86-64 stand-ins')
    def test_the_kill_file_ends_partial_conversion_at_the_next_conversion(self):
        kill = Path(BUILD.name) / f'partial-off'
        kill.unlink(missing_ok=True)
        try:
            _, converts, _, summary = self.partial('partial-cw', frames=20, PARTIAL_KILL_AT='10',
                                                   PARTIAL_KILL_FILE=str(kill))
        finally:
            kill.unlink(missing_ok=True)
        self.assertEqual(summary['bad_frames'], '0')
        self.assertTrue(all(c['kind'] == 'partial' for c in converts[2:10]))
        self.assertEqual({(c['kind'], c['reason']) for c in converts[10:]}, {('full', 'kill')})

    @unittest.skipUnless(platform.machine() == 'x86_64', 'the paint frames are x86-64 stand-ins')
    def test_extra_jobs_over_the_time_budget_merge_the_rest_into_one_job(self):
        # Each fake job takes 2 ms against a 1 ms budget: it trips after the
        # first extra job, and the remaining rects become one bounding box,
        # the vendor's job. The whole destination equals the rotated source
        # after every conversion (bad_frames 0).
        frames, converts, _, summary = self.partial('partial-cw', frames=20, FAKE_GE2D_DELAY_US='2000',
                                                    RC2FX_TEST_PARTIAL_BUDGET_US='1000',
                                                    RC2FX_GUI_PARTIAL_MERGE_PX='1')
        self.assertEqual(summary['bad_frames'], '0')
        self.assertTrue(all(f[5] == '0' for f in frames), 'every conversion returned success')
        merged = [c for c in converts if c['reason'] == 'budget-merged']
        self.assertTrue(merged, 'multi-job conversions ran past the budget')
        self.assertTrue(all(c['kind'] == 'partial' for c in merged))
        self.assertTrue(all(c['jobs'] == '2' for c in merged), 'the first extra job, then the merged box')
        self.assertTrue(all(sum(map(int, c['job_px'].split(','))) == int(c['px']) for c in merged))
        self.assertFalse([c for c in converts if c['reason'] == 'budget'], 'no full frame for the budget')
        self.assertEqual({c['reason'] for c in converts if c['kind'] == 'full'}, {'forced'})
        # The same jobs under a larger budget are converted partially.
        _, converts, _, _ = self.partial('partial-cw', frames=20, FAKE_GE2D_DELAY_US='2000',
                                         RC2FX_TEST_PARTIAL_BUDGET_US='3000',
                                         RC2FX_GUI_PARTIAL_MERGE_PX='1')
        self.assertTrue([c for c in converts if c['kind'] == 'partial' and int(c['jobs']) >= 3])

    @unittest.skipUnless(platform.machine() == 'x86_64', 'the paint frames are x86-64 stand-ins')
    def test_a_budget_box_over_half_the_frame_is_a_full_frame(self):
        # A far-corner rect makes the box over the remaining rects most of
        # the frame: then, and only then, a full frame with reason budget.
        _, converts, _, summary = self.partial('partial-cw', frames=20, FAKE_GE2D_DELAY_US='2000',
                                               RC2FX_TEST_PARTIAL_BUDGET_US='1000',
                                               RC2FX_GUI_PARTIAL_MERGE_PX='1', PARTIAL_WIDE='1')
        self.assertEqual(summary['bad_frames'], '0')
        budget = [c for c in converts if c['reason'] == 'budget']
        self.assertTrue(budget)
        self.assertTrue(all(c['kind'] == 'full' and c['px'] == str(1280 * 720) for c in budget))
        # When the far-corner rect was itself the first extra job, the box over
        # the rest is small and still merges: every merged box is at most half.
        for c in converts:
            if c['reason'] == 'budget-merged':
                self.assertLessEqual(int(c['job_px'].split(',')[-1]) * 100, 1280 * 720 * 50, c)

    @unittest.skipUnless(platform.machine() == 'x86_64', 'the paint frames are x86-64 stand-ins')
    def test_a_failed_extra_job_falls_back_to_a_full_frame(self):
        # Output QBUFs: frames 0 and 1 are full (1, 2); frame 2's first extra job is 3.
        _, converts, _, summary = self.partial('partial-cw', frames=10, FAKE_GE2D_FAIL_QBUF='3')
        self.assertEqual(summary['bad_frames'], '0')
        self.assertEqual((converts[2]['kind'], converts[2]['reason']), ('full', 'selection-or-qbuf'))
        self.assertTrue(all(c['kind'] == 'partial' for c in converts[3:]), 'partial conversion resumes')

    @unittest.skipUnless(platform.machine() == 'x86_64', 'the paint frames are x86-64 stand-ins')
    def test_a_failed_dequeue_drains_and_disables_partial_conversion(self):
        # Capture DQBUFs: frames 0 and 1 (1, 2); frame 2's extra job is 3.
        frames, converts, _, summary = self.partial('partial-cw', frames=10, FAKE_GE2D_FAIL_DQBUF='3')
        self.assertEqual(summary['bad_frames'], '0')
        self.assertTrue(all(f[5] == '0' for f in frames), 'the vendor sequence still succeeds')
        self.assertEqual({(c['kind'], c['reason']) for c in converts[2:]}, {('full', 'job-failed')})

    @unittest.skipUnless(platform.machine() == 'x86_64', 'the paint frames are x86-64 stand-ins')
    def test_verify_checks_the_first_four_partial_conversions_per_destination(self):
        frames, converts, verifies, summary = self.partial('partial-verify-cw', frames=30, FAKE_GE2D_DIR='cw')
        self.assertEqual(summary['bad_frames'], '0')
        self.assertEqual([v['dest_fd'] for v in verifies], ['9', '10'] * 4, 'four per destination, then none')
        for v in verifies:
            self.assertEqual((v['mismatches'], v['mode'], v['matches_ccw']), ('0', 'cw', '0'), v)
            self.assertEqual(v['matches_cw'], v['of'])
            self.assertGreater(int(v['of']), 1000, 'enough points tell the rotations apart')
        self.assertTrue(all(c['kind'] == 'partial' for c in converts[2:]))

    @unittest.skipUnless(platform.machine() == 'x86_64', 'the paint frames are x86-64 stand-ins')
    def test_verify_with_the_wrong_orientation_disables_and_names_the_right_one(self):
        frames, converts, verifies, summary = self.partial('partial-verify-ccw', frames=12, FAKE_GE2D_DIR='cw')
        self.assertEqual(len(verifies), 1, 'the first verified conversion fails; no partial conversion after it')
        v = verifies[0]
        self.assertNotEqual(v['mismatches'], '0')
        self.assertEqual(v['mode'], 'ccw')
        self.assertGreater(int(v['matches_cw']), 0.9 * int(v['of']), 'the destination holds the CW rotation')
        self.assertLess(int(v['matches_ccw']), 0.1 * int(v['of']))
        self.assertEqual((converts[2]['kind'], converts[3]['kind']), ('partial', 'full'))
        self.assertEqual({(c['kind'], c['reason']) for c in converts[3:]}, {('full', 'verify-fail')})
        self.assertEqual([f[7] for f in frames[-4:]], ['0'] * 4, 'full frames are identical again')

    @unittest.skipUnless(platform.machine() == 'x86_64', 'the paint frames are x86-64 stand-ins')
    def test_verify_finds_a_pixel_written_outside_any_paint_and_stops(self):
        frames, converts, verifies, summary = self.partial('partial-verify-cw', frames=20, PARTIAL_ROGUE_AT='8',
                                                           RC2FX_TEST_PARTIAL_VERIFY_EVERY='1')
        bad = [v for v in verifies if v['mismatches'] != '0']
        self.assertEqual(len(bad), 1)
        self.assertEqual((bad[0]['first'], bad[0]['mismatches']), ('1270,19', '1'),
                         'source (1270, 700) rotated clockwise')
        self.assertEqual(bad[0]['matches_cw'], bad[0]['of'], 'the orientation itself is right')
        after = converts[converts.index(next(c for c in converts if c['reason'] == 'verify-fail')):]
        self.assertEqual({(c['kind'], c['reason']) for c in after}, {('full', 'verify-fail')})
        self.assertEqual([f[7] for f in frames[-4:]], ['0'] * 4)

    @unittest.skipUnless(platform.machine() == 'x86_64', 'the paint frames are x86-64 stand-ins')
    def test_each_conversion_logs_every_jobs_pixels_and_time(self):
        _, converts, _, _ = self.partial('partial-cw', frames=12, FAKE_GE2D_DELAY_US='300')
        for c in converts:
            px, us = c['job_px'].split(','), c['job_us'].split(',')
            self.assertEqual((len(px), len(us)), (int(c['jobs']), int(c['jobs'])), c)
            self.assertEqual(sum(map(int, px)), int(c['px']))
            self.assertTrue(all(int(u) >= 300 for u in us), 'each job is timed, the vendor job included')
        self.assertEqual(converts[0]['job_px'], '921600')

    @unittest.skipUnless(platform.machine() == 'x86_64', 'the paint frames are x86-64 stand-ins')
    def test_every_verify_skip_reason_is_logged_with_its_values(self):
        cases = {
            'descriptor-null': dict(reason='descriptor-null', dfd='-1'),
            'dest-mismatch': dict(reason='dest-mismatch'),
            'dst-null': dict(reason='dst-null', dst='0'),
            'querybuf': dict(reason='querybuf'),  # errno comes from the fake's libc instance
            'mmap': dict(reason='mmap', errno='22'),
        }
        for case, want in cases.items():
            with self.subTest(case):
                env = {'PARTIAL_SKIP': case}
                if case == 'querybuf':
                    env['FAKE_GE2D_FAIL_QUERYBUF'] = '1'
                if case == 'mmap':
                    env['FAKE_GE2D_BAD_OFFSET'] = '1'
                out, stats = run('partial-verify-cw', 'partial', '6', module='partial.so', extra=env)
                log = (Path(BUILD.name) / f'log-{os.getpid()}-{run.n - 1}').read_text().splitlines()
                skips = [dict(f.split('=', 1) for f in line.split()[1:]) for line in log
                         if line.startswith('verify-skip ')]
                # 6 frames: 2 full, then 4 partial, all within the first 4 per destination.
                self.assertEqual(len(skips), 4, 'every scheduled check skipped, once per conversion')
                self.assertEqual([row['cap_dest'] for row in skips], ['9', '10', '9', '10'])
                for row in skips:
                    for key, value in want.items():
                        self.assertEqual(row[key], value, row)
                if case == 'dest-mismatch':  # the fd of the other destination
                    self.assertEqual([row['dfd'] != row['cap_dest'] for row in skips], [True] * 4)
                self.assertFalse([line for line in log if line.startswith('verify ')], 'no check ran')

    @unittest.skipUnless(platform.machine() == 'x86_64', 'the paint frames are x86-64 stand-ins')
    def test_verify_maps_the_source_writable_as_vb2_requires(self):
        # vb2 rejects an OUTPUT mapping without VM_WRITE; the test build
        # enforces that rule, so a read-only mapping would show as a skip.
        out, stats = run('partial-verify-cw', 'partial', '6', module='partial.so')
        log = (Path(BUILD.name) / f'log-{os.getpid()}-{run.n - 1}').read_text().splitlines()
        self.assertFalse([line for line in log if line.startswith('verify-skip ')], 'the mapping was accepted')
        verifies = [line for line in log if line.startswith('verify ')]
        self.assertEqual(len(verifies), 4)
        self.assertTrue(all('mismatches=0' in line for line in verifies))

    @unittest.skipUnless(platform.machine() == 'x86_64', 'the paint frames are x86-64 stand-ins')
    def test_partial_modes_are_inert_in_the_other_builds(self):
        for module in ('pacing.so', 'dirty.so'):
            with self.subTest(module):
                out, stats = run('partial-cw', 'partial', '4', module=module)
                self.assertEqual(stats, [], 'not a mode these builds accept')
                self.assertIn('bad_frames 0', out, 'every conversion is a plain full frame')

    def test_off_or_unset_binds_nothing(self):
        for mode in ('off', None, 'bogus', '20', '7', '0', '', '1', '02'):
            with self.subTest(mode):
                out, stats = run(mode, 'pump', '6', '0')
                t = passes(out)
                self.assertLess((t[-1] - t[0]) / 1e6, 30)
                self.assertEqual(stats, [])


def log_lines():
    """The last run's log as (kind, fields) pairs, in order."""
    log = (Path(BUILD.name) / f'log-{os.getpid()}-{run.n - 1}').read_text().splitlines()
    return [(line.split()[0], dict(f.split('=', 1) for f in line.split()[1:])) for line in log]


def log_file():
    """The path of the last run's log."""
    return Path(BUILD.name) / f'log-{os.getpid()}-{run.n - 1}'


def family():
    """The last run's log lines from "<log>.1" then "<log>", oldest first."""
    base = log_file()
    text = ''
    older = base.with_name(base.name + '.1')
    if older.exists():
        text += older.read_text()
    if base.exists():
        text += base.read_text()
    return text.splitlines()


class RotationTests(unittest.TestCase):
    """gui_pacing.c's log rotation: a full log becomes <log>.1, a fresh one
    starts, one thread rotates at a time and the others drop their line."""

    @unittest.skipUnless(platform.machine() == 'x86_64', 'the paint frames are x86-64 stand-ins')
    def test_a_full_log_rotates_and_every_line_survives(self):
        cap = 2000  # two counters lines (about 700 bytes each) fit; the third rotates the log
        out, _ = run('partial-cw', 'partial', '40', module='partial.so',
                     extra=dict(RC2FX_GUI_PACING_QUIET='1', FAKE_GE2D_DELAY_US='2000',
                                RC2FX_TEST_COUNTERS_EVERY_MS='1', RC2FX_GUI_PACING_TEST_LOG_CAP=str(cap)))
        self.assertIn('bad_frames 0', out)
        base = log_file()
        older = base.with_name(base.name + '.1')
        self.assertTrue(older.exists(), 'the full log was rotated to .1')
        self.assertTrue(base.exists(), 'a fresh log was opened')
        current = base.read_text()
        self.assertLessEqual(len(current), cap)
        # Every line in both files is whole and parseable: nothing was cut.
        lines = [line for line in family() if line]
        self.assertTrue(lines)
        for line in lines:
            fields = line.split()
            self.assertTrue(fields[0].replace('-', '').isalpha(), line)
            for field in fields[1:]:
                self.assertIn('=', field, line)
        counters = [dict(f.split('=', 1) for f in line.split()[1:]) for line in lines if line.startswith('counters ')]
        self.assertTrue(counters, 'a counters line reached a file')
        self.assertGreaterEqual(int(counters[-1]['log_rotations']), 1)
        self.assertGreaterEqual(int(counters[-1]['log_dropped']), 0)
        # The log is reopened with the same name, append-only per rotation.
        self.assertEqual(base.parent, older.parent)

    @unittest.skipUnless(platform.machine() == 'x86_64', 'the paint frames are x86-64 stand-ins')
    def test_two_threads_logging_at_once_rotate_and_write_whole_lines(self):
        # Paints here and committed frames on another thread: both log.
        cap = 900
        out, _ = run('observe', 'race', '300', module='dirty.so',
                     extra={'RC2FX_GUI_PACING_TEST_LOG_CAP': str(cap)})
        self.assertIn('raced 300', out)
        base = log_file()
        older = base.with_name(base.name + '.1')
        self.assertTrue(older.exists(), 'the racing writers rotated the log')
        self.assertLessEqual(len(base.read_text()), cap + 640, 'one racing line may cross the cap')
        lines = [line for line in family() if line]
        kinds = {line.split()[0] for line in lines}
        self.assertIn('paint', kinds)
        self.assertIn('frame', kinds)
        for line in lines:
            self.assertTrue(line.split()[0].replace('-', '').isalpha(), line)
            for field in line.split()[1:]:
                self.assertIn('=', field, line)


    @unittest.skipUnless(platform.machine() == 'x86_64', 'the paint frames are x86-64 stand-ins')
    def test_a_rotation_never_hands_its_descriptor_to_the_gui(self):
        # The rotation must not close the live log descriptor: another thread
        # of the GUI that opens a file in that window would get the number and
        # the next log line would land in its file. So a third thread opens,
        # appends to and closes a file of its own throughout the run, and that
        # file must hold nothing but its own lines.
        canary = Path(BUILD.name) / 'canary'
        out, _ = run('observe', 'race', '400', module='dirty.so',
                     extra={'RC2FX_GUI_PACING_TEST_LOG_CAP': '900', 'RC2FX_TEST_CANARY': str(canary)})
        self.assertIn('raced 400', out)
        base = log_file()
        self.assertTrue(base.with_name(base.name + '.1').exists(), 'the log did rotate')
        self.assertTrue(canary.exists(), 'the opening thread ran')
        stray = [line for line in canary.read_text().splitlines() if line != 'canary']
        self.assertEqual(stray, [], 'no log line landed in a file the GUI opened')


    def test_the_rotation_never_closes_the_live_descriptor(self):
        # The runtime test above cannot force the few-instruction window in
        # which an old implementation's stale descriptor would hit the file
        # the GUI opened (verified: it passes on that implementation), so pin
        # the invariant in the source: the new log is opened aside and dup3'd
        # over the live number, which is then never closed.
        source = (HERE / 'gui_pacing.c').read_text()
        body = source[source.index('static void rotate_log(void) {'):]
        body = body[:body.index('\n}\n')]
        self.assertIn('SYS_dup3', body)
        self.assertNotIn('SYS_close, log_fd', body)
        self.assertIn('SYS_renameat', body)
        self.assertIn('SYS_openat', body)


class RotateTests(unittest.TestCase):
    """rotate/rotate.c against its scalar reference. On an aarch64 host this
    runs the NEON path; elsewhere both sides are the scalar code, and
    ProductionPlanTests runs the NEON path under qemu-aarch64 when zig and
    qemu are installed."""

    def test_rotation_matches_the_scalar_reference_whole_and_in_strips(self):
        program = Path(BUILD.name) / 'test_rotate'
        subprocess.run([CC, '-O2', '-Wall', '-Wextra', '-Werror', str(HERE / 'rotate' / 'test_rotate.c'),
                        str(HERE / 'rotate' / 'rotate.c'), '-o', str(program)], check=True)
        r = subprocess.run([str(program)], capture_output=True, text=True, timeout=120)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertRegex(r.stdout, r'^\d{4,} cases, 0 failures$')


class CpuPathTests(unittest.TestCase):
    """Fixed CPU rotation of large redraws; small damage still uses GE2D. The
    host build takes the CPU path only with RC2FX_TEST_CPU_ROTATE, so the same
    module run without it is the GE2D reference the CPU path is compared with."""
    @unittest.skipUnless(platform.machine() == 'x86_64', 'x86-64 paint stand-ins')
    def test_failed_medium_token_refreshes_damage_on_reuse(self):
        for direction in ('cw', 'ccw'):
            out, lines = run('partial-' + direction, 'partial', '8',
                             module='partial-plan3.so', extra={
                                 'RC2FX_TEST_CPU_ROTATE': '1',
                                 'PARTIAL_EVENT_CASE': 'B',
                                 'FAKE_GE2D_DIR': direction,
                                 'FAKE_GE2D_FAIL_QBUF': '3',
                                 'PARTIAL_RECOVER_QUEUES': '1',
                                 'PARTIAL_REAL_DEST_FDS': '1'})
            # One failed presentation is unavoidable; its damage must not
            # remain stale when the alternating destination is reused.
            self.assertIn('bad_frames 1', out)
            self.assertIn('failed_qbuf_cpu_writes 0', out)
            frames = [line for line in out.splitlines() if line.startswith('frame ')]
            self.assertTrue(all(line.endswith('mismatches 0') for line in frames[3:]), out)

    @unittest.skipUnless(platform.machine() == 'x86_64', 'x86-64 paint stand-ins')
    def test_cpu_regions_keep_offsets_edges_and_alternating_destinations(self):
        for direction in ('cw', 'ccw'):
            out, lines = run('partial-verify-' + direction, 'partial', '40',
                             module='partial-plan3.so', extra={
                                 'RC2FX_TEST_CPU_ROTATE': '1', 'PARTIAL_CPU_REGIONS': '1',
                                 'FAKE_GE2D_DIR': direction})
            self.assertIn('bad_frames 0', out)
            updates = [f for k, f in lines if k == 'convert'][2:]
            self.assertEqual(len(updates), 38)
            self.assertTrue(all(c['kind'] == 'cpu' and c['reason'] == 'medium' for c in updates))
            self.assertTrue(all(32768 <= int(c['cpu_px']) < 1280 * 720 for c in updates))
            verifies = [f for k, f in lines if k == 'verify']
            self.assertTrue(verifies)
            self.assertTrue(all(v['mismatches'] == '0' for v in verifies))

    @unittest.skipUnless(platform.machine() == 'x86_64', 'x86-64 paint stand-ins')
    def test_notification_redraw_avoids_large_ge2d_jobs_without_losing_pixels(self):
        for direction in ('cw', 'ccw'):
            for event in ('A', 'B'):
                with self.subTest(direction=direction, event=event):
                    out, lines = run('partial-' + direction, 'partial', '6',
                                     module='partial-plan3.so', extra={
                                         'RC2FX_TEST_CPU_ROTATE': '1',
                                         'PARTIAL_EVENT_CASE': event,
                                         'FAKE_GE2D_DIR': direction})
                    self.assertIn('bad_frames 0', out)
                    conversions = [f for k, f in lines if k == 'convert']
                    updates = conversions[2:4]
                    self.assertTrue(updates)
                    self.assertTrue(all(c['kind'] == 'partial' and c['job_px'] == '64'
                                        for c in conversions[4:]))
                    if event == 'B':
                        self.assertTrue(all(c['kind'] == 'cpu' and c['reason'] == 'medium'
                                            for c in updates), updates)
                        self.assertTrue(all(c['job_px'] == '256' for c in updates))
                        self.assertTrue(all(c.get('cpu_px') == '34686' for c in updates), updates)
                    else:
                        self.assertTrue(all(c['kind'] == 'partial' for c in updates), updates)

    @unittest.skipUnless(platform.machine() == 'x86_64', 'x86-64 paint stand-ins')
    def test_fixed_cpu_rotation_is_pixel_identical_in_both_directions(self):
        for direction in ('cw', 'ccw'):
            for module in ('partial.so', 'partial-plan3.so'):
                with self.subTest(direction=direction, module=module):
                    out, _ = run('partial-' + direction, 'partial', '60', module=module,
                                 extra={'RC2FX_TEST_CPU_ROTATE': '1',
                                        'FAKE_GE2D_DIR': direction,
                                        'RC2FX_GUI_PACING_QUIET': '1',
                                        'RC2FX_TEST_COUNTERS_EVERY_MS': '50',
                                        'FAKE_GE2D_DELAY_US': '3000'})
                    self.assertIn('bad_frames 0', out)
                    counters = [f for k, f in log_lines() if k == 'counters'][-1]
                    self.assertGreater(int(counters['cpu_frames']), 0)
                    self.assertGreater(int(counters['partial']), 0)
                    self.assertGreater(int(counters['cpu_us']), 0)
                    self.assertEqual(counters['cpu_fail'], '0')
                    for removed in ('mode', 'cpu_share', 'split_frames', 'force_full', 'forced_full'):
                        self.assertNotIn(removed, counters)

    @unittest.skipUnless(platform.machine() == 'x86_64', 'x86-64 paint stand-ins')
    def test_verify_covers_the_cpu_path(self):
        out, _ = run('partial-verify-cw', 'partial', '60', module='partial.so',
                     extra={'RC2FX_TEST_CPU_ROTATE': '1', 'FAKE_GE2D_DELAY_US': '2000'})
        self.assertIn('bad_frames 0', out)
        verifies = [f for k, f in log_lines() if k == 'verify']
        self.assertTrue(verifies)
        self.assertTrue(all(v['mismatches'] == '0' for v in verifies))

    @unittest.skipUnless(platform.machine() == 'x86_64', 'x86-64 paint stand-ins')
    def test_old_setting_files_and_overrides_have_no_effect(self):
        path = Path(BUILD.name) / 'obsolete-screen-setting'
        path.write_text('hardware\\n')
        out, _ = run('partial-cw', 'partial', '60', module='partial.so',
                     extra={'RC2FX_TEST_CPU_ROTATE': '1',
                            'RC2FX_GUI_MODE_FILE': str(path),
                            'RC2FX_GUI_CPU_SHARE_PERCENT': '0',
                            'RC2FX_GUI_CPU_ROTATE_PERCENT': '101',
                            'RC2FX_GUI_FORCE_FULL_FILE': str(path),
                            'RC2FX_GUI_PACING_QUIET': '1',
                            'RC2FX_TEST_COUNTERS_EVERY_MS': '50',
                            'FAKE_GE2D_DELAY_US': '3000'})
        self.assertIn('bad_frames 0', out)
        counters = [f for k, f in log_lines() if k == 'counters'][-1]
        self.assertGreater(int(counters['cpu_frames']), 0)
        self.assertFalse(Path(str(path) + '.active').exists())
        self.assertEqual(path.read_text(), 'hardware\\n')

    def both(self, mode, frames, module, **env):
        """The same paints through GE2D jobs only (the reference) and with the
        CPU path: each run's output and log lines."""
        env = dict(env, PARTIAL_DEST_HASH='1')
        reference = run(mode, 'partial', str(frames), module=module, extra=env)
        cpu = run(mode, 'partial', str(frames), module=module, extra=dict(env, RC2FX_TEST_CPU_ROTATE='1'))
        return reference, cpu

    @staticmethod
    def images(out):
        """Per conversion: its result and mismatch count, and the destination's hash."""
        return [line for line in out.splitlines() if line.startswith(('frame ', 'dest_hash '))]

    @unittest.skipUnless(platform.machine() == 'x86_64', 'x86-64 paint stand-ins')
    def test_the_cpu_path_leaves_the_same_image_as_ge2d_alone(self):
        # Every destination after every conversion, GE2D alone against the CPU
        # path, over meter paints, the captured events, random and clustered
        # paints, and large rectangles on the source's edges, over the token
        # job and at the 32,768-pixel threshold.
        patterns = [
            (40, {}),
            (6, {'PARTIAL_EVENT_CASE': 'A'}),
            (6, {'PARTIAL_EVENT_CASE': 'B'}),
            (12, {'PARTIAL_CAPACITY': '1'}),
            (12, {'PARTIAL_CPU_REGIONS': '1'}),
            (30, {'PARTIAL_RANDOM': '1'}),
            (30, {'PARTIAL_RANDOM': '1', 'PARTIAL_RANDOM_W': '400', 'PARTIAL_RANDOM_H': '300', 'PARTIAL_SEED': '1'}),
            (6, {'PARTIAL_EVENT_CASE': '0:0:1280:26;0:694:1280:26'}),
            (6, {'PARTIAL_EVENT_CASE': '0:0:46:720;1234:0:46:720'}),
            (6, {'PARTIAL_EVENT_CASE': '0:0:200:200;1003:67:7:8'}),
            (6, {'PARTIAL_EVENT_CASE': '100:100:256:128;900:500:10:10'}),
        ] + [(40, {'PARTIAL_CLUSTERS': '1', 'PARTIAL_SEED': str(seed)}) for seed in range(4)]
        # The two-job planner is not the mixer's build: a shorter list for it.
        fewer = [patterns[0], patterns[2], patterns[4], patterns[-1]]
        kinds = set()
        for direction in ('cw', 'ccw'):
            for module in ('partial.so', 'partial-plan3.so'):
                for frames, pattern in (patterns if module == 'partial-plan3.so' else fewer):
                    with self.subTest(direction=direction, module=module, pattern=pattern):
                        (ref_out, ref_lines), (cpu_out, cpu_lines) = self.both(
                            'partial-verify-' + direction, frames, module, FAKE_GE2D_DIR=direction,
                            RC2FX_TEST_PARTIAL_VERIFY_EVERY='1', **pattern)
                        self.assertIn('bad_frames 0', ref_out)
                        self.assertIn('bad_frames 0', cpu_out)
                        self.assertEqual(len(self.images(ref_out)), 2 * frames)
                        self.assertEqual(self.images(cpu_out), self.images(ref_out))
                        self.assertFalse([f for k, f in ref_lines if k == 'convert' and f['kind'] == 'cpu'],
                                         'the reference never uses the CPU path')
                        for lines in (ref_lines, cpu_lines):
                            self.assertTrue(all(f['mismatches'] == '0' for k, f in lines if k == 'verify'))
                        converts = [f for k, f in cpu_lines if k == 'convert']
                        # On the CPU: one token GE2D job, and a full frame or a
                        # plan with a large job.
                        on_cpu = [c for c in converts if c['kind'] == 'cpu']
                        self.assertGreaterEqual(len(on_cpu), 2, 'the first frame into each destination')
                        self.assertTrue(all(c['jobs'] == '1' and c['job_px'] == '256' and int(c['cpu_px']) >= 32768
                                            for c in on_cpu), on_cpu)
                        # Left to GE2D: plans with no large job.
                        plans = [c for c in converts if c['kind'] == 'partial']
                        self.assertTrue(all(c['cpu_px'] == '0' and max(map(int, c['job_px'].split(','))) < 32768
                                            for c in plans), plans)
                        kinds |= {(c['kind'], c['reason']) for c in converts}
        self.assertLessEqual({('cpu', 'forced'), ('cpu', 'area'), ('cpu', 'medium'), ('partial', '-')}, kinds)

    @unittest.skipUnless(platform.machine() == 'x86_64', 'x86-64 paint stand-ins')
    def test_only_plans_with_a_job_of_32768_pixels_or_more_move_to_the_cpu(self):
        cases = [
            ('100:100:256:128', ('cpu', 'medium', '1', '32768')),               # exactly the threshold
            ('100:100:256:127', ('partial', '-', '1', '0')),                    # one row under it
            ('100:100:64:64;900:500:64:64', ('partial', '-', '2', '0')),        # small jobs stay on GE2D
            ('100:100:256:127;900:500:64:64', ('partial', '-', '2', '0')),      # 36,608 in all, no large job
            ('0:0:200:200;900:500:10:10', ('cpu', 'medium', '1', '40100')),     # the whole plan goes with its large job
        ]
        for module in ('partial.so', 'partial-plan3.so'):
            for rects, want in cases:
                with self.subTest(module=module, rects=rects):
                    out, lines = run('partial-cw', 'partial', '6', module=module,
                                     extra={'RC2FX_TEST_CPU_ROTATE': '1', 'PARTIAL_EVENT_CASE': rects})
                    self.assertIn('bad_frames 0', out)
                    updates = [f for k, f in lines if k == 'convert'][2:4]
                    self.assertEqual([(c['kind'], c['reason'], c['jobs'], c['cpu_px']) for c in updates], [want] * 2)

    @unittest.skipUnless(platform.machine() == 'x86_64', 'x86-64 paint stand-ins')
    def test_a_failed_selection_on_any_path_is_a_full_frame_with_consistent_counts(self):
        # Clustered paints mix GE2D plans, large redraws on the CPU and full
        # frames, so the selection changes often. Whichever S_SELECTION fails,
        # that conversion becomes a full GE2D frame and no destination is stale.
        reasons = set()
        for nth in range(3, 20):
            with self.subTest(nth=nth):
                out, lines = run('partial-cw', 'partial', '40', module='partial-plan3.so', extra={
                    'RC2FX_TEST_CPU_ROTATE': '1', 'PARTIAL_CLUSTERS': '1', 'PARTIAL_SEED': '2',
                    'FAKE_GE2D_FAIL_SELECTION': str(nth), 'RC2FX_TEST_VERBOSE_EVERY_MS': '1'})
                self.assertIn('bad_frames 0', out)
                counters = [f for k, f in lines if k == 'counters'][-1]
                self.assertEqual(int(counters['partial']) + int(counters['full']), int(counters['conversions']))
                failed = [f for k, f in lines if k == 'convert' and 'selection' in f['reason']]
                self.assertEqual([c['kind'] for c in failed], ['full'])
                reasons.add(failed[0]['reason'])
        # A plan's last job or the CPU path's token job; an extra job.
        self.assertEqual(reasons, {'selection', 'selection-or-qbuf'})

    @unittest.skipUnless(platform.machine() == 'x86_64', 'x86-64 paint stand-ins')
    def test_the_kill_file_ends_cpu_rotation(self):
        kill = Path(BUILD.name) / 'partial-off'
        kill.unlink(missing_ok=True)
        try:
            out, lines = run('partial-cw', 'partial', '20', module='partial-plan3.so', extra={
                'RC2FX_TEST_CPU_ROTATE': '1', 'PARTIAL_CPU_REGIONS': '1', 'PARTIAL_KILL_AT': '10',
                'PARTIAL_KILL_FILE': str(kill)})
        finally:
            kill.unlink(missing_ok=True)
        self.assertIn('bad_frames 0', out)
        converts = [(f['kind'], f['reason']) for k, f in lines if k == 'convert']
        self.assertEqual(converts[:10], [('cpu', 'forced')] * 2 + [('cpu', 'medium')] * 8)
        self.assertEqual(set(converts[10:]), {('full', 'kill')}, 'GE2D full frames, as RØDE converts them')

    @unittest.skipUnless(platform.machine() == 'x86_64', 'x86-64 paint stand-ins')
    def test_the_quiet_cpu_path_leaves_the_ge2d_image(self):
        # Quiet mode is how the mixer runs: no convert lines, only counters.
        quiet = {'RC2FX_GUI_PACING_QUIET': '1', 'RC2FX_TEST_COUNTERS_EVERY_MS': '1'}
        for direction in ('cw', 'ccw'):
            for seed in ('5', '6'):
                with self.subTest(direction=direction, seed=seed):
                    (ref_out, ref_lines), (cpu_out, cpu_lines) = self.both(
                        'partial-' + direction, 30, 'partial-plan3.so', FAKE_GE2D_DIR=direction,
                        PARTIAL_CLUSTERS='1', PARTIAL_SEED=seed, **quiet)
                    self.assertIn('bad_frames 0', cpu_out)
                    self.assertEqual(self.images(cpu_out), self.images(ref_out))
                    self.assertEqual(len(self.images(cpu_out)), 60)
                    for lines in (ref_lines, cpu_lines):
                        self.assertFalse([k for k, _ in lines
                                          if k in ('convert', 'paint', 'verify') or k.startswith('dirty-')])
                    ref = [f for k, f in ref_lines if k == 'counters'][-1]
                    cpu = [f for k, f in cpu_lines if k == 'counters'][-1]
                    self.assertEqual(ref['cpu_frames'], '0')
                    # More than the two first frames: large redraws too.
                    self.assertGreater(int(cpu['cpu_frames']), 2)
                    self.assertEqual((cpu['cpu_fail'], cpu['disabled']), ('0', '-'))
                    self.assertEqual((cpu['partial'], cpu['full_by_reason']), (ref['partial'], ref['full_by_reason']))

    def test_production_has_no_mode_or_benchmark_controls(self):
        data = (Path(BUILD.name) / 'partial-plan3-production.so').read_bytes()
        for knob in (b'RC2FX_GUI_MODE_FILE', b'RC2FX_GUI_CPU_SHARE_PERCENT',
                     b'RC2FX_GUI_CPU_ROTATE_PERCENT', b'RC2FX_GUI_FORCE_FULL_FILE',
                     b'RC2FX_TEST_FORCE_FULL_FILE', b'RC2FX_TEST_CPU_ROTATE',
                     b'screen-conversion.active', b'gui-force-full'):
            self.assertNotIn(knob, data)


def tool(*names):
    """The first of these programs that is on PATH, or None."""
    for name in names:
        if shutil.which(name):
            return shutil.which(name)
    return None


ZIG = tool('zig')
QEMU = tool('qemu-aarch64', 'qemu-aarch64-static')
A64 = ['-target', 'aarch64-linux-musl', '-mcpu=cortex_a53', '-O2', '-static']


class ProductionPlanTests(unittest.TestCase):
    """gui_partial.c as the mixer builds it, driven by partial_plan_test.c: no
    RC2FX_PACING_TEST, so the CPU rule is always on. The program checks every
    destination against the rotated source after every conversion and exits
    non-zero on a difference. It needs no paint stand-ins, so it runs on any
    Linux host, and under qemu-aarch64 with the NEON rotation."""

    WARN = ['-O2', '-Wall', '-Wextra', '-Werror', '-Wno-unused-function']
    BUILDS = {'plan3': ['-DRC2FX_GUI_RECT_PLAN3'], 'baseline': []}

    @classmethod
    def setUpClass(cls):
        for name, flags in cls.BUILDS.items():
            subprocess.run([CC, *cls.WARN, *flags, str(HERE / 'partial_plan_test.c'),
                            '-o', str(Path(BUILD.name) / f'plan-{name}')], check=True)

    def plan(self, build, *args, runner=()):
        """One run: exit status, the per-conversion lines, and the totals."""
        r = subprocess.run([*runner, str(Path(BUILD.name) / f'plan-{build}'), *args], capture_output=True, text=True,
                           timeout=300)
        lines = r.stdout.splitlines()
        frames = [line for line in lines if line.startswith('frame ')]
        totals = {name: int(value) for line in lines if not line.startswith(('frame ', 'counters '))
                  for name, value in re.findall(r'(\w+) (\d+)', line)}
        return r.returncode, frames, totals, r.stdout + r.stderr

    def test_every_conversion_leaves_the_rotated_source(self):
        for planner in ('plan3', 'baseline'):
            for direction, seed in (('cw', '1'), ('ccw', '2'), ('cw', '3'), ('ccw', '4')):
                with self.subTest(planner=planner, direction=direction, seed=seed):
                    status, frames, totals, out = self.plan(planner, direction, seed, '250')
                    self.assertEqual(status, 0, out)
                    self.assertEqual(len(frames), 250)
                    self.assertEqual((totals['bad'], totals['unchecked'], totals['cpu_fail']), (0, 0, 0))
                    # Large redraws on the CPU, meter updates as GE2D jobs, and full frames.
                    self.assertGreater(totals['medium'], 5)
                    self.assertGreater(totals['ge2d_plan'], 50)
                    self.assertGreater(totals['full'], 5)
                    self.assertGreater(totals['extra_jobs'], 20)
                    # The three-job planner has no full frame that GE2D
                    # converts unless something failed.
                    if planner == 'plan3':
                        self.assertEqual(totals['cpu_frames'], totals['medium'] + totals['full'])

    def test_a_failed_token_job_or_selection_leaves_no_stale_destination(self):
        # The program fails one output QBUF or one S_SELECTION. A CPU
        # conversion whose token QBUF failed must write nothing, and every
        # destination must be right at its next conversion.
        failed_tokens = 0
        for direction in ('cw', 'ccw'):
            for what, nths in (('fail-qbuf', (1, 2, 3, 10, 13, 25, 44)), ('fail-selection', (1, 2, 5, 13, 34))):
                for nth in nths:
                    with self.subTest(direction=direction, what=what, nth=nth):
                        status, frames, totals, out = self.plan('plan3', direction, '7', '80', what, str(nth))
                        self.assertEqual(status, 0, out)
                        self.assertEqual(totals['bad'], 0)
                        failed_tokens += sum('result -1 kind cpu' in line for line in frames)
        self.assertGreaterEqual(failed_tokens, 4, 'CPU conversions met a failed token QBUF')

    @unittest.skipIf(platform.machine() == 'aarch64', 'this host runs the NEON path in the tests above')
    @unittest.skipUnless(ZIG and QEMU, 'the NEON path needs zig and qemu-aarch64 (or an aarch64 host)')
    def test_the_neon_rotation_under_qemu_gives_the_same_images(self):
        # The mixer's CPU (Cortex-A53). The planner and the strip loop are the
        # real ones; rotate_rect is the NEON kernel with its scalar edges.
        qemu = (QEMU, '-cpu', 'cortex-a53')
        program = Path(BUILD.name) / 'plan-plan3-aarch64'
        subprocess.run([ZIG, 'cc', *A64, *self.WARN[1:], '-DRC2FX_GUI_RECT_PLAN3', str(HERE / 'partial_plan_test.c'),
                        '-o', str(program)], check=True)
        for direction, seed in (('cw', '1'), ('ccw', '2')):
            with self.subTest(direction=direction, seed=seed):
                status, frames, totals, out = self.plan('plan3-aarch64', direction, seed, '250', runner=qemu)
                self.assertEqual(status, 0, out)
                self.assertGreater(totals['medium'], 5)
                host_status, host_frames, _, _ = self.plan('plan3', direction, seed, '250')
                self.assertEqual(host_status, 0)
                self.assertEqual(frames, host_frames, 'the same paths and the same images as the scalar host run')
        for direction, what, nth in (('cw', 'fail-qbuf', '10'), ('ccw', 'fail-qbuf', '25')):
            with self.subTest(direction=direction, what=what, nth=nth):
                status, frames, totals, out = self.plan('plan3-aarch64', direction, '7', '80', what, nth, runner=qemu)
                self.assertEqual(status, 0, out)
        # rotate_rect against its scalar reference, whole and in strips.
        rotate = Path(BUILD.name) / 'test_rotate-aarch64'
        subprocess.run([ZIG, 'cc', *A64, '-Wall', '-Wextra', '-Werror', str(HERE / 'rotate' / 'test_rotate.c'),
                        str(HERE / 'rotate' / 'rotate.c'), '-o', str(rotate)], check=True)
        r = subprocess.run([*qemu, str(rotate)], capture_output=True, text=True, timeout=600)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertRegex(r.stdout, r'^\d{4,} cases, 0 failures$')


class QuietAndMergeTests(unittest.TestCase):
    QUIET = {'RC2FX_GUI_PACING_QUIET': '1'}

    @unittest.skipUnless(platform.machine() == 'x86_64', 'the paint frames are x86-64 stand-ins')
    def test_quiet_partial_logs_start_first_pass_and_periodic_counters_only(self):
        # 200 conversions with two 6 ms jobs each: more than 2 s, counters every 500 ms.
        out, _ = run('partial-cw', 'partial', '200', module='partial.so',
                     extra=dict(self.QUIET, FAKE_GE2D_DELAY_US='6000', RC2FX_TEST_COUNTERS_EVERY_MS='500'))
        self.assertIn('bad_frames 0', out)
        lines = [x for x in log_lines() if x[0] != 'sha-ok']
        kinds = [k for k, _ in lines]
        self.assertEqual(kinds[:2], ['start', 'first-pass'])
        self.assertEqual(set(kinds[2:]), {'counters'}, kinds)
        self.assertGreaterEqual(kinds.count('counters'), 3)
        self.assertEqual((lines[0][1]['merge_px'], lines[0][1]['quiet']), ('80000', '1'))
        last = lines[-1][1]
        self.assertEqual(int(last['partial']) + int(last['full']), int(last['conversions']))
        self.assertGreater(int(last['partial']), 100)
        self.assertEqual((last['full'], last['full_by_reason'], last['disabled']), ('2', 'forced:2', '-'))
        self.assertEqual((last['planner_policy'], last['soft_budget_exceeded']), ('baseline', '0'))
        self.assertGreater(int(last['extra_jobs']), 0)
        # Every conversion's vendor job alone takes 6 ms (fake GE2D delay).
        self.assertGreater(int(last['ge2d_us']), int(last['conversions']) * 5000)
        times = [int(f['t']) for k, f in lines if k == 'counters']
        self.assertTrue(all(b - a >= 500_000_000 for a, b in zip(times, times[1:])), 'at most one per interval')

    @unittest.skipUnless(platform.machine() == 'x86_64', 'the paint frames are x86-64 stand-ins')
    def test_periodic_counters_include_effective_planner_settings(self):
        cases = [
            ('partial.so', {}, 'baseline', '80000', '8000000'),
            ('partial-plan3.so', {}, 'plan3', '20000', '8000000'),
            ('partial-plan3.so', {'RC2FX_GUI_PARTIAL_MERGE_PX': '40000',
                                  'RC2FX_TEST_PARTIAL_BUDGET_US': '1234'},
             'plan3', '40000', '1234000'),
        ]
        for module, override, policy, pixels, budget in cases:
            with self.subTest(module=module, override=override):
                out, _ = run('partial-cw', 'partial', '8', module=module,
                             extra=dict(self.QUIET, RC2FX_TEST_COUNTERS_EVERY_MS='1', **override))
                self.assertIn('bad_frames 0', out)
                counters = [f for k, f in log_lines() if k == 'counters']
                self.assertTrue(counters)
                self.assertTrue(all((c['planner_policy'], c['merge_px'], c['budget_ns']) ==
                                    (policy, pixels, budget) for c in counters))

    @unittest.skipUnless(platform.machine() == 'x86_64', 'the paint frames are x86-64 stand-ins')
    def test_quiet_counters_default_to_one_per_minute(self):
        run('partial-cw', 'partial', '40', module='partial.so', extra=self.QUIET)
        self.assertEqual([k for k, _ in log_lines() if k != 'sha-ok'], ['start', 'first-pass'])

    @unittest.skipUnless(platform.machine() == 'x86_64', 'the paint frames are x86-64 stand-ins')
    def test_quiet_paints_format_nothing_and_still_feed_the_conversion(self):
        # Paints with rectangles outside the source, a long list and rejected
        # lists: quiet mode writes no paint or dirty- line for any of them.
        out, _ = run('observe', 'dirty', module='dirty.so', extra=self.QUIET)
        self.assertIn('dirty done', out)
        self.assertIn('errno_after_paint 55', out, 'the GUI errno is unchanged')
        self.assertEqual([k for k, _ in log_lines() if k != 'sha-ok'], ['start', 'first-pass'])
        # The paint's rectangles still reach the conversion: every destination
        # matches the source, and a rejected paint still forces full frames.
        for module in ('partial.so', 'partial-plan3.so'):
            with self.subTest(module=module):
                out, _ = run('partial-cw', 'partial', '8', module=module,
                             extra=dict(self.QUIET, PARTIAL_INVALID='1', RC2FX_TEST_COUNTERS_EVERY_MS='1'))
                self.assertIn('bad_frames 0', out)
                lines = log_lines()
                self.assertFalse([k for k, _ in lines if k == 'paint' or k.startswith('dirty-')])
                last = [f for k, f in lines if k == 'counters'][-1]
                self.assertEqual(last['full_by_reason'], 'forced:4', 'two at the start, two after the rejected paint')
                self.assertGreater(int(last['partial']), 0)

    @unittest.skipUnless(platform.machine() == 'x86_64', 'the paint frames are x86-64 stand-ins')
    def test_quiet_logs_the_disable_once_with_its_reason(self):
        kill = Path(BUILD.name) / 'partial-off'
        kill.unlink(missing_ok=True)
        try:
            out, _ = run('partial-cw', 'partial', '20', module='partial.so',
                         extra=dict(self.QUIET, PARTIAL_KILL_AT='10', PARTIAL_KILL_FILE=str(kill),
                                    RC2FX_TEST_COUNTERS_EVERY_MS='1'))
        finally:
            kill.unlink(missing_ok=True)
        self.assertIn('bad_frames 0', out)
        lines = log_lines()
        disables = [f for k, f in lines if k == 'disable']
        self.assertEqual([(d['reason'], d['conversions']) for d in disables], [('kill', '11')])
        self.assertFalse([k for k, _ in lines if k in ('fallback', 'convert', 'frame', 'paint', 'stats', 'exit')])
        last = [f for k, f in lines if k == 'counters'][-1]
        self.assertEqual(last['disabled'], 'kill')
        self.assertIn('kill:', last['full_by_reason'])

    @unittest.skipUnless(platform.machine() == 'x86_64', 'the paint frames are x86-64 stand-ins')
    def test_quiet_logs_each_transient_fallback_and_job_failure(self):
        run('partial-cw', 'partial', '10', module='partial.so', extra=dict(self.QUIET, FAKE_GE2D_FAIL_QBUF='3'))
        events = [(k, f['reason']) for k, f in log_lines() if k in ('fallback', 'disable')]
        self.assertEqual(events, [('fallback', 'selection-or-qbuf')])
        # A budget overrun merges the rest: a partial conversion, no fallback line.
        run('partial-cw', 'partial', '20', module='partial.so',
            extra=dict(self.QUIET, FAKE_GE2D_DELAY_US='2000', RC2FX_TEST_PARTIAL_BUDGET_US='1000',
                       RC2FX_GUI_PARTIAL_MERGE_PX='1'))
        events = {(k, f['reason']) for k, f in log_lines() if k in ('fallback', 'disable')}
        self.assertEqual(events, set())
        # Only a box over half the frame is a full-frame fallback, with its line.
        run('partial-cw', 'partial', '20', module='partial.so',
            extra=dict(self.QUIET, FAKE_GE2D_DELAY_US='2000', RC2FX_TEST_PARTIAL_BUDGET_US='1000',
                       RC2FX_GUI_PARTIAL_MERGE_PX='1', PARTIAL_WIDE='1'))
        events = {(k, f['reason']) for k, f in log_lines() if k in ('fallback', 'disable')}
        self.assertEqual(events, {('fallback', 'budget')})
        run('partial-cw', 'partial', '10', module='partial.so', extra=dict(self.QUIET, FAKE_GE2D_FAIL_DQBUF='3'))
        events = [(k, f['reason']) for k, f in log_lines() if k in ('fallback', 'disable')]
        self.assertEqual(events, [('disable', 'job-failed')], 'the disable once, then no fallback lines')

    @unittest.skipUnless(platform.machine() == 'x86_64', 'the paint frames are x86-64 stand-ins')
    def test_quiet_counters_report_budget_merged_conversions(self):
        out, _ = run('partial-cw', 'partial', '20', module='partial.so',
                     extra=dict(self.QUIET, FAKE_GE2D_DELAY_US='2000', RC2FX_TEST_PARTIAL_BUDGET_US='1000',
                                RC2FX_GUI_PARTIAL_MERGE_PX='1', RC2FX_TEST_COUNTERS_EVERY_MS='1'))
        self.assertIn('bad_frames 0', out)
        last = [f for k, f in log_lines() if k == 'counters'][-1]
        self.assertGreater(int(last['budget_merged']), 0)
        self.assertLessEqual(int(last['budget_merged']), int(last['partial']), 'counted as partial')
        self.assertNotIn('budget', last['full_by_reason'])
        # Without slow jobs nothing is merged for time.
        run('partial-cw', 'partial', '20', module='partial.so',
            extra=dict(self.QUIET, RC2FX_TEST_COUNTERS_EVERY_MS='1'))
        last = [f for k, f in log_lines() if k == 'counters'][-1]
        self.assertEqual(last['budget_merged'], '0')

    @unittest.skipUnless(platform.machine() == 'x86_64', 'the paint frames are x86-64 stand-ins')
    def test_quiet_verify_failure_is_one_disable_line(self):
        run('partial-verify-ccw', 'partial', '12', module='partial.so', extra=dict(self.QUIET, FAKE_GE2D_DIR='cw'))
        lines = log_lines()
        self.assertEqual([(k, f['reason']) for k, f in lines if k in ('fallback', 'disable')],
                         [('disable', 'verify-fail')])
        self.assertFalse([k for k, _ in lines if k in ('verify', 'verify-skip')])

    def test_quiet_in_the_other_builds_drops_frame_and_stats_lines(self):
        for module in ('pacing.so',) + (('dirty.so',) if platform.machine() == 'x86_64' else ()):
            with self.subTest(module):
                run('observe', 'frames', '4', module=module, extra=self.QUIET)
                kinds = [k for k, _ in log_lines() if k != 'sha-ok']
                self.assertEqual(kinds, ['start', 'first-pass'])
                run('observe', 'pump', '6', '0', module=module,
                    extra=dict(self.QUIET, RC2FX_TEST_COUNTERS_EVERY_MS='0'))
                counters = [f for k, f in log_lines() if k == 'counters']
                self.assertEqual(counters[-1]['passes'], '6')

    def test_quiet_other_values_keep_the_full_log(self):
        for value in ('0', 'yes', ''):
            with self.subTest(value):
                run('observe', 'frames', '4', extra={'RC2FX_GUI_PACING_QUIET': value})
                kinds = [k for k, _ in log_lines()]
                self.assertEqual(kinds.count('frame'), 4)
                self.assertEqual(kinds[-1], 'exit')

    @unittest.skipUnless(platform.machine() == 'x86_64', 'the paint frames are x86-64 stand-ins')
    def test_the_merge_override_is_used_and_shown_on_the_start_line(self):
        for value, effective in (('0', '0'), ('1000000', '1000000'), ('0001234', '1234'), ('80000', '80000')):
            with self.subTest(value):
                out, _ = run('partial-cw', 'partial', '12', module='partial.so',
                             extra={'RC2FX_GUI_PARTIAL_MERGE_PX': value})
                self.assertIn('bad_frames 0', out)
                lines = log_lines()
                start = [f for k, f in lines if k == 'start'][0]
                self.assertEqual((start['merge_px'], start['quiet']), (effective, '0'))
                self.assertFalse([k for k, _ in lines if k == 'merge-px-invalid'])

    @unittest.skipUnless(platform.machine() == 'x86_64', 'the paint frames are x86-64 stand-ins')
    def test_the_merge_override_changes_the_jobs(self):
        # merge 0: only overlapping rectangles merge, so more jobs than the default.
        jobs = {}
        for value in ('0', '1000000'):
            run('partial-cw', 'partial', '30', module='partial.so', extra={'RC2FX_GUI_PARTIAL_MERGE_PX': value})
            jobs[value] = sum(int(f['jobs']) for k, f in log_lines() if k == 'convert' and f['kind'] == 'partial')
        self.assertGreater(jobs['0'], jobs['1000000'])

    @unittest.skipUnless(platform.machine() == 'x86_64', 'the paint frames are x86-64 stand-ins')
    def test_an_invalid_merge_override_keeps_the_default_and_is_logged_once(self):
        for value, shown in (('1000001', '1000001'), ('-1', '-1'), ('', '-'), ('12a', '12a'), (' 5', '?5'),
                             ('99999999999999999999', '99999999999999999999'), ('1e5', '1e5'), ('+5', '+5'),
                             ('5 6', '5?6')):
            with self.subTest(value):
                out, _ = run('partial-cw', 'partial', '8', module='partial.so',
                             extra=dict(self.QUIET, RC2FX_GUI_PARTIAL_MERGE_PX=value))
                self.assertIn('bad_frames 0', out)
                lines = log_lines()
                invalid = [f for k, f in lines if k == 'merge-px-invalid']
                self.assertEqual(len(invalid), 1)
                self.assertEqual((invalid[0]['value'], invalid[0]['using']), (shown, '80000'))
                start = [f for k, f in lines if k == 'start'][0]
                self.assertEqual(start['merge_px'], '80000')


# ---- the counters line's plan-shape and size fields -------------------------

PATHS = ('path_ge2d_plan', 'path_cpu_medium', 'path_cpu_full', 'path_ge2d_full')
PX_EDGES = (1024, 2048, 4096, 8192, 16384, 32768)
PLAN_SMALL_PX = 8192  # plan_multi_le8192 counts the GE2D plans of two or more jobs up to this many pixels
FULL_PX = 1280 * 720
NEW_FIELDS = PATHS + (
    'plan_jobs', 'plan_px_hist', 'plan_multi_le8192', 'plan_empty', 'ge2d_px', 'cpu_px', 'merge_added_px',
    'cpu_full_frames', 'cpu_full_us', 'cpu_medium_frames', 'cpu_medium_us',
    'cpu_stage_us', 'cpu_rotate_us', 'cpu_yield_us', 'paints', 'paint_rejects')
OLD_FIELDS = ('pid', 't', 'passes', 'conversions', 'partial', 'full', 'full_by_reason', 'budget_merged',
              'planner_policy', 'merge_px', 'budget_ns', 'soft_budget_exceeded', 'extra_jobs', 'ge2d_us', 'disabled',
              'cpu_frames', 'cpu_us', 'cpu_max_us', 'cpu_fail', 'log_rotations', 'log_dropped', 'log_reopen_failed')


def px_bucket(px):
    """The plan_px_hist bucket of a plan of `px` pixels: up to each edge, then above."""
    return sum(px > edge for edge in PX_EDGES)


def numbers(counters, key):
    return [int(x) for x in counters[key].split(',')]


def check_counters(test, c):
    """What must hold in every counters line of a conversion build."""
    def n(key):
        return int(c[key])
    plan_jobs, hist = numbers(c, 'plan_jobs'), numbers(c, 'plan_px_hist')
    # Every conversion took exactly one path.
    test.assertEqual(sum(n(path) for path in PATHS), n('conversions'), c)
    test.assertEqual(n('path_ge2d_plan') + n('path_cpu_medium'), n('partial'), c)
    test.assertEqual(n('path_cpu_full') + n('path_ge2d_full'), n('full'), c)
    # Every GE2D plan has one job count and one size.
    test.assertEqual(len(plan_jobs), 3 if c['planner_policy'] == 'plan3' else 4, c)
    test.assertEqual(sum(plan_jobs), n('path_ge2d_plan'), c)
    test.assertEqual(len(hist), len(PX_EDGES) + 1, c)
    test.assertEqual(sum(hist), n('path_ge2d_plan'), c)
    test.assertLessEqual(n('plan_multi_le8192'), min(sum(plan_jobs[1:]), sum(hist[:4])), c)
    test.assertLessEqual(n('plan_empty'), min(plan_jobs[0], hist[0]), c)
    # Completed CPU rotations by kind, and their time.
    kinds = ('full', 'medium')
    test.assertEqual(sum(n(f'cpu_{kind}_frames') for kind in kinds), n('cpu_frames'), c)
    for kind in kinds:
        test.assertLessEqual(n(f'cpu_{kind}_frames'), n(f'path_cpu_{kind}'), c)
        if not n(f'cpu_{kind}_frames'):
            test.assertEqual(n(f'cpu_{kind}_us'), 0, c)
    by_kind = sum(n(f'cpu_{kind}_us') for kind in kinds)
    test.assertTrue(n('cpu_us') - 1 <= by_kind <= n('cpu_us'), c)  # each is rounded down to a microsecond
    # The three stages are all of a rotation but its loop overhead.
    stages = n('cpu_stage_us') + n('cpu_rotate_us') + n('cpu_yield_us')
    test.assertLessEqual(stages, n('cpu_us'), c)
    test.assertGreaterEqual(stages, n('cpu_us') - n('cpu_us') // 50 - 10, c)
    if not n('cpu_frames'):
        test.assertEqual(n('cpu_px'), 0, c)
    test.assertLessEqual(n('paint_rejects'), n('paints'), c)


def check_against_converts(test, c, converts, clean):
    """A counters line against the convert lines of the conversions it covers."""
    def n(key):
        return int(c[key])
    plans = [f for f in converts if f['kind'] == 'partial']
    cpu = [f for f in converts if f['kind'] == 'cpu']
    test.assertEqual(n('conversions'), len(converts))
    test.assertEqual(n('path_ge2d_plan'), len(plans))
    test.assertEqual(n('path_cpu_medium'), sum(f['reason'] == 'medium' for f in cpu))
    test.assertEqual(n('path_cpu_full'), sum(f['reason'] != 'medium' for f in cpu))
    test.assertEqual(n('path_ge2d_full'), sum(f['kind'] == 'full' for f in converts))
    jobs = [0] * len(numbers(c, 'plan_jobs'))
    hist = [0] * (len(PX_EDGES) + 1)
    for f in plans:
        jobs[int(f['jobs']) - 1] += 1
        hist[px_bucket(int(f['px']))] += 1
    test.assertEqual(numbers(c, 'plan_jobs'), jobs)
    test.assertEqual(numbers(c, 'plan_px_hist'), hist)
    test.assertEqual(n('plan_multi_le8192'),
                     sum(int(f['jobs']) >= 2 and int(f['px']) <= PLAN_SMALL_PX for f in plans))
    test.assertLessEqual(n('plan_empty'), sum(f['job_px'] == '64' for f in plans))
    test.assertEqual(n('cpu_px'), sum(int(f['cpu_px']) for f in converts))
    if clean:  # every job was accepted
        test.assertEqual(n('ge2d_px'), sum(int(f['px']) for f in converts))
        test.assertEqual(n('extra_jobs'), sum(int(f['jobs']) - 1 for f in plans))
        test.assertEqual(n('cpu_frames'), len(cpu))


def model_merge(rects, merge_px, cap):
    """The planner's greedy merge, written again here: the jobs it leaves and
    the pixels its merges add (bounding box less the two rectangles, where
    that is positive)."""
    def box(a, b):
        x0, y0 = min(a[0], b[0]), min(a[1], b[1])
        x1, y1 = max(a[0] + a[2], b[0] + b[2]), max(a[1] + a[3], b[1] + b[3])
        return (x0, y0, x1 - x0, y1 - y0)

    def area(r):
        return r[2] * r[3]
    r, added = [tuple(x) for x in rects], 0
    while len(r) > 1:
        best = None
        for i in range(len(r)):
            for j in range(i + 1, len(r)):
                extra = area(box(r[i], r[j])) - area(r[i]) - area(r[j])
                if best is None or extra < best[0]:
                    best = (extra, i, j)
        if best[0] >= merge_px and len(r) <= cap:
            break
        added += max(best[0], 0)
        r[best[1]] = box(r[best[1]], r[best[2]])
        r[best[2]] = r[-1]
        r.pop()
    return r, added


EVENT_A = [(167, 362, 28, 3), (167, 341, 10, 7), (185, 341, 10, 7), (411, 455, 28, 9), (411, 444, 10, 8),
           (429, 444, 10, 8), (1010, 45, 3, 30), (1018, 45, 7, 8), (1019, 67, 7, 8), (167, 365, 28, 3),
           (167, 342, 10, 8), (185, 342, 10, 8), (411, 463, 28, 9), (411, 446, 10, 8), (429, 446, 10, 7),
           (1010, 45, 2, 30)]
EVENT_B = [(131, 0, 274, 120), (411, 466, 28, 20), (411, 453, 10, 8), (429, 453, 10, 7), (968, 45, 7, 30),
           (979, 45, 7, 8), (979, 67, 8, 8), (411, 449, 28, 26), (411, 444, 10, 5), (429, 444, 10, 5),
           (972, 45, 9, 30), (981, 45, 8, 8), (981, 67, 8, 8)]


def event_rects(event):
    if event in ('A', 'B'):
        return EVENT_A if event == 'A' else EVENT_B
    return [tuple(int(v) for v in rect.split(':')) for rect in event.split(';')]


def fake_totals(out):
    """The fake GE2D's own count of the jobs it ran and the pixels it wrote."""
    summary = [line.split() for line in out.splitlines() if line.startswith('partial frames')][0]
    return int(summary[summary.index('jobs') + 1]), int(summary[summary.index('pixels') + 1])


def ge2d_jobs(c):
    """The GE2D jobs a counters line accounts for when none failed: each
    plan's jobs, and one for every other conversion."""
    return (sum((i + 1) * count for i, count in enumerate(numbers(c, 'plan_jobs'))) +
            sum(int(c[path]) for path in PATHS[1:]))


AT_EXIT = {'RC2FX_TEST_COUNTERS_AT_EXIT': '1'}
QUIET_EXIT = dict(AT_EXIT, RC2FX_GUI_PACING_QUIET='1')


@unittest.skipUnless(platform.machine() == 'x86_64', 'the paint frames are x86-64 stand-ins')
class CounterTests(unittest.TestCase):
    """The plan-shape and size fields at the end of the counters line, in
    quiet mode as the mixer runs. RC2FX_TEST_COUNTERS_AT_EXIT adds one more
    counters line when the program exits, so the totals cover every
    conversion and can be compared with the fake GE2D's own totals."""

    def last(self, lines):
        counters = [f for k, f in lines if k == 'counters']
        self.assertTrue(counters, 'a counters line was written')
        return counters[-1]

    def test_the_new_fields_follow_the_earlier_ones_in_a_fixed_order(self):
        for module in ('partial.so', 'partial-plan3.so'):
            with self.subTest(module=module):
                run('partial-cw', 'partial', '8', module=module,
                    extra=dict(QUIET_EXIT, RC2FX_TEST_COUNTERS_EVERY_MS='1'))
                lines = [line for line in log_file().read_text().splitlines() if line.startswith('counters ')]
                self.assertGreaterEqual(len(lines), 8, 'before each pass after the first, and at exit')
                for line in lines:
                    self.assertEqual(tuple(f.split('=', 1)[0] for f in line.split()[1:]), OLD_FIELDS + NEW_FIELDS)
                    for field in line.split()[1:]:
                        key, value = field.split('=', 1)
                        if key not in ('full_by_reason', 'planner_policy', 'disabled'):
                            self.assertRegex(value, r'^\d+(,\d+)*$', field)
                # No other line kind appeared in quiet mode.
                self.assertEqual({k for k, _ in log_lines()}, {'sha-ok', 'start', 'first-pass', 'counters'})

    def test_every_counters_line_keeps_the_sums_in_quiet_mode(self):
        # Meter paints, the captured events, clustered and random paints,
        # large redraws and rejected paints; GE2D alone and with the CPU rule.
        patterns = [(40, {}), (6, {'PARTIAL_EVENT_CASE': 'A'}), (6, {'PARTIAL_EVENT_CASE': 'B'}),
                    (12, {'PARTIAL_CAPACITY': '1'}), (12, {'PARTIAL_CPU_REGIONS': '1'}), (30, {'PARTIAL_RANDOM': '1'}),
                    (10, {'PARTIAL_INVALID': '1'})]
        patterns += [(40, {'PARTIAL_CLUSTERS': '1', 'PARTIAL_SEED': str(seed)}) for seed in range(3)]
        paths = {path: 0 for path in PATHS}
        for module in ('partial.so', 'partial-plan3.so'):
            for cpu in ({}, {'RC2FX_TEST_CPU_ROTATE': '1'}):
                for frames, pattern in patterns:
                    with self.subTest(module=module, cpu=cpu, pattern=pattern):
                        out, lines = run('partial-ccw', 'partial', str(frames), module=module, extra=dict(
                            QUIET_EXIT, RC2FX_TEST_COUNTERS_EVERY_MS='1', FAKE_GE2D_DIR='ccw', **cpu, **pattern))
                        self.assertIn('bad_frames 0', out)
                        counters = [f for k, f in lines if k == 'counters']
                        # One before each pass after the first and one at exit; one before
                        # the first pass too when the program took a millisecond to reach it.
                        self.assertIn(len(counters), (frames, frames + 1))
                        for c in counters:
                            check_counters(self, c)
                        last = counters[-1]
                        self.assertEqual(int(last['conversions']), frames)
                        # The fake GE2D ran the jobs and wrote the pixels the line reports.
                        self.assertEqual((ge2d_jobs(last), int(last['ge2d_px'])), fake_totals(out))
                        self.assertEqual(int(last['extra_jobs']),
                                         sum(i * count for i, count in enumerate(numbers(last, 'plan_jobs'))))
                        self.assertEqual((last['cpu_fail'], last['disabled']), ('0', '-'))
                        if not cpu:
                            self.assertEqual((last['cpu_px'], last['cpu_frames']), ('0', '0'))
                        for path in PATHS:
                            paths[path] += int(last[path])
        for path in PATHS:
            self.assertGreater(paths[path], 20, path)

    def test_known_plans_give_the_expected_counts_and_pixels(self):
        # One paint of the listed rectangles at frame 2, six frames: two full
        # frames first, the plan into each destination, then two conversions
        # with nothing pending (an 8x8 token job each).
        cases = [  # rectangles, the plan's jobs, the plan's pixels
            ('100:100:32:32', 1, 1024), ('100:100:41:25', 1, 1025),
            ('100:100:64:32', 1, 2048), ('100:10:3:683', 1, 2049),
            ('100:100:64:64', 1, 4096), ('100:100:17:241', 1, 4097),
            ('100:100:128:64', 1, 8192), ('100:100:64:64;900:500:64:64', 2, 8192),
            ('100:100:64:64;900:500:64:65', 2, 8256),
            ('100:100:128:128', 1, 16384), ('100:100:145:113', 1, 16385),
            ('100:100:256:127', 1, 32512), ('100:100:256:128', 1, 32768), ('100:100:99:331', 1, 32769),
            ('100:100:10:10;120:100:10:10', 1, 300),    # merged: the box adds 100 pixels
            ('8:8:30:30;8:8:30:30', 1, 900),             # the same rectangle twice: nothing added
            ('100:100:20:20;110:110:20:20', 1, 900),     # overlapping: the box is 100 over the two
            ('A', 3, 2020), ('B', 3, 34686),
        ]
        for event, jobs, px in cases:
            rects = event_rects(event)
            model, added = model_merge(rects, 20000, 3)
            self.assertEqual((len(model), sum(r[2] * r[3] for r in model)), (jobs, px), 'the model agrees')
            large = any(r[2] * r[3] >= 32768 for r in model)
            for cpu in (False, True):
                with self.subTest(event=event, cpu=cpu):
                    out, lines = run('partial-cw', 'partial', '6', module='partial-plan3.so', extra=dict(
                        QUIET_EXIT, PARTIAL_EVENT_CASE=event, RC2FX_TEST_PLAN_LOG='1',
                        **({'RC2FX_TEST_CPU_ROTATE': '1'} if cpu else {})))
                    self.assertIn('bad_frames 0', out)
                    plans = [f for k, f in lines if k == 'test-plan']
                    self.assertEqual({(int(r.split(':')[0]), int(r.split(':')[1]), int(r.split(':')[2]),
                                       int(r.split(':')[3])) for r in plans[0]['rects'].split(';')}, set(model))
                    c = self.last(lines)
                    check_counters(self, c)
                    on_cpu = cpu and large       # the plan itself
                    want = {
                        'conversions': 6, 'partial': 4, 'full': 2,
                        'path_ge2d_plan': 2 if on_cpu else 4,
                        'path_cpu_medium': 2 if on_cpu else 0,
                        'path_cpu_full': 2 if cpu else 0,
                        'path_ge2d_full': 0 if cpu else 2,
                        'plan_multi_le8192': 2 if jobs >= 2 and px <= PLAN_SMALL_PX and not on_cpu else 0,
                        'plan_empty': 2,
                        'ge2d_px': (2 * 256 if cpu else 2 * FULL_PX) + (2 * 256 if on_cpu else 2 * px) + 2 * 64,
                        'cpu_px': (2 * FULL_PX if cpu else 0) + (2 * px if on_cpu else 0),
                        'merge_added_px': 2 * added,
                        'extra_jobs': 0 if on_cpu else 2 * (jobs - 1),
                        'cpu_frames': (2 if cpu else 0) + (2 if on_cpu else 0),
                        'cpu_full_frames': 2 if cpu else 0,
                        'cpu_medium_frames': 2 if on_cpu else 0,
                        'paints': 1, 'paint_rejects': 0, 'cpu_fail': 0,
                    }
                    self.assertEqual({key: int(c[key]) for key in want}, want)
                    plan_jobs, hist = [2, 0, 0], [2, 0, 0, 0, 0, 0, 0]
                    if not on_cpu:
                        plan_jobs[jobs - 1] += 2
                        hist[px_bucket(px)] += 2
                    self.assertEqual((numbers(c, 'plan_jobs'), numbers(c, 'plan_px_hist')), (plan_jobs, hist))
                    self.assertEqual((ge2d_jobs(c), int(c['ge2d_px'])), fake_totals(out))
                    if cpu:
                        self.assertGreater(int(c['cpu_full_us']), 0)
                        self.assertGreater(int(c['cpu_stage_us']) + int(c['cpu_rotate_us']), 0)
        # The cases reach every size bucket, and merged pixels both ways.
        self.assertEqual({px_bucket(px) for _, _, px in cases}, set(range(7)))
        self.assertEqual(px_bucket(64), 0)

    def test_paints_and_rejected_paints_are_counted_in_quiet_mode(self):
        # Eight frames with a paint each, and one rejected paint at frame 4.
        out, lines = run('partial-cw', 'partial', '8', module='partial-plan3.so',
                         extra=dict(QUIET_EXIT, PARTIAL_INVALID='1'))
        self.assertIn('bad_frames 0', out)
        c = self.last(lines)
        check_counters(self, c)
        self.assertEqual((c['paints'], c['paint_rejects'], c['full_by_reason']), ('9', '1', 'forced:4'))
        # No paint at all after the first frames: the counts stay.
        out, lines = run('partial-cw', 'partial', '6', module='partial-plan3.so',
                         extra=dict(QUIET_EXIT, PARTIAL_EVENT_CASE='A'))
        self.assertEqual((self.last(lines)['paints'], self.last(lines)['paint_rejects']), ('1', '0'))

    def test_the_counters_agree_with_the_convert_lines(self):
        # Verbose mode logs each conversion: the totals at exit must be the
        # sums over those lines, with failures injected and without.
        clean = [(40, {}), (12, {'PARTIAL_CPU_REGIONS': '1'}), (30, {'PARTIAL_RANDOM': '1'}),
                 (20, {'FAKE_GE2D_DELAY_US': '2000', 'RC2FX_TEST_PARTIAL_BUDGET_US': '1000',
                       'RC2FX_GUI_PARTIAL_MERGE_PX': '1'})]
        clean += [(40, {'PARTIAL_CLUSTERS': '1', 'PARTIAL_SEED': str(seed)}) for seed in (3, 4)]
        # Not clean: extra jobs ran and then the conversion became a full frame.
        failing = [(10, {'FAKE_GE2D_FAIL_QBUF': '3'}), (10, {'FAKE_GE2D_FAIL_DQBUF': '3'}),
                   (20, {'FAKE_GE2D_DELAY_US': '2000', 'RC2FX_TEST_PARTIAL_BUDGET_US': '1000',
                         'RC2FX_GUI_PARTIAL_MERGE_PX': '1', 'PARTIAL_WIDE': '1'})]
        failing += [(40, {'PARTIAL_CLUSTERS': '1', 'PARTIAL_SEED': '2', 'FAKE_GE2D_FAIL_SELECTION': str(nth)})
                    for nth in (3, 8, 12, 15)]
        for module in ('partial.so', 'partial-plan3.so'):
            for cpu in ({}, {'RC2FX_TEST_CPU_ROTATE': '1'}):
                for frames, pattern in clean + failing:
                    with self.subTest(module=module, cpu=cpu, pattern=pattern):
                        out, lines = run('partial-cw', 'partial', str(frames), module=module,
                                         extra=dict(AT_EXIT, **cpu, **pattern))
                        self.assertIn('bad_frames 0', out)
                        c = self.last(lines)
                        check_counters(self, c)
                        converts = [f for k, f in lines if k == 'convert']
                        check_against_converts(self, c, converts, (frames, pattern) in clean)
                        # Whatever failed, the pixels are those the fake GE2D wrote.
                        self.assertEqual(int(c['ge2d_px']), fake_totals(out)[1])

    def test_a_failed_cpu_conversion_is_counted_in_its_path_but_not_as_a_rotation(self):
        # The token QBUF of the first large redraw fails: the conversion took
        # the CPU path, no rotation completed, and no pixels were counted.
        out, lines = run('partial-cw', 'partial', '8', module='partial-plan3.so', extra=dict(
            QUIET_EXIT, RC2FX_TEST_CPU_ROTATE='1', PARTIAL_EVENT_CASE='B', FAKE_GE2D_FAIL_QBUF='3',
            PARTIAL_RECOVER_QUEUES='1', PARTIAL_REAL_DEST_FDS='1'))
        self.assertIn('failed_qbuf_cpu_writes 0', out)
        c = self.last(lines)
        check_counters(self, c)
        # Frames 0, 1: full. 2: the plan, failed. 3, 4: full again (both refreshed). 5-7: nothing pending.
        self.assertEqual({key: int(c[key]) for key in PATHS + ('cpu_fail', 'cpu_medium_frames', 'cpu_full_frames')},
                         {'path_ge2d_plan': 3, 'path_cpu_medium': 1, 'path_cpu_full': 4, 'path_ge2d_full': 0,
                          'cpu_fail': 1, 'cpu_medium_frames': 0, 'cpu_full_frames': 4})
        self.assertEqual(int(c['cpu_px']), 4 * FULL_PX)
        self.assertEqual(int(c['ge2d_px']), fake_totals(out)[1])
        self.assertEqual(int(c['ge2d_px']), 4 * 256 + 3 * 64)

    def test_the_line_fits_its_buffer_with_every_counter_at_its_largest(self):
        source = (HERE / 'gui_partial.c').read_text()
        size = int(re.search(r'^#define COUNTERS_LINE_MAX (\d+)$', source, re.M).group(1))
        self.assertIn('static char line[COUNTERS_LINE_MAX];', source)
        kill = Path(BUILD.name) / 'partial-off'
        lengths = set()
        for module, values in (('partial.so', 4), ('partial-plan3.so', 3)):
            with self.subTest(module=module):
                kill.touch()
                try:
                    out, _ = run('partial-cw', 'partial', '6', module=module, extra=dict(
                        QUIET_EXIT, RC2FX_TEST_COUNTERS_HUGE='1', RC2FX_TEST_COUNTERS_EVERY_MS='1',
                        RC2FX_GUI_PARTIAL_MERGE_PX='1000000', RC2FX_TEST_PARTIAL_BUDGET_US='18446744073709551'))
                finally:
                    kill.unlink()
                self.assertIn('bad_frames 0', out)
                text = log_file().read_text()
                self.assertTrue(text.endswith('\n'), 'the last line is whole')
                lines = [line for line in text.splitlines() if line.startswith('counters ')]
                self.assertIn(len(lines), (6, 7), 'every counters line was written: none was too long to write')
                for line in lines:
                    fields = dict(f.split('=', 1) for f in line.split()[1:])
                    self.assertEqual(tuple(fields), OLD_FIELDS + NEW_FIELDS)
                    # Counters print as 20 digits, times (ns / 1000) as 17, and every reason is listed.
                    huge = [key for key, value in fields.items() if re.fullmatch(r'1844674\d{13}', value)]
                    times = [key for key, value in fields.items() if re.fullmatch(r'1844674\d{10}', value)]
                    # 12 earlier counters, the test's budget_ns, and 13 of the new fields.
                    self.assertEqual(len(huge), 26, huge)
                    self.assertEqual(sorted(times), sorted(
                        ['ge2d_us', 'cpu_us', 'cpu_max_us', 'cpu_full_us', 'cpu_medium_us',
                         'cpu_stage_us', 'cpu_rotate_us', 'cpu_yield_us']))
                    self.assertEqual(len(fields['budget_ns']), 20)
                    self.assertEqual(len(fields['full_by_reason'].split(',')), 13)
                    self.assertTrue(all(re.fullmatch(r'[a-z-]+:1844674\d{13}', r)
                                        for r in fields['full_by_reason'].split(',')))
                    self.assertTrue(all(re.fullmatch(r'1844674\d{13}', v) for v in
                                        fields['plan_jobs'].split(',') + fields['plan_px_hist'].split(',')))
                    self.assertEqual(len(fields['plan_jobs'].split(',')), values)
                    # The longest values this run does not have: a 10-digit pid, a 20-digit
                    # time and the longest disable reason (17 letters).
                    longest = (len(line) + 1 + (10 - len(fields['pid'])) + (20 - len(fields['t'])) +
                               (17 - len(fields['disabled'])))
                    self.assertLess(longest, size * 3 // 4, 'well inside the buffer')
                    self.assertGreater(longest, 1000, 'longer than the buffer this line had before')
                    lengths.add((longest, module))
                self.assertEqual(fields['disabled'], 'kill', 'the last line, after the kill file was seen')
        # The two-job planner's line is the longest (a fourth plan_jobs value).
        self.assertLess(max(lengths)[0], size // 2 + 256, f'about half the buffer at most: {sorted(lengths)}')
        CounterTests.longest = sorted(lengths)


def plan_run(build, *args, runner=()):
    """One run of a partial_plan_test build: exit status, the per-conversion
    lines, the totals, the module's counters line, and all output."""
    r = subprocess.run([*runner, str(Path(BUILD.name) / f'plan-{build}'), *args], capture_output=True, text=True,
                       timeout=300)
    lines = r.stdout.splitlines()
    frames = [line for line in lines if line.startswith('frame ')]
    totals = {name: int(value) for line in lines if not line.startswith(('frame ', 'counters '))
              for name, value in re.findall(r'(\w+) (\d+)', line)}
    counters = [dict(f.split('=', 1) for f in line.split()[1:]) for line in lines if line.startswith('counters ')]
    return r.returncode, frames, totals, counters[-1] if counters else None, r.stdout + r.stderr


class ProductionCounterTests(unittest.TestCase):
    """The counters line of gui_partial.c as the mixer builds it (the CPU rule
    always on), from partial_plan_test.c, which prints the line after its last
    conversion and keeps its own tallies of the paths and of the stand-in
    GE2D's jobs and pixels."""

    # The fields that hold a clock reading, a duration or the process id.
    TIMED = {'pid', 't', 'ge2d_us', 'cpu_us', 'cpu_max_us', 'cpu_full_us', 'cpu_medium_us', 'cpu_stage_us',
             'cpu_rotate_us', 'cpu_yield_us', 'soft_budget_exceeded'}

    @classmethod
    def setUpClass(cls):
        for name, flags in ProductionPlanTests.BUILDS.items():
            subprocess.run([CC, *ProductionPlanTests.WARN, *flags, str(HERE / 'partial_plan_test.c'),
                            '-o', str(Path(BUILD.name) / f'plan-{name}')], check=True)

    def check(self, totals, c, frames, clean=True):
        check_counters(self, c)
        self.assertEqual(int(c['conversions']), frames)
        self.assertEqual(int(c['path_ge2d_plan']), totals['ge2d_plan'])
        self.assertEqual(int(c['path_cpu_medium']), totals['medium'])
        self.assertEqual(int(c['path_cpu_full']) + int(c['path_ge2d_full']), totals['full'])
        # The stand-in GE2D wrote exactly the pixels the line reports, failures or not.
        self.assertEqual(int(c['ge2d_px']), totals['pixels'])
        self.assertEqual((int(c['cpu_frames']), int(c['cpu_fail'])), (totals['cpu_frames'], totals['cpu_fail']))
        if clean:
            self.assertEqual(ge2d_jobs(c), totals['jobs'])
            self.assertEqual(int(c['extra_jobs']),
                             sum(i * count for i, count in enumerate(numbers(c, 'plan_jobs'))))
            for kind in ('full', 'medium'):
                self.assertEqual(c[f'cpu_{kind}_frames'], c[f'path_cpu_{kind}'])
            if c['planner_policy'] == 'plan3':  # no full frame that GE2D converts unless something failed
                self.assertEqual(c['path_ge2d_full'], '0')

    def test_the_sums_hold_with_the_cpu_rule_always_on(self):
        for planner in ('plan3', 'baseline'):
            for direction, seed in (('cw', '1'), ('ccw', '2'), ('cw', '3'), ('ccw', '4')):
                with self.subTest(planner=planner, direction=direction, seed=seed):
                    status, frames, totals, c, out = plan_run(planner, direction, seed, '250')
                    self.assertEqual(status, 0, out)
                    self.check(totals, c, 250)
                    # All the paths, job counts and most sizes occur.
                    self.assertGreater(int(c['path_cpu_medium']), 5)
                    self.assertGreater(int(c['path_cpu_full']), 5)
                    self.assertTrue(all(count > 0 for count in numbers(c, 'plan_jobs')[:3]), c)
                    self.assertGreaterEqual(sum(count > 0 for count in numbers(c, 'plan_px_hist')), 5)
                    self.assertGreater(int(c['plan_multi_le8192']), 5)
                    self.assertGreater(int(c['cpu_px']), int(c['path_cpu_full']) * FULL_PX)
                    self.assertGreater(int(c['merge_added_px']), 0)
                    self.assertGreater(int(c['paints']), 200)
                    self.assertGreater(int(c['paint_rejects']), 0)
                    for key in ('cpu_full_us', 'cpu_medium_us', 'cpu_stage_us', 'cpu_rotate_us'):
                        self.assertGreater(int(c[key]), 0, key)

    def test_the_sums_hold_when_a_job_or_selection_fails(self):
        for direction in ('cw', 'ccw'):
            for what, nths in (('fail-qbuf', (1, 2, 3, 10, 13, 25, 44)), ('fail-selection', (1, 2, 5, 13, 34))):
                for nth in nths:
                    with self.subTest(direction=direction, what=what, nth=nth):
                        status, frames, totals, c, out = plan_run('plan3', direction, '7', '80', what, str(nth))
                        self.assertEqual(status, 0, out)
                        self.check(totals, c, 80, clean=False)

    def test_the_stage_and_kind_times_are_exact_under_a_known_clock(self):
        # Under the program's own clock each strip of a CPU rotation is 1 ms of
        # staging, 10 ms of rotation and 100 ms inside the yield, and a
        # rotation lasts 1 ms more than its strips. A full frame is 45 strips.
        for direction, seed in (('cw', '1'), ('ccw', '2')):
            with self.subTest(direction=direction, seed=seed):
                status, frames, totals, c, out = plan_run('plan3', direction, seed, '120', 'stage-clock', '1')
                self.assertEqual(status, 0, out)
                _, real_frames, _, real, _ = plan_run('plan3', direction, seed, '120')
                self.assertEqual(frames, real_frames, 'the clock changes no conversion')
                check_counters(self, c)
                strips, odd = divmod(int(c['cpu_stage_us']), 1000)
                self.assertEqual(odd, 0)
                self.assertGreater(strips, 45 * int(c['cpu_full_frames']))
                self.assertEqual(int(c['cpu_rotate_us']), 10000 * strips)
                self.assertEqual(int(c['cpu_yield_us']), 100000 * strips)
                self.assertEqual(int(c['cpu_us']), 111000 * strips + 1000 * int(c['cpu_frames']))
                full = 45 * 111000 + 1000
                self.assertGreater(int(c['cpu_full_frames']), 2)
                self.assertEqual(int(c['cpu_full_us']), full * int(c['cpu_full_frames']))
                # The longest is a full frame or a plan of tall jobs, which has more strips.
                self.assertGreaterEqual(int(c['cpu_max_us']), full)
                self.assertEqual((int(c['cpu_max_us']) - 1000) % 111000, 0)
                # The rest is the large plans': whole strips, at least one for each rotation.
                count, us = int(c['cpu_medium_frames']), int(c['cpu_medium_us'])
                self.assertGreater(count, 2)
                self.assertEqual((us - 1000 * count) % 111000, 0)
                self.assertGreaterEqual(us, 112000 * count)
                self.assertEqual(int(c['cpu_full_us']) + int(c['cpu_medium_us']), int(c['cpu_us']))
                # Everything that is not a time is as in the run on the real clock.
                self.assertEqual({k: v for k, v in c.items() if k not in self.TIMED},
                                 {k: v for k, v in real.items() if k not in self.TIMED})

    def test_the_production_module_has_the_new_fields_and_no_counters_test_controls(self):
        data = (Path(BUILD.name) / 'partial-plan3-production.so').read_bytes()
        self.assertIn(b'path_ge2d_plan=', data)
        self.assertIn(b'plan_multi_le8192=', data)
        for knob in (b'RC2FX_TEST_COUNTERS_HUGE', b'RC2FX_TEST_COUNTERS_AT_EXIT'):
            self.assertNotIn(knob, data)

    @unittest.skipIf(platform.machine() == 'aarch64', 'this host runs the aarch64 build in the tests above')
    @unittest.skipUnless(ZIG and QEMU, 'the aarch64 build needs zig and qemu-aarch64 (or an aarch64 host)')
    def test_the_counts_under_qemu_are_those_of_the_host_run(self):
        # The mixer's CPU (Cortex-A53), with the NEON rotation.
        qemu = (QEMU, '-cpu', 'cortex-a53')
        program = Path(BUILD.name) / 'plan-plan3-aarch64'
        subprocess.run([ZIG, 'cc', *A64, *ProductionPlanTests.WARN[1:], '-DRC2FX_GUI_RECT_PLAN3',
                        str(HERE / 'partial_plan_test.c'), '-o', str(program)], check=True)
        for direction, seed in (('cw', '1'), ('ccw', '2')):
            with self.subTest(direction=direction, seed=seed):
                status, frames, totals, c, out = plan_run('plan3-aarch64', direction, seed, '250', runner=qemu)
                self.assertEqual(status, 0, out)
                self.check(totals, c, 250)
                host_status, host_frames, _, host_c, _ = plan_run('plan3', direction, seed, '250')
                self.assertEqual(host_status, 0)
                self.assertEqual(frames, host_frames, 'the same paths and the same images as the scalar host run')
                # Every count that does not measure time is the host's.
                self.assertEqual({k: v for k, v in c.items() if k not in self.TIMED},
                                 {k: v for k, v in host_c.items() if k not in self.TIMED})


if __name__ == '__main__':
    unittest.main()
