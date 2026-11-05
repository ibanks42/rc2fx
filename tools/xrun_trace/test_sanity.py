"""Public attribution must refuse plausible-looking but distorted kernel time."""
import unittest
from analyze import analyze

AUDIT={'clientTiming':{'registrations':[{'name':'usb1out','tid':42}]}}
def periodic(distorted=False):
    rows=[]
    for index in range(100):
        t=10+index*.002333333
        if distorted:t=10+(index//12)*.028+(index%12)*.00008
        rows.append(f'jackd-100 [002] d..2 {t:.6f}: sched_switch: prev_comm=idle prev_pid=0 prev_prio=120 prev_state=R ==> next_comm=jackd next_pid=100 next_prio=48')
        rows.append(f'main-55 [002] d..2 {t+.0007:.6f}: sched_waking: comm=USBOutJckThread pid=42 prio=49 target_cpu=001')
    rows.append(f'jackd-100 [002] d..2 {t+.002:.6f}: fault: kind=1 client="usb1out" mono_ns={int((t+.002)*1e9)}')
    return '\n'.join(rows)

class Sanity(unittest.TestCase):
    def test_plateau_and_jump_clock_cannot_produce_scheduler_attribution(self):
        result=analyze(periodic(True),AUDIT)
        self.assertEqual(result['classification'],'invalid-kernel-timestamps')
        self.assertFalse(result['kernelAttributionValid'])
        self.assertNotIn('callback',result)
        self.assertNotIn('upstreamMainCallback',result)
    def test_normal_cadence_passes_timing_gate(self):
        result=analyze(periodic(),AUDIT)
        self.assertTrue(result['kernelAttributionValid'])
        self.assertEqual(result['kernelTimingSanity']['jackDriver']['inBandFraction'],1)
    def test_tiny_capture_cannot_claim_a_validated_clock(self):
        result=analyze(periodic().splitlines()[-1],AUDIT)
        self.assertFalse(result['kernelAttributionValid'])
