#!/usr/bin/env python3
"""Host test verifying LD_AUDIT bench-gauge symbol binding against stub libraries.

Build first: cargo build --locked -p rc2fx-native
Run:         python3 mixer/scripts/tests/gauge_binding.py
Set RC2FX_REQUIRE_NATIVE=1 to fail instead of skipping when the library is missing.
"""
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).parents[3]
LIBRARY = Path(os.environ.get('RC2FX_NATIVE_LIBRARY',
                              ROOT / 'target/debug/librc2fx_native.so'))
SCRIPT = (Path(__file__).parents[1] / 'rc2fx.sh').read_text()
REQUIRED = os.environ.get('RC2FX_REQUIRE_NATIVE') == '1'

STUBS = r'''
#define _GNU_SOURCE
#include <dlfcn.h>
#include <stdio.h>
#define WHO(f) do { Dl_info i; dladdr(__builtin_return_address(0), &i); printf("%s %s\n", f, i.dli_fname); } while (0)
#ifdef ASOUND
long snd_pcm_avail(void *p) { WHO("snd_pcm_avail"); return 500; }
int snd_pcm_stream(void *p) { return 0; }
long snd_pcm_writei(void *p) { WHO("snd_pcm_writei"); return 1; }
#endif
#ifdef SRC
int src_process(void *a, void *b) { WHO("src_process"); return 7; }
#endif
#ifdef JACK
unsigned jack_frames_since_cycle_start(const void *c) { WHO("jack_frames_since_cycle_start"); return 100; }
#endif
'''
MAIN = r'''
#include <stdio.h>
long snd_pcm_avail(void *); long snd_pcm_writei(void *);
int src_process(void *, void *); unsigned jack_frames_since_cycle_start(const void *);
int main(void) {
  setvbuf(stdout, 0, _IOLBF, 0);
  long a = snd_pcm_avail((void *)0x10); unsigned f = jack_frames_since_cycle_start((void *)0x20);
  int s = src_process(0, 0); long w = snd_pcm_writei(0);
  printf("results %ld %u %d %ld\n", a, f, s, w);
  return 0;
}
'''
WRAPPED = {'snd_pcm_avail', 'src_process', 'jack_frames_since_cycle_start'}


@unittest.skipUnless(REQUIRED or (shutil.which('gcc') and LIBRARY.exists()),
                     'needs gcc and a host build of librc2fx_native.so')
