import copy
import unittest
import json
import tempfile
from pathlib import Path
from bench import check, gauge_measurement, level, measurement, preserve_fault
from analyze import captured_flight_audit

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
    def test_report_updated_during_snapshot_is_not_stale(self):
        self.current['audit']['monotonicNs']=100_032_550_997
        self.current['uptimeAfter']=100.05
        self.assertNotIn('Stale telemetry',check(self.base,self.base,self.current))
    def test_stalled_or_stale_telemetry_cannot_pass(self):
        self.current['audit']['callbacks']=100
        self.current['uptime']=106
        errors=check(self.base,self.base,self.current)
        self.assertIn('Stale telemetry',errors)
        self.assertIn('Callbacks stopped',errors)

    def timed(self,seconds,histogram,calls,wall,native,owned):
        timing=lambda total:{'calls':calls,'totalNs':total}
        return {'audit':{'monotonicNs':seconds*1_000_000_000,'cycle':{'endPos':{'histogram':histogram}},
                         'callbackTiming':[{'callbackWall':timing(wall),'nativeWall':timing(native),'ownedWall':timing(owned)}]}}
    def test_measurement_counts_only_the_window_and_only_late_bins(self):
        base=self.timed(100,[9,9,9,9,1,1,0,0],1000,10_000_000,8_000_000,2_000_000)
        current=self.timed(1000,[9,99,999,9999,4,2,1,1],3000,1_778_000_000,1_414_000_000,364_000_000)
        result=measurement(base,current)
        self.assertEqual(result['windowSeconds'],900)
        self.assertEqual(result['slowCallbacks'],6)
        self.assertEqual(result['slowCallbacksPerHour'],24)
        self.assertEqual(result['meanCallbackWallUs'],884)
        self.assertEqual(result['meanNativeWallUs'],703)
        self.assertEqual(result['meanOwnedWallUs'],181)
    def test_measurement_without_progress_reports_no_mean(self):
        base=self.timed(100,[0]*8,1000,1,1,1)
        result=measurement(base,base)
        self.assertIsNone(result['slowCallbacksPerHour'])
        self.assertIsNone(result['meanCallbackWallUs'])

    def gauged(self,handles,past=0,bins=(0,)*8):
        return {'audit':{'gauges':{'epoch':5,'handleOverflow':0,'unattributedSrcCalls':0,'handles':handles,
                                   'vendorJob':{'pastJackdRead':past,'endPosition':{'histogram':list(bins)}}}}}
    def handle(self,direction,cycles,low,high,src=0,calls=0,epoch=5):
        return dict(client='usb1out',direction=direction,epoch=epoch,cycles=cycles,availErrors=0,lowestPosition=low,
                    highestPosition=high,srcCalls=calls,srcTotalNs=src,srcMaxCycleNs=200_000)
    def test_playback_fill_uses_the_ring_and_capture_fill_is_direct(self):
        base=self.gauged([self.handle('playback',100,500,640),self.handle('capture',100,40,90)],2,[1]*8)
        current=self.gauged([self.handle('playback',300,500,650,34_000_000,4000),self.handle('capture',300,30,90)],3,[2]*8)
        result=gauge_measurement(base,current,784)
        playback,capture=result['handles']
        self.assertEqual(playback['lowestFillFrames'],134)
        self.assertEqual(capture['lowestFillFrames'],30)
        self.assertEqual(playback['srcCallsPerCycle'],20)
        self.assertEqual(playback['srcMeanUsPerCycle'],170)
        self.assertEqual(result['vendorJob']['pastJackdRead'],1)
        self.assertEqual(result['vendorJob']['endPositionHistogram'],[1]*8)
    def test_a_handle_not_yet_at_the_epoch_is_marked_and_an_empty_one_has_no_fill(self):
        current=self.gauged([self.handle('playback',0,None,None,epoch=4)])
        row=gauge_measurement(self.gauged([]),current,784)['handles'][0]
        self.assertFalse(row['extremesFromEpoch'])
        self.assertIsNone(row['lowestFillFrames'])
        self.assertIsNone(row['srcMeanUsPerCycle'])
    def test_the_level_names_every_load_on_the_run(self):
        self.assertEqual(level(False,False),'counters')
        self.assertEqual(level(True,True),'trace+gauges')

    def test_first_fault_history_survives_later_checkpoints_and_restart(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory);capture=root/'capture';capture.mkdir()
            self.current['audit']['clientTiming']['flightFrozenAtNs']=100
            preserve_fault(root,self.base,self.current,['xruns changed'])
            first=copy.deepcopy(self.current['audit'])
            self.current['audit']['clientTiming']['flightFrozenAtNs']=0
            self.current['audit']['processPid']=43
            preserve_fault(root,self.base,self.current,['Mixer restarted'])
            (capture/'flight-token').write_text('123\n')
            (capture/'collected-audit.json').write_text(json.dumps(self.current['audit']))
            self.assertEqual(captured_flight_audit(capture),first)
            self.assertEqual(json.loads((root/'first-observed-fault.json').read_text())['audit'],first)

    def test_saved_history_from_another_capture_is_not_used(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory);capture=root/'capture';capture.mkdir()
            self.current['audit']['clientTiming']['flightFrozenAtNs']=100
            preserve_fault(root,self.base,self.current,['xruns changed'])
            (capture/'flight-token').write_text('456\n')
            (capture/'collected-audit.json').write_text(json.dumps(self.base['audit']))
            self.assertEqual(captured_flight_audit(capture),self.base['audit'])
