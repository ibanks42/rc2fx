#!/usr/bin/env python3
"""Host-only tests of rc2fx.sh's JACK error recorder (jack_recorder and the
jackd replacement it uses, bb_replace_jack).

python3 xtask/mixer/tests/recorder_launcher.py

Runs the script's real functions against a fake /proc, cpuset and service in
a temporary directory. The original jackd is a fake process entry; the
replacement (and the fallback) is a real child process, a fake jackd that
writes its own /proc entries the way a JACK server would look: threads in the
root cpuset, a FIFO 51 driver thread, PCM devices open, the preload mapped
only when it was given. Thread moves update those entries through the
script's bb_move_task/bb_set_cpus seams. No real audio, scheduling or /run
writes.
"""
import hashlib
import os
import signal
import stat
import subprocess
import tempfile
import time
import unittest
from pathlib import Path

SCRIPT = (Path(__file__).parents[1] / 'rc2fx.sh').read_text()
BLOCK = SCRIPT.split('# ---- JACK error recorder', 1)[1].split('\n', 1)[1].split(
    '# ---- end of the JACK error recorder', 1)[0]
ORIGINAL = 555555
CALLME = 666666
FOREIGN = 777777
ARGV = ['jackd', '--port-max', '166', '-P', '51', '-d', 'alsa', '-C', 'hw:0,3', '-Phw:0,0',
        '-r', '48000', '-p', '112', '-f', '72']
PCMS = ['/dev/snd/pcmC0D0p', '/dev/snd/pcmC0D3c']


def stat_line(pid, comm, policy=0, priority=0, nice=0, started=123):
    fields = ['S'] + ['0'] * 49  # field 3 onwards; field n is index n - 3
    fields[19 - 3], fields[22 - 3], fields[40 - 3], fields[41 - 3] = str(nice), str(started), str(priority), str(policy)
    return f'{pid} ({comm}) ' + ' '.join(fields) + '\n'


def tid_of(pid, index):
    return pid if index == 0 else pid * 10 + index


def task(directory, tid, comm, cpuset, cpus, policy=0, priority=0, nice=0, started=123):
    directory.mkdir(parents=True, exist_ok=True)
    (directory / 'comm').write_text(comm + '\n')
    (directory / 'cgroup').write_text(f'1:cpuset:{cpuset}\n')
    (directory / 'status').write_text(f'Name:\t{comm}\nCpus_allowed_list:\t{cpus}\n')
    (directory / 'stat').write_text(stat_line(tid, comm, policy, priority, nice, started))


FAKE_JACKD = r'''#!/bin/bash
# A stand-in JACK server: writes its own fake /proc entries, then waits.
p="$TEST_PROC/$$"
mkdir -p "$p/task" "$p/fd"
printf '%s\0' "$0" "$@" > "$p/cmdline"
env -0 > "$p/environ"
echo jackd > "$p/comm"
ln -sf "$TEST_JACKD" "$p/exe"
: > "$p/maps"
test -n "${LD_PRELOAD:-}" && test "$TEST_MODE" != nolib && echo "7f00-7f01 r-xp 0 0:0 0 $LD_PRELOAD" >> "$p/maps"
echo "7f02-7f03 r-xp 0 0:0 0 /usr/lib/libjackserver.so.0" >> "$p/maps"
started=$(( $(date +%s%N) / 1000000 ))
stat() { printf '%s (jackd) S 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 %s 0 0 %s 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 %s %s 0 0 0 0 0 0 0 0 0 0 0\n' "$1" "$2" "$started" "$3" "$4"; }
stat $$ 0 0 0 > "$p/stat"
threads=4; test "$TEST_MODE" = extra-thread && threads=5
# Thread 0 is the main thread (its id is the pid).
tid() { if test "$1" = 0; then echo $$; else echo $(( $$ * 10 + $1 )); fi; }
for ((i = 0; i < threads; i++)); do
  t="$p/task/$(tid "$i")"; mkdir -p "$t"
  echo jackd > "$t/comm"
  printf '1:cpuset:/\n' > "$t/cgroup"
  printf 'Cpus_allowed_list:\t%s\n' "$TEST_ROOT_CPUS" > "$t/status"
  policy=0; priority=0
  if test "$i" = 2; then policy=1; priority=51; fi
  # A server whose fourth thread runs at another real-time priority.
  if test "$TEST_MODE" = placement && test "$i" = 3; then policy=1; priority=60; fi
  if test "$TEST_MODE" = extra-thread && test "$i" = 4; then policy=1; priority=51; fi
  stat "$(tid "$i")" 0 "$priority" "$policy" > "$t/stat"
done
# A JACK start that moves an audio IRQ (irq-stuck: and it cannot be put back).
case "$TEST_MODE" in
  irq-moves|irq-stuck) chmod u+w "$TEST_PROC/irq/24/smp_affinity_list"; echo 0-3 > "$TEST_PROC/irq/24/smp_affinity_list";;
esac
test "$TEST_MODE" = irq-stuck && chmod 444 "$TEST_PROC/irq/24/smp_affinity_list"
# A server that dies at once and whose pid now belongs to another process.
if test "$TEST_MODE" = reborn; then
  echo "$$" >> "$TEST_LOG/servers"
  sleep 1.5; started=$((started + 1)); stat $$ 0 0 0 > "$p/stat"; exec sleep 60
fi
if test "$TEST_MODE" != notready; then
  ln -sf /dev/snd/pcmC0D3c "$p/fd/3"
  ln -sf /dev/snd/pcmC0D0p "$p/fd/4"
fi
echo "$$" >> "$TEST_LOG/servers"
exec sleep 60
'''

