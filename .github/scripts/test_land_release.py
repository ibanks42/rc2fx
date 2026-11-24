#!/usr/bin/env python3
"""Tests for land_release.py against throwaway git repositories."""

import importlib.util
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).parent
spec = importlib.util.spec_from_file_location("land_release", HERE / "land_release.py")
lr = importlib.util.module_from_spec(spec)
spec.loader.exec_module(lr)
spec = importlib.util.spec_from_file_location("release", HERE / "release.py")
sys.path.insert(0, str(HERE))
release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)


def run(cwd, *args):
    return subprocess.run(
        ["git", *args], cwd=cwd, check=True, capture_output=True, text=True
    ).stdout.strip()


class LandTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        root = Path(self.tmp.name)
        self.remote = root / "remote.git"
        run(root, "init", "-q", "--bare", "-b", "main", str(self.remote))
        self.seed = root / "seed"
        self.work = root / "work"
        for path in (self.seed, self.work):
            run(root, "clone", "-q", str(self.remote), str(path))
            run(path, "config", "user.name", "Test")
            run(path, "config", "user.email", "test@example.invalid")
        self.commit(
            self.seed,
            "base",
            {
                "config/mixer-release.json": "1\n",
                "config/app-release.json": "1\n",
                "CHANGELOG.md": "1\n",
                "src.rs": "1\n",
            },
        )
        run(self.seed, "push", "-q", "origin", "HEAD:main")
        run(self.work, "fetch", "-q", "origin")
        run(self.work, "checkout", "-q", "--detach", "origin/main")
        self.source = run(self.work, "rev-parse", "HEAD")
        self.cwd = os.getcwd()
        os.chdir(self.work)

    def tearDown(self):
        os.chdir(self.cwd)
        self.tmp.cleanup()

    def commit(self, path, subject, files):
        for name, text in files.items():
            (path / name).parent.mkdir(parents=True, exist_ok=True)
            (path / name).write_text(text)
            run(path, "add", name)
        run(path, "commit", "-q", "-m", subject)
        return run(path, "rev-parse", "HEAD")

    def land_elsewhere(self, subject, files):
        run(self.seed, "fetch", "-q", "origin")
        run(self.seed, "reset", "-q", "--hard", "origin/main")
        self.commit(self.seed, subject, files)
        run(self.seed, "push", "-q", "origin", "HEAD:main")

    def remote_main(self):
        return run(self.remote, "rev-parse", "main")

    def test_an_unmoved_main_gets_the_version_commit_as_is(self):
        ours = self.commit(
            self.work,
            "chore(mixer-release): 0.2.2",
            {"config/mixer-release.json": "2\n"},
        )
        self.assertEqual(lr.land("mixer", self.source, "origin", []), ours)
        self.assertEqual(self.remote_main(), ours)

    def test_a_mixer_release_lands_on_top_of_the_app_version_commit(self):
        ours = self.commit(
            self.work,
            "chore(mixer-release): 0.2.2",
            {"config/mixer-release.json": "2\n"},
        )
        self.land_elsewhere(
            "chore(release): v0.2.0",
            {"CHANGELOG.md": "2\n", "config/app-release.json": "2\n"},
        )
        app = self.remote_main()
        landed = lr.land("mixer", self.source, "origin", [])
        self.assertNotEqual(landed, ours)
        self.assertEqual(self.remote_main(), landed)
        self.assertEqual(run(self.work, "rev-parse", f"{landed}^"), app)
        self.assertEqual(run(self.work, "show", f"{landed}:CHANGELOG.md"), "2")
        self.assertEqual(
            run(self.work, "show", f"{landed}:config/mixer-release.json"), "2"
        )

    def test_an_app_release_lands_on_top_of_the_mixer_version_commit_and_moves_its_tag(
        self,
    ):
        self.commit(
            self.work, "chore(release): v0.2.0", {"config/app-release.json": "2\n"}
        )
        run(self.work, "tag", "v0.2.0")
        self.land_elsewhere(
            "chore(mixer-release): 0.2.2", {"config/mixer-release.json": "2\n"}
        )
        landed = lr.land("app", self.source, "origin", ["v0.2.0"])
        self.assertEqual(self.remote_main(), landed)
        self.assertEqual(run(self.remote, "rev-parse", "v0.2.0^{commit}"), landed)

    def test_an_initial_release_without_a_version_commit_only_pushes_its_tag(self):
        run(self.work, "tag", "v0.1.0")
        self.land_elsewhere(
            "chore(mixer-release): 0.2.2", {"config/mixer-release.json": "2\n"}
        )
        moved = self.remote_main()
        self.assertEqual(lr.land("app", self.source, "origin", ["v0.1.0"]), self.source)
        self.assertEqual(self.remote_main(), moved)
        self.assertEqual(run(self.remote, "rev-parse", "v0.1.0^{commit}"), self.source)

    def test_any_other_change_to_main_stops_the_release(self):
        self.commit(
            self.work,
            "chore(mixer-release): 0.2.2",
            {"config/mixer-release.json": "2\n"},
        )
        cases = [
            ("fix: something", {"src.rs": "2\n"}),
            # Right subject, but it reaches into the mixer build.
            ("chore(release): v0.2.0", {"CHANGELOG.md": "2\n", "src.rs": "3\n"}),
            # The mixer's own kind of commit is not the other workflow's.
            ("chore(mixer-release): 0.2.3", {"config/mixer-release.json": "3\n"}),
        ]
        for subject, files in cases:
            run(
                self.seed,
                "push",
                "-q",
                "-f",
                "origin",
                f"{self.source}:refs/heads/main",
            )
            self.land_elsewhere(subject, files)
            moved = self.remote_main()
            with self.subTest(subject):
                with self.assertRaises(lr.Refused):
                    lr.land("mixer", self.source, "origin", [])
                self.assertEqual(self.remote_main(), moved)

    def test_app_version_files_match_what_release_py_writes(self):
        written = {str(path) for path in release.GO_VERSIONS} | {
            str(release.APP_RELEASE),
            "CHANGELOG.md",
        }
        self.assertEqual(written, lr.APP_FILES)


if __name__ == "__main__":
    unittest.main()