class Binding(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        if not LIBRARY.exists():
            raise FileNotFoundError(f'{LIBRARY}: build it with cargo build -p rc2fx-native')
        cls.temp = tempfile.TemporaryDirectory()
        cls.addClassCleanup(cls.temp.cleanup)
        root = cls.root = Path(cls.temp.name)
        (root / 'stubs.c').write_text(STUBS)
        (root / 'main.c').write_text(MAIN)
        for define, name in (('ASOUND', 'libasound.so.2'), ('SRC', 'libsamplerate.so.0'), ('JACK', 'libjack.so.0')):
            subprocess.run(['gcc', '-w', '-shared', '-fPIC', f'-D{define}', f'-Wl,-soname,{name}', 'stubs.c', '-o', name, '-ldl'],
                           cwd=root, check=True)
        subprocess.run(['gcc', '-w', 'main.c', '-o', 'main', '-L.', '-l:libasound.so.2', '-l:libsamplerate.so.0',
                        '-l:libjack.so.0', f'-Wl,-rpath,{root}', '-Wl,-z,lazy'], cwd=root, check=True)

    def run_with(self, gauges, src_mode=''):
        result = subprocess.run(['./main'], cwd=self.root, capture_output=True, text=True, timeout=20,
                                env=dict(os.environ, LD_AUDIT=str(LIBRARY), RC2FX_GAUGES=gauges, RC2FX_SRC_MODE=src_mode))
        self.assertEqual(result.returncode, 0, result.stderr)
        lines = [line.split() for line in result.stdout.splitlines()]
        callers = {line[0]: line[1] for line in lines if line[0] != 'results'}
        self.assertEqual(len(callers), 4)
        return callers, next(line[1:] for line in lines if line[0] == 'results')

    def test_without_the_marker_every_symbol_stays_with_the_program(self):
        callers, results = self.run_with('0')
        self.assertEqual(set(callers.values()), {'./main'})
        self.assertEqual(results, ['500', '100', '7', '1'])

    def test_with_the_marker_only_the_three_gauged_symbols_are_wrapped(self):
        callers, results = self.run_with('1')
        self.assertEqual({name for name, caller in callers.items() if caller != './main'}, WRAPPED)
        self.assertEqual(results, ['500', '100', '7', '1'])

    def test_a_library_other_than_the_verified_one_stays_unbound(self):
        for mode in ('fast', 'verify'):
            callers, results = self.run_with('0', mode)
            self.assertEqual(set(callers.values()), {'./main'})
            self.assertEqual(results, ['500', '100', '7', '1'])

    def test_plan_with_floating_point_parameters_parses_under_ld_audit(self):
        import json
        plan = {
            'version': 1,
            'sampleRate': 48000,
            'frames': 112,
            'paths': [{
                'direction': 'input',
                'ports': ['capture_1'],
                'mode': 'processed',
                'hardware': False,
                'target': {
                    'id': 'mic-1',
                    'name': 'Mic 4',
                    'kind': 'mic',
                    'channels': 1,
                    'bypass': False,
                    'order': ['highpass', 'suppression', 'eq', 'compressor', 'gain', 'limiter'],
                    'highpass': {'enabled': True, 'frequency': 80.5},
                    'suppression': {
                        'enabled': False, 'wet': 0.75, 'lookahead': 0,
                        'gate': {
                            'strength': 0.5, 'advanced': False,
                            'threshold': 0.35, 'reduction': 12.5,
                            'attack': 5.5, 'hold': 350.25, 'release': 164.3,
                        },
                    },
                    'eq': {
                        'enabled': True, 'inputGainDb': -1.5, 'outputGainDb': 0.75,
                        'bands': [{
                            'id': 'b1', 'enabled': True, 'type': 'bell', 'mode': 'apo-dr',
                            'solo': False, 'frequency': 1000.5, 'gain': 2.25, 'q': 1.414,
                        }],
                    },
                    'compressor': {
                        'enabled': True, 'threshold': -18.5, 'ratio': 3.5,
                        'attack': 10.5, 'release': 120.25, 'makeup': 2.5,
                    },
                    'gain': {'enabled': True, 'db': -3.5},
                    'limiter': {'enabled': True, 'ceiling': -1.5, 'release': 45.5},
                },
            }],
        }
        plan_path = self.root / 'float-plan.json'
        report_path = self.root / 'float-report.json'
        plan_path.write_text(json.dumps(plan))
        env = dict(os.environ, LD_AUDIT=str(LIBRARY),
                   RC2FX_NATIVE_PROBE_PLAN=str(plan_path),
                   RC2FX_NATIVE_PROBE_REPORT=str(report_path))
        result = subprocess.run(['./main'], cwd=self.root, capture_output=True, text=True, timeout=20, env=env)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(report_path.exists(), f'no probe report written; stderr={result.stderr}')
        report = json.loads(report_path.read_text())
        self.assertEqual(report.get('planErrors'), 0, f'float plan refused: {result.stderr}')

        # Also confirm stderr diagnostics are emitted under LD_AUDIT when a plan is invalid.
        bad_path = self.root / 'bad-plan.json'
        bad_report = self.root / 'bad-report.json'
        bad_path.write_text('{"unknown": 1}')
        bad_env = dict(os.environ, LD_AUDIT=str(LIBRARY),
                       RC2FX_NATIVE_PROBE_PLAN=str(bad_path),
                       RC2FX_NATIVE_PROBE_REPORT=str(bad_report))
        bad_res = subprocess.run(['./main'], cwd=self.root, capture_output=True, text=True, timeout=20, env=bad_env)
        self.assertEqual(bad_res.returncode, 0, bad_res.stderr)
        self.assertIn('RC2FX native plan refused:', bad_res.stderr)


class Launcher(unittest.TestCase):
    def test_the_flag_comes_only_from_the_ram_marker(self):
        self.assertIn('local client_timing=0 gauges=0\n', SCRIPT)
        self.assertIn('test -f /run/rc2fx-gauges && gauges=1\n', SCRIPT)
        self.assertIn('RC2FX_GAUGES="$gauges"', SCRIPT)

    def test_the_resampler_mode_defaults_to_fast(self):
        self.assertIn('local src_mode=fast\n', SCRIPT)
        self.assertIn('RC2FX_SRC_MODE="$src_mode"', SCRIPT)


class Required(unittest.TestCase):
    # The run below sets the requirement, so it skips this test itself.
    @unittest.skipIf(REQUIRED, 'only outside a run that requires the library')
    def test_a_missing_build_fails_instead_of_skipping_when_required(self):
        missing = Path(__file__).with_name('no-such-librc2fx_native.so')
        result = subprocess.run([sys.executable, __file__, 'Binding'], capture_output=True, text=True, timeout=60,
                                env=dict(os.environ, RC2FX_REQUIRE_NATIVE='1', RC2FX_NATIVE_LIBRARY=str(missing)))
        self.assertNotEqual(result.returncode, 0, result.stderr)
        self.assertIn(missing.name, result.stderr)
        self.assertNotIn('skipped', result.stderr)


if __name__ == '__main__':
    unittest.main()
