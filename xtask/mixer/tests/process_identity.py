#!/usr/bin/env python3
"""Exercise real watchdog PID/start-time checks without touching real processes."""
import subprocess
import tempfile
import unittest
from pathlib import Path
SOURCE=(Path(__file__).parents[1]/'rc2fx.sh').read_text()
HELPERS='\n'.join(next(line for line in SOURCE.splitlines() if line.startswith(name+'()')) for name in ['start_time','alive'])
MIXER='  mixer_alive() {'+SOURCE.split('  mixer_alive() {',1)[1].split('\n  }',1)[0]+'\n  }\n'
class Identity(unittest.TestCase):
    def run_fixture(self,state='S',start='123',comm='rc_audio_mixer',present=True):
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary);proc=root/'proc';task=proc/'4242';task.mkdir(parents=True)
            fields=[state]+['0']*39;fields[19]=start
            if present:(task/'stat').write_text('4242 ('+comm+') '+' '.join(fields)+'\n')
            source=(HELPERS+'\n'+MIXER).replace('/proc/',str(proc)+'/')
            body='native_pid=4242; native_start=123; alive 4242 && echo alive; mixer_alive && echo same; start_time 4242 || true'
            result=subprocess.run(['bash','-c','set -euo pipefail\n'+source+'\n'+body],capture_output=True,text=True,timeout=5)
            self.assertEqual(result.returncode,0,result.stderr)
            return result.stdout.splitlines()
    def test_matching_live_process(self):self.assertEqual(self.run_fixture(),['alive','same','123'])
    def test_zombie_cannot_pass_liveness(self):self.assertEqual(self.run_fixture(state='Z'),['123'])
    def test_reused_pid_cannot_match_owned_mixer(self):self.assertEqual(self.run_fixture(start='124'),['alive','124'])
    def test_vanished_process_is_not_alive(self):self.assertEqual(self.run_fixture(present=False),[])
    def test_parentheses_and_z_in_comm_are_not_process_state(self):self.assertEqual(self.run_fixture(comm='odd ) Z ) ( name'),['alive','same','123'])
if __name__=='__main__':unittest.main()
