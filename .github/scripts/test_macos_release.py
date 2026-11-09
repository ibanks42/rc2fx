#!/usr/bin/env python3
"""Offline tests for macos_release.py: an in-memory fake replaces `gh`."""

import hashlib
import importlib.util
import io
import json
import os
import shutil
import tempfile
import unittest
import zipfile
from pathlib import Path

SCRIPT = Path(__file__).with_name("macos_release.py")
spec = importlib.util.spec_from_file_location("macos_release", SCRIPT)
mr = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mr)

REPO = "ibanks42/rc2fx"
SOURCE = "a" * 40
RELEASE_COMMIT = "b" * 40
TAG = "v0.2.0"
VERSION = "0.2.0"
PENDING = (
    "<!-- rc2fx:macos-pending -->\n"
    "macOS download is pending. It will be added to this release when a macOS runner is available.\n"
    "<!-- /rc2fx:macos-pending -->\n"
)


def sha(data):
    return hashlib.sha256(data).hexdigest()


def raw_zip(entries):
    raw = io.BytesIO()
    with zipfile.ZipFile(raw, "w") as package:
        for name, data in entries:
            package.writestr(name, data)
    return raw.getvalue()


APP_FILES = {
    "RC2FX.app/Contents/MacOS/rc2fx": b"binary",
    "RC2FX.app/Contents/MacOS/UpdateMac": b"updater",
    "RC2FX.app/Contents/Info.plist": b"plist",
}


def mac_zip(app=True, data=b""):
    """Velopack's portable zip: the app bundle at the root."""
    if not app:
        return raw_zip([("rc2fx", b"binary")])
    return raw_zip([(n, d + data) for n, d in APP_FILES.items()])


def mac_pkg(data=b"installer", magic=b"xar!"):
    return magic + data


def mac_nupkg(data=b"", nuspec="io.github.ibanks42.rc2fx.nuspec"):
    entries = [("lib/app/RC2FX.app/Contents/MacOS/rc2fx", b"binary" + data)]
    if nuspec:
        entries.append((nuspec, b"<package/>"))
    return raw_zip(entries)


MAC = "rc2fx-macos-arm64.zip"
PKG = "rc2fx-macos-arm64.pkg"
NUPKG = f"io.github.ibanks42.rc2fx-{VERSION}-osx-full.nupkg"
FEED = "releases.osx.json"
MACOS = (MAC, PKG, NUPKG, FEED)
SETUP = "rc2fx-windows-x64-setup.exe"


def feed_asset(nupkg=None, **changes):
    asset = {"PackageId": "io.github.ibanks42.rc2fx", "Version": VERSION, "Type": "Full",
             "FileName": NUPKG, "SHA1": "x", "SHA256": sha(nupkg or mac_nupkg()).upper(),
             "Size": 1, "NotesMarkdown": "", "NotesHtml": ""}
    asset.update(changes)
    return asset


def mac_feed(nupkg=None, assets=None):
    """The feed vpk writes for the update package."""
    return json.dumps({"Assets": assets if assets is not None else [feed_asset(nupkg)]}).encode()


def mac_files(**overrides):
    """The four macOS packages by name, OVERRIDES given by name."""
    files = {MAC: mac_zip(), PKG: mac_pkg(), NUPKG: mac_nupkg()}
    files.update(overrides)
    files.setdefault(FEED, mac_feed(files[NUPKG]))
    return files


def other(tag, draft=False, prerelease=False):
    """Another release in the repository."""
    return {"id": 1, "tag_name": tag, "draft": draft, "prerelease": prerelease, "assets": []}


