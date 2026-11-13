#!/usr/bin/env python3
"""Host-only tests of install.sh's rollback keeping and space check (no mixer).

python3 -I xtask/mixer/tests/install.py

Runs the whole script under /bin/sh with RC2FX_INSTALL_ROOT at a fake root:
an upload of dummy files with a matching SHA256SUMS, a stub rc2fx-firmware,
and a stub rc2fx.sh whose `restart` exits 3 (the "starts at the next
power-on" path, so no readiness wait). `df` can be stubbed through PATH.
"""
import hashlib
import os
import subprocess
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).parents[1] / 'install.sh'
GUI_CASE = '  gui-launch) '
BINARIES = ['librc2fx_native.so', 'rc2fxd', 'rc2fx-supervisor', 'rc2fx-firmware', 'firmware-profiles.json',
            'librc2fx_gui.so', 'librc2fx_jackerr.so',
            'rc2fx_jackerr_snapshot', 'mixer-release.json']
SCRIPTS = ['rc2fx.sh', 'rc2fx-shim.sh', 'rc2fx-gui-shim.sh', 'install.sh']
# A runtime script with the GUI shim's case, and one from before it.
RC2FX_SH = f'''#!/bin/bash
echo "rc2fx.sh $*" >> "$RC2FX_INSTALL_ROOT/calls"
case "$1" in
{GUI_CASE}exit 0 ;;
  restart) exit 3 ;;
esac
exit 0
'''
OLD_RC2FX_SH = '#!/bin/bash\ncase "$1" in\n  start) exit 0 ;;\nesac\nexit 0\n'


