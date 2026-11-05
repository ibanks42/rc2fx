import unittest
from run import validate_clock

class Clock(unittest.TestCase):
    def samples(self, stale=False):
        bounds=[];trace=[]
        for cpu in range(4):
            for n in range(8):
                before=10_000_000_000+cpu*1_000_000+n*10_000
                bounds.append(f'{cpu} {n} {before} {before+4000}')
                stamp=before+2000-140_000_000-(n*5000 if stale and cpu else 0)
                trace.append(f'task-1 [{cpu:03}] d..2 {stamp/1e9:.6f}: tracing_mark_write: RC2FX_CLOCK cpu={cpu} n={n}')
        return '\n'.join(bounds),'\n'.join(trace)
    def test_shared_epoch_offset_is_accepted(self):
        self.assertEqual(validate_clock(*self.samples())['samples'],32)
    def test_stale_per_cpu_clock_is_rejected(self):
        with self.assertRaisesRegex(RuntimeError,'inconsistent'):
            validate_clock(*self.samples(stale=True))
    def test_missing_calibration_is_rejected(self):
        with self.assertRaisesRegex(RuntimeError,'incomplete'):validate_clock('','')