class FakeGh:
    """An in-memory repository. `ops` records every mutating call in order."""

    def __init__(self):
        self.repo = REPO
        self.exists = True
        self.draft = True
        self.body = "Notes.\n\n" + PENDING
        self.commit = {"sha": RELEASE_COMMIT, "commit": {"message": f"chore(release): {TAG}\n"},
                       "parents": [{"sha": SOURCE}]}
        self.assets = {}
        for name in mr.expected_assets(VERSION) - set(MACOS):
            self.assets[name] = name.encode()
        self.assets["SHA256SUMS"] = b"partial\n"
        self.bad_digest = set()
        self.immutable = False
        self.tag_name = TAG
        self.fail_upload = False
        self.publish_by_hand_after = None  # publish the draft after N writes
        self.others = []  # other releases in the list
        self.release_after = None  # (N, release): it appears after N writes
        self.ops = []

    # Reads
    def _assets(self):
        return [
            {"id": n, "name": n,
             "digest": "sha256:" + (sha(b"lie") if n in self.bad_digest else sha(d))}
            for n, d in sorted(self.assets.items())
        ]

    def _release(self):
        return {"id": 99, "tag_name": self.tag_name, "draft": self.draft,
                "immutable": self.immutable, "body": self.body, "assets": self._assets()}

    def release(self, release_id):
        return self._release() if self.exists and release_id == 99 else None

    def releases(self):
        if not self.exists:
            return [{"tag_name": "v0.9.0", "draft": False, "assets": []}]
        return [*self.others, self._release()]

    def api(self, path):
        if path == f"commits/{TAG}":
            return self.commit if self.exists else None
        return None

    def asset_bytes(self, asset_id):
        return self.assets[asset_id]

    # Writes
    def _write(self, op):
        assert self.draft or not self.immutable, f"{op[0]} on an immutable release"
        self.ops.append(op)
        if self.publish_by_hand_after == len(self.ops):
            self.draft = False
        if self.release_after and self.release_after[0] == len(self.ops):
            self.others.append(self.release_after[1])

    def upload_asset(self, release_id, path):
        path = Path(path)
        if self.fail_upload:
            raise mr.Refused("upload failed")
        assert release_id == 99, "upload to another release"
        assert path.name not in self.assets, "upload over an existing asset"
        self._write(("upload", path.name))
        self.assets[path.name] = path.read_bytes()

    def delete_asset(self, asset_id):
        self._write(("delete", asset_id))
        del self.assets[asset_id]

    def _patch(self, tag):
        # GitHub: a PATCH on a draft without tag_name untags the draft.
        if self.draft and tag is None:
            self.tag_name = "untagged-0123456789abcdef"

    def set_notes(self, release_id, tag, notes):
        self._patch(tag)
        self._write(("notes", notes))
        self.body = notes

    def publish(self, release_id, tag):
        self._patch(tag)
        assert self.draft and tag == self.tag_name == TAG, "published with the wrong tag"
        self.ops.append(("publish", release_id))
        self.draft = False


def complete_sums(gh):
    return "".join(f"{sha(d)}  {n}\n" for n, d in sorted(gh.assets.items()) if n != "SHA256SUMS")


def published_complete(**overrides):
    """A fake whose release is already published and complete."""
    gh = FakeGh()
    gh.assets.update(mac_files(**overrides))
    gh.assets["SHA256SUMS"] = complete_sums(gh).encode()
    gh.body = "Notes.\n"
    gh.draft = False
    return gh


