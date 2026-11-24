#!/usr/bin/env python3
"""Host tests for `rc2fx.sh image-check` and `flash-verify` using temp directories."""

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from _common import TempDirTestCase, run_rc2fx, source_rc2fx
from _common import sha256_bytes as sha

FILES = {
    "usr/bin/rode-init-update-env.sh": b"#!/bin/sh\n# mounts the settings partition\n",
    "etc/hostname": b"rodecaster-pro-ii\n",
    "usr/lib/libthing.so": b"\x7fELF" + b"x" * 5000,
}


class ImageCheck(TempDirTestCase):
    def setUp(self):
        super().setUp()
        base = self.root
        self.image_root, self.other, self.data = (
            base / "root",
            base / "other",
            base / "data",
        )
        self.logs = base / "logs"
        for d in (self.data, self.logs):
            d.mkdir()
        for top in (self.image_root, self.other):
            for name, data in FILES.items():
                path = top / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(data)
        self.manifest = base / "image.sha256"
        self.write_manifest(FILES)
        self.evidence = base / "evidence"

    def write_manifest(self, files):
        self.manifest.write_text("".join(f"{sha(d)}  {n}\n" for n, d in files.items()))

    def run_check(self):
        env = {
            "RC2FX_IMAGE_MANIFEST": str(self.manifest),
            "RC2FX_IMAGE_ROOT": str(self.image_root),
            "RC2FX_OTHER_ROOT": str(self.other),
            "RC2FX_DATA": str(self.data),
            "RC2FX_LOGS": str(self.logs),
            "RC2FX_EVIDENCE_ROOT": str(self.evidence),
        }
        out = run_rc2fx("image-check", env=env, timeout=60)
        self.assertEqual(out.returncode, 0, out.stdout + out.stderr)
        record = (self.data / "image-check").read_text().splitlines()
        self.assertEqual(
            oct((self.data / "image-check").stat().st_mode & 0o777), "0o600"
        )
        return record, out.stdout

    def test_a_matching_image_passes(self):
        record, _ = self.run_check()
        self.assertIn("status=ok", record)
        self.assertIn(f"manifest={sha(self.manifest.read_bytes())}", record)
        self.assertFalse(any(r.startswith(("repaired=", "damaged=")) for r in record))

    def test_an_earlier_result_for_the_same_image_is_set_aside_first(self):
        # A reflash of the same RØDE version has the same manifest, so an
        # old "repaired" would otherwise answer for the new flash.
        digest = sha(self.manifest.read_bytes())
        stale = f"schema=1\nstatus=repaired\nmanifest={digest}\nrepaired=etc/hostname\n"
        (self.data / "image-check").write_text(stale)
        record, _ = self.run_check()
        self.assertIn("status=ok", record)
        self.assertFalse(any(r.startswith("repaired=") for r in record))
        self.assertEqual((self.data / "image-check.previous").read_text(), stale)

    def test_a_zero_filled_file_is_restored_in_place_from_the_other_root(self):
        name = "usr/bin/rode-init-update-env.sh"
        target = self.image_root / name
        target.write_bytes(b"\0" * len(FILES[name]))
        inode = target.stat().st_ino
        record, out = self.run_check()
        self.assertEqual(target.read_bytes(), FILES[name])
        self.assertEqual(
            target.stat().st_ino, inode, "same size rewrites the same file"
        )
        self.assertIn("status=repaired", record)
        self.assertIn(f"repaired={name}", record)
        self.assertIn("Repaired from the other root partition", out)

    def evidence_dirs(self, kind):
        return sorted(
            d
            for d in self.evidence.iterdir()
            if d.is_dir() and d.name.endswith("-" + kind)
        )

    def test_damaged_files_are_kept_as_found_before_the_repair(self):
        name = "usr/bin/rode-init-update-env.sh"
        size = len(FILES[name])
        (self.image_root / name).write_bytes(b"\0" * size)
        self.run_check()
        (found,) = self.evidence_dirs("boot")
        self.assertEqual(
            (found / "files" / name).read_bytes(),
            b"\0" * size,
            "the copy is the damaged file",
        )
        line = (found / "files.txt").read_text()
        self.assertIn(f"{name} size={size} nul={size} ", line)
        self.assertIn(f"want={sha(FILES[name])}", line)
        self.assertTrue((found / "storage.txt").exists())
        history = (self.evidence / "history.log").read_text()
        self.assertIn(f"image check at boot: 1 damaged: {name} [{found.name}]", history)
        self.assertEqual(
            (self.image_root / name).read_bytes(), FILES[name], "and then repaired"
        )

    def test_a_clean_image_keeps_no_evidence_but_notes_the_check(self):
        self.run_check()
        self.assertEqual(self.evidence_dirs("boot"), [])
        self.assertIn(
            "image check at boot: 0 damaged",
            (self.evidence / "history.log").read_text(),
        )

    def run_verify(self, partition, status="0", archive=""):
        env = {
            "RC2FX_UPDATE_PARTITION": str(partition),
            "RC2FX_DATA": str(self.data),
            "RC2FX_LOGS": str(self.logs),
            "RC2FX_EVIDENCE_ROOT": str(self.evidence),
        }
        out = run_rc2fx("flash-verify", status, archive, env=env, timeout=60)
        self.assertEqual(out.returncode, 0, out.stdout + out.stderr)

    def test_flash_verify_checks_the_new_partition_before_the_reboot(self):
        partition = self.root / "update-partition"
        for name, data in FILES.items():
            path = partition / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
        (partition / "opt/rc2fx").mkdir(parents=True)
        (partition / "opt/rc2fx/image.sha256").write_text(self.manifest.read_text())
        name = "usr/lib/libthing.so"
        damaged = FILES[name][:1000] + b"\0" * (len(FILES[name]) - 1000)
        (partition / name).write_bytes(damaged)
        self.run_verify(partition, "0")
        (found,) = self.evidence_dirs("verify")
        log = (found / "verify.log").read_text()
        self.assertIn("rode-update.sh exited 0", log)
        self.assertIn("Checked 3 files", log)
        self.assertIn(": 1 damaged", log)
        self.assertIn(
            f"{name} size={len(damaged)} nul={len(damaged) - 1000} ",
            (found / "files.txt").read_text(),
        )
        self.assertEqual((found / "files" / name).read_bytes(), damaged)
        self.assertIn(
            f"flash verify (before RØDE's reboot): 1 damaged: {name}",
            (self.evidence / "history.log").read_text(),
        )

    def test_flash_verify_rewrites_damaged_files_from_the_archive(self):
        import tarfile

        base = self.root
        partition = base / "update-partition"
        for name, data in FILES.items():
            path = partition / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
        (partition / "opt/rc2fx").mkdir(parents=True)
        (partition / "opt/rc2fx/image.sha256").write_text(self.manifest.read_text())
        # RØDE's files are stored as ./path in the archive.
        archive = base / "archive.tar.gz"
        with tarfile.open(archive, "w:gz") as tar:
            for name in FILES:
                tar.add(partition / name, arcname="./" + name)
        damaged = ["usr/bin/rode-init-update-env.sh", "usr/lib/libthing.so"]
        for name in damaged:
            (partition / name).write_bytes(b"\0" * len(FILES[name]))
        self.run_verify(partition, "0", str(archive))
        for name in damaged:
            self.assertEqual((partition / name).read_bytes(), FILES[name], name)
        (found,) = self.evidence_dirs("verify")
        self.assertIn(
            "Repair round 1: 2 rewritten, 0 still damaged",
            (found / "verify.log").read_text(),
        )
        history = (self.evidence / "history.log").read_text()
        self.assertIn("flash verify (before RØDE's reboot): 2 damaged", history)
        self.assertIn(
            "flash repair (before RØDE's reboot): 0 still damaged after round 1",
            history,
        )
        self.assertEqual(
            sorted(p.name for p in (found / "files").rglob("*") if p.is_file()),
            ["libthing.so", "rode-init-update-env.sh"],
            "the damaged copies are kept",
        )

    def test_flash_verify_without_a_manifest_only_notes_it(self):
        partition = self.root / "update-partition"
        partition.mkdir()
        self.run_verify(partition, "1")
        self.assertIn(
            "flash verify: no manifest on the new partition (rode-update.sh exit 1)",
            (self.evidence / "history.log").read_text(),
        )

    def test_a_missing_or_resized_file_is_copied_back(self):
        (self.image_root / "etc/hostname").unlink()
        (self.image_root / "usr/lib/libthing.so").write_bytes(b"short")
        record, _ = self.run_check()
        for name in ("etc/hostname", "usr/lib/libthing.so"):
            self.assertEqual((self.image_root / name).read_bytes(), FILES[name])
            self.assertIn(f"repaired={name}", record)
        self.assertEqual(list(self.image_root.rglob("*.rc2fx-repair")), [])

    def test_a_file_whose_other_copy_differs_is_reported_and_left_alone(self):
        name = "usr/lib/libthing.so"
        (self.image_root / name).write_bytes(b"\0" * len(FILES[name]))
        (self.other / name).write_bytes(b"another firmware version")
        record, _ = self.run_check()
        self.assertEqual(
            (self.image_root / name).read_bytes(), b"\0" * len(FILES[name])
        )
        self.assertIn("status=damaged", record)
        self.assertIn(f"damaged={name}", record)

    def test_a_path_outside_the_root_is_never_written(self):
        outside = self.root / "outside"
        files = dict(FILES)
        files["../outside"] = b"not ours"
        (self.other.parent / "outside").write_bytes(b"kept")
        self.write_manifest(files)
        record, _ = self.run_check()
        self.assertEqual(outside.read_bytes(), b"kept")
        self.assertIn("damaged=../outside", record)

    def test_damage_is_reported_and_a_later_check_after_repair_passes(self):
        name = "etc/hostname"
        (self.image_root / name).write_bytes(b"\0" * len(FILES[name]))
        self.assertIn("status=repaired", self.run_check()[0])
        self.assertIn("status=ok", self.run_check()[0])

    def test_a_root_too_full_to_record_anything_still_gets_its_repair(self):
        # A damaged image filled the mixer's root within seconds of starting:
        # nothing new can be written there, but a file can be rewritten in place.
        name = "usr/bin/rode-init-update-env.sh"
        (self.image_root / name).write_bytes(b"\0" * len(FILES[name]))
        self.data.chmod(0o500)
        try:
            env = {
                "RC2FX_IMAGE_MANIFEST": str(self.manifest),
                "RC2FX_IMAGE_ROOT": str(self.image_root),
                "RC2FX_OTHER_ROOT": str(self.other),
                "RC2FX_DATA": str(self.data),
                "RC2FX_LOGS": str(self.logs),
            }
            out = run_rc2fx("image-check", env=env, timeout=60)
        finally:
            self.data.chmod(0o700)
        self.assertEqual(out.returncode, 0, out.stdout + out.stderr)
        self.assertEqual((self.image_root / name).read_bytes(), FILES[name])
        self.assertIn("Could not record the result", out.stdout)
        self.assertFalse((self.data / "image-check").exists())

    def test_manifest_check_failed_continues_when_last_output_line_is_not_failed(self):
        # A non-FAILED trailing line (such as a summary warning or OK line)
        # must not return nonzero from manifest_check_failed under set -e.
        body = f"""
fake_checker() {{
  printf '%s\\n' 'a/broken.bin: FAILED' 'sha256sum: WARNING: 1 computed checksum did NOT match'
  return 1
}}
failed=()
manifest_check_failed '{self.image_root}' '{self.manifest}' fake_checker
printf 'after1:%s\\n' "${{failed[*]}}"
ok_checker() {{
  printf '%s\\n' 'etc/hostname: OK'
}}
failed=()
manifest_check_failed '{self.image_root}' '{self.manifest}' ok_checker
printf 'after2:%s\\n' "${{#failed[@]}}"
"""
        out = source_rc2fx(body)
        self.assertEqual(out.returncode, 0, out.stdout + out.stderr)
        self.assertEqual(out.stdout.splitlines(), ["after1:a/broken.bin", "after2:0"])


if __name__ == "__main__":
    unittest.main()