class Install(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        self.opt = self.root / 'opt'
        self.up = self.root / 'run' / 'rc2fx-upload'
        self.up.mkdir(parents=True)
        (self.root / 'usr' / 'sbin').mkdir(parents=True)
        self.bin = self.root / 'fakebin'
        self.bin.mkdir()
        self.upload(b'x' * 4096)

    def tearDown(self):
        self.tmp.cleanup()

    def upload(self, payload: bytes, marker='new'):
        self.up.mkdir(parents=True, exist_ok=True)
        sums = ''
        for name in BINARIES:
            data = b'#!/bin/sh\nexit 0\n' if name == 'rc2fx-firmware' else payload + name.encode()
            (self.up / name).write_bytes(data)
            sums += f'{hashlib.sha256(data).hexdigest()}  {name}\n'
        (self.up / 'SHA256SUMS').write_text(sums)
        (self.up / 'rc2fx.sh').write_text(RC2FX_SH + f'# {marker}\n')
        for name in ['rc2fx-shim.sh', 'rc2fx-gui-shim.sh']:
            (self.up / name).write_text('#!/bin/sh\n')
        (self.up / 'install.sh').write_text(SCRIPT.read_text())

    def runtime(self, path: Path, script=RC2FX_SH, age=0, data=False):
        """A runtime directory as an earlier install left it."""
        path.mkdir(parents=True)
        (path / 'rc2fx.sh').write_text(script)
        (path / 'librc2fx_native.so').write_bytes(b'old')
        if data:
            (path / 'data').mkdir()
            (path / 'data' / 'settings.json').write_text('{}')
        if age:
            t = 1_700_000_000 - age
            os.utime(path / 'rc2fx.sh', (t, t))
        return path

    def retained(self, tag, **kwargs):
        return self.runtime(self.opt / f'.rc2fx-retained.{tag}', **kwargs)

    def fake_df(self, kib):
        df = self.bin / 'df'
        df.write_text('#!/bin/sh\necho "Filesystem 1024-blocks Used Available Capacity Mounted on"\n'
                      f'echo "/dev/root 476000 0 {kib} 99% /"\n')
        df.chmod(0o755)

    def install(self, *args):
        env = dict(os.environ, RC2FX_INSTALL_ROOT=str(self.root),
                   PATH=f'{self.bin}:{os.environ["PATH"]}')
        return subprocess.run(['/bin/sh', str(SCRIPT), *args], env=env, capture_output=True,
                              text=True, timeout=60)

    def retained_dirs(self):
        return sorted(p.name for p in self.opt.glob('.rc2fx-retained.*'))

    def assert_installed(self, result, marker='new'):
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('starts with the mixer at the next power-on', result.stdout)
        self.assertIn(f'# {marker}', (self.opt / 'rc2fx' / 'rc2fx.sh').read_text())
        for name in ['librc2fx_jackerr.so', 'rc2fx_jackerr_snapshot']:
            self.assertTrue((self.opt / 'rc2fx' / name).is_file(), name)
        self.assertTrue(os.access(self.opt / 'rc2fx' / 'rc2fx_jackerr_snapshot', os.X_OK),
                        'snapshot must be executable')
        self.assertFalse(list(self.opt.glob('.rc2fx-stage.*')))

    def test_the_upload_is_removed_after_an_install_and_kept_after_a_failure(self):
        (self.up / 'rc2fxd').write_bytes(b'corrupt')  # SHA256SUMS no longer matches
        self.assertNotEqual(self.install().returncode, 0)
        self.assertTrue(self.up.is_dir(), 'a failed install keeps the upload')
        self.upload(b'x' * 4096)
        self.assert_installed(self.install())
        self.assertFalse(self.up.exists(), 'the upload stays in RAM after a good install')

    def test_a_first_install_keeps_no_rollback(self):
        self.assert_installed(self.install())
        self.assertFalse((self.opt / '.rc2fx-previous').exists())
        self.assertEqual(self.retained_dirs(), [])

    def test_repeated_installs_keep_exactly_one_rollback(self):
        for n in range(4):
            self.upload(b'y' * 4096, marker=f'install {n}')
            self.assert_installed(self.install(), marker=f'install {n}')
        self.assertIn('# install 2', (self.opt / '.rc2fx-previous' / 'rc2fx.sh').read_text())
        self.assertEqual(self.retained_dirs(), [], 'no rollback beyond .rc2fx-previous')

    def test_the_old_rollback_is_deleted_only_after_the_stage_verifies(self):
        self.runtime(self.opt / 'rc2fx')
        old = self.runtime(self.opt / '.rc2fx-previous')
        (self.up / 'rc2fxd').write_bytes(b'corrupt')  # SHA256SUMS no longer matches
        result = self.install()
        self.assertNotEqual(result.returncode, 0)
        self.assertTrue((old / 'librc2fx_native.so').exists(), 'kept while the stage failed')
        self.assertEqual((self.opt / 'rc2fx' / 'librc2fx_native.so').read_bytes(), b'old')
        self.upload(b'z' * 4096)
        self.assert_installed(self.install())
        self.assertEqual((self.opt / '.rc2fx-previous' / 'librc2fx_native.so').read_bytes(), b'old')
        self.assertEqual(self.retained_dirs(), [], 'the older rollback was deleted, not retained')

    def test_recorder_payloads_are_required_and_checksum_verified(self):
        for name in ['librc2fx_jackerr.so', 'rc2fx_jackerr_snapshot']:
            self.upload(b'q' * 4096)
            (self.up / name).unlink()
            result = self.install()
            self.assertNotEqual(result.returncode, 0, f'missing {name} accepted')
            self.upload(b'q' * 4096)
            (self.up / name).write_bytes(b'corrupt')
            result = self.install()
            self.assertNotEqual(result.returncode, 0, f'corrupt {name} accepted')
            self.upload(b'q' * 4096)

    def test_leftover_copies_go_except_the_oldest_from_before_the_gui_shim(self):
        self.runtime(self.opt / 'rc2fx')
        self.runtime(self.opt / '.rc2fx-previous')
        for tag in ['gui1', 'gui2', 'gui3']:
            self.retained(tag, age=100)
        self.retained('original', script=OLD_RC2FX_SH, age=5000)
        self.retained('later-old', script=OLD_RC2FX_SH, age=3000)
        self.retained('newest-old', script=OLD_RC2FX_SH, age=10)
        (self.opt / '.rc2fx-retained.empty').mkdir()  # no rc2fx.sh: pre-GUI, but new
        self.assert_installed(self.install())
        self.assertEqual(self.retained_dirs(), ['.rc2fx-retained.original'])

    def test_a_copy_holding_saved_settings_is_never_deleted(self):
        self.runtime(self.opt / 'rc2fx')
        self.retained('settings', data=True)
        self.retained('plain')
        previous = self.runtime(self.opt / '.rc2fx-previous', data=True)
        result = self.install()
        self.assert_installed(result)
        names = self.retained_dirs()
        self.assertIn('.rc2fx-retained.settings', names)
        self.assertNotIn('.rc2fx-retained.plain', names)
        # The rollback with settings was moved aside, not deleted.
        self.assertEqual(len(names), 2, names)
        self.assertIn('holds saved settings', result.stderr)
        self.assertFalse(previous.exists() and (previous / 'data').exists())

    def test_an_original_install_as_the_old_rollback_is_retained(self):
        # The original install is the old rollback and no copy is older: kept.
        self.runtime(self.opt / 'rc2fx')
        self.runtime(self.opt / '.rc2fx-previous', script=OLD_RC2FX_SH)
        self.assert_installed(self.install())
        names = self.retained_dirs()
        self.assertEqual(len(names), 1, names)
        self.assertEqual((self.opt / names[0] / 'rc2fx.sh').read_text(), OLD_RC2FX_SH)
        # Once an older original is retained, a pre-GUI rollback goes.
        self.runtime(self.opt / '.rc2fx-previous-next', script=OLD_RC2FX_SH)
        os.replace(self.opt / '.rc2fx-previous', self.opt / '.rc2fx-previous-gone')
        os.replace(self.opt / '.rc2fx-previous-next', self.opt / '.rc2fx-previous')
        self.upload(b'x' * 4096)  # every install uploads afresh; a good one removes the upload
        self.assert_installed(self.install())
        self.assertEqual(self.retained_dirs(), names)

    def test_too_little_free_space_refuses_before_staging(self):
        self.runtime(self.opt / 'rc2fx')
        old = self.runtime(self.opt / '.rc2fx-previous')
        self.retained('leftover')
        bundle = sum((self.up / n).stat().st_size for n in BINARIES + ['SHA256SUMS'] + SCRIPTS)
        kib = (bundle + 1023) // 1024
        self.fake_df(3 * kib - 1)
        result = self.install()
        self.assertEqual(result.returncode, 1)
        self.assertIn(f'Not enough free space to install: {3 * kib - 1} KiB free', result.stderr)
        self.assertIn(f'{3 * kib} KiB needed (3 x the {kib} KiB bundle)', result.stderr)
        self.assertFalse(list(self.opt.glob('.rc2fx-stage.*')))
        self.assertTrue(old.exists(), 'the rollback is untouched')
        self.assertEqual((self.opt / 'rc2fx' / 'librc2fx_native.so').read_bytes(), b'old')
        # Leftover copies are pruned first, so a disk they filled recovers.
        self.assertEqual(self.retained_dirs(), [])
        self.fake_df(3 * kib)
        self.assert_installed(self.install())

    def test_a_pending_transaction_refuses_without_pruning(self):
        self.runtime(self.opt / 'rc2fx')
        self.retained('leftover')
        (self.opt / '.rc2fx-install-pending').touch()
        result = self.install()
        self.assertEqual(result.returncode, 1)
        self.assertIn('needs readiness checks or rollback', result.stderr)
        self.assertEqual(self.retained_dirs(), ['.rc2fx-retained.leftover'])

    def test_rollback_restores_the_kept_rollback(self):
        self.runtime(self.opt / 'rc2fx')
        self.assert_installed(self.install())
        (self.opt / '.rc2fx-install-pending').touch()
        (self.opt / 'rc2fx' / 'data').mkdir()
        result = self.install('rollback')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((self.opt / 'rc2fx' / 'librc2fx_native.so').read_bytes(), b'old')
        self.assertTrue((self.opt / 'rc2fx' / 'data').is_dir(), 'settings follow the rollback')
        self.assertFalse((self.opt / '.rc2fx-install-pending').exists())


if __name__ == '__main__':
    unittest.main()
