#!/usr/bin/env python3
"""Host-only proof of what the bench gauges bind, through the real loader.

cargo build --locked --manifest-path crates/rc2fx-native/Cargo.toml
python3 xtask/mixer/tests/gauge_binding.py

Stub libasound, libsamplerate and libjack report who called them. A wrapper
shows up as a call from the audit library instead of from the program.
"""
import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).parents[3]
LIBRARY = Path(os.environ.get('RC2FX_NATIVE_LIBRARY',
                              ROOT / 'crates/rc2fx-native/target/debug/librc2fx_native.so'))
SCRIPT = (Path(__file__).parents[1] / 'rc2fx.sh').read_text()

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


@unittest.skipUnless(shutil.which('gcc') and LIBRARY.exists(), 'needs gcc and a host build of librc2fx_native.so')
class Binding(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
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

    def run_with(self, gauges):
        result = subprocess.run(['./main'], cwd=self.root, capture_output=True, text=True, timeout=20,
                                env=dict(os.environ, LD_AUDIT=str(LIBRARY), RC2FX_GAUGES=gauges))
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


class Launcher(unittest.TestCase):
    def test_the_flag_comes_only_from_the_ram_marker(self):
        self.assertIn('local client_timing=0 gauges=0\n', SCRIPT)
        self.assertIn('test -f /run/rc2fx-gauges && gauges=1\n', SCRIPT)
        self.assertIn('RC2FX_GAUGES="$gauges"', SCRIPT)


if __name__ == '__main__':
    unittest.main()
