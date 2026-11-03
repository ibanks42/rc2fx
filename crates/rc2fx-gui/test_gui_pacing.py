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
        cap = 600
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


class CpuPathTests(unittest.TestCase):
    """Fixed CPU rotation of large redraws; small damage still uses GE2D."""
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

    def test_production_has_no_mode_or_benchmark_controls(self):
        data = (Path(BUILD.name) / 'partial-plan3-production.so').read_bytes()
        for knob in (b'RC2FX_GUI_MODE_FILE', b'RC2FX_GUI_CPU_SHARE_PERCENT',
                     b'RC2FX_GUI_CPU_ROTATE_PERCENT', b'RC2FX_GUI_FORCE_FULL_FILE',
                     b'RC2FX_TEST_FORCE_FULL_FILE', b'RC2FX_TEST_CPU_ROTATE',
                     b'screen-conversion.active', b'gui-force-full'):
            self.assertNotIn(knob, data)


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


if __name__ == '__main__':
    unittest.main()
