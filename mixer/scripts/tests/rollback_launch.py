#!/usr/bin/env python3
"""Verify that launching from /opt/.rc2fx-previous passes that copy's release
manifest to rc2fxd (so mixer_update::installed_bundle recognizes it and triggers
recover()), and still honors /opt/rc2fx/experimental-duo.
"""
from pathlib import Path
import os
import subprocess
import tempfile
import unittest

SCRIPTS = Path(__file__).parents[1]
SCRIPT = (SCRIPTS / 'rc2fx.sh').read_text()
SHIM = (SCRIPTS / 'rc2fx-shim.sh').read_text()
HOME = next(line for line in SCRIPT.splitlines() if line.startswith('home='))
ARGUMENTS = 'controller_arguments() {' + SCRIPT.split('\ncontroller_arguments() {', 1)[1].split('\n}\n', 1)[0] + '\n}\n'
DUO = SCRIPT[SCRIPT.index('# Only an opted-in Duo'):SCRIPT.index('export RC2FX_EXPERIMENTAL_DUO\n')]


class RollbackLaunch(unittest.TestCase):
    def launch(self, *, pending):
        """Run rc2fx-shim.sh against a fake /opt and return the --release path."""
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            opt, run = root / 'opt', root / 'run'
            run.mkdir()
            for name in ['rc2fx', '.rc2fx-previous']:
                bundle = opt / name
                bundle.mkdir(parents=True)
                (bundle / 'mixer-release.json').write_text('{}')
                (bundle / 'rc2fx.sh').write_text(
                    f'{HOME}\ndata=/data run=/run/rc2fx show= profile_id=\n{ARGUMENTS}'
                    'case $1 in\n  launch) controller_arguments; printf \'%s\\n\' "${controller[@]}" ;;\nesac\n')
            if pending:
                (opt / '.rc2fx-update-pending').write_text('{}')
            shim = SHIM.replace('/opt/', f'{opt}/').replace('/run/rc2fx-update/', f'{run}/') \
                .replace('/run/rc2fx-dev/', f'{root}/no-dev/').replace('/usr/bin/rc_audio_mixer', 'false')
            env = {k: v for k, v in os.environ.items() if not k.startswith('RC2FX_')}
            result = subprocess.run(['bash', '-c', shim], env=env, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            arguments = result.stdout.splitlines()
            return opt, arguments[arguments.index('--release') + 1]

    def duo(self, *, live=None, previous=None, pending=True):
        """Run rc2fx-shim.sh against a fake /opt and return (bundle_dir, RC2FX_EXPERIMENTAL_DUO)."""
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            opt, run = root / 'opt', root / 'run'
            run.mkdir()
            for name, marker in [('rc2fx', live), ('.rc2fx-previous', previous)]:
                bundle = opt / name
                bundle.mkdir(parents=True)
                if marker is not None:
                    (bundle / 'experimental-duo').write_text(marker)
                duo = DUO.replace('/opt/', f'{opt}/')
                (bundle / 'rc2fx.sh').write_text(
                    f'set -euo pipefail\n{HOME}\n{duo}\n'
                    'case $1 in\n  launch) printf \'%s %s\\n\' "$home" "$RC2FX_EXPERIMENTAL_DUO" ;;\nesac\n')
            if pending:
                (opt / '.rc2fx-update-pending').write_text('{}')
            shim = SHIM.replace('/opt/', f'{opt}/').replace('/run/rc2fx-update/', f'{run}/') \
                .replace('/run/rc2fx-dev/', f'{root}/no-dev/').replace('/usr/bin/rc_audio_mixer', 'false')
            env = {k: v for k, v in os.environ.items() if not k.startswith('RC2FX_')}
            result = subprocess.run(['bash', '-c', shim], env=env, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            home, opted_in = result.stdout.split()
            return Path(home).relative_to(opt).as_posix(), opted_in

    def test_an_opted_in_duo_keeps_its_opt_in_in_the_rollback_copy(self):
        # Rollback copy has no marker of its own; it should pick up /opt/rc2fx's.
        self.assertEqual(self.duo(live='1\n'), ('.rc2fx-previous', '1'))

    def test_the_installed_bundle_reads_its_own_marker(self):
        self.assertEqual(self.duo(live='1\n', pending=False), ('rc2fx', '1'))

    def test_a_marker_beside_the_script_still_counts(self):
        self.assertEqual(self.duo(previous='1\n'), ('.rc2fx-previous', '1'))

    def test_no_marker_or_another_value_is_no_opt_in(self):
        self.assertEqual(self.duo(), ('.rc2fx-previous', '0'))
        self.assertEqual(self.duo(live='0\n', previous='yes\n'), ('.rc2fx-previous', '0'))

    def test_pending_update_launches_the_rollback_copy_with_its_release(self):
        opt, release = self.launch(pending=True)
        self.assertEqual(release, f'{opt}/.rc2fx-previous/mixer-release.json')

    def test_committed_bundle_launches_with_the_live_release(self):
        opt, release = self.launch(pending=False)
        self.assertEqual(release, f'{opt}/rc2fx/mixer-release.json')


if __name__ == '__main__':
    unittest.main()
