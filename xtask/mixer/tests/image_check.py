#!/usr/bin/env python3
"""Host-only tests of rc2fx.sh image-check (no mixer).

python3 -I xtask/mixer/tests/image_check.py

Runs the real script with the image root, the other root partition, the
manifest and the data folder all in a temporary directory.
"""
import hashlib
import os
import subprocess
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).parents[1] / 'rc2fx.sh'
FILES = {
    'usr/bin/rode-init-update-env.sh': b'#!/bin/sh\n# mounts the settings partition\n',
    'etc/hostname': b'rodecaster-pro-ii\n',
    'usr/lib/libthing.so': b'\x7fELF' + b'x' * 5000,
}


def sha(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


class ImageCheck(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        base = Path(self.tmp.name)
        self.root, self.other, self.data = base / 'root', base / 'other', base / 'data'
        self.logs = base / 'logs'
        for d in (self.data, self.logs):
            d.mkdir()
        for top in (self.root, self.other):
            for name, data in FILES.items():
                path = top / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(data)
        self.manifest = base / 'image.sha256'
        self.write_manifest(FILES)

    def tearDown(self):
        self.tmp.cleanup()

    def write_manifest(self, files):
        self.manifest.write_text(''.join(f'{sha(d)}  {n}\n' for n, d in files.items()))

    def run_check(self):
        env = dict(os.environ,
                   RC2FX_IMAGE_MANIFEST=str(self.manifest), RC2FX_IMAGE_ROOT=str(self.root),
                   RC2FX_OTHER_ROOT=str(self.other), RC2FX_DATA=str(self.data),
                   RC2FX_LOGS=str(self.logs))
        out = subprocess.run(['/bin/bash', str(SCRIPT), 'image-check'], env=env,
                             capture_output=True, text=True, timeout=60)
        self.assertEqual(out.returncode, 0, out.stdout + out.stderr)
        record = (self.data / 'image-check').read_text().splitlines()
        self.assertEqual(oct((self.data / 'image-check').stat().st_mode & 0o777), '0o600')
        return record, out.stdout

    def test_a_matching_image_passes(self):
        record, _ = self.run_check()
        self.assertIn('status=ok', record)
        self.assertIn(f'manifest={sha(self.manifest.read_bytes())}', record)
        self.assertFalse(any(r.startswith(('repaired=', 'damaged=')) for r in record))

    def test_a_zero_filled_file_is_restored_in_place_from_the_other_root(self):
        name = 'usr/bin/rode-init-update-env.sh'
        target = self.root / name
        target.write_bytes(b'\0' * len(FILES[name]))
        inode = target.stat().st_ino
        record, out = self.run_check()
        self.assertEqual(target.read_bytes(), FILES[name])
        self.assertEqual(target.stat().st_ino, inode, 'same size rewrites the same file')
        self.assertIn('status=repaired', record)
        self.assertIn(f'repaired={name}', record)
        self.assertIn('Repaired from the other root partition', out)

    def test_a_missing_or_resized_file_is_copied_back(self):
        (self.root / 'etc/hostname').unlink()
        (self.root / 'usr/lib/libthing.so').write_bytes(b'short')
        record, _ = self.run_check()
        for name in ('etc/hostname', 'usr/lib/libthing.so'):
            self.assertEqual((self.root / name).read_bytes(), FILES[name])
            self.assertIn(f'repaired={name}', record)
        self.assertEqual(list(self.root.rglob('*.rc2fx-repair')), [])

    def test_a_file_whose_other_copy_differs_is_reported_and_left_alone(self):
        name = 'usr/lib/libthing.so'
        (self.root / name).write_bytes(b'\0' * len(FILES[name]))
        (self.other / name).write_bytes(b'another firmware version')
        record, _ = self.run_check()
        self.assertEqual((self.root / name).read_bytes(), b'\0' * len(FILES[name]))
        self.assertIn('status=damaged', record)
        self.assertIn(f'damaged={name}', record)

    def test_a_path_outside_the_root_is_never_written(self):
        outside = Path(self.tmp.name) / 'outside'
        files = dict(FILES)
        files['../outside'] = b'not ours'
        (self.other.parent / 'outside').write_bytes(b'kept')
        self.write_manifest(files)
        record, _ = self.run_check()
        self.assertEqual(outside.read_bytes(), b'kept')
        self.assertIn('damaged=../outside', record)

    def test_damage_is_reported_and_a_later_check_after_repair_passes(self):
        name = 'etc/hostname'
        (self.root / name).write_bytes(b'\0' * len(FILES[name]))
        self.assertIn('status=repaired', self.run_check()[0])
        self.assertIn('status=ok', self.run_check()[0])

    def test_a_root_too_full_to_record_anything_still_gets_its_repair(self):
        # A damaged image filled the mixer's root within seconds of starting:
        # nothing new can be written there, but a file can be rewritten in place.
        name = 'usr/bin/rode-init-update-env.sh'
        (self.root / name).write_bytes(b'\0' * len(FILES[name]))
        self.data.chmod(0o500)
        try:
            env = dict(os.environ,
                       RC2FX_IMAGE_MANIFEST=str(self.manifest), RC2FX_IMAGE_ROOT=str(self.root),
                       RC2FX_OTHER_ROOT=str(self.other), RC2FX_DATA=str(self.data),
                       RC2FX_LOGS=str(self.logs))
            out = subprocess.run(['/bin/bash', str(SCRIPT), 'image-check'], env=env,
                                 capture_output=True, text=True, timeout=60)
        finally:
            self.data.chmod(0o700)
        self.assertEqual(out.returncode, 0, out.stdout + out.stderr)
        self.assertEqual((self.root / name).read_bytes(), FILES[name])
        self.assertIn('Could not record the result', out.stdout)
        self.assertFalse((self.data / 'image-check').exists())


if __name__ == '__main__':
    unittest.main()
