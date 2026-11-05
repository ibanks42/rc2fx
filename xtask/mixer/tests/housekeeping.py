#!/usr/bin/env python3
"""Host-only tests of rc2fx.sh's relocate_housekeeping (no mixer, fake /proc).

python3 xtask/mixer/tests/housekeeping.py

Extracts the function, points RC2FX_PROC at a fake /proc tree and stubs
taskset through PATH, then checks what is moved: only the mixer's own
SCHED_OTHER threads whose affinity is exactly CPU 0.
"""
import os
import re
import subprocess
import tempfile
import unittest
from pathlib import Path

SCRIPT = (Path(__file__).parents[1] / 'rc2fx.sh').read_text()
START = SCRIPT.index('# RØDE pins about fifteen SCHED_OTHER threads')
END = SCRIPT.index('# Started by launch beside the mixer it watches', START)
BLOCK = SCRIPT[START:END]

HARNESS = '''set -euo pipefail
logs=$RC2FX_LOGS
say() { printf '%s %s\\n' "$(cat "$RC2FX_ROOT/stamp")" "$*"; }
'''


class Tree:
    """A fake /proc/<pid>/task with a thread per (comm, affinity, policy)."""

    def __init__(self, root: Path, pid: int = 4242):
        self.root = root
        self.proc = root / 'proc'
        self.pid = pid
        self.tasks = self.proc / str(pid) / 'task'
        self.tasks.mkdir(parents=True)

    def thread(self, tid, comm, affinity='0', policy=0, present=True):
        task = self.tasks / str(tid)
        task.mkdir()
        (task / 'comm').write_text(comm + '\n')
        (task / 'status').write_text(f'Name:\t{comm}\nCpus_allowed_list:\t{affinity}\n')
        # /proc/<pid>/task/<tid>/stat, fields 3 onwards after the comm; the
        # script strips up to the LAST ") " and reads awk field 39, which is
        # stat field 41 (policy). 37 filler fields put it there.
        fields = ['S'] + ['0'] * 37 + [str(policy)] + ['0'] * 5
        (task / 'stat').write_text(f'{tid} ({comm}) ' + ' '.join(fields) + '\n')
        return task


