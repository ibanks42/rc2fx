import unittest
from observation import ObservationPolicy
class Observation(unittest.TestCase):
    def test_xrun_is_recorded_without_ending_continuous_observation(self):
        p=ObservationPolicy(keep_going=True);r=p.observe(['xruns changed','Fault recorder froze'],False)
        self.assertFalse(r['stop']);self.assertEqual(r['state'],'RUNNING');self.assertTrue(r['qualificationFailed'])
    def test_later_clean_checkpoint_cannot_erase_a_fault_or_pass_at_end(self):
        p=ObservationPolicy(keep_going=True);p.observe(['xruns changed'],False)
        self.assertEqual(p.observe([],False)['observedErrors'],['xruns changed'])
        r=p.observe([],True);self.assertTrue(r['stop']);self.assertEqual(r['state'],'FAIL')
    def test_clean_full_duration_passes(self):
        p=ObservationPolicy(keep_going=True);self.assertEqual(p.observe([],True)['state'],'PASS')
    def test_fail_fast_mode_remains_available(self):
        r=ObservationPolicy().observe(['xruns changed'],False)
        self.assertTrue(r['stop']);self.assertEqual(r['state'],'FAIL')
