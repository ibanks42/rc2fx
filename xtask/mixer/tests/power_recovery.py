#!/usr/bin/env python3
"""Check the explicit one-shot power-recovery command without device writes."""
from pathlib import Path
import os
import shutil
import subprocess
import tempfile
import unittest

REPO = Path(__file__).parents[3]
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

class DeveloperOnlyCallers(unittest.TestCase):
    """The GUI experiments arm before anything else and stop if they cannot; start-up never arms."""
    def test_only_the_command_and_ui_preview_arm(self):
        calls = [line.strip() for line in SCRIPT.splitlines() if line.strip().startswith(('arm_power_recovery', 'arm-power'))]
        self.assertEqual(calls, ['arm_power_recovery \\', 'arm_power_recovery() {', 'arm-power-recovery) arm_power_recovery ;;'])

    def ui_preview(self, arm):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            (root / 'ui.so').touch()
            body = SCRIPT.split('\nui_preview() {', 1)[1].split('\n}\n', 1)[0].replace('in /run/*)', f'in {root}/*)')
            stubs = f'id() {{ echo 0; }}\narm_power_recovery() {{ echo armed; {arm}; }}\nui_preview() {{{body}\n}}\n'
            return subprocess.run(['bash', '-c', stubs + f"ui_preview --restart-gui '{root}/ui.so' 10"], capture_output=True, text=True)

    def test_ui_preview_arms_then_goes_on_or_stops(self):
        went_on = 'Only the qualified Pro II is admitted'  # its next check: no test host is a mixer
        armed, refused = self.ui_preview('return 0'), self.ui_preview('return 4')
        self.assertEqual((armed.stdout, went_on in armed.stderr), ('armed\n', True))
        self.assertEqual((refused.returncode, refused.stdout, went_on in refused.stderr), (1, 'armed\n', False))
        self.assertIn('not started: power recovery could not be armed', refused.stderr)

    def preview_sh(self, flag):
        """Run preview.sh in a copy of the tree; `ssh` and the build only log, and the read-back prints `flag`."""
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for path in ['experiments/device-panel/preview.sh', 'xtask/mixer/rc2fx.sh']:
                (root / path).parent.mkdir(parents=True)
                shutil.copy(REPO / path, root / path)
            (root / 'experiments/device-panel/build.sh').write_text(f'echo build >> {root}/log; exit 1\n')
            (root / 'ssh').write_text(f'#!/bin/bash\ncat > /dev/null; echo "${{!#}}" >> {root}/log; echo {flag}\n')
            (root / 'ssh').chmod(0o755)
            env = dict(os.environ, PATH=f'{root}:{os.environ["PATH"]}', RC2FX_MIXER_IP='fake.invalid', RC2FX_MIXER_KEY='', RC2FX_MIXER_KNOWN_HOSTS='')
            result = subprocess.run(['bash', str(root / 'experiments/device-panel/preview.sh'), '--restart-gui'], env=env,
                                    stdin=subprocess.DEVNULL, capture_output=True, text=True)
            return result, (root / 'log').read_text().splitlines()

    def test_preview_sh_arms_before_it_builds_or_stops(self):
        arming = ['/bin/bash -s -- arm-power-recovery', 'fw_printenv -n reboot_pwr_button_bypass']
        self.assertEqual(self.preview_sh(1)[1], arming + ['build'])
        refused, log = self.preview_sh(0)
        self.assertEqual((refused.returncode, log), (1, arming))
        self.assertIn('preview.sh was not started: power recovery could not be armed', refused.stderr)

if __name__ == '__main__':
    unittest.main()