class Tests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / 'stamp').write_text('STAMP')
        logs = self.root / 'logs'
        logs.mkdir()
        self.log = logs / 'rc2fx.log'
        bin = self.root / 'bin'
        bin.mkdir()
        self.moved = self.root / 'moved'
        (bin / 'taskset').write_text(f'#!/bin/sh\necho "$@" >> {self.moved}\n')
        (bin / 'taskset').chmod(0o755)
        self.tree = Tree(self.root)
        self.env = dict(os.environ, RC2FX_ROOT=str(self.root), RC2FX_LOGS=str(logs),
                        RC2FX_PROC=str(self.tree.proc), PATH=f'{bin}:{os.environ["PATH"]}')

    def run_move(self, pid=None):
        body = f'relocate_housekeeping {pid or self.tree.pid}'
        r = subprocess.run(['bash', '-c', HARNESS + BLOCK + '\n' + body], capture_output=True, text=True,
                           env=self.env, cwd=self.root, timeout=30)
        self.assertEqual(r.returncode, 0, r.stderr)
        return r

    def logged(self):
        return self.log.read_text() if self.log.exists() else ''

    def test_moves_only_the_mixers_own_sched_other_threads_pinned_to_cpu0(self):
        self.tree.thread(101, 'rc_audio_mixer', '0', 0)
        self.tree.thread(102, 'HIDThread', '0', 0)
        self.tree.thread(103, 'motion', '0,1')          # other affinity
        self.tree.thread(104, 'audio', '0', 1)          # SCHED_FIFO
        self.tree.thread(105, 'rc2fx-worker', '0', 0)   # ours
        self.tree.thread(106, 'JUCE Timer - 0', '0', 0)  # spaces
        self.tree.thread(107, 'weird ) name', '0', 0)   # a ')' in the comm
        self.tree.thread(108, 'rc2fxd', '0', 0)         # starts with rc2fx
        self.run_move()
        moved = self.moved.read_text().splitlines() if self.moved.exists() else []
        # The mixer's main thread (rc_audio_mixer), the HID and timer threads
        # and the odd comms move; rc2fx* threads, RT threads and any other
        # affinity do not.
        self.assertEqual(moved, ['-p 0xa 101', '-p 0xa 102', '-p 0xa 106', '-p 0xa 107'])
        log = self.logged()
        self.assertIn('Moved 4 RØDE housekeeping threads off CPU 0', log)
        self.assertIn('rc_audio_mixer;HIDThread;JUCE Timer - 0;weird ) name', log)
        self.assertIn('STAMP', log)

    def test_an_unchanged_scan_forks_no_text_processing_commands(self):
        self.tree.thread(101,'HIDThread','1,3',0)
        self.tree.thread(102,'Pool','0',2)
        calls=self.root/'external-calls'
        for command in ['cat','awk','sed']:
            path=self.root/'bin'/command
            path.write_text(f'#!/bin/sh\necho {command} >> {calls}\nexit 97\n')
            path.chmod(0o755)
        self.run_move()
        self.assertFalse(calls.exists(),calls.read_text() if calls.exists() else '')
        self.assertFalse(self.moved.exists())

    def test_nothing_to_move_logs_nothing(self):
        self.tree.thread(101, 'rc2fx-worker', '0', 0)
        self.tree.thread(102, 'audio', '0', 1)
        self.tree.thread(103, 'Timer', '1')
        self.run_move()
        self.assertFalse(self.moved.exists())
        self.assertEqual(self.logged(), '')

    def test_a_thread_that_exits_mid_scan_is_ignored(self):
        self.tree.thread(101, 'HIDThread', '0', 0)
        gone = self.tree.thread(102, 'Timer', '0', 0)
        for name in ('comm', 'status', 'stat'):
            (gone / name).unlink()
        self.run_move()
        self.assertEqual((self.moved.read_text().splitlines()), ['-p 0xa 101'])

    def moved_tids(self):
        return self.moved.read_text().splitlines() if self.moved.exists() else []

    def test_affinity_is_read_past_tab_or_space_separators(self):
        for tid, separator in ((101, '\t'), (102, ' '), (103, ' \t ')):
            task = self.tree.thread(tid, f'T{tid}')
            (task / 'status').write_text(f'Name:\tT{tid}\nCpus_allowed_list:{separator}0\nMems_allowed_list:\t0\n')
        self.run_move()
        self.assertEqual(self.moved_tids(), ['-p 0xa 101', '-p 0xa 102', '-p 0xa 103'])

    def test_only_the_exact_key_at_a_line_start_counts(self):
        self.tree.thread(101, 'Cpus_allowed_list: 0', '1,3')  # the key inside a comm
        other = self.tree.thread(102, 'Mems')
        (other / 'status').write_text('Name:\tMems\nMems_allowed_list:\t0\nCpus_allowed:\t1\n')
        wide = self.tree.thread(103, 'Wide')
        (wide / 'status').write_text('Name:\tWide\nCpus_allowed_list:\t0-3\n')
        self.run_move()
        self.assertEqual(self.moved_tids(), [])

    def test_a_final_line_without_a_newline_still_counts(self):
        task = self.tree.thread(101, 'Tail')
        (task / 'status').write_text('Name:\tTail\nCpus_allowed_list:\t0')
        self.run_move()
        self.assertEqual(self.moved_tids(), ['-p 0xa 101'])

    def test_a_thread_gone_before_status_never_reuses_the_previous_affinity(self):
        self.tree.thread(101, 'HIDThread')
        (self.tree.thread(102, 'Timer') / 'status').unlink()
        emptied = self.tree.thread(103, 'Emptied')
        (emptied / 'status').write_text('')
        unreadable = self.tree.thread(104, 'Unreadable')
        (unreadable / 'status').unlink()
        (unreadable / 'status').mkdir()  # opens, then read fails
        self.tree.thread(105, 'Last')
        r = self.run_move()
        self.assertEqual(self.moved_tids(), ['-p 0xa 101', '-p 0xa 105'])
        self.assertEqual(r.stderr, '')

    def test_each_status_file_is_read_once_not_per_line(self):
        self.assertIn("IFS= read -r -d ''", BLOCK)
        self.assertNotIn('while read', BLOCK)

    def test_the_supervisor_runs_it_from_tick_10_every_5_ticks(self):
        # Drive the extracted hook with stubs: ticks 10 and 15 run, 9 and
        # 11-14 do not.
        hook = SCRIPT[SCRIPT.index('    # RØDE\'s CPU0-pinned housekeeping threads hold locks'):
                     SCRIPT.index('    tick=$((tick + 1))')]
        self.assertIn('relocate_housekeeping "$native_pid"', hook)
        body = f'''calls=0
mixer_alive() {{ return 0; }}
relocate_housekeeping() {{ calls=$((calls + 1)); }}
native_pid=1
for tick in 9 10 11 12 13 14 15; do
{hook}
done
echo "calls $calls"
'''
        r = subprocess.run(['bash', '-c', HARNESS + body], capture_output=True, text=True, env=self.env,
                           cwd=self.root, timeout=30)
        self.assertEqual(r.stdout.split(), ['calls', '2'], r.stderr)

if __name__ == '__main__':
    unittest.main()
