"""Run with git-cliff 2.14.1 on PATH: python3 .github/scripts/test_release.py."""

import json
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import tomllib
import unittest


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / ".github/scripts/release.py"


class ReleaseTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.env = {k: v for k, v in os.environ.items() if k not in ("ACT", "GITHUB_OUTPUT")}
        self.put("Cargo.toml", '[workspace]\nmembers = ["crates/app", "crates/core"]\n[workspace.package]\nversion = "0.1.0"\n')
        self.put("crates/app/Cargo.toml", '[package]\nname = "app"\nversion = "0.1.0"\n')
        self.put("crates/core/Cargo.toml", '[package]\nname = "core"\nversion.workspace = true\n')
        lock = 'version = 4\n\n[[package]]\nname = "app"\nversion = "0.1.0"\n\n[[package]]\nname = "core"\nversion = "0.1.0"\n'
        for filename in ("Cargo.lock", "crates/rc2fx-native/Cargo.lock", "crates/rc2fx-bench/Cargo.lock"):
            self.put(filename, lock)
        self.put("cliff.toml", (ROOT / "cliff.toml").read_text())
        self.put_go_desktop()
        self.git("init", "-b", "main")
        self.git("config", "user.name", "Test")
        self.git("config", "user.email", "test@example.invalid")
        self.git("add", ".")
        self.git("commit", "-m", "Initial project")

    def put(self, name, text):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)

    def git(self, *args):
        return subprocess.check_output(["git", *args], cwd=self.root, env=self.env, text=True).strip()

    def prepare(self, bump="patch", check=True):
        return subprocess.run(["python3", str(SCRIPT), "prepare", "--bump", bump], cwd=self.root,
                              env=self.env, check=check, capture_output=True, text=True)

    def put_go_desktop(self, version="0.1.0"):
        self.put("desktop/go.mod", "module github.com/ibanks42/rc2fx/desktop\n")
        self.put("desktop/installer/installer.go", f'package installer\n\nconst AppVersion = "{version}"\n')
        self.put("desktop/transport/mock.go", f'package transport\n\nconst mockRuntime = "{version}"\n')

    def test_go_desktop_release_downloads_mixer_and_bumps_the_go_versions(self):
        self.put("config/mixer-release.json", '{"version":"9.9.9","protocolVersion":1}')
        self.git("add", ".")
        self.git("commit", "-m", "feat: go desktop")
        self.git("tag", "v0.1.0")
        self.put("desktop/app/settings.go", "changed")
        self.git("add", ".")
        self.git("commit", "-m", "fix: change desktop settings")
        self.prepare()
        notes = (self.root / "target/release-input/release-notes.md").read_text()
        self.assertIn("Mixer software is downloaded separately", notes)
        self.assertNotIn("Bundled RC2FX", notes)
        go_files = ["desktop/installer/installer.go", "desktop/transport/mock.go"]
        for name in go_files:
            self.assertIn('= "0.1.1"\n', (self.root / name).read_text(), name)
        plan = json.loads((self.root / "target/release-input/plan.json").read_text())
        self.assertLessEqual(set(go_files), set(plan["files"]))
        with tarfile.open(self.root / "target/release-input/versioned.tar") as archive:
            self.assertLessEqual(set(go_files), set(archive.getnames()))
        for filename in ("Cargo.lock", "crates/rc2fx-native/Cargo.lock", "crates/rc2fx-bench/Cargo.lock"):
            packages = tomllib.loads((self.root / filename).read_text())["package"]
            self.assertTrue(all(p["version"] == "0.1.1" for p in packages), filename)
        self.assertIn('version = "0.1.1"', (self.root / "crates/app/Cargo.toml").read_text())
        self.assertIn("version.workspace = true", (self.root / "crates/core/Cargo.toml").read_text())

    def test_go_version_that_differs_from_the_workspace_is_refused(self):
        self.git("tag", "v0.1.0")
        self.put("desktop/transport/mock.go", 'package transport\n\nconst mockRuntime = "0.0.9"\n')
        self.git("add", ".")
        self.git("commit", "-m", "feat: go desktop")
        result = self.prepare(check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("mockRuntime differs from workspace version", result.stderr)

    def test_existing_tag_and_changelog(self):
        self.git("tag", "v0.1.0")
        self.put("CHANGELOG.md", "# Changelog\n\nOlder entry\n")
        self.git("add", ".")
        self.git("commit", "-m", "fix: repair release")
        self.prepare()
        plan = json.loads((self.root / "target/release-input/plan.json").read_text())
        self.assertEqual(plan["tag"], "v0.1.1")
        changelog = (self.root / "CHANGELOG.md").read_text()
        self.assertLess(changelog.index("Repair release"), changelog.index("Older entry"))
        self.assertNotIn("Initial project", (self.root / "target/release-input/release-notes.md").read_text())

    def test_subsequent_release_uses_selected_bump(self):
        self.git("tag", "v0.1.0")
        self.put("extra", "new")
        self.git("add", ".")
        self.git("commit", "-m", "feat: add feature")
        for bump, expected in (("patch", "v0.1.1"), ("minor", "v0.2.0"), ("major", "v1.0.0")):
            with self.subTest(bump=bump):
                self.prepare(bump)
                plan = json.loads((self.root / "target/release-input/plan.json").read_text())
                self.assertEqual(plan["tag"], expected)
                self.git("restore", ".")

    def test_act_commit_uses_isolated_index(self):
        self.git("tag", "v0.1.0")
        self.put("extra", "new")
        self.git("add", ".")
        self.git("commit", "-m", "fix: repair")
        self.prepare()
        self.put(".git/index.lock", "copied host lock")
        env = dict(self.env, ACT="true")
        subprocess.run(["python3", str(SCRIPT), "commit"], cwd=self.root, env=env,
                       check=True, capture_output=True, text=True)
        self.assertEqual(self.git("tag", "--list", "v0.1.1"), "v0.1.1")
        self.assertEqual(self.git("log", "-1", "--format=%s"), "chore(release): v0.1.1")
        self.assertEqual((self.root / ".git/index.lock").read_text(), "copied host lock")

    def test_missing_current_version_tag_rejected(self):
        result = self.prepare(check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Tag v0.1.0 for the current version is missing", result.stderr)

    def test_no_new_commits_rejected(self):
        self.git("tag", "v0.1.0")
        self.assertNotEqual(self.prepare(check=False).returncode, 0)

    def test_dirty_checkout_rejected(self):
        self.put("Cargo.lock", "dirty")
        self.assertNotEqual(self.prepare(check=False).returncode, 0)

    def test_existing_next_tag_rejected(self):
        self.git("tag", "v0.1.0")
        self.put("extra", "new")
        self.git("add", ".")
        self.git("commit", "-m", "fix: repair")
        self.git("tag", "v0.1.1")
        self.assertNotEqual(self.prepare(check=False).returncode, 0)


if __name__ == "__main__":
    unittest.main()