PRELUDE = r'''
set -uo pipefail
say() { printf '%s\n' "$*"; }
start_time() { local record rest f; record=$(cat "$blackbox_proc/$1/stat") || return 1; rest=${record##*) }; read -r -a f <<< "$rest"; printf '%s\n' "${f[19]}"; }
alive() {
  if test -e "$blackbox_proc/$1/fake"; then test ! -e "$blackbox_proc/$1/dead"; else builtin kill -0 "$1" 2>/dev/null; fi
}
kill() {
  local signal=$1 pid=$2
  if test -e "$blackbox_proc/$pid/fake"; then
    echo "$signal $pid" >> "$TEST_LOG/kills"
    if test "$TEST_MODE" = stubborn && test "$signal" = -TERM; then return 0; fi
    touch "$blackbox_proc/$pid/dead"; return 0
  fi
  builtin kill "$signal" "$pid"
}
pgrep() {
  local d found=1
  for d in "$blackbox_proc"/[0-9]*; do
    if test "$(cat "$d/comm" 2>/dev/null)" = jackd && alive "${d##*/}"; then echo "${d##*/}"; found=0; fi
  done
  return "$found"
}
'''

SEAMS = r'''
# The replacement alone, in the fixture's directory, as jack_recorder calls it
# for an attempt directory.
replace_in_test_dir() {
  local pid
  pid=$(pgrep -x jackd) || return 1
  bb_replace_jack "$test_dir" "$test_dir/librc2fx_jackerr.so" "$test_dir/rc2fx_jackerr_snapshot" \
    "$test_limit" 'Test recorder' "$pid"
}
# Moving a thread rewrites its fake entries, as the kernel would.
bb_move_task() {
  local t
  for t in "$blackbox_proc"/*/task/"$1"; do
    printf '1:cpuset:%s\n' "$2" > "$t/cgroup"
    printf 'Cpus_allowed_list:\t%s\n' "$(cat "$blackbox_cpuset$2/cpuset.cpus")" > "$t/status"
  done
  echo "move $1 $2" >> "$TEST_LOG/placement"
}
bb_set_cpus() {
  local t
  for t in "$blackbox_proc"/*/task/"$1"; do printf 'Cpus_allowed_list:\t%s\n' "$2" > "$t/status"; done
  echo "cpus $1 $2" >> "$TEST_LOG/placement"
}
'''


