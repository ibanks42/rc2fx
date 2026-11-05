"""Run with git-cliff 2.14.1 on PATH: python3 .github/scripts/test_release.py."""

import json
import os
from pathlib import Path
import subprocess
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
        self.put("docs/updates/source/mixer-notes.md", "# Mixer notes\n\n"
                 "## [0.1.0]\n\n- Initial fixture.\n\n"
                 "## [0.1.1]\n\n- Updated fixture.\n\n"
                 "## [0.4.2]\n\n- Separate mixer fixture.\n")
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

    def test_bootstrap_preserves_all_local_versions_and_keeps_history(self):
        self.prepare()
        plan = json.loads((self.root / "target/release-input/plan.json").read_text())
        self.assertEqual(plan["tag"], "v0.1.0")
        self.assertEqual(self.git("tag"), "")
        for filename in ("Cargo.lock", "crates/rc2fx-native/Cargo.lock", "crates/rc2fx-bench/Cargo.lock"):
            packages = tomllib.loads((self.root / filename).read_text())["package"]
            self.assertTrue(all(p["version"] == "0.1.0" for p in packages))
        self.assertIn("Initial project", (self.root / "target/release-input/release-notes.md").read_text())
        self.assertIn('version = "0.1.0"', (self.root / "crates/app/Cargo.toml").read_text())
        self.assertIn("version.workspace = true", (self.root / "crates/core/Cargo.toml").read_text())

    def test_desktop_bump_keeps_mixer_identity_and_mentions_it_in_notes(self):
        manifest = '{"schemaVersion":1,"version":"0.4.2","protocolVersion":1}\n'
        self.put("config/mixer-release.json", manifest)
        self.git("add", ".")
        self.git("commit", "-m", "feat: mixer identity")
        self.git("tag", "mixer-v0.4.2")
        self.prepare()
        self.assertEqual((self.root / "config/mixer-release.json").read_text(), manifest)
        self.assertIn("Bundled RC2FX mixer software: 0.4.2", (self.root / "target/release-input/release-notes.md").read_text())
        plan = json.loads((self.root / "target/release-input/plan.json").read_text())
        self.assertEqual(plan["tag"], "v0.1.0")
        self.assertNotIn("config/mixer-release.json", plan["files"])

    def test_bootstrap_minor(self):
        self.prepare("minor")
        self.assertIn('version = "0.1.0"', (self.root / "Cargo.toml").read_text())

    def test_download_only_app_release_is_independent_of_pending_mixer_notes(self):
        self.put("crates/rc2fx-app/Cargo.toml", '[features]\ndefault = []\nembedded = []\n')
        self.put("config/mixer-release.json", '{"version":"9.9.9","protocolVersion":1}')
        self.git("add", ".")
        self.git("commit", "-m", "feat: download mixer independently")
        self.prepare()
        notes = (self.root / "target/release-input/release-notes.md").read_text()
        self.assertIn("Mixer software is downloaded separately", notes)
        self.assertNotIn("Bundled RC2FX", notes)
        self.assertEqual(json.loads((self.root / "config/mixer-release.json").read_text())["version"], "9.9.9")

    def test_bootstrap_major(self):
        self.prepare("major")
        self.assertIn('version = "0.1.0"', (self.root / "Cargo.toml").read_text())

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

    def test_replacement_first_release_regenerates_changelog(self):
        self.put("CHANGELOG.md", "# Changelog\n\n## [0.1.0] - old\nStale entry\n")
        self.git("add", ".")
        self.git("commit", "-m", "fix: replace first release")
        self.prepare()
        changelog = (self.root / "CHANGELOG.md").read_text()
        self.assertEqual(changelog.count("## [0.1.0]"), 1)
        self.assertEqual(changelog.count("# Changelog"), 1)
        self.assertNotIn("Stale entry", changelog)
        self.assertIn("Replace first release", changelog)

    def test_mixer_warning(self):
        self.put("config/mixer-release.json", '{"version":"0.1.0"}')
        self.put("CHANGELOG.md", "# Changelog\n\n## [0.1.0] - 2026-01-01\n\n- Initial mixer.\n")
        self.git("add", ".")
        self.git("commit", "-m", "feat: add mixer manifest")
        self.git("tag", "v0.1.0")
        self.put("xtask/mixer/rc2fx.sh", "changed")
        self.git("add", ".")
        self.git("commit", "-m", "fix: change mixer startup")
        self.assertIn("::warning title=Mixer version unchanged::", self.prepare().stdout)
        self.git("restore", ".")
        self.put("config/mixer-release.json", '{"version":"0.1.1"}')
        self.git("add", ".")
        self.git("commit", "-m", "fix: bump mixer version")
        self.assertNotIn("::warning", self.prepare().stdout)

    def test_app_only_change_does_not_warn_about_mixer(self):
        self.put("config/mixer-release.json", '{"version":"0.1.0"}')
        self.git("add", ".")
        self.git("commit", "-m", "feat: add mixer manifest")
        self.git("tag", "v0.1.0")
        self.put("crates/rc2fx-app/src/settings.rs", "changed")
        self.git("add", ".")
        self.git("commit", "-m", "fix: change desktop settings")
        self.assertNotIn("::warning", self.prepare().stdout)

    def test_new_mixer_version_requires_notes_before_bumping_app(self):
        self.put("config/mixer-release.json", '{"version":"0.1.0"}')
        self.git("add", ".")
        self.git("commit", "-m", "feat: initial mixer")
        self.git("tag", "v0.1.0")
        self.put("config/mixer-release.json", '{"version":"0.2.0"}')
        self.git("add", ".")
        self.git("commit", "-m", "feat: new mixer without notes")
        result = self.prepare(check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("needs curated mixer release notes", result.stderr)
        self.assertEqual(self.git("status", "--porcelain"), "")

    def test_app_only_release_allows_unknown_legacy_mixer_notes(self):
        self.put("config/mixer-release.json", '{"version":"0.9.0"}')
        self.git("add", ".")
        self.git("commit", "-m", "feat: historical mixer without notes")
        self.git("tag", "v0.1.0")
        self.put("desktop", "changed")
        self.git("add", ".")
        self.git("commit", "-m", "fix: desktop only")
        self.prepare()

    def test_act_commit_uses_isolated_index(self):
        self.prepare()
        self.put(".git/index.lock", "copied host lock")
        env = dict(self.env, ACT="true")
        subprocess.run(["python3", str(SCRIPT), "commit"], cwd=self.root, env=env,
                       check=True, capture_output=True, text=True)
        self.assertEqual(self.git("tag"), "v0.1.0")
        self.assertEqual(self.git("log", "-1", "--format=%s"), "chore(release): v0.1.0")
        self.assertEqual((self.root / ".git/index.lock").read_text(), "copied host lock")

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
