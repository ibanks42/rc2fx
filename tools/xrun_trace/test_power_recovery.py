"""Arm, check, then run: run.py and bench.py arm power recovery before anything else, and stop if they cannot."""
import contextlib
import io
import json
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch
import bench
import run

ARMING=['bash -s -- arm-power-recovery','fw_printenv -n reboot_pwr_button_bypass']

class Reached(Exception):
    """The tool got past arming, to its first other step."""

class Mixer:
    """Stands in for run.ssh: keeps the flag, logs every command, and stops the tool at its first command after arming."""
    def __init__(self,read_back=None):self.flag='0';self.read_back=read_back;self.events=[]
    def ssh(self,command,data=None,timeout=60):
        self.events.append(command)
        if command==ARMING[0]:self.flag='1';return b''
        if command==ARMING[1]:return (self.read_back or self.flag).encode()
        raise Reached(command)
    def local(self,arguments,**kwargs):
        """Stands in for zig, pactl and pw-play: logged, and playback stops the tool."""
        self.events.append(arguments[0])
        if arguments[0]=='pw-play':raise Reached(arguments[0])
        if arguments[0]=='zig':Path(arguments[arguments.index('-o')+1]).write_bytes(b'binary')
        return json.dumps([{'name':f'alsa_output.usb-R__DE_RODECaster_Pro_II_GV0044100-00.{n}','description':''} for n in range(6)])

class PowerRecovery(unittest.TestCase):
    def tool(self,module,mixer,*arguments):
        """Run a tool's main(); returns its exit status and what it wrote to stderr."""
        with tempfile.TemporaryDirectory() as directory:
            argv=['tool',*arguments,'--out',directory+'/out','--bundle',directory]
            stderr=io.StringIO()
            with patch.object(sys,'argv',argv),patch.object(run,'ssh',mixer.ssh),patch.object(module.subprocess,'run',mixer.local),\
                    patch.object(bench.subprocess,'check_output',mixer.local),patch.object(bench.subprocess,'Popen',mixer.local),\
                    contextlib.redirect_stdout(io.StringIO()),contextlib.redirect_stderr(stderr):
                return module.main(),stderr.getvalue()

    def test_every_run_py_action_but_status_arms_and_verifies_first(self):
        for action in run.ACTIONS:
            with self.subTest(action=action):
                mixer=Mixer()
                with self.assertRaises(Reached):self.tool(run,mixer,action)
                # Reached: the action then went on to its own first step.
                if action=='status':self.assertEqual(mixer.events,['bash -s -- xrun-trace status'])
                else:self.assertEqual(mixer.events[:2],ARMING)

    def test_run_py_does_nothing_else_when_arming_fails(self):
        for action in run.ACTIONS:
            if action=='status':continue
            mixer=Mixer(read_back='0')
            with self.assertRaisesRegex(run.PowerRecoveryNotArmed,rf"`run.py {action}` was not started: power recovery could not be armed \(.*'0', not 1\)"):
                self.tool(run,mixer,action)
            self.assertEqual(mixer.events,ARMING)

    def test_bench_py_arms_before_it_lists_sinks_or_plays(self):
        for flags in [[],['--no-trace','--gauges']]:
            mixer=Mixer()
            self.assertEqual(self.tool(bench,mixer,*flags)[0],1)  # the stand-in stops it at its first stream
            self.assertEqual(mixer.events,ARMING+['pactl','pw-play'])

    def test_bench_py_aborts_when_arming_fails(self):
        mixer=Mixer(read_back='0')
        status,stderr=self.tool(bench,mixer)
        self.assertEqual(status,1)
        self.assertIn('`bench.py` was not started: power recovery could not be armed',stderr)
        self.assertEqual(mixer.events,ARMING)

if __name__=='__main__':unittest.main()
