import unittest
from analyze import _attribute as analyze
# Synthetic mechanism fixtures bypass cadence validation; test_sanity covers the public gate.

AUDIT={'clientTiming':{'registrations':[{'name':'usb1out','tid':42}]}}
def line(t,event,data,tid=42,cpu=1,comm='USBOutJckThread'):
    return f'{comm}-{tid} [{cpu:03}] d..2 {t:.6f}: {event}: {data}\n'
def switch(t,prev,state,next,comm='competitor'):
    return line(t,'sched_switch',f'prev_comm=USBOutJckThread prev_pid={prev} prev_prio=49 prev_state={state} ==> next_comm={comm} next_pid={next} next_prio=1',prev)
def fault(t):return line(t,'fault','(0xabcd) fmt="JackEngine::XRun: client %s finished after current callback" client="usb1out"',100)
class Analyze(unittest.TestCase):
    def test_wakeup_logged_after_target_runs_is_not_a_preemption(self):
        text=line(1,'callback_enter','')+line(1.0001,'sched_wakeup','comm=USBOutJckThread pid=42 prio=49 target_cpu=001',100)+line(1.0004,'callback_exit','')+fault(1.0005)
        result=analyze(text,AUDIT)
        self.assertEqual(result['callback']['runnableDelayUs'],0)

    def test_external_client_identity_comes_from_its_waited_semaphore(self):
        text=line(.999,'sys_enter','NR 98 (abcd0000, 0, 0, 0, 0, 0)',55)+line(1,'sys_exit','NR 98 = 0',55)+line(1.0001,'sched_switch','prev_comm=external prev_pid=55 prev_prio=54 prev_state=R ==> next_comm=known-hog next_pid=77 next_prio=20',55)+line(1.004,'fault','kind=1 client="sip-audio-out"',100)
        result=analyze(text,{},client_maps={'maps':'abcd0000-abcd1000 rw-s 0 0 0 /dev/shm/jack_sem.0_default_sip-audio-out'})
        self.assertEqual(result['classification'],'runnable-delay')
        self.assertEqual(result['callback']['tid'],55)
        self.assertEqual(result['callback']['competitors'][0]['comm'],'known-hog')
    def test_late_usb_entry_preserves_the_upstream_budget_evidence(self):
        text=switch(.999,0,'R',42,'USBOutJckThread')+fault(1.003)
        flight={'clientTiming':{'flightFrozenAtNs':1003100000,'registrations':[{'tid':42,'flight':{'activeStartNs':0,'recent':{'rows':[[1002500000,1003200000,1000000000,2333333]],'dropped':0,'unavailableInWindow':0}}}]}}
        result=analyze(text,AUDIT,flight_audit=flight)
        self.assertEqual(result['classification'],'upstream-cycle-budget-exhausted')
        self.assertGreater(result['callback']['entryPositionUs'],2300)

    def test_global_trace_epoch_is_aligned_at_fault_hook(self):
        text=switch(.999,0,'R',77,'known-hog')+line(1,'sched_wakeup','comm=USBOutJckThread pid=42 prio=49 target_cpu=001',100)+line(1.003,'fault','kind=1 client="usb1out" mono_ns=1143000000',100)
        result=analyze(text,AUDIT)
        self.assertAlmostEqual(result['clockAlignment']['offsetSeconds'],.14)
        self.assertAlmostEqual(result['fault']['time'],1.143)
        self.assertEqual(result['classification'],'runnable-delay')

    def test_deadline_before_callback_entry_reports_pending_dispatch(self):
        text=switch(.999,0,'R',77,'known-hog')+line(1,'sched_wakeup','comm=USBOutJckThread pid=42 prio=49 target_cpu=001',100)+fault(1.003)
        result=analyze(text,AUDIT)
        self.assertEqual(result['classification'],'runnable-delay')
        self.assertAlmostEqual(result['pendingDispatch']['runnableDelayUs'],3000)
        self.assertEqual(result['callback']['competitors'][0]['comm'],'known-hog')

    def test_matches_published_jack_fault_to_frozen_native_history(self):
        text=switch(.999,0,'R',42,'USBOutJckThread')+switch(1.0001,42,'R+',77,'known-hog')+switch(1.004,77,'S',42,'USBOutJckThread')+line(1.00425,'sys_enter','NR 98 (0, 0, 0, 0, 0, 0)',100)
        flight={'clientTiming':{'flightFrozenAtNs':1004500000,'registrations':[{'tid':42,'flight':{'activeStartNs':0,'recent':{'rows':[[1000000000,1004100000,0,2333333]],'dropped':0,'unavailableInWindow':0}}}]}}
        records=[{'index':1,'kind':'clientFinishedLate','client':'usb1out','tNs':1004200000}]
        result=analyze(text,AUDIT,'',records,flight)
        self.assertEqual(result['classification'],'runnable-delay')
        self.assertEqual(result['callback']['competitors'][0]['comm'],'known-hog')
    def test_detects_delayed_entry_even_when_callback_itself_is_fast(self):
        text=switch(.9999,0,'R',77,'known-hog')+line(1,'sched_wakeup','comm=USBOutJckThread pid=42 prio=49 target_cpu=001',100)+switch(1.004,77,'R',42,'USBOutJckThread')+line(1.0041,'callback_enter','(0x100)')+line(1.0042,'callback_exit','(0x100)')+fault(1.0043)
        result=analyze(text,AUDIT)
        self.assertEqual(result['classification'],'runnable-delay')
        self.assertAlmostEqual(result['callback']['dispatch']['runnableDelayUs'],4000,delta=2)
        self.assertEqual(result['callback']['dispatch']['competitors'][0]['comm'],'known-hog')
    def test_names_competitor_that_preempted_the_callback(self):
        text=line(1,'callback_enter','(0x100)')+switch(1.0001,42,'R+',77,'known-hog')+switch(1.004,77,'S',42,'USBOutJckThread')+line(1.0041,'callback_exit','(0x100)')+fault(1.0042)
        result=analyze(text,AUDIT)
        self.assertEqual(result['classification'],'runnable-delay')
        self.assertAlmostEqual(result['callback']['runnableDelayUs'],3900,delta=2)
        self.assertEqual(result['callback']['competitors'][0]['tid'],77)
    def test_blocking_syscall_is_not_reported_as_cpu_starvation(self):
        text=line(1,'callback_enter','(0x100)')+line(1.0001,'sys_enter','NR 29 (3, 1234, 0, 0, 0, 0)')+switch(1.0002,42,'S',0,'swapper/1')+line(1.004,'sched_wakeup','comm=USBOutJckThread pid=42 prio=49 target_cpu=001',90)+switch(1.0041,0,'R',42,'USBOutJckThread')+line(1.0042,'sys_exit','NR 29 = 0')+line(1.0043,'callback_exit','(0x100)')+fault(1.0044)
        result=analyze(text,AUDIT)
        self.assertEqual(result['classification'],'blocking-syscall')
        self.assertEqual(result['callback']['syscalls'][0]['nr'],29)
        self.assertAlmostEqual(result['callback']['blockedUs'],3800,delta=2)
    def test_running_callback_cost_is_distinct(self):
        result=analyze(line(1,'callback_enter','(0x100)')+line(1.004,'callback_exit','(0x100)')+fault(1.0041),AUDIT)
        self.assertEqual(result['classification'],'on-cpu-or-irq')
    def test_missing_identity_or_callback_does_not_invent_causality(self):
        result=analyze(fault(1),AUDIT)
        self.assertEqual(result['classification'],'insufficient-capture')
        result=analyze(line(1,'callback_enter','(0x100)')+fault(1.004),{'clientTiming':{'registrations':[]}})
        self.assertEqual(result['classification'],'insufficient-capture')
    def test_loss_is_explicit_and_prevents_conclusive_classification(self):
        text=line(1,'callback_enter','(0x100)')+switch(1.0001,42,'R',77)+switch(1.004,77,'S',42)+line(1.0041,'callback_exit','(0x100)')+fault(1.0042)
        result=analyze(text,AUDIT,'commit overrun: 2\ndropped events: 0\n')
        self.assertEqual(result['classification'],'insufficient-capture')
        self.assertTrue(result['limitations'])
if __name__=='__main__':unittest.main()
