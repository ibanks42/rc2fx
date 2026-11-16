"""Tests for mixer_version.py: python3 .github/scripts/test_mixer_version.py"""
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import mixer_version as mv  # noqa: E402

SCRIPT = Path(__file__).with_name("mixer_version.py")


class NextVersion(unittest.TestCase):
    def test_a_bump_applies_to_the_latest_published_release(self):
        self.assertEqual(mv.next_version((0, 2, 0), (0, 2, 0), "patch"), ((0, 2, 1), True))
        self.assertEqual(mv.next_version((0, 2, 0), (0, 2, 0), "minor"), ((0, 3, 0), True))
        self.assertEqual(mv.next_version((0, 2, 0), (0, 2, 0), "major"), ((1, 0, 0), True))

    def test_a_manual_bump_that_matches_is_kept(self):
        self.assertEqual(mv.next_version((0, 2, 1), (0, 2, 0), "patch"), ((0, 2, 1), False))

    def test_a_disagreeing_checkout_is_refused(self):
        with self.assertRaisesRegex(ValueError, "says 0.2.1, but a minor bump"):
            mv.next_version((0, 2, 1), (0, 2, 0), "minor")
        with self.assertRaises(ValueError):
            mv.next_version((0, 1, 9), (0, 2, 0), "patch")

    def test_the_first_release_uses_the_checked_in_version(self):
        self.assertEqual(mv.next_version((0, 1, 1), None, "minor"), ((0, 1, 1), False))

    def test_only_stable_mixer_tags_count(self):
        tags = ["mixer-v0.1.2", "mixer-v0.2.0", "v0.9.0", "mixer-v0.10.0-rc1", "mixer-v0.10.0"]
        self.assertEqual(mv.latest(tags), (0, 10, 0))
        self.assertIsNone(mv.latest(["v0.1.6"]))


class Script(unittest.TestCase):
    def run_in(self, version, tags, bump):
        with tempfile.TemporaryDirectory() as tmp:
            repo = Path(tmp)
            git = lambda *a: subprocess.run(["git", *a], cwd=repo, check=True, capture_output=True)
            git("init", "-q")
            git("-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "x")
            for tag in tags:
                git("tag", tag)
            (repo / "config").mkdir()
            manifest = repo / "config/mixer-release.json"
            manifest.write_text(json.dumps({"schemaVersion": 1, "version": version, "protocolVersion": 1}, indent=2) + "\n")
            out = repo / "out"
            env = {"GITHUB_OUTPUT": str(out), "PATH": "/usr/bin:/bin"}
            done = subprocess.run([sys.executable, str(SCRIPT), "--bump", bump], cwd=repo, env=env,
                                  capture_output=True, text=True)
            return done, json.loads(manifest.read_text()), out.read_text() if out.exists() else ""

    def test_the_manifest_and_outputs_follow_the_bump(self):
        done, manifest, out = self.run_in("0.2.0", ["mixer-v0.2.0"], "patch")
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual(manifest["version"], "0.2.1")
        self.assertEqual(manifest["protocolVersion"], 1)
        self.assertIn("version=0.2.1\nchanged=true\n", out)

    def test_a_published_checkout_moves_past_its_release(self):
        done, manifest, out = self.run_in("0.2.1", ["mixer-v0.2.0", "mixer-v0.2.1"], "patch")
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual(manifest["version"], "0.2.2")
        self.assertIn("changed=true", out)

    def test_a_disagreeing_checkout_fails_the_run(self):
        done, manifest, _ = self.run_in("0.2.1", ["mixer-v0.2.0"], "minor")
        self.assertNotEqual(done.returncode, 0)
        self.assertEqual(manifest["version"], "0.2.1", "left as it was")


if __name__ == "__main__":
    unittest.main()
