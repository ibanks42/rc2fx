"""Tests for release.py (requires git-cliff 2.14.1 on PATH)."""

import hashlib
import io
import json
import os
import subprocess
import sys
import tarfile
import tempfile
import unittest
import warnings
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / ".github/scripts/release.py"
sys.path.insert(0, str(ROOT / ".github/scripts"))
import release as rel


def _raw_zip(entries):
    raw = io.BytesIO()
    with zipfile.ZipFile(raw, "w") as package:
        for name, data in entries:
            package.writestr(zipfile.ZipInfo(name, (2026, 1, 1, 0, 0, 0)), data)
    return raw.getvalue()


def _raw_tar_gz(entries):
    raw = io.BytesIO()
    with tarfile.open(fileobj=raw, mode="w:gz") as archive:
        for name, data in entries:
            info = tarfile.TarInfo(name)
            info.size = len(data)
            archive.addfile(info, io.BytesIO(data))
    return raw.getvalue()


def _feed(version, filename, payload, **changes):
    asset = {
        "PackageId": rel.PACK_ID,
        "Version": version,
        "Type": "Full",
        "FileName": filename,
        "SHA1": "x",
        "SHA256": hashlib.sha256(payload).hexdigest().upper(),
        "Size": len(payload),
    }
    asset.update(changes)
    return json.dumps({"Assets": [asset]}).encode()


class ReleaseTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.env = {
            k: v for k, v in os.environ.items() if k not in ("ACT", "GITHUB_OUTPUT")
        }
        self.put(
            "config/app-release.json",
            '{\n  "schemaVersion": 1,\n  "version": "0.1.0"\n}\n',
        )
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
        return subprocess.check_output(
            ["git", *args], cwd=self.root, env=self.env, text=True
        ).strip()

    def prepare(self, bump="patch", check=True):
        return subprocess.run(
            ["python3", str(SCRIPT), "prepare", "--bump", bump],
            cwd=self.root,
            env=self.env,
            check=check,
            capture_output=True,
            text=True,
        )

    def put_go_desktop(self, version="0.1.0"):
        self.put("desktop/go.mod", "module github.com/ibanks42/rc2fx/desktop\n")
        self.put(
            "desktop/installer/installer.go",
            f'package installer\n\nconst AppVersion = "{version}"\n',
        )

    def test_go_desktop_release_downloads_mixer_and_bumps_the_app_versions(self):
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
        self.assertEqual(
            json.loads((self.root / "config/app-release.json").read_text())["version"],
            "0.1.1",
        )
        version_files = ["config/app-release.json", "desktop/installer/installer.go"]
        self.assertIn(
            '= "0.1.1"\n', (self.root / "desktop/installer/installer.go").read_text()
        )
        plan = json.loads((self.root / "target/release-input/plan.json").read_text())
        self.assertLessEqual(set(version_files), set(plan["files"]))
        with tarfile.open(self.root / "target/release-input/versioned.tar") as archive:
            self.assertLessEqual(set(version_files), set(archive.getnames()))

    def test_go_version_that_differs_from_app_release_is_refused(self):
        self.git("tag", "v0.1.0")
        self.put(
            "desktop/installer/installer.go",
            'package installer\n\nconst AppVersion = "0.0.9"\n',
        )
        self.git("add", ".")
        self.git("commit", "-m", "feat: go desktop")
        result = self.prepare(check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(
            "AppVersion differs from config/app-release.json version", result.stderr
        )

    def test_real_repository_go_versions_match_app_release(self):
        import re
        import sys

        sys.path.insert(0, str(ROOT / ".github/scripts"))
        import release

        current = release.current_app_version(ROOT)
        for path, declaration in release.GO_VERSIONS.items():
            text = (ROOT / path).read_text()
            found = re.findall(
                rf'^({re.escape(declaration)} = )"([^"\n]*)"$', text, flags=re.MULTILINE
            )
            self.assertEqual(len(found), 1, f"{path}: {declaration}")
            self.assertEqual(
                found[0][1], current, f"{path}: {declaration} != {current}"
            )

    def test_existing_tag_and_changelog(self):
        self.git("tag", "v0.1.0")
        self.put("CHANGELOG.md", "# Changelog\n\nOlder entry\n")
        self.git("add", ".")
        self.git("commit", "-m", "fix: repair release")
        self.prepare()
        plan = json.loads((self.root / "target/release-input/plan.json").read_text())
        self.assertEqual(plan["tag"], "v0.1.1")
        changelog = (self.root / "CHANGELOG.md").read_text()
        self.assertLess(
            changelog.index("Repair release"), changelog.index("Older entry")
        )
        self.assertNotIn(
            "Initial project",
            (self.root / "target/release-input/release-notes.md").read_text(),
        )

    def test_subsequent_release_uses_selected_bump(self):
        self.git("tag", "v0.1.0")
        self.put("extra", "new")
        self.git("add", ".")
        self.git("commit", "-m", "feat: add feature")
        for bump, expected in (
            ("patch", "v0.1.1"),
            ("minor", "v0.2.0"),
            ("major", "v1.0.0"),
        ):
            with self.subTest(bump=bump):
                self.prepare(bump)
                plan = json.loads(
                    (self.root / "target/release-input/plan.json").read_text()
                )
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
        subprocess.run(
            ["python3", str(SCRIPT), "commit"],
            cwd=self.root,
            env=env,
            check=True,
            capture_output=True,
            text=True,
        )
        self.assertEqual(self.git("tag", "--list", "v0.1.1"), "v0.1.1")
        self.assertEqual(self.git("log", "-1", "--format=%s"), "chore(release): v0.1.1")
        self.assertEqual(
            (self.root / ".git/index.lock").read_text(), "copied host lock"
        )

    def test_missing_current_version_tag_rejected(self):
        result = self.prepare(check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Tag v0.1.0 for the current version is missing", result.stderr)

    def test_no_new_commits_rejected(self):
        self.git("tag", "v0.1.0")
        self.assertNotEqual(self.prepare(check=False).returncode, 0)

    def test_dirty_checkout_rejected(self):
        self.put("config/app-release.json", "dirty")
        self.assertNotEqual(self.prepare(check=False).returncode, 0)

    def test_existing_next_tag_rejected(self):
        self.git("tag", "v0.1.0")
        self.put("extra", "new")
        self.git("add", ".")
        self.git("commit", "-m", "fix: repair")
        self.git("tag", "v0.1.1")
        self.assertNotEqual(self.prepare(check=False).returncode, 0)


class VerifyAssetsTests(unittest.TestCase):
    VERSION = "0.2.4"

    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)

    def _valid_files(self):
        v = self.VERSION
        appimage = b"\x7fELF\x02\x01\x01\x00AI\x02rest"
        win_nupkg = _raw_zip(
            [
                ("io.github.ibanks42.rc2fx.nuspec", b"<package/>"),
                ("lib/app/rc2fx.exe", b"MZapp"),
                ("lib/app/velopack_libc.dll", b"MZdll"),
            ]
        )
        linux_nupkg = _raw_zip(
            [
                ("io.github.ibanks42.rc2fx.nuspec", b"<package/>"),
                (f"lib/app/{rel.PACK_ID}.AppImage", appimage),
            ]
        )
        mac_nupkg = _raw_zip(
            [
                ("io.github.ibanks42.rc2fx.nuspec", b"<package/>"),
                ("lib/app/Contents/MacOS/rc2fx", b"bin"),
                ("lib/app/Contents/MacOS/UpdateMac", b"upd"),
                ("lib/app/Contents/Info.plist", b"plist"),
            ]
        )
        return {
            "rc2fx-linux-x64.AppImage": appimage,
            f"rc2fx-{v}-appimage-runtime-sources.tar.gz": _raw_tar_gz(
                [("README", b"ok")]
            ),
            f"{rel.PACK_ID}-{v}-linux-full.nupkg": linux_nupkg,
            "releases.linux.json": _feed(
                v, f"{rel.PACK_ID}-{v}-linux-full.nupkg", linux_nupkg
            ),
            "rc2fx-windows-x64-setup.exe": b"MZsetup",
            f"{rel.PACK_ID}-{v}-full.nupkg": win_nupkg,
            "releases.win.json": _feed(v, f"{rel.PACK_ID}-{v}-full.nupkg", win_nupkg),
            "rc2fx-macos-universal.pkg": b"xar!installer",
            f"{rel.PACK_ID}-{v}-osx-full.nupkg": mac_nupkg,
            "releases.osx.json": _feed(
                v, f"{rel.PACK_ID}-{v}-osx-full.nupkg", mac_nupkg
            ),
        }

    def _stage(self, files, name="assets"):
        directory = self.root / name
        directory.mkdir()
        for filename, data in files.items():
            (directory / filename).write_bytes(data)
        return directory

    def test_valid_ten_assets_pass_without_writing_sums(self):
        files = self._valid_files()
        directory = self._stage(files)
        sums = rel.verify_assets(directory, self.VERSION)
        expected = "".join(
            f"{hashlib.sha256(files[n]).hexdigest()}  {n}\n" for n in sorted(files)
        )
        self.assertEqual(sums, expected)
        self.assertFalse((directory / "SHA256SUMS").exists())

    def test_missing_extra_and_symlinked_assets_are_rejected(self):
        files = self._valid_files()
        missing = dict(files)
        missing.pop("rc2fx-macos-universal.pkg")
        with self.assertRaisesRegex(ValueError, "Missing or unexpected"):
            rel.verify_assets(self._stage(missing, "missing"), self.VERSION)

        extra = {**files, "rc2fx-windows-x64.zip": b"x"}
        with self.assertRaisesRegex(ValueError, "Missing or unexpected"):
            rel.verify_assets(self._stage(extra, "extra"), self.VERSION)

        directory = self._stage(files, "symlink")
        target = self.root / "real.pkg"
        target.write_bytes(files["rc2fx-macos-universal.pkg"])
        (directory / "rc2fx-macos-universal.pkg").unlink()
        (directory / "rc2fx-macos-universal.pkg").symlink_to(target)
        with self.assertRaisesRegex(ValueError, "not a regular file"):
            rel.verify_assets(directory, self.VERSION)

    def test_corrupt_packages_and_feeds_are_rejected(self):
        v = self.VERSION
        linux_nupkg = f"{rel.PACK_ID}-{v}-linux-full.nupkg"
        win_nupkg = f"{rel.PACK_ID}-{v}-full.nupkg"
        mac_nupkg = f"{rel.PACK_ID}-{v}-osx-full.nupkg"

        def bad_case(label, mutate, pattern):
            with self.subTest(label):
                files = self._valid_files()
                mutate(files)
                with self.assertRaisesRegex(ValueError, pattern):
                    rel.verify_assets(self._stage(files, f"bad-{label}"), v)

        bad_case(
            "empty-sources",
            lambda f: f.update(
                {f"rc2fx-{v}-appimage-runtime-sources.tar.gz": _raw_tar_gz([])}
            ),
            "Empty runtime",
        )
        bad_case(
            "bad-appimage",
            lambda f: (
                f.update({"rc2fx-linux-x64.AppImage": b"not-elf"}),
                f.update(
                    {
                        linux_nupkg: _raw_zip(
                            [(f"lib/app/{rel.PACK_ID}.AppImage", b"not-elf")]
                        )
                    }
                ),
                f.update(
                    {"releases.linux.json": _feed(v, linux_nupkg, f[linux_nupkg])}
                ),
            ),
            "Invalid AppImage",
        )
        bad_case(
            "bad-exe",
            lambda f: f.update({"rc2fx-windows-x64-setup.exe": b"PK"}),
            "Invalid Windows",
        )
        bad_case(
            "bad-pkg",
            lambda f: f.update({"rc2fx-macos-universal.pkg": b"PK"}),
            "Invalid macOS",
        )
        bad_case(
            "mismatched-linux-nupkg",
            lambda f: (
                f.update(
                    {
                        linux_nupkg: _raw_zip(
                            [(f"lib/app/{rel.PACK_ID}.AppImage", b"other")]
                        )
                    }
                ),
                f.update(
                    {"releases.linux.json": _feed(v, linux_nupkg, f[linux_nupkg])}
                ),
            ),
            "holds another AppImage",
        )
        bad_case(
            "missing-win-dll",
            lambda f: (
                f.update({win_nupkg: _raw_zip([("lib/app/rc2fx.exe", b"MZ")])}),
                f.update({"releases.win.json": _feed(v, win_nupkg, f[win_nupkg])}),
            ),
            "velopack_libc.dll",
        )
        bad_case(
            "missing-mac-nuspec",
            lambda f: (
                f.update(
                    {
                        mac_nupkg: _raw_zip(
                            [
                                ("lib/app/Contents/MacOS/rc2fx", b"b"),
                                ("lib/app/Contents/MacOS/UpdateMac", b"u"),
                                ("lib/app/Contents/Info.plist", b"p"),
                            ]
                        )
                    }
                ),
                f.update({"releases.osx.json": _feed(v, mac_nupkg, f[mac_nupkg])}),
            ),
            "no .nuspec",
        )
        bad_case(
            "missing-mac-updater",
            lambda f: (
                f.update(
                    {
                        mac_nupkg: _raw_zip(
                            [
                                ("io.github.ibanks42.rc2fx.nuspec", b"<p/>"),
                                ("lib/app/Contents/MacOS/rc2fx", b"b"),
                                ("lib/app/Contents/Info.plist", b"p"),
                            ]
                        )
                    }
                ),
                f.update({"releases.osx.json": _feed(v, mac_nupkg, f[mac_nupkg])}),
            ),
            "UpdateMac",
        )
        bad_case(
            "unsafe-zip-path",
            lambda f: (
                f.update(
                    {
                        mac_nupkg: _raw_zip(
                            [
                                ("io.github.ibanks42.rc2fx.nuspec", b"<p/>"),
                                ("lib/app/../evil", b"x"),
                                ("lib/app/Contents/MacOS/rc2fx", b"b"),
                                ("lib/app/Contents/MacOS/UpdateMac", b"u"),
                                ("lib/app/Contents/Info.plist", b"p"),
                            ]
                        )
                    }
                ),
                f.update({"releases.osx.json": _feed(v, mac_nupkg, f[mac_nupkg])}),
            ),
            "unsafe path",
        )
        bad_case(
            "bad-feed-digest",
            lambda f: f.update(
                {
                    "releases.osx.json": _feed(v, mac_nupkg, b"different"),
                }
            ),
            "does not describe",
        )
        bad_case(
            "delta-feed",
            lambda f: f.update(
                {
                    "releases.win.json": _feed(
                        v, win_nupkg, f[win_nupkg], Type="Delta"
                    ),
                }
            ),
            "does not describe",
        )
        with warnings.catch_warnings():
            warnings.simplefilter("ignore")
            dup = _raw_zip(
                [
                    ("io.github.ibanks42.rc2fx.nuspec", b"<p/>"),
                    ("lib/app/Contents/MacOS/rc2fx", b"b"),
                    ("lib/app/Contents/MacOS/rc2fx", b"b"),
                    ("lib/app/Contents/MacOS/UpdateMac", b"u"),
                    ("lib/app/Contents/Info.plist", b"p"),
                ]
            )
        bad_case(
            "dup-zip-entry",
            lambda f: (
                f.update({mac_nupkg: dup}),
                f.update({"releases.osx.json": _feed(v, mac_nupkg, dup)}),
            ),
            "repeats an entry name",
        )


if __name__ == "__main__":
    unittest.main()