class Fixture:
    def __init__(self, test, mode='success', **options):
        self.test, self.mode = test, mode
        self.temp = tempfile.TemporaryDirectory()
        root = self.root = Path(self.temp.name)
        self.diag, self.home, self.logs, self.bin = root / 'diag', root / 'home', root / 'logs', root / 'bin'
        self.data = root / 'data'
        self.data.mkdir()
        self.proc, self.cpuset = root / 'proc', root / 'cpuset'
        for p in (self.diag, self.home, self.logs, self.bin, self.proc, self.cpuset / 'audio'):
            p.mkdir(parents=True, exist_ok=True)
        self.diag.chmod(0o700)
        (self.cpuset / 'cpuset.cpus').write_text('0-3\n')
        (self.cpuset / 'audio' / 'cpuset.cpus').write_text('1-3\n')
        (self.cpuset / 'tasks').write_text('')
        (self.cpuset / 'audio' / 'tasks').write_text('')
        (self.home / 'librc2fx_native.so').write_bytes(b'fixture native library')
        (self.diag / 'librc2fx_jackerr.so').write_bytes(b'fixture recorder')
        self.executable(self.diag / 'rc2fx_jackerr_snapshot',
                        'exit 1\n' if mode == 'nosnapshot' else 'printf snapshot > "$2"\n')
        self.jackd = self.bin / 'jackd'
        self.executable(self.jackd, FAKE_JACKD[len('#!/bin/bash\n'):])
        self.executable(self.bin / 'taskset', 'echo "taskset $*" >> "$TEST_LOG/placement"; shift 2; exec "$@"\n')
        self.executable(self.bin / 'chrt', 'echo "chrt $*" >> "$TEST_LOG/placement"; shift 2; exec "$@"\n')
        self.executable(self.bin / 'callme', 'echo "$1" >> "$TEST_LOG/callme"\n'
                        + ('test "$1" != stop\n' if mode == 'callme-fails' else ''))
        # The original server and the processes around it.
        self.original_env = {'PATH': '/usr/bin:/bin', 'TEST_MARK': 'value with spaces', 'TEST_PROC': str(self.proc),
                             'TEST_JACKD': str(self.jackd), 'TEST_LOG': str(root), 'TEST_MODE': mode,
                             'TEST_ROOT_CPUS': '0-3'}
        self.process(ORIGINAL, 'jackd', ARGV, self.original_env, maps='/usr/lib/libjackserver.so.0\n', pcms=PCMS)
        threads = [(0, '/audio', '1-3', 0, 0), (1, '/audio', '1-3', 0, 0), (2, '/audio', '1-3', 1, 51), (3, '/audio', '1-3', 0, 0)]
        if mode == 'ambiguous':
            threads[1] = (1, '/', '0-3', 0, 0)
        if mode == 'rtprio':
            threads[3] = (3, '/audio', '1-3', 1, 60)
        if mode == 'startup':
            # A freshly started server: only the main thread moved to /audio.
            threads = [(0, '/audio', '1-3', 0, 0), (1, '/', '0-3', 0, 0), (2, '/', '0-3', 1, 51), (3, '/', '0-3', 0, 0)]
        for index, cpuset, cpus, policy, priority in threads:
            tid = tid_of(ORIGINAL, index)
            task(self.proc / str(ORIGINAL) / 'task' / str(tid), tid, 'jackd', cpuset, cpus, policy, priority,
                 nice=5 if mode == 'niced' else 0)
        if mode == 'unreadable':
            (self.proc / str(ORIGINAL) / 'task' / str(tid_of(ORIGINAL, 3)) / 'status').unlink()
        self.process(CALLME, 'callme-t', ['callme-t'], {}, maps='/usr/lib/libjack.so.0\n')
        if mode == 'foreign':
            self.process(FOREIGN, 'other-client', ['other-client'], {}, maps='/usr/lib/libjack.so.0.1.0\n')
        for irq, cpus in (('24', '1-3'), ('27', '1-3'))[:1 if mode == 'noirq' else 2]:
            (self.proc / 'irq' / irq).mkdir(parents=True, exist_ok=True)
            (self.proc / 'irq' / irq / 'smp_affinity_list').write_text(cpus + '\n')
            (self.proc / 'irq' / irq / 'effective_affinity_list').write_text('1\n')
        self.overrides = options

    def executable(self, path, body):
        path.write_text('#!/bin/bash\n' + body)
        path.chmod(0o700)

    def process(self, pid, comm, argv, env, maps='', pcms=()):
        p = self.proc / str(pid)
        (p / 'fd').mkdir(parents=True, exist_ok=True)
        (p / 'fake').write_text('')
        (p / 'comm').write_text(comm + '\n')
        (p / 'cmdline').write_bytes(b''.join(a.encode() + b'\0' for a in argv))
        (p / 'environ').write_bytes(b''.join(f'{k}={v}'.encode() + b'\0' for k, v in env.items()))
        (p / 'maps').write_text(maps)
        (p / 'stat').write_text(stat_line(pid, comm))
        (p / 'exe').symlink_to(self.jackd if comm == 'jackd' else '/usr/bin/' + comm)
        for n, target in enumerate(pcms, 3):
            (p / 'fd' / str(n)).symlink_to(target)

    def run(self, timeout=40, command='replace_in_test_dir'):
        settings = '\n'.join([
            f'home={self.home}', f'logs={self.logs}', f'data={self.data}',
            f'export TEST_LOG={self.root} TEST_MODE={self.mode}',
            f'export PATH={self.bin}:$PATH',
        ])
        overrides = '\n'.join([
            f'test_dir={self.diag}', 'test_limit=8388608',
            f'blackbox_proc={self.proc}', f'blackbox_cpuset={self.cpuset}',
            f'blackbox_jackd={self.jackd}', f'blackbox_callme={self.bin / "callme"}', f'blackbox_owner={os.getuid()}',
            *(f'{name}={value}' for name, value in self.overrides.items()),
        ])
        script = (settings + '\n' + PRELUDE + BLOCK + SEAMS + overrides
                  + f'\n{command}; status=$?; wait; exit $status\n')
        self.result = subprocess.run(['bash', '-c', script], capture_output=True, text=True, timeout=timeout,
                                     env=dict(os.environ, TEST_LOG=str(self.root), TEST_MODE=self.mode))
        return self.result.returncode

    def lines(self, name):
        path = self.root / name
        return path.read_text().splitlines() if path.exists() else []

    def log(self):
        path = self.logs / 'rc2fx.log'
        return path.read_text() if path.exists() else ''

    def servers(self):
        return [int(p) for p in self.lines('servers')]

    def server_args(self, pid):
        return (self.proc / str(pid) / 'cmdline').read_bytes().split(b'\0')[1:-1]

    def server_env(self, pid):
        return dict(e.decode().split('=', 1) for e in (self.proc / str(pid) / 'environ').read_bytes().split(b'\0') if e)

    def close(self):
        for pid in self.servers():
            try:
                os.kill(pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
        self.temp.cleanup()


class ReplacementTests(unittest.TestCase):
    def fixture(self, mode='success', **overrides):
        fixture = Fixture(self, mode, **overrides)
        self.addCleanup(fixture.close)
        return fixture

    def assert_untouched(self, f):
        self.assertFalse((f.proc / str(ORIGINAL) / 'dead').exists(), 'the original jackd was stopped')
        self.assertEqual(f.lines('kills'), [])
        self.assertEqual(f.servers(), [])

    def test_another_jack_client_prevents_the_replacement(self):
        f = self.fixture('foreign')
        self.assertNotEqual(f.run(), 0)
        self.assert_untouched(f)
        self.assertEqual(f.lines('callme'), [])
        self.assertIn('other JACK clients 777777(other-client)', f.log())

    def test_a_server_a_stock_start_would_not_reproduce_is_left_alone(self):
        for mode in ('niced', 'rtprio', 'unreadable', 'noirq'):
            with self.subTest(mode):
                f = self.fixture(mode)
                self.assertNotEqual(f.run(), 0)
                self.assert_untouched(f)
                self.assertEqual(f.lines('callme'), [], 'checked before callme stops')
                self.assertIn('skipped', f.log())
                self.assertTrue((f.diag / 'preflight-jack.inventory').exists())
                self.assertEqual(list(f.diag.glob('jackerr.*')), [])

    def test_a_failed_callme_stop_starts_it_again(self):
        f = self.fixture('callme-fails')
        self.assertNotEqual(f.run(), 0)
        self.assert_untouched(f)
        self.assertEqual(f.lines('callme'), ['stop', 'start'])

    def test_an_ambiguous_inventory_restores_callme_and_leaves_jack(self):
        f = self.fixture('ambiguous')
        self.assertNotEqual(f.run(), 0)
        self.assert_untouched(f)
        self.assertEqual(f.lines('callme'), ['stop', 'start'])
        self.assertIn('roles with two placements: thread|jackd|0|0', f.log())
        # The evidence of the refusal is kept; no ring directory is made.
        self.assertIn('thread|jackd|0|0|0|/|0-3', (f.diag / 'preflight-jack.inventory').read_text())
        self.assertIn('thread|jackd|0|0|0|/|0-3', (f.diag / 'original-jack.inventory').read_text())
        self.assertEqual(list(f.diag.glob('jackerr.*')), [])

    def test_the_recording_server_matches_the_old_one_by_role(self):
        f = self.fixture('success')
        self.assertEqual(f.run(), 0, f.result.stdout + f.result.stderr + f.log())
        (server,) = f.servers()
        self.assertEqual(f.lines('kills'), [f'-TERM {ORIGINAL}'])
        self.assertEqual(f.lines('callme'), ['stop', 'start'])
        self.assertEqual(f.server_args(server), [a.encode() for a in ARGV[1:]])
        env = f.server_env(server)
        self.assertEqual(env['TEST_MARK'], 'value with spaces')
        self.assertEqual(env['LD_PRELOAD'], str(f.diag / 'librc2fx_jackerr.so'))
        ring = Path(env['RC2FX_JACKERR_DIR'])
        self.assertEqual(ring.parent, f.diag)
        self.assertEqual(stat.S_IMODE(ring.stat().st_mode), 0o700)
        self.assertEqual((f.diag / 'jackerr-dir').read_text().strip(), str(ring))
        self.assertTrue((f.diag / 'ready.snapshot').exists())
        self.assertFalse((f.diag / 'failed').exists())
        pid, started = (f.diag / 'jackd.pid').read_text().split()
        self.assertEqual(int(pid), server)
        self.assertEqual(started, (f.proc / pid / 'stat').read_text().split()[21], 'the birth token')
        placement = f.lines('placement')
        # Started like restartJack: from the root cpuset, all CPUs, ordinary policy.
        self.assertIn('taskset -c 0-3 chrt -o 0 env -i', ' '.join(placement[:2]))
        self.assertIn(str(server), (f.cpuset / 'tasks').read_text().split(), 'launched from the root cpuset')
        # Then every thread to its role's placement, and nothing else.
        moves = [line for line in placement if line.startswith('move ')]
        self.assertEqual(sorted(moves), sorted(f'move {tid_of(server, i)} /audio' for i in range(4)))
        inventory = (f.diag / 'original-jack.inventory').read_text()
        self.assertIn('main|jackd|0|0|0|/audio|1-3', inventory)
        self.assertIn('thread|jackd|1|51|0|/audio|1-3', inventory)
        self.assertIn('irq 24 1-3', inventory)
        self.assertIn('effective 24 1', inventory)
        self.assertIn('effective 27 1', (f.diag / 'after-jack.irqs').read_text())

    def test_a_freshly_started_server_is_matched_role_by_role(self):
        # As on the device at mixer start: main in /audio 1-3, the FIFO 51
        # driver and the other thread(s) still in the root cpuset.
        f = self.fixture('startup')
        self.assertEqual(f.run(), 0, f.log())
        (server,) = f.servers()
        moves = sorted(line for line in f.lines('placement') if line.startswith('move '))
        self.assertEqual(moves, sorted([f'move {server} /audio'] + [f'move {tid_of(server, i)} /' for i in (1, 2, 3)]))
        new = sorted(f'{(f.proc / str(server) / "task" / str(tid_of(server, i)) / "cgroup").read_text().split(":")[2].strip()}'
                     for i in range(4))
        self.assertEqual(new, ['/', '/', '/', '/audio'])
        self.assertIn('thread|jackd|1|51|0|/|0-3', (f.diag / 'original-jack.inventory').read_text())

    def test_a_recorder_that_does_not_load_restores_a_plain_server(self):
        for mode in ('nolib', 'nosnapshot'):
            with self.subTest(mode):
                f = self.fixture(mode)
                self.assertEqual(f.run(), 1)
                logger, plain = f.servers()
                self.assertTrue((f.diag / 'failed').exists())
                self.assertNotIn('LD_PRELOAD', f.server_env(plain))
                self.assertEqual(f.server_args(plain), [a.encode() for a in ARGV[1:]])
                with self.assertRaises(ProcessLookupError):
                    os.kill(logger, 0)  # the recording server was stopped
                self.assertIn('Plain jackd restored', f.log())
                self.assertEqual(f.lines('callme'), ['stop', 'start'])

    def test_a_server_whose_threads_do_not_match_is_not_kept(self):
        # A thread at another real-time priority is another role: never ready.
        f = self.fixture('placement')
        self.assertEqual(f.run(), 1)
        self.assertTrue((f.diag / 'failed').exists())
        self.assertIn('roles differ', f.log())
        self.assertIn('PLAIN JACKD DID NOT COME UP CORRECTLY', f.log())

    def test_a_server_with_an_extra_thread_is_never_ready(self):
        f = self.fixture('extra-thread')
        self.assertEqual(f.run(), 1)
        self.assertTrue((f.diag / 'failed').exists())
        self.assertIn('was not ready, placed, recording', f.log())

    def test_a_server_that_never_opens_its_devices_is_bounded(self):
        f = self.fixture('notready')
        started = time.monotonic()
        self.assertEqual(f.run(), 1)
        self.assertLess(time.monotonic() - started, 30, 'readiness is bounded')
        self.assertIn('PLAIN JACKD DID NOT COME UP CORRECTLY', f.log())
        self.assertEqual(f.lines('callme'), ['stop', 'start'])

    def test_a_jackd_that_ignores_term_is_killed_only_as_itself(self):
        f = self.fixture('stubborn')
        self.assertEqual(f.run(), 0, f.log())
        self.assertEqual(f.lines('kills'), [f'-TERM {ORIGINAL}', f'-KILL {ORIGINAL}'])

    def test_a_dead_server_whose_pid_was_reused_is_never_signalled(self):
        f = self.fixture('reborn')
        self.assertEqual(f.run(), 1)
        for server in f.servers():
            os.kill(server, 0)  # still running: neither was signalled
        self.assertIn('PLAIN JACKD DID NOT COME UP CORRECTLY', f.log())
        self.assertEqual(f.lines('callme'), ['stop', 'start'])

    def test_irqs_a_start_moved_are_put_back_or_the_server_is_not_kept(self):
        f = self.fixture('irq-moves')
        self.assertEqual(f.run(), 0, f.log())
        self.assertEqual((f.proc / 'irq/24/smp_affinity_list').read_text().strip(), '1-3')
        f = self.fixture('irq-stuck')
        self.assertEqual(f.run(), 1)
        self.assertTrue((f.diag / 'failed').exists())
        self.assertIn('PLAIN JACKD DID NOT COME UP CORRECTLY', f.log())
        self.assertIn('irq 24 0-3', (f.diag / 'after-jack.irqs').read_text(), 'what was left is recorded')

    def make_ring(self, f, name, acked, size=0):
        d = f.diag / name
        d.mkdir()
        (d / 'ring-900000-1.ring').write_bytes(b'r' * size)
        if acked:
            (d / 'acked').write_text('')
        return d

    def test_no_evidence_is_removed_even_once_the_host_archived_it(self):
        f = self.fixture('success')
        archived = self.make_ring(f, 'jackerr.archived', True, size=64)
        self.make_ring(f, 'jackerr.kept', False, size=64)
        (f.diag / 'jackd.log').write_text('archived log\n')
        (f.diag / 'jackd.log.acked').write_text('')
        (f.diag / 'fallback-jackd.log').write_text('not archived\n')
        before = {p.relative_to(f.diag): p.read_bytes() for p in f.diag.rglob('*') if p.is_file()}
        self.assertEqual(f.run(), 0, f.log())
        names = sorted(p.name for p in f.diag.glob('jackerr.*'))
        self.assertIn('jackerr.archived', names)
        self.assertIn('jackerr.kept', names)
        self.assertEqual(len(names), 3, 'the two kept and the new ring')
        for name, content in before.items():
            current = (f.diag / name).read_bytes()
            self.assertTrue(current.startswith(content), f'{name} lost data')
        self.assertTrue((archived / 'acked').exists())
        self.assertFalse((f.diag / 'prune.log').exists(), 'nothing was pruned')

    def test_archived_rings_do_not_count_toward_the_limit(self):
        f = self.fixture('success', test_limit=1000)
        self.make_ring(f, 'jackerr.archived', True, size=2000)
        self.assertEqual(f.run(), 0, f.log())
        self.assertTrue((f.diag / 'jackerr.archived' / 'ring-900000-1.ring').exists())

    def test_too_much_unarchived_evidence_refuses_the_replacement(self):
        f = self.fixture('success', test_limit=1000)
        self.make_ring(f, 'jackerr.kept', False, size=2000)
        self.assertNotEqual(f.run(), 0)
        self.assert_untouched(f)
        self.assertEqual(f.lines('callme'), [])
        self.assertIn('not yet archived', f.log())
        self.assertIn('nothing deleted', f.log())
        self.assertEqual(sorted(p.name for p in f.diag.glob('jackerr.*')), ['jackerr.kept'])
        self.assertEqual((f.diag / 'jackerr.kept' / 'ring-900000-1.ring').stat().st_size, 2000)


class RecorderTests(unittest.TestCase):
    """The persistent recorder: the bundle's own library, no manual marker."""

    def fixture(self, mode='success', listed=('librc2fx_jackerr.so', 'rc2fx_jackerr_snapshot'),
                files=('librc2fx_jackerr.so', 'rc2fx_jackerr_snapshot'), **overrides):
        f = Fixture(self, mode, **overrides)
        self.addCleanup(f.close)
        f.recorder = f.root / 'recorder'
        f.overrides.setdefault('recorder_dir', f.recorder)
        if 'librc2fx_jackerr.so' in files:
            (f.home / 'librc2fx_jackerr.so').write_bytes(b'bundled recorder library')
        if 'rc2fx_jackerr_snapshot' in files:
            f.executable(f.home / 'rc2fx_jackerr_snapshot',
                         'exit 1\n' if mode == 'nosnapshot' else 'printf snapshot > "$2"\n')
        names = ['librc2fx_native.so', *listed]
        lines = []
        for name in names:
            path = f.home / name
            digest = hashlib.sha256(path.read_bytes()).hexdigest() if path.exists() else '1' * 64
            lines.append(f'{digest}  {name}\n')
        (f.home / 'SHA256SUMS').write_text(''.join(lines))
        return f

    def bundle_ok(self, f):
        """What launch() checks first: the whole bundle against SHA256SUMS."""
        return subprocess.run(['sha256sum', '-c', '--quiet', 'SHA256SUMS'], cwd=f.home,
                              capture_output=True).returncode == 0

    def assert_untouched(self, f):
        self.assertFalse((f.proc / str(ORIGINAL) / 'dead').exists(), 'the original jackd was stopped')
        self.assertEqual(f.lines('kills'), [])
        self.assertEqual(f.servers(), [])
        self.assertEqual(f.lines('callme'), [])

    def attempts(self, f):
        return sorted(f.recorder.glob('run.*'))

    def status(self, f):
        return (f.recorder / 'status').read_text().split(' ', 1)[0]

    def test_a_bundled_recorder_is_preloaded_without_any_marker(self):
        f = self.fixture()
        self.assertEqual(f.run(command='jack_recorder'), 0,
                         f.result.stdout + f.result.stderr + f.log())
        (server,) = f.servers()
        self.assertEqual(f.lines('kills'), [f'-TERM {ORIGINAL}'])
        self.assertEqual(f.lines('callme'), ['stop', 'start'])
        env = f.server_env(server)
        self.assertEqual(env['LD_PRELOAD'], str(f.home / 'librc2fx_jackerr.so'))
        self.assertNotIn('RC2FX_BLACKBOX', env)
        self.assertEqual(f.server_args(server), [a.encode() for a in ARGV[1:]])
        (attempt,) = self.attempts(f)
        self.assertEqual(stat.S_IMODE(f.recorder.stat().st_mode), 0o700)
        self.assertEqual(stat.S_IMODE(attempt.stat().st_mode), 0o700)
        ring = Path(env['RC2FX_JACKERR_DIR'])
        self.assertEqual(ring.parent, attempt, 'all evidence of the attempt in its own directory')
        self.assertEqual((attempt / 'jackerr-dir').read_text().strip(), str(ring))
        for name in ('ready.snapshot', 'preflight-jack.inventory', 'original-jack.inventory', 'jackd.pid'):
            self.assertTrue((attempt / name).exists(), name)
        pid, started = (attempt / 'jackd.pid').read_text().split()
        self.assertEqual((f.recorder / 'active').read_text().split(), [str(attempt), pid, started])
        self.assertEqual(self.status(f), 'active')
        self.assertFalse((f.recorder / 'failed').exists())
        self.assertIn('JACK recorder active: pid', f.log())

    def test_a_bundle_built_before_the_recorder_runs_plain_jack_and_stays_valid(self):
        f = self.fixture(listed=(), files=())
        self.assertTrue(self.bundle_ok(f), 'the old bundle still passes launch()')
        self.assertNotEqual(f.run(command='jack_recorder'), 0)
        self.assert_untouched(f)
        self.assertEqual(self.status(f), 'not-bundled')
        self.assertEqual(self.attempts(f), [])

    def test_a_corrupt_recorder_bundle_never_reaches_jackd(self):
        cases = {
            'listed, missing': dict(files=('rc2fx_jackerr_snapshot',)),
            'one listed': dict(listed=('librc2fx_jackerr.so',)),
            'unlisted file': dict(listed=('rc2fx_jackerr_snapshot',)),
        }
        for name, options in cases.items():
            with self.subTest(name):
                f = self.fixture(**options)
                self.assertNotEqual(f.run(command='jack_recorder'), 0)
                self.assert_untouched(f)
                self.assertEqual(self.status(f), 'corrupt')
                self.assertEqual(self.attempts(f), [])
        # A library changed after packaging: launch() itself goes stock, and
        # the recorder refuses on its own as well.
        f = self.fixture()
        (f.home / 'librc2fx_jackerr.so').write_bytes(b'tampered')
        self.assertFalse(self.bundle_ok(f), 'launch() would start the stock mixer')
        self.assertNotEqual(f.run(command='jack_recorder'), 0)
        self.assert_untouched(f)
        self.assertEqual(self.status(f), 'corrupt')

    def test_a_linked_library_or_a_shared_state_directory_is_refused(self):
        f = self.fixture()
        target = f.root / 'elsewhere.so'
        target.write_bytes((f.home / 'librc2fx_jackerr.so').read_bytes())
        (f.home / 'librc2fx_jackerr.so').unlink()
        (f.home / 'librc2fx_jackerr.so').symlink_to(target)
        self.assertNotEqual(f.run(command='jack_recorder'), 0)
        self.assert_untouched(f)
        self.assertEqual(self.status(f), 'corrupt')
        for kind in ('link', 'open'):
            with self.subTest(kind):
                f = self.fixture()
                if kind == 'link':
                    (f.root / 'other').mkdir(mode=0o700)
                    f.recorder.symlink_to(f.root / 'other')
                else:
                    f.recorder.mkdir()
                    f.recorder.chmod(0o755)
                self.assertNotEqual(f.run(command='jack_recorder'), 0)
                self.assert_untouched(f)
                self.assertEqual(list((f.root / 'other').iterdir()) if kind == 'link' else [], [])

    def test_the_persistent_off_switch_keeps_plain_jack(self):
        f = self.fixture()
        (f.data / 'jack-recorder-off').write_text('')
        self.assertNotEqual(f.run(command='jack_recorder'), 0)
        self.assert_untouched(f)
        self.assertEqual(self.status(f), 'off')
        self.assertIn('JACK recorder off', f.log())

    def test_a_failed_replacement_restores_plain_jack_and_is_not_retried_this_boot(self):
        f = self.fixture('nolib')
        self.assertEqual(f.run(command='jack_recorder'), 1)
        logger, plain = f.servers()
        (attempt,) = self.attempts(f)
        self.assertTrue((attempt / 'failed').exists())
        self.assertEqual((f.recorder / 'failed').read_text().strip(), str(attempt), 'the boot latch')
        self.assertTrue((attempt / 'fallback-jackd.log').exists())
        self.assertNotIn('LD_PRELOAD', f.server_env(plain))
        self.assertIn('Plain jackd restored', f.log())
        self.assertEqual(self.status(f), 'failed')
        # The next RC2FX start of this boot leaves the plain server alone.
        self.assertNotEqual(f.run(command='jack_recorder'), 0)
        self.assertEqual(f.servers(), [logger, plain])
        self.assertEqual(self.attempts(f), [attempt], 'no second attempt')
        self.assertIn('failed earlier this boot', f.log())
        os.kill(plain, 0)

    def test_every_attempt_keeps_its_own_evidence(self):
        # A server the recorder refuses after inspecting it, at two starts.
        f = self.fixture('ambiguous')
        self.assertNotEqual(f.run(command='jack_recorder'), 0)
        (first,) = self.attempts(f)
        kept = {p.name: p.read_bytes() for p in first.iterdir()}
        self.assertIn('preflight-jack.inventory', kept)
        self.assertEqual(self.status(f), 'refused')
        self.assertNotEqual(f.run(command='jack_recorder'), 0)
        second = [a for a in self.attempts(f) if a != first]
        self.assertEqual(len(second), 1)
        self.assertEqual({p.name: p.read_bytes() for p in first.iterdir()}, kept, 'the first attempt untouched')
        self.assertTrue((second[0] / 'preflight-jack.inventory').exists())

    def test_a_running_recorder_is_kept_and_only_claimed_when_verified(self):
        f = self.fixture()
        self.assertEqual(f.run(command='jack_recorder'), 0, f.log())
        (server,) = f.servers()
        (attempt,) = self.attempts(f)
        # A mixer restart without a JACK restart: nothing stopped or started.
        self.assertEqual(f.run(command='jack_recorder'), 0, f.log())
        self.assertEqual(f.servers(), [server])
        self.assertEqual(self.attempts(f), [attempt])
        self.assertEqual(f.lines('kills'), [f'-TERM {ORIGINAL}'])
        self.assertIn(f'JACK recorder active: pid {server}', f.log())
        # Another generation of that pid (start time) is not the recorder's.
        pointer = (f.recorder / 'active').read_text().split()
        (f.recorder / 'active').write_text(f'{pointer[0]} {pointer[1]} 1\n')
        self.assertNotEqual(f.run(command='jack_recorder'), 0)
        self.assertEqual(self.status(f), 'present-unverified')
        (f.recorder / 'active').write_text(' '.join(pointer) + '\n')
        # Without its readiness evidence the same server is only "present".
        (attempt / 'ready.snapshot').unlink()
        self.assertNotEqual(f.run(command='jack_recorder'), 0)
        self.assertEqual(f.servers(), [server])
        self.assertEqual(self.status(f), 'present-unverified')
        self.assertIn('cannot be verified; left as it is', f.log())

    def test_an_unverified_preload_is_never_doubled(self):
        f = self.fixture()
        with (f.proc / str(ORIGINAL) / 'maps').open('a') as maps:
            maps.write(f'7f00-7f01 r-xp 0 0:0 0 {f.home / "librc2fx_jackerr.so"}\n')
        self.assertNotEqual(f.run(command='jack_recorder'), 0)
        self.assert_untouched(f)
        self.assertEqual(self.status(f), 'present-unverified')

    def test_too_much_evidence_starts_no_recorder_and_deletes_nothing(self):
        # Every byte under the root counts, logs and snapshots included.
        f = self.fixture(recorder_evidence_limit=1000)
        f.recorder.mkdir(mode=0o700)
        old = f.recorder / 'run.earlier'
        old.mkdir()
        (old / 'jackd.log').write_bytes(b'l' * 2000)
        before = {p: p.read_bytes() for p in f.recorder.rglob('*') if p.is_file()}
        self.assertNotEqual(f.run(command='jack_recorder'), 0)
        self.assert_untouched(f)
        self.assertEqual(self.status(f), 'quota')
        self.assertIn('nothing deleted', f.log())
        self.assertEqual(self.attempts(f), [old], 'no new attempt')
        for path, content in before.items():
            self.assertEqual(path.read_bytes(), content)

    def test_status_reports_the_recorder_state(self):
        f = self.fixture()
        self.assertEqual(f.run(command='jack_recorder'), 0, f.log())
        f.run(command='recorder_status_line')
        self.assertIn('JACK recorder: active pid', f.result.stdout)
        self.assertIn('recorder_status_line', SCRIPT.split('\nstatus() {', 1)[1].split('\n}\n', 1)[0])


class LaunchOrderTests(unittest.TestCase):
    """Where launch() reaches the recorder (static: launch needs a device)."""

    def test_stock_starts_and_first_boot_exit_before_the_recorder(self):
        body = SCRIPT.split('\nlaunch() {\n', 1)[1].split('\n}\n', 1)[0]
        logging = body.index('jack_recorder')
        for guard in ("test -e /opt/rc2fx/disabled", "test -e /usr/bin/first-boot",
                      'sha256sum -c SHA256SUMS', "say 'Stock mixer started: the launch plan failed'"):
            self.assertLess(body.index(guard), logging, guard)
        self.assertEqual(body.count('exec "$stock_mixer" "$@"'), 2)
        self.assertLess(body.rindex('exec "$stock_mixer" "$@"'), logging)
        self.assertNotIn('rm -rf "$recorder_dir', SCRIPT)
        self.assertNotIn('bb_prune', SCRIPT, 'no evidence pruning anywhere')


if __name__ == '__main__':
    unittest.main()
