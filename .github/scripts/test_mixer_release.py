"""Run with: python3 .github/scripts/test_mixer_release.py."""

import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / ".github/scripts/mixer_release.py"
SPEC = importlib.util.spec_from_file_location("mixer_release", SCRIPT)
mixer_release = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(mixer_release)

IDENTITY = '{"schemaVersion":1,"version":"0.4.2","protocolVersion":1}\n'
CARGO_TOML = """\
[workspace]
members = ["crates/rc2fx-device", "crates/rc2fx-firmware"]

[workspace.package]
version = "0.1.0"
"""
CARGO_LOCK = """\
version = 4

[[package]]
name = "rc2fx-device"
version = "0.1.0"
dependencies = ["serde 1.0.0"]

[[package]]
name = "rc2fx-firmware"
version = "0.1.0"
dependencies = []

[[package]]
name = "serde"
version = "1.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "aaa"
dependencies = []
"""
NATIVE_LOCK = """\
version = 4

[[package]]
name = "rc2fx-native"
version = "0.1.0"
dependencies = []
"""


class MixerReleaseTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.env = {k: v for k, v in os.environ.items() if k not in ("ACT", "GITHUB_OUTPUT")}
        self.put("config/mixer-release.json", IDENTITY)
        self.put("config/firmware-profiles.json", "{}\n")
        self.put("Cargo.toml", CARGO_TOML)
        self.put("Cargo.lock", CARGO_LOCK)
        self.put("crates/rc2fx-device/Cargo.toml",
                 '[package]\nname = "rc2fx-device"\nversion.workspace = true\n')
        self.put("crates/rc2fx-firmware/Cargo.toml",
                 '[package]\nname = "rc2fx-firmware"\nversion.workspace = true\n')
        self.put("crates/rc2fx-native/Cargo.toml",
                 '[package]\nname = "rc2fx-native"\nversion.workspace = true\n')
        self.put("crates/rc2fx-native/Cargo.lock", NATIVE_LOCK)
        self.put("about.toml", "[about]\n")
        self.put("LICENSE-MIT", "MIT\n")
        self.put("THIRD_PARTY_NOTICES.md", "# Notices\n")
        for name in mixer_release.SCRIPTS:
            self.put(f"xtask/mixer/{name}", f"#!/bin/sh\n# {name}\n")
        self.git("init", "-b", "main")
        self.git("config", "user.name", "Test")
        self.git("config", "user.email", "test@example.invalid")
        self.git("add", ".")
        self.git("commit", "-m", "Initial mixer")

        self.staged = self.root / "staged"
        previous = mixer_release.OUTPUT
        mixer_release.OUTPUT = self.staged
        self.addCleanup(setattr, mixer_release, "OUTPUT", previous)

    def put(self, name, text):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)

    def git(self, *args):
        return subprocess.check_output(["git", *args], cwd=self.root, env=self.env, text=True).strip()

    def members(self):
        members = [(f"runtime/{name}", f"runtime {name}\n".encode())
                   for name in mixer_release.RUNTIME]
        members += [(f"xtask/mixer/{name}", f"script {name}\n".encode())
                    for name in mixer_release.SCRIPTS]
        members += [
            ("LICENSES.html", b"<html>notices</html>"),
            ("config/mixer-release.json", (self.root / "config/mixer-release.json").read_bytes()),
            ("config/firmware-profiles.json", (self.root / "config/firmware-profiles.json").read_bytes()),
        ]
        return members

    def archive(self, members):
        path = self.root / "rc2fx-mixer-0.4.2.tar.gz"
        with tarfile.open(path, "w:gz") as archive:
            for name, data in members:
                member = tarfile.TarInfo(name)
                member.size = len(data)
                member.mode = 0o644
                archive.addfile(member, io.BytesIO(data))
        return path

    def metadata(self, archive):
        return {
            "identity": json.loads(IDENTITY),
            "sha256": hashlib.sha256(archive.read_bytes()).hexdigest(),
            "sourceSha256": mixer_release.source_digest(self.root),
        }

    def valid(self):
        archive = self.archive(self.members())
        return archive, self.metadata(archive)

    def test_local_version_churn_keeps_the_source_digest(self):
        before = mixer_release.source_digest(self.root)
        self.put("Cargo.toml", CARGO_TOML.replace('0.1.0', '0.9.0'))
        self.put("Cargo.lock", CARGO_LOCK.replace('0.1.0', '0.9.0'))
        self.put("crates/rc2fx-device/Cargo.toml",
                 '[package]\nname = "rc2fx-device"\nversion = "0.9.0"\n')
        self.put("crates/rc2fx-native/Cargo.lock", NATIVE_LOCK.replace('0.1.0', '0.9.0'))
        self.git("add", ".")
        self.assertEqual(before, mixer_release.source_digest(self.root))

    def test_dependency_change_changes_the_source_digest(self):
        before = mixer_release.source_digest(self.root)
        self.put("Cargo.lock", CARGO_LOCK
                 .replace('"serde 1.0.0"', '"serde 1.0.1"')
                 .replace('name = "serde"\nversion = "1.0.0"',
                          'name = "serde"\nversion = "1.0.1"'))
        self.git("add", ".")
        self.assertNotEqual(before, mixer_release.source_digest(self.root))

    def test_unrelated_desktop_dependency_does_not_change_runtime_lock(self):
        extra = '\n[[package]]\nname = "desktop-only"\nversion = "1.0.0"\nsource = "registry+https://example.invalid"\n'
        before = mixer_release.runtime_lock((CARGO_LOCK + extra).encode(), ['rc2fx-device'])
        after = mixer_release.runtime_lock((CARGO_LOCK + extra.replace('1.0.0', '2.0.0')).encode(), ['rc2fx-device'])
        self.assertEqual(before, after)

    def test_runtime_dependency_change_changes_runtime_lock(self):
        roots = ['rc2fx-device', 'rc2fx-firmware']
        before = mixer_release.runtime_lock(CARGO_LOCK.encode(), roots)
        after = mixer_release.runtime_lock(CARGO_LOCK
                                           .replace('"serde 1.0.0"', '"serde 1.0.1"')
                                           .replace('name = "serde"\nversion = "1.0.0"',
                                                    'name = "serde"\nversion = "1.0.1"')
                                           .encode(), roots)
        self.assertNotEqual(before, after)

    def test_license_notice_inputs_change_the_source_digest(self):
        before = mixer_release.source_digest(self.root)
        self.put("THIRD_PARTY_NOTICES.md", "# Changed notices\n")
        self.git("add", ".")
        self.assertNotEqual(before, mixer_release.source_digest(self.root))

    def test_desktop_only_crates_are_not_mixer_inputs(self):
        before = mixer_release.source_digest(self.root)
        self.put("crates/rc2fx-app/src/lib.rs", "// desktop only\n")
        self.put("crates/rc2fx-installer/src/lib.rs", "// desktop only\n")
        self.put("crates/rc2fx-transport/src/lib.rs", "// desktop only\n")
        self.put("crates/rc2fx-bench/src/main.rs", "// desktop only\n")
        self.git("add", ".")
        self.assertEqual(before, mixer_release.source_digest(self.root))

    def test_valid_archive_is_staged(self):
        archive, metadata = self.valid()
        mixer_release.validate_archive(archive, metadata, self.root)
        self.assertTrue((self.staged / "runtime/rc2fxd").is_file())
        self.assertTrue((self.staged / "xtask/mixer/rc2fx.sh").is_file())
        self.assertTrue((self.staged / "LICENSES.html").is_file())

    def test_validation_stages_without_overwriting_checkout_files(self):
        members = []
        for name, data in self.members():
            if name == "config/mixer-release.json":
                data = (json.dumps(json.loads(IDENTITY), indent=2) + "\n").encode()
            members.append((name, data))
        archive = self.archive(members)
        config_before = (self.root / "config/mixer-release.json").read_bytes()
        script_before = (self.root / "xtask/mixer/rc2fx.sh").read_bytes()

        mixer_release.validate_archive(archive, self.metadata(archive), self.root)

        self.assertEqual((self.root / "config/mixer-release.json").read_bytes(), config_before)
        self.assertEqual((self.root / "xtask/mixer/rc2fx.sh").read_bytes(), script_before)
        self.assertEqual((self.staged / "config/mixer-release.json").read_bytes(),
                         (json.dumps(json.loads(IDENTITY), indent=2) + "\n").encode())
        self.assertEqual((self.staged / "xtask/mixer/rc2fx.sh").read_bytes(),
                         b"script rc2fx.sh\n")

    def test_checksum_mismatch_is_rejected(self):
        archive, metadata = self.valid()
        metadata["sha256"] = "0" * 64
        with self.assertRaisesRegex(ValueError, "checksum mismatch"):
            mixer_release.validate_archive(archive, metadata, self.root)

    def test_path_traversal_member_is_rejected(self):
        archive = self.archive(self.members() + [("../escape", b"no\n")])
        with self.assertRaisesRegex(ValueError, "Invalid mixer archive member"):
            mixer_release.validate_archive(archive, self.metadata(archive), self.root)

    def test_duplicate_member_is_rejected(self):
        archive = self.archive(self.members() + [("runtime/rc2fxd", b"again\n")])
        with self.assertRaisesRegex(ValueError, "Invalid mixer archive member"):
            mixer_release.validate_archive(archive, self.metadata(archive), self.root)

    def test_source_mismatch_is_rejected(self):
        archive, metadata = self.valid()
        metadata["sourceSha256"] = "0" * 64
        with self.assertRaisesRegex(ValueError, "sources changed"):
            mixer_release.validate_archive(archive, metadata, self.root)

    def test_identity_mismatch_is_rejected(self):
        archive, metadata = self.valid()
        metadata["identity"] = {**metadata["identity"], "version": "9.9.9"}
        with self.assertRaisesRegex(ValueError, "identity differs"):
            mixer_release.validate_archive(archive, metadata, self.root)

    def test_incomplete_archive_is_rejected(self):
        members = [member for member in self.members() if member[0] != "runtime/SHA256SUMS"]
        archive = self.archive(members)
        with self.assertRaisesRegex(ValueError, "Incomplete mixer archive"):
            mixer_release.validate_archive(archive, self.metadata(archive), self.root)

    def test_archive_without_licenses_is_rejected(self):
        members = [member for member in self.members() if member[0] != "LICENSES.html"]
        archive = self.archive(members)
        with self.assertRaisesRegex(ValueError, "Incomplete mixer archive"):
            mixer_release.validate_archive(archive, self.metadata(archive), self.root)


if __name__ == "__main__":
    unittest.main()
