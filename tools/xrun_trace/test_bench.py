import copy
import unittest
from bench import check

class Qualification(unittest.TestCase):
    def setUp(self):
        self.base={'home':'/opt/rc2fx','bootId':'boot','uptime':100,'tracing':1,'audit':{'processPid':42,'monotonicNs':100_000_000_000,'callbacks':100,'xruns':3,'dspFaults':0,'capacityFailures':0,'planErrors':0,'controlServerErrors':0,'helper':{'late':0,'stalls':0},'clientTiming':{'flightToken':123,'flightFrozenAtNs':0}}}
        self.current=copy.deepcopy(self.base)
        self.current['audit']['callbacks']+=1
    def test_control_run_does_not_require_a_kernel_capture(self):
        self.current['tracing']=0
        self.assertEqual(check(self.base,self.base,self.current,expect_trace=False),[])
    def test_suppression_deadline_failure_cannot_be_hidden_by_zero_xruns(self):
        self.base['audit']['workers']=[{'id':'mic-1','worker':{'missedDeadlines':0}}]
        self.current['audit']['workers']=[{'id':'mic-1','worker':{'missedDeadlines':1}}]
        self.assertIn('Suppression missedDeadlines changed',check(self.base,self.base,self.current))
    def test_progress_without_faults_is_valid(self):
        self.assertEqual(check(self.base,self.base,self.current),[])
    def test_counter_fault_is_a_failure_even_if_capture_failed(self):
        self.current['audit']['xruns']+=1
        self.assertIn('xruns changed',check(self.base,self.base,self.current))
    def test_frozen_trace_is_a_failure_even_before_counter_update(self):
        self.current['tracing']=0
        self.assertIn('Fault recorder froze',check(self.base,self.base,self.current))
    def test_restarted_process_cannot_qualify_on_reset_counters(self):
        self.current['audit']['processPid']=43
        self.assertIn('Mixer restarted',check(self.base,self.base,self.current))
    def test_stalled_or_stale_telemetry_cannot_pass(self):
        self.current['audit']['callbacks']=100
        self.current['uptime']=106
        errors=check(self.base,self.base,self.current)
        self.assertIn('Stale telemetry',errors)
        self.assertIn('Callbacks stopped',errors)