class InTemp(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.old = os.getcwd()
        os.chdir(self.tmp.name)

    def tearDown(self):
        os.chdir(self.old)
        self.tmp.cleanup()


class Assets(unittest.TestCase):
    def test_twelve_packages_are_expected(self):
        expected = mr.expected_assets(VERSION)
        self.assertEqual(len(expected), 12)
        self.assertIn(SETUP, expected)
        # Published names have no version: releases/latest/download links them.
        self.assertIn("rc2fx-linux-x64.AppImage", expected)
        self.assertIn("rc2fx-windows-x64.zip", expected)
        self.assertIn(f"io.github.ibanks42.rc2fx-{VERSION}-linux-full.nupkg", expected)
        self.assertIn("releases.linux.json", expected)
        self.assertIn(f"io.github.ibanks42.rc2fx-{VERSION}-full.nupkg", expected)
        self.assertIn("releases.win.json", expected)
        self.assertEqual(mr.mac_names(VERSION), MACOS)
        self.assertLessEqual(set(MACOS), expected)
        self.assertNotIn("SHA256SUMS", expected)


class Publish(InTemp):
    def package(self, data=None, leave_out=(), **overrides):
        """The macOS package directory; DATA replaces the zip, OVERRIDES any
        file by its variable name (pkg, nupkg, feed)."""
        names = {"pkg": PKG, "nupkg": NUPKG, "feed": FEED}
        files = mac_files(**{names[k]: v for k, v in overrides.items()},
                          **({MAC: data} if data is not None else {}))
        directory = Path("target/macos-assets")
        directory.mkdir(parents=True, exist_ok=True)
        for name, content in files.items():
            if name not in leave_out:
                (directory / name).write_bytes(content)
        return directory

    def ours(self, **overrides):
        return {name: sha(data) for name, data in mac_files(**overrides).items()}

    def test_the_draft_is_finished_and_published_last(self):
        gh = FakeGh()
        sums = mr.publish(gh, TAG, SOURCE, self.package())
        self.assertEqual(gh.ops[-1], ("publish", 99), "publishing is the last call")
        self.assertEqual([op for op, _ in gh.ops].count("publish"), 1)
        self.assertEqual(gh.ops[:4], [("upload", name) for name in MACOS])
        self.assertIn(("delete", "SHA256SUMS"), gh.ops)
        self.assertIn(("upload", "SHA256SUMS"), gh.ops)
        self.assertIn(("notes", "Notes.\n"), gh.ops)
        self.assertEqual(sums, complete_sums(gh))
        self.assertEqual(gh.assets["SHA256SUMS"].decode(), complete_sums(gh))
        self.assertFalse(gh.draft)

    def test_a_draft_behind_a_published_release_is_refused_before_any_change(self):
        # Compared as numbers: v0.10.0 is newer than v0.2.0, v0.1.9 is not.
        for tag in ("v0.2.1", "v0.3.0", "v0.10.0", "v1.0.0"):
            gh = FakeGh()
            gh.others = [other("v0.1.0"), other(tag)]
            with self.assertRaisesRegex(mr.Refused, f"older than published release {tag}"):
                mr.publish(gh, TAG, SOURCE, self.package())
            self.assertEqual(gh.ops, [], tag)
            self.assertTrue(gh.draft, tag)

    def test_a_draft_at_or_above_every_published_release_is_published(self):
        gh = FakeGh()
        gh.others = [other("v0.1.9"), other("v0.1.0")]
        mr.publish(gh, TAG, SOURCE, self.package())
        self.assertEqual(gh.ops[-1], ("publish", 99))

    def test_only_published_stable_app_releases_count_as_newer(self):
        gh = FakeGh()
        gh.others = [
            other("v9.0.0", draft=True),
            other("v9.0.1", prerelease=True),
            other("mixer-v9.0.0"),
            other("v9.0.0-rc1"),
            other("v9.0"),
            other("v09.0.0"),
            other(None),
        ]
        mr.publish(gh, TAG, SOURCE, self.package())
        self.assertEqual(gh.ops[-1], ("publish", 99))

    def test_a_release_published_during_the_run_stops_the_final_publish(self):
        gh = FakeGh()
        gh.release_after = (1, other("v0.3.0"))  # right after the first macOS upload
        with self.assertRaisesRegex(mr.Refused, "older than published release v0.3.0"):
            mr.publish(gh, TAG, SOURCE, self.package())
        self.assertNotIn("publish", [op for op, _ in gh.ops])
        self.assertTrue(gh.draft)

    def test_an_older_complete_release_stays_a_read_only_success(self):
        gh = published_complete()
        gh.others = [other("v0.3.0")]
        self.assertEqual(mr.publish(gh, TAG, SOURCE, self.package()), "already complete\n")
        self.assertEqual(gh.ops, [])

    def test_an_older_release_published_early_by_hand_is_still_finished(self):
        gh = FakeGh()
        gh.draft = False
        gh.others = [other("v0.3.0")]
        mr.publish(gh, TAG, SOURCE, self.package())
        self.assertNotIn("publish", [op for op, _ in gh.ops], "latest is left as chosen")
        self.assertTrue(mr.complete(gh, gh._release(), VERSION, self.ours()))

    def test_a_published_retry_is_read_only(self):
        gh = published_complete()
        self.assertEqual(mr.publish(gh, TAG, SOURCE, self.package()), "already complete\n")
        self.assertEqual(gh.ops, [])

    def test_a_release_published_early_by_hand_is_finished_but_not_republished(self):
        gh = FakeGh()
        gh.draft = False
        mr.publish(gh, TAG, SOURCE, self.package())
        for name in MACOS:
            self.assertIn(("upload", name), gh.ops)
        self.assertNotIn("publish", [op for op, _ in gh.ops], "latest is left as chosen")
        self.assertEqual(gh.assets["SHA256SUMS"].decode(), complete_sums(gh))
        self.assertNotIn("rc2fx:macos-pending", gh.body)

    def test_a_draft_published_by_hand_mid_run_is_finished_without_publishing_again(self):
        gh = FakeGh()
        gh.publish_by_hand_after = 1  # right after the first macOS upload
        mr.publish(gh, TAG, SOURCE, self.package())
        self.assertFalse(gh.draft)
        self.assertNotIn("publish", [op for op, _ in gh.ops])
        self.assertEqual(gh.assets["SHA256SUMS"].decode(), complete_sums(gh))

    def test_immutable_incomplete_releases_and_different_zips_are_refused(self):
        changes = [
            lambda gh: (setattr(gh, "draft", False), setattr(gh, "immutable", True)),
            lambda gh: (setattr(gh, "draft", False), gh.assets.update({MAC: b"another build"})),
            lambda gh: (setattr(gh, "draft", False), gh.assets.update({PKG: mac_pkg(b"other")})),
            lambda gh: gh.assets.pop("releases.win.json"),
            lambda gh: gh.assets.pop("rc2fx-windows-x64.zip"),
            lambda gh: gh.assets.pop(SETUP),
        ]
        for change in changes:
            gh = FakeGh()
            change(gh)
            with self.assertRaises(mr.Refused):
                mr.publish(gh, TAG, SOURCE, self.package())
            self.assertEqual(gh.ops, [])
        # Published by hand, then made immutable before it was complete.
        gh = FakeGh()
        gh.draft = False
        original = gh._write
        def lock_after_first(op):
            original(op)
            gh.immutable = True
        gh._write = lock_after_first
        with self.assertRaises(mr.Refused) as caught:
            mr.publish(gh, TAG, SOURCE, self.package())
        self.assertIn("immutable", str(caught.exception))

    def test_nothing_is_published_when_any_step_fails(self):
        failures = {
            "upload failure": lambda gh: setattr(gh, "fail_upload", True),
            "digest mismatch": lambda gh: gh.bad_digest.add("rc2fx-linux-x64.AppImage"),
            "missing package": lambda gh: gh.assets.pop("releases.linux.json"),
            "unexpected asset": lambda gh: gh.assets.update({"surprise.txt": b"x"}),
            "different mac zip": lambda gh: gh.assets.update({MAC: b"another build"}),
            "different mac pkg": lambda gh: gh.assets.update({PKG: mac_pkg(b"another build")}),
            "different mac feed": lambda gh: gh.assets.update({FEED: b"{}"}),
            "missing windows feed": lambda gh: gh.assets.pop("releases.win.json"),
            "missing setup": lambda gh: gh.assets.pop(SETUP),
            "bad tag": lambda gh: gh.commit.update(parents=[{"sha": "c" * 40}]),
        }
        for label, change in failures.items():
            gh = FakeGh()
            change(gh)
            with self.assertRaises(mr.Refused, msg=label):
                mr.publish(gh, TAG, SOURCE, self.package())
            self.assertTrue(gh.draft, label)
            self.assertNotIn("publish", [op for op, _ in gh.ops], label)

    def test_a_draft_that_loses_its_tag_is_never_published(self):
        gh = FakeGh()
        original = gh._write
        def untag_after_first(op):
            original(op)
            gh.tag_name = "untagged-0123456789abcdef"
        gh._write = untag_after_first
        with self.assertRaises(mr.Refused) as caught:
            mr.publish(gh, TAG, SOURCE, self.package())
        self.assertIn("no longer carries tag", str(caught.exception))
        self.assertTrue(gh.draft)
        self.assertNotIn("publish", [op for op, _ in gh.ops])

    def test_the_draft_keeps_its_tag_through_every_update(self):
        gh = FakeGh()
        mr.publish(gh, TAG, SOURCE, self.package())
        self.assertEqual(gh.tag_name, TAG)
        self.assertFalse(gh.draft)

    def test_a_partly_finished_draft_resumes_without_reuploading(self):
        gh = FakeGh()
        gh.assets[MAC] = mac_zip()
        gh.assets[NUPKG] = mac_nupkg()
        mr.publish(gh, TAG, SOURCE, self.package())
        self.assertEqual(gh.ops[:2], [("upload", PKG), ("upload", FEED)])
        self.assertNotIn(("upload", MAC), gh.ops)
        self.assertNotIn(("upload", NUPKG), gh.ops)
        self.assertEqual(gh.ops[-1], ("publish", 99))
        gh = FakeGh()
        gh.assets.update(mac_files())
        mr.publish(gh, TAG, SOURCE, self.package())
        for name in MACOS:
            self.assertNotIn(("upload", name), gh.ops)
        self.assertEqual(gh.ops[-1], ("publish", 99))

    def test_all_four_macos_packages_are_uploaded_when_absent(self):
        gh = FakeGh()
        mr.publish(gh, TAG, SOURCE, self.package())
        uploads = [name for op, name in gh.ops if op == "upload"]
        self.assertEqual(uploads, [*MACOS, "SHA256SUMS"], "the feed after its package")
        for name, data in mac_files().items():
            self.assertEqual(gh.assets[name], data)
            self.assertIn(f"{sha(data)}  {name}\n", gh.assets["SHA256SUMS"].decode())
        self.assertTrue(mr.complete(gh, gh._release(), VERSION, self.ours()))

    def test_a_different_existing_copy_of_any_macos_package_is_not_replaced(self):
        others = {MAC: mac_zip(data=b"another build"), PKG: mac_pkg(b"another build"),
                  NUPKG: mac_nupkg(b"another build"), FEED: mac_feed(mac_nupkg(b"another build"))}
        for name, theirs in others.items():
            for draft in (True, False):
                gh = FakeGh()
                gh.draft = draft
                gh.assets[name] = theirs
                with self.assertRaisesRegex(mr.Refused, f"already has a different {name}"):
                    mr.publish(gh, TAG, SOURCE, self.package())
                self.assertEqual(gh.ops, [], f"{name}: nothing is uploaded, not even the others")
                self.assertEqual(gh.assets[name], theirs)
            gh = published_complete(**{name: theirs})
            with self.assertRaisesRegex(mr.Refused, f"already has a different {name}"):
                mr.publish(gh, TAG, SOURCE, self.package())
            self.assertEqual(gh.ops, [], name)

    def test_complete_checks_every_macos_digest(self):
        gh = published_complete()
        release = gh._release()
        self.assertTrue(mr.complete(gh, release, VERSION, self.ours()))
        self.assertTrue(mr.complete(gh, release, VERSION, None))
        for name in MACOS:
            mac_shas = self.ours()
            mac_shas[name] = sha(b"another build")
            self.assertFalse(mr.complete(gh, release, VERSION, mac_shas), name)
        gh.bad_digest.add(FEED)
        with self.assertRaises(mr.Refused):
            mr.complete(gh, gh._release(), VERSION, self.ours())

    def refused(self, pattern, **package):
        gh = FakeGh()
        with self.assertRaisesRegex(mr.Refused, pattern):
            mr.publish(gh, TAG, SOURCE, self.package(**package))
        self.assertEqual(gh.ops, [])

    def test_bad_installer_packages_are_refused(self):
        self.refused("empty", pkg=b"")
        for pkg in [mac_pkg(magic=b"xar?"), b"xar", mac_zip(), b"\0" * 64]:
            self.refused("not a xar archive", pkg=pkg)
        mr.publish(FakeGh(), TAG, SOURCE, self.package(pkg=b"xar!"))

    def test_an_update_package_without_a_root_nuspec_is_refused(self):
        for nupkg in [mac_nupkg(nuspec=None), mac_nupkg(nuspec="lib/rc2fx.nuspec")]:
            self.refused("no .nuspec at its root", nupkg=nupkg)
        self.refused("not a zip", nupkg=b"not a zip")

    def test_a_feed_that_does_not_describe_this_package_is_refused(self):
        feeds = {
            "wrong digest": mac_feed(assets=[feed_asset(SHA256=sha(b"another build"))]),
            "wrong version": mac_feed(assets=[feed_asset(Version="0.2.1")]),
            "wrong package id": mac_feed(assets=[feed_asset(PackageId="RC2FX")]),
            "delta": mac_feed(assets=[feed_asset(Type="Delta")]),
            "wrong file": mac_feed(assets=[feed_asset(FileName=MAC)]),
            "numeric field": mac_feed(assets=[feed_asset(Version=2)]),
        }
        for label, feed in feeds.items():
            with self.subTest(label):
                self.refused("does not describe", feed=feed)
        two = mac_feed(assets=[feed_asset(), feed_asset(Version="0.1.0")])
        for label, feed in {"two assets": two, "no assets": mac_feed(assets=[]),
                            "no list": b'{"Assets": {}}', "a list": b"[]"}.items():
            with self.subTest(label):
                self.refused("exactly one package", feed=feed)
        self.refused("not an object", feed=mac_feed(assets=["x"]))
        for feed in [b"not json", b"\xff\xfe"]:
            self.refused("not JSON", feed=feed)
        # The digest is compared without regard to case.
        lower = mac_feed(assets=[feed_asset(SHA256=sha(mac_nupkg()))])
        mr.publish(FakeGh(), TAG, SOURCE, self.package(feed=lower))

    def test_a_feed_for_another_update_package_is_refused(self):
        # A sound feed and package, but the feed's digest is another build's.
        self.refused("does not describe", nupkg=mac_nupkg(b"another build"), feed=mac_feed())

    def test_symlinked_packages_are_refused(self):
        for name in MACOS:
            directory = self.package()
            real = Path("real") / name
            real.parent.mkdir(exist_ok=True)
            real.write_bytes((directory / name).read_bytes())
            (directory / name).unlink()
            (directory / name).symlink_to(real.resolve())
            with self.assertRaisesRegex(mr.Refused, "not a regular file", msg=name):
                mr.publish(FakeGh(), TAG, SOURCE, directory)
            (directory / name).unlink()

    def test_a_package_directory_missing_any_package_is_refused(self):
        for name in MACOS:
            gh = FakeGh()
            with self.assertRaisesRegex(mr.Refused, "must hold exactly", msg=name):
                mr.publish(gh, TAG, SOURCE, self.package(leave_out=(name,)))
            self.assertEqual(gh.ops, [])
            shutil.rmtree("target/macos-assets")

    def test_bad_packages_and_package_directories_are_refused(self):
        for data in [mac_zip(app=False), b"not a zip"]:
            gh = FakeGh()
            with self.assertRaises(mr.Refused):
                mr.publish(gh, TAG, SOURCE, self.package(data))
            self.assertEqual(gh.ops, [])
        directory = self.package()
        (directory / "other.zip").write_bytes(b"x")
        with self.assertRaisesRegex(mr.Refused, "must hold exactly"):
            mr.publish(FakeGh(), TAG, SOURCE, directory)
        (directory / "other.zip").unlink()
        (directory / "dir").mkdir()
        with self.assertRaisesRegex(mr.Refused, "must hold exactly"):
            mr.publish(FakeGh(), TAG, SOURCE, directory)

    def test_duplicate_or_unsafe_zip_entries_are_refused_even_with_good_sums(self):
        import warnings
        exe = "RC2FX.app/Contents/MacOS/rc2fx"
        app = list(APP_FILES.items())
        for pattern, entries in [
            ("repeats an entry name", app + [(exe, b"binary")]),
            ("outside RC2FX.app/", app + [("other/x", b"x")]),
            ("outside RC2FX.app/", app + [("rc2fx/RC2FX.app/Contents/Info.plist", b"x")]),
            ("outside RC2FX.app/", app + [("__MACOSX/._RC2FX.app.bak", b"x")]),
            ("outside RC2FX.app/", app + [("__MACOSX/._RC2FX.app/", b"")]),
            ("outside RC2FX.app/", app + [("__MACOSX/._other", b"x")]),
            ("outside RC2FX.app/", app + [("__MACOSX/other/._x", b"x")]),
            ("outside RC2FX.app/", app + [("__MACOSX/RC2FX.app.bak/._x", b"x")]),
            ("outside RC2FX.app/", app + [("__MACOSX", b"x")]),
            ("outside RC2FX.app/", app + [("RC2FX.app.bak/x", b"x")]),
            ("unsafe path", app + [("RC2FX.app/../x", b"x")]),
            ("unsafe path", app + [("RC2FX.app//x", b"x")]),
            ("unsafe path", app + [("__MACOSX/RC2FX.app/../../x", b"x")]),
            ("unsafe path", app + [("__MACOSX/RC2FX.app/./._x", b"x")]),
            ("unsafe path", app + [("__MACOSX/RC2FX.app/Contents\\._x", b"x")]),
        ]:
            gh = FakeGh()
            with warnings.catch_warnings():
                warnings.simplefilter("ignore")
                data = raw_zip(entries)
            with self.assertRaisesRegex(mr.Refused, pattern, msg=entries[-1][0]):
                mr.publish(gh, TAG, SOURCE, self.package(data))
            self.assertEqual(gh.ops, [])
        mr.publish(FakeGh(), TAG, SOURCE, self.package(raw_zip([("RC2FX.app/", b"")] + app)))

    def test_sequestered_apple_double_entries_of_the_app_are_accepted(self):
        # What `ditto -c -k --rsrc --keepParent --sequesterRsrc` writes for
        # files that carry extended attributes.
        sequestered = [("__MACOSX/", b""), ("__MACOSX/._RC2FX.app", b"\0\x05\x16\x07"),
                       ("__MACOSX/RC2FX.app/", b""),
                       ("__MACOSX/RC2FX.app/Contents/", b""),
                       ("__MACOSX/RC2FX.app/Contents/MacOS/._rc2fx", b"\0\x05\x16\x07"),
                       ("__MACOSX/RC2FX.app/Contents/._Info.plist", b"\0\x05\x16\x07")]
        gh = FakeGh()
        mr.publish(gh, TAG, SOURCE, self.package(raw_zip([*APP_FILES.items(), *sequestered])))
        self.assertEqual(gh.ops[-1], ("publish", 99))
        # The sequestered copies do not stand in for the app's own files.
        gh = FakeGh()
        entries = [(n, d) for n, d in APP_FILES.items() if not n.endswith("UpdateMac")]
        entries.append(("__MACOSX/RC2FX.app/Contents/MacOS/UpdateMac", b"updater"))
        with self.assertRaisesRegex(mr.Refused, "has no RC2FX.app/Contents/MacOS/UpdateMac"):
            mr.publish(gh, TAG, SOURCE, self.package(raw_zip(entries)))
        self.assertEqual(gh.ops, [])

    def test_an_app_without_its_executable_updater_or_plist_is_refused(self):
        for member in APP_FILES:
            entries = [(n, d) for n, d in APP_FILES.items() if n != member]
            gh = FakeGh()
            with self.assertRaisesRegex(mr.Refused, f"has no {member}"):
                mr.publish(gh, TAG, SOURCE, self.package(raw_zip(entries)))
            self.assertEqual(gh.ops, [])

    def test_a_bare_executable_without_the_app_is_refused(self):
        gh = FakeGh()
        with self.assertRaisesRegex(mr.Refused, "RC2FX.app"):
            mr.publish(gh, TAG, SOURCE, self.package(mac_zip(app=False)))
        self.assertEqual(gh.ops, [])

    def test_missing_release_or_bad_arguments_are_refused(self):
        gh = FakeGh()
        gh.exists = False
        with self.assertRaises(mr.Refused):
            mr.publish(gh, TAG, SOURCE, self.package())
        for tag, source in [("mixer-v1.0.0", SOURCE), (TAG, "not-a-commit"), (TAG, "c" * 40)]:
            gh = FakeGh()
            with self.assertRaises(mr.Refused):
                mr.publish(gh, tag, source, self.package())
            self.assertEqual(gh.ops, [])


class Adapter(unittest.TestCase):
    """The real Gh class against a mocked subprocess.run, with the shapes the
    gh CLI actually returns."""

    def setUp(self):
        self.calls = []
        self.replies = []
        self.original = mr.subprocess.run

        def run(args, input=None, capture_output=False, text=False):
            self.calls.append(args)
            stdout, returncode, stderr = self.replies.pop(0)
            if not text and isinstance(stdout, str):
                stdout = stdout.encode()
            return mr.subprocess.CompletedProcess(args, returncode, stdout,
                                                  stderr if text else stderr.encode())

        mr.subprocess.run = run
        self.gh = mr.Gh(REPO)

    def tearDown(self):
        mr.subprocess.run = self.original

    def test_slurped_release_pages_are_flattened(self):
        pages = [[{"tag_name": "v0.9.0", "draft": False}], [{"tag_name": TAG, "draft": True, "id": 5}]]
        self.replies.append((json.dumps(pages), 0, ""))
        self.assertEqual(mr.find_release(self.gh, TAG)["id"], 5)
        self.assertIn("--paginate", self.calls[0])
        self.assertIn("--slurp", self.calls[0])
        for bad in ['{"message": "x"}', "[1, 2]", "[[1]]"]:
            self.replies.append((bad, 0, ""))
            with self.assertRaises(mr.Refused):
                self.gh.releases()

    def test_both_patches_always_send_the_tag(self):
        for call in (lambda: self.gh.set_notes(5, TAG, "notes"), lambda: self.gh.publish(5, TAG)):
            self.replies.append(("{}", 0, ""))
            call()
            args = self.calls[-1]
            self.assertEqual(args[args.index("--method") + 1], "PATCH")
            self.assertIn(f"tag_name={TAG}", args)

    def test_asset_bytes_are_binary_and_publish_sends_typed_fields(self):
        payload = bytes(range(256))
        self.replies.append((payload, 0, ""))
        self.assertEqual(self.gh.asset_bytes(42), payload)
        self.assertIn("Accept: application/octet-stream", self.calls[-1])
        self.replies.append(("{}", 0, ""))
        self.gh.publish(5, TAG)
        args = self.calls[-1]
        self.assertEqual(args[args.index("--method") + 1], "PATCH")
        self.assertEqual(args[args.index("-F") + 1], "draft=false")
        self.assertIn("make_latest=true", args)
        self.assertIn(f"tag_name={TAG}", args)
        self.assertTrue(args[args.index("-F") - 1].endswith("/releases/5") or
                        f"repos/{REPO}/releases/5" in args)

    def test_failures_and_not_found_are_reported(self):
        self.replies.append(("", 1, "gh: Not Found (HTTP 404)"))
        self.assertIsNone(self.gh.api("commits/vX"))
        self.replies.append(("", 1, "gh: Server Error (HTTP 500)"))
        with self.assertRaises(mr.Refused):
            self.gh.api("commits/vX")
        self.replies.append(("", 1, "upload refused"))
        with self.assertRaises(mr.Refused):
            self.gh.upload_asset(5, Path("rc2fx-macos-arm64.zip"))
        self.assertTrue(self.calls[-1][-3].startswith("https://uploads.github.com/repos/"))


if __name__ == "__main__":
    unittest.main()
