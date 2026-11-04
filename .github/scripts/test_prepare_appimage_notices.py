#!/usr/bin/env python3
"""Offline tests for prepare-appimage-notices.py: no network. One test runs
the local `git` binary on a temporary repository to check that git_source
pins the commit and archives only the requested recipe directories."""

import importlib.util
import io
import subprocess
import tarfile
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).with_name("prepare-appimage-notices.py")
spec = importlib.util.spec_from_file_location("notices", SCRIPT)
notices = importlib.util.module_from_spec(spec)
spec.loader.exec_module(notices)


def tarball(entries):
    """A tar.gz from (name, data-or-None, type) entries."""
    raw = io.BytesIO()
    with tarfile.open(fileobj=raw, mode="w:gz") as tar:
        for name, data, kind in entries:
            info = tarfile.TarInfo(name)
            info.type = kind
            if kind == tarfile.SYMTYPE:
                info.linkname = "/etc/passwd"
                tar.addfile(info)
            elif kind == tarfile.DIRTYPE:
                tar.addfile(info)
            else:
                info.size = len(data)
                tar.addfile(info, io.BytesIO(data))
    return raw.getvalue()


SHA = "a" * 128


class Paths(unittest.TestCase):
    def test_git_source_pins_commit_and_archives_only_requested_recipes(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            recipe = root / "main/zstd/APKBUILD"
            recipe.parent.mkdir(parents=True)
            recipe.write_text("pkgver=1.5.6\n")
            (root / "unrelated").write_text("not bundled")
            def git(*args):
                return subprocess.check_output(["git", "-C", tmp, *args], text=True).strip()
            git("init", "-q")
            git("add", ".")
            git("-c", "user.name=Test", "-c", "user.email=test@example.invalid",
                "commit", "-qm", "fixture")
            commit = git("rev-parse", "HEAD")
            archive = notices.git_source(tmp, commit, "aports-test", ("main/zstd",))
            self.assertEqual(notices.aport_files(archive, "main/zstd"),
                             {"APKBUILD": b"pkgver=1.5.6\n"})
            with tarfile.open(fileobj=io.BytesIO(archive)) as tar:
                self.assertFalse(any("unrelated" in name for name in tar.getnames()))
            with self.assertRaises(notices.Failure):
                notices.git_source(tmp, "HEAD", "aports-test")

    def test_unsafe_names_are_refused(self):
        for name in ["../x", "a/../../x", "/etc/passwd", "a//b", "./a", "a\\b", "", "a/."]:
            with self.assertRaises(notices.Failure, msg=name):
                notices.safe_relative(name)
        self.assertEqual(notices.safe_relative("a/b.patch"), "a/b.patch")

    def test_aport_archives_with_escaping_or_linked_entries_are_refused(self):
        good = tarball([
            ("aports-c/main/zlib", None, tarfile.DIRTYPE),
            ("aports-c/main/zlib/APKBUILD", b"pkgver=1\n", tarfile.REGTYPE),
            ("aports-c/main/zlib/fix.patch", b"--- a\n", tarfile.REGTYPE),
            ("aports-c/main/zstd/APKBUILD", b"other\n", tarfile.REGTYPE),
        ])
        self.assertEqual(
            notices.aport_files(good, "main/zlib"),
            {"APKBUILD": b"pkgver=1\n", "fix.patch": b"--- a\n"},
        )
        escaping = tarball([("aports-c/main/zlib/../../../evil", b"x", tarfile.REGTYPE)])
        with self.assertRaises(notices.Failure):
            notices.aport_files(escaping, "main/zlib")
        linked = tarball([("aports-c/main/zlib/APKBUILD", None, tarfile.SYMTYPE)])
        with self.assertRaises(notices.Failure):
            notices.aport_files(linked, "main/zlib")


class Checksums(unittest.TestCase):
    def test_a_mismatch_fails_and_names_the_source(self):
        data = b"payload"
        notices.require("sha256", data, notices.digest("sha256", data), "ok")
        with self.assertRaises(notices.Failure) as caught:
            notices.require("sha256", data + b"!", notices.digest("sha256", data), "https://x/y")
        self.assertIn("https://x/y", str(caught.exception))
        with self.assertRaises(notices.Failure):
            notices.require("sha512", data, SHA, "z")

    def test_sha512sums_are_parsed_strictly(self):
        apkbuild = f'pkgver=1.3.1\nsha512sums="\n{SHA}  zlib-1.3.1.tar.gz\n{"b" * 128}  fix.patch\n"\n'
        self.assertEqual(
            notices.parse_sums(apkbuild),
            [(SHA, "zlib-1.3.1.tar.gz"), ("b" * 128, "fix.patch")],
        )
        for bad in [
            'pkgver=1\n',
            f'sha512sums="\n{SHA[:-1]}  short\n"\n',
            f'sha512sums="\n{SHA}  two names\n"\n',
        ]:
            with self.assertRaises(notices.Failure, msg=bad):
                notices.parse_sums(bad)

    def test_recipe_pins_and_urls_come_from_the_runtime_script(self):
        script = (
            "wget https://example.org/fuse-3.15.0.tar.xz\n"
            f'echo "{"c" * 64}  fuse-3.15.0.tar.xz" | sha256sum -c -\n'
            'wget "https://example.org/archive/0.5.2.tar.gz"\n'
            f'echo "{"d" * 64}  0.5.2.tar.gz" | sha256sum -c -\n'
        )
        self.assertEqual(
            notices.recipe_pins(script),
            {"fuse-3.15.0.tar.xz": "c" * 64, "0.5.2.tar.gz": "d" * 64},
        )
        self.assertEqual(
            notices.recipe_url(script, "0.5.2.tar.gz"), "https://example.org/archive/0.5.2.tar.gz"
        )
        with self.assertRaises(notices.Failure):
            notices.recipe_url(script, "missing.tar.gz")

    def test_apkbuild_fields(self):
        apkbuild = 'pkgver=1.5.6\npkgrel=2\nlicense="BSD-3-Clause OR GPL-2.0-or-later"\n'
        self.assertEqual(notices.apkbuild_field(apkbuild, "pkgver"), "1.5.6")
        self.assertEqual(
            notices.apkbuild_field(apkbuild, "license"), "BSD-3-Clause OR GPL-2.0-or-later"
        )
        with self.assertRaises(notices.Failure):
            notices.apkbuild_field(apkbuild, "pkgdesc")


class Licences(unittest.TestCase):
    def test_licence_files_are_taken_from_the_top_of_the_source(self):
        source = tarball([
            ("zlib-1.3.1/LICENSE", b"top", tarfile.REGTYPE),
            ("zlib-1.3.1/contrib/x/LICENSE", b"nested", tarfile.REGTYPE),
        ])
        self.assertEqual(notices.member(source, "LICENSE"), b"top")
        with self.assertRaises(notices.Failure):
            notices.member(source, "COPYRIGHT")


class Bundle(unittest.TestCase):
    def test_the_bundle_is_sorted_with_fixed_metadata(self):
        files = {"b.txt": b"2", "a/x.txt": b"1"}
        with tempfile.TemporaryDirectory() as tmp:
            one, two = Path(tmp, "one.tar.gz"), Path(tmp, "two.tar.gz")
            notices.write_bundle(one, files)
            notices.write_bundle(two, dict(reversed(list(files.items()))))
            self.assertEqual(one.read_bytes(), two.read_bytes())
            with tarfile.open(one) as tar:
                members = tar.getmembers()
            self.assertEqual(
                [m.name for m in members],
                ["rc2fx-appimage-runtime-sources/a/x.txt", "rc2fx-appimage-runtime-sources/b.txt"],
            )
            self.assertTrue(all(m.mtime == 0 and m.uid == 0 and m.mode == 0o644 for m in members))


if __name__ == "__main__":
    unittest.main()
