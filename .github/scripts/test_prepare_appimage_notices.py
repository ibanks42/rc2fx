#!/usr/bin/env python3
"""Offline tests for prepare-appimage-notices.py (uses local git on a temp repo for git_source)."""

import importlib.util
import io
import re
import subprocess
import tarfile
import tempfile
import unittest
import zipfile
from pathlib import Path

SCRIPT = Path(__file__).with_name("prepare-appimage-notices.py")
spec = importlib.util.spec_from_file_location("notices", SCRIPT)
notices = importlib.util.module_from_spec(spec)
spec.loader.exec_module(notices)


def tarball(entries):
    """Build a tar.gz from (name, data, type) entries."""
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


class Runtime(unittest.TestCase):
    """Verify vendored runtime extraction and commit validation."""

    def nupkg(self, runtime):
        raw = io.BytesIO()
        with zipfile.ZipFile(raw, "w") as package:
            package.writestr(notices.VPK_RUNTIME, runtime)
        return raw.getvalue()

    def pinned(self, package, runtime):
        """Override VPK_SHA256 and RUNTIME_SHA256 with the fixture digests."""
        saved = notices.VPK_SHA256, notices.RUNTIME_SHA256
        notices.VPK_SHA256 = notices.digest("sha256", package)
        notices.RUNTIME_SHA256 = notices.digest("sha256", runtime)
        self.addCleanup(lambda: setattr(notices, "VPK_SHA256", saved[0]))
        self.addCleanup(lambda: setattr(notices, "RUNTIME_SHA256", saved[1]))

    def test_the_vendored_runtime_is_read_from_the_checked_package(self):
        runtime = (b"\x7fELF runtime https://github.com/AppImage/type2-runtime/commit/"
                   + notices.RUNTIME_COMMIT[:7].encode() + b"\0")
        package = self.nupkg(runtime)
        self.pinned(package, runtime)
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp, "vpk.nupkg")
            path.write_bytes(package)
            self.assertEqual(notices.vendored_runtime(path, None), runtime)
            path.write_bytes(package + b"tampered")
            with self.assertRaises(notices.Failure):
                notices.vendored_runtime(path, None)

    def test_a_runtime_from_another_commit_is_refused(self):
        for runtime in [
            b"https://github.com/AppImage/type2-runtime/commit/0000000",
            b"no version string",
            b"https://github.com/AppImage/type2-runtime/commit/3d17002 "
            b"https://github.com/AppImage/type2-runtime/commit/dd6cebe",
        ]:
            with self.assertRaises(notices.Failure, msg=runtime):
                notices.check_runtime_commit(runtime)

    def test_a_package_without_the_runtime_is_refused(self):
        raw = io.BytesIO()
        with zipfile.ZipFile(raw, "w") as package:
            package.writestr("vendor/other", b"x")
        package = raw.getvalue()
        self.pinned(package, b"")
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp, "vpk.nupkg")
            path.write_bytes(package)
            with self.assertRaises(notices.Failure):
                notices.vendored_runtime(path, None)

    def test_the_pins_agree_with_setup_vpk(self):
        root = SCRIPT.parent.parent.parent
        config = root / "config/velopack.json"
        self.assertTrue(config.is_file(), "config/velopack.json is missing")
        setup = SCRIPT.with_name("setup-vpk.sh").read_text()
        self.assertIn("config/velopack.json", setup)
        # Ensure setup-vpk.sh's sed extractor reads the same pins.
        extracted = subprocess.check_output(
            [
                "bash",
                "-c",
                f'velopack_config={config}; '
                'sed -n "s/^[[:space:]]*\\"version\\"[[:space:]]*:[[:space:]]*\\"\\([^\\"]*\\)\\".*/\\1/p" "$velopack_config" | head -n 1; '
                'sed -n "s/^[[:space:]]*\\"vpkSha256\\"[[:space:]]*:[[:space:]]*\\"\\([^\\"]*\\)\\".*/\\1/p" "$velopack_config" | head -n 1',
            ],
            text=True,
        ).splitlines()
        self.assertEqual(extracted, [notices.VPK_VERSION, notices.VPK_SHA256])

    def test_the_app_links_the_velopack_that_vpk_packs_with(self):
        # Packaging, ./xtask fetch-velopack, and prepare-appimage-notices.py all share config/velopack.json.
        root = SCRIPT.parent.parent.parent
        fetch = (root / "desktop/internal/xtask/deps/velopack.go").read_text()
        self.assertIn('"config", "velopack.json"', fetch)
        licenses = (root / "desktop/internal/xtask/licenses/licenses.go").read_text()
        self.assertIn("config", licenses)
        self.assertIn("velopack.json", licenses)
        self.assertTrue((root / f"licenses/velopack-{notices.VPK_VERSION}.json").is_file())

    def test_the_build_image_has_the_go_go_mod_pins(self):
        # Keep the AlmaLinux build container's Go toolchain in sync with desktop/go.mod.
        root = SCRIPT.parent.parent.parent
        gomod = (root / "desktop/go.mod").read_text()
        match = re.search(r"^toolchain go(\d+\.\d+\.\d+)$", gomod, re.M)
        self.assertIsNotNone(match, "desktop/go.mod has no toolchain line")
        version = match[1]
        image = SCRIPT.with_name("package-check.Containerfile").read_text()
        self.assertIn(f"GOTOOLCHAIN=go{version} ", image)
        self.assertIn(f"grep -q ' go{re.escape(version)} '", image)


class Licenses(unittest.TestCase):
    def test_license_files_are_taken_from_the_top_of_the_source(self):
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
