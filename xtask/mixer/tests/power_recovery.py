#!/usr/bin/env python3
"""Check the explicit one-shot power-recovery command without device writes."""
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = (Path(__file__).parents[1] / 'rc2fx.sh').read_text()
FUNCTION = SCRIPT.split('arm_power_recovery() {', 1)[1].split('\n}\n', 1)[0]

class PowerRecovery(unittest.TestCase):
    def run_case(self, initial, *, first_boot=False, write_fails=False):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'value').write_text(initial)
            if first_boot:
                (root / 'first-boot').touch()
            function = ('arm_power_recovery() {' + FUNCTION + '\n}').replace(
                '/usr/bin/first-boot', str(root / 'first-boot')).replace(
                '/usr/bin/production-boot', str(root / 'production-boot'))
            stub = f'''
fw_printenv() {{ test "$*" = '-n reboot_pwr_button_bypass' || return 9; cat '{root}/value'; }}
fw_setenv() {{ test "$*" = 'reboot_pwr_button_bypass 1' || return 9;
 echo write >> '{root}/writes';
 {'return 1' if write_fails else "echo 1 > '" + str(root) + "/value'"}; }}
'''
            result = subprocess.run(['bash', '-c', stub + function + '\narm_power_recovery'], capture_output=True, text=True)
            writes = (root / 'writes').read_text() if (root / 'writes').exists() else ''
            return result.returncode, writes

    def test_arms_once_and_is_idempotent(self):
        self.assertEqual(self.run_case('0'), (0, 'write\n'))
        self.assertEqual(self.run_case('1'), (0, ''))

    def test_refuses_unknown_state_and_first_boot(self):
        self.assertEqual(self.run_case('unknown'), (1, ''))
        self.assertEqual(self.run_case('0', first_boot=True), (4, ''))

    def test_write_failure_is_not_success(self):
        self.assertEqual(self.run_case('0', write_fails=True), (1, 'write\n'))

if __name__ == '__main__':
    unittest.main()
