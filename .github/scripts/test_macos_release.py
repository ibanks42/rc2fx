#!/usr/bin/env python3
"""Offline tests for macos_release.py: an in-memory fake replaces `gh`."""

import hashlib
import importlib.util
import io
import json
import os
import tarfile
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
FILES = ["Cargo.toml", "Cargo.lock", "CHANGELOG.md"]


def sha(data):
    return hashlib.sha256(data).hexdigest()


def plan(**changes):
    value = {"tag": TAG, "previous": "v0.1.0", "source": SOURCE, "files": FILES}
    value.update(changes)
    return value


def versioned(members):
    raw = io.BytesIO()
    with tarfile.open(fileobj=raw, mode="w") as tar:
        for name, data, kind in members:
            info = tarfile.TarInfo(name)
            info.type = kind
            if kind == tarfile.SYMTYPE:
                info.linkname = "/etc/passwd"
                tar.addfile(info)
            else:
                info.size = len(data)
                tar.addfile(info, io.BytesIO(data))
    return raw.getvalue()


def raw_zip(entries):
    raw = io.BytesIO()
    with zipfile.ZipFile(raw, "w") as package:
        for name, data in entries:
            package.writestr(name, data)
    return raw.getvalue()


def mac_zip(tamper=False, unlisted=False):
    files = {"rc2fx.app/Contents/MacOS/rc2fx": b"binary", "LICENSES.html": b"notices"}
    sums = "".join(f"{sha(d)}  {n}\n" for n, d in files.items())
    if tamper:
        files["LICENSES.html"] = b"changed"
    raw = io.BytesIO()
    with zipfile.ZipFile(raw, "w") as package:
        for name, data in files.items():
            package.writestr("rc2fx/" + name, data)
        if unlisted:
            package.writestr("rc2fx/extra", b"x")
        package.writestr("rc2fx/SHA256SUMS", sums)
    return raw.getvalue()


class FakeGh:
    def __init__(self, input_files=None):
        self.repo = REPO
        self.run = {
            "repository": {"full_name": REPO}, "path": ".github/workflows/release.yml",
            "event": "workflow_dispatch", "head_branch": "main", "status": "completed",
            "conclusion": "success", "head_sha": SOURCE,
        }
        self.artifact = {"name": "release-input", "expired": False}
        self.release = {"tag_name": TAG, "draft": False, "body": "Notes.\n\n" + PENDING, "assets": []}
        self.published = True
        self.commit = {"sha": RELEASE_COMMIT, "commit": {"message": f"chore(release): {TAG}\n"},
                       "parents": [{"sha": SOURCE}]}
        self.latest = TAG
        self.input_files = input_files if input_files is not None else {
            "plan.json": json.dumps(plan()).encode()}
        self.assets = {}
        for name in mr.expected_assets(VERSION) - {f"rc2fx-{VERSION}-macos-arm64.zip"}:
            self.assets[name] = name.encode()
        self.assets["SHA256SUMS"] = b"old\n"
        self.uploads = []
        self.edits = []

    def _release(self):
        release = dict(self.release)
        release["assets"] = [{"name": n, "digest": "sha256:" + sha(d)} for n, d in sorted(self.assets.items())]
        return release

    def api(self, path):
        if path == "actions/runs/7":
            return self.run
        if path == "actions/runs/7/artifacts?name=release-input":
            return {"artifacts": [self.artifact] if self.artifact else []}
        if path == f"releases/tags/{TAG}":
            return self._release() if self.published else None
        if path == f"commits/{TAG}":
            return self.commit if self.published else None
        if path == "releases/latest":
            return {"tag_name": self.latest}
        return None

    def download_artifact(self, run_id, name, directory):
        directory = Path(directory)
        directory.mkdir(parents=True, exist_ok=True)
        for file, data in self.input_files.items():
            (directory / file).write_bytes(data)

    def download_asset(self, tag, name, directory):
        Path(directory).mkdir(parents=True, exist_ok=True)
        path = Path(directory) / name
        path.write_bytes(self.assets[name])
        return path

    def upload_asset(self, tag, path, clobber=False):
        path = Path(path)
        if path.name in self.assets and not clobber:
            raise AssertionError("upload without clobber over an existing asset")
        self.uploads.append((path.name, clobber))
        self.assets[path.name] = path.read_bytes()

    def edit_notes(self, tag, notes, **options):
        self.edits.append((notes, options))
        self.release["body"] = notes


class InTemp(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.old = os.getcwd()
        os.chdir(self.tmp.name)

    def tearDown(self):
        os.chdir(self.old)
        self.tmp.cleanup()


class Prepare(InTemp):
    def outputs(self, gh, automatic=False):
        lines = []
        return mr.prepare(gh, 7, automatic, lines.append)

    def test_a_published_release_is_ready(self):
        self.assertEqual(self.outputs(FakeGh()), {"source": SOURCE, "tag": TAG, "ready": "true"})

    def test_a_dry_run_is_skipped_automatically_and_refused_by_hand(self):
        gh = FakeGh()
        gh.published = False
        self.assertEqual(self.outputs(gh, automatic=True)["ready"], "false")
        for path in Path(".release-input").iterdir():
            path.unlink()
        with self.assertRaises(mr.Refused):
            self.outputs(gh)

    def test_runs_that_are_not_successful_main_release_dispatches_are_refused(self):
        for key, value in [
            ("path", ".github/workflows/check.yml"), ("event", "push"), ("head_branch", "dev"),
            ("conclusion", "failure"), ("status", "in_progress"),
            ("repository", {"full_name": "someone/fork"}),
        ]:
            gh = FakeGh()
            gh.run[key] = value
            with self.assertRaises(mr.Refused, msg=key):
                self.outputs(gh, automatic=True)

    def test_expired_or_missing_artifacts_are_refused(self):
        gh = FakeGh()
        gh.artifact["expired"] = True
        with self.assertRaises(mr.Refused) as caught:
            self.outputs(gh)
        self.assertIn("expired", str(caught.exception))
        gh = FakeGh()
        gh.artifact = None
        with self.assertRaises(mr.Refused):
            self.outputs(gh)

    def test_a_plan_that_does_not_match_the_run_is_refused(self):
        for bad in [
            plan(source="c" * 40), plan(tag="v0.2"), plan(tag="mixer-v0.2.0"), plan(tag="v01.2.0"),
            plan(files=["../escape"]), plan(files=[]), {**plan(), "extra": 1},
        ]:
            gh = FakeGh({"plan.json": json.dumps(bad).encode()})
            with self.assertRaises(mr.Refused, msg=bad):
                self.outputs(gh, automatic=True)
            for path in Path(".release-input").iterdir():
                path.unlink()

    def test_a_tag_that_is_not_the_release_commit_on_the_source_is_refused(self):
        for change in [
            lambda gh: gh.commit.update(parents=[{"sha": "c" * 40}]),
            lambda gh: gh.commit.update(parents=[{"sha": SOURCE}, {"sha": "d" * 40}]),
            lambda gh: gh.commit.update(commit={"message": "feat: something"}),
            lambda gh: gh.release.update(draft=True),
        ]:
            gh = FakeGh()
            change(gh)
            with self.assertRaises(mr.Refused):
                self.outputs(gh, automatic=True)
            for path in Path(".release-input").iterdir():
                path.unlink()

    def test_a_non_empty_input_directory_is_refused(self):
        Path(".release-input").mkdir()
        Path(".release-input/leftover").write_text("x")
        with self.assertRaises(mr.Refused):
            self.outputs(FakeGh())


class Stage(InTemp):
    def setUp(self):
        super().setUp()
        self.setUp_manifest()

    def setUp_manifest(self):
        Path("Cargo.toml").write_text(
            '[workspace]\nmembers = ["crates/rc2fx-app", "xtask"]\n')

    def write_input(self, members, the_plan=None):
        directory = Path(".release-input")
        directory.mkdir()
        (directory / "plan.json").write_text(json.dumps(the_plan or plan()))
        (directory / "versioned.tar").write_bytes(versioned(members))

    def test_only_the_planned_files_are_extracted(self):
        self.write_input([(name, f"new {name}".encode(), tarfile.REGTYPE) for name in FILES])
        self.assertEqual(mr.stage(SOURCE, TAG, SOURCE, Path(".")), sorted(FILES))
        self.assertEqual(Path("Cargo.toml").read_text(), "new Cargo.toml")

    def test_workspace_manifests_are_allowed_but_nothing_else(self):
        allowed = FILES + ["crates/rc2fx-app/Cargo.toml", "crates/rc2fx-native/Cargo.lock"]
        self.write_input([(n, b"x", tarfile.REGTYPE) for n in allowed], plan(files=allowed))
        self.assertEqual(mr.stage(SOURCE, TAG, SOURCE, Path(".")), sorted(allowed))
        self.setUp_manifest()
        for outside in [".github/workflows/release.yml", "xtask/mixer/rc2fx.sh", "crates/rc2fx-app/src/lib.rs"]:
            for path in Path(".release-input").iterdir():
                path.unlink()
            Path(".release-input").rmdir()
            files = FILES + [outside]
            self.write_input([(n, b"x", tarfile.REGTYPE) for n in files], plan(files=files))
            with self.assertRaises(mr.Refused, msg=outside):
                mr.stage(SOURCE, TAG, SOURCE, Path("."))
            self.assertFalse(Path(outside).exists(), outside)

    def test_extra_missing_linked_or_escaping_members_are_refused(self):
        good = [(name, b"x", tarfile.REGTYPE) for name in FILES]
        for members in [
            good + [("extra", b"x", tarfile.REGTYPE)],
            good[:-1],
            good[:-1] + [(FILES[-1], b"", tarfile.SYMTYPE)],
            good + [(FILES[0], b"y", tarfile.REGTYPE)],
        ]:
            self.write_input(members)
            with self.assertRaises(mr.Refused):
                mr.stage(SOURCE, TAG, SOURCE, Path("."))
            for path in Path(".release-input").iterdir():
                path.unlink()
            Path(".release-input").rmdir()
        self.assertFalse(Path("extra").exists())
        escaping = [("../evil", b"x", tarfile.REGTYPE)]
        with self.assertRaises(mr.Refused):
            self.write_input(escaping, plan(files=["../evil"]))
            mr.stage(SOURCE, TAG, SOURCE, Path("."))
        self.assertFalse(Path("../evil").exists())

    def test_the_checkout_and_arguments_must_match_the_plan(self):
        self.write_input([(name, b"x", tarfile.REGTYPE) for name in FILES])
        with self.assertRaises(mr.Refused):
            mr.stage(SOURCE, TAG, "c" * 40, Path("."))
        with self.assertRaises(mr.Refused):
            mr.stage(SOURCE, "v9.9.9", SOURCE, Path("."))
        self.assertEqual(Path("Cargo.toml").read_text()[:11], "[workspace]")


class Publish(InTemp):
    def package(self, data=None):
        directory = Path("target/macos-assets")
        directory.mkdir(parents=True, exist_ok=True)
        (directory / f"rc2fx-{VERSION}-macos-arm64.zip").write_bytes(data or mac_zip())
        return directory

    def test_upload_rebuilds_sums_and_removes_only_the_pending_notice(self):
        gh = FakeGh()
        sums = mr.publish(gh, TAG, SOURCE, self.package())
        name = f"rc2fx-{VERSION}-macos-arm64.zip"
        self.assertIn((name, False), gh.uploads)
        self.assertIn(("SHA256SUMS", True), gh.uploads)
        expected = "".join(f"{sha(d)}  {n}\n" for n, d in sorted(gh.assets.items()) if n != "SHA256SUMS")
        self.assertEqual(sums, expected)
        self.assertEqual(gh.assets["SHA256SUMS"].decode(), expected)
        # Only the notes change: no latest flag is passed either way.
        self.assertEqual(gh.edits, [("Notes.\n", {})])

    def test_publishing_again_is_idempotent_and_keeps_latest(self):
        gh = FakeGh()
        mr.publish(gh, TAG, SOURCE, self.package())
        gh.uploads.clear()
        mr.publish(gh, TAG, SOURCE, self.package())
        self.assertEqual(gh.uploads, [("SHA256SUMS", True)])
        self.assertEqual(len(gh.edits), 1, "no notice left to remove")

    def test_a_different_existing_mac_package_is_never_replaced(self):
        gh = FakeGh()
        gh.assets[f"rc2fx-{VERSION}-macos-arm64.zip"] = b"someone else's build"
        with self.assertRaises(mr.Refused):
            mr.publish(gh, TAG, SOURCE, self.package())
        self.assertEqual(gh.uploads, [])
        self.assertEqual(gh.assets["SHA256SUMS"], b"old\n")

    def test_bad_packages_and_package_directories_are_refused(self):
        for data in [mac_zip(tamper=True), mac_zip(unlisted=True), b"not a zip"]:
            gh = FakeGh()
            with self.assertRaises(mr.Refused):
                mr.publish(gh, TAG, SOURCE, self.package(data))
            self.assertEqual(gh.uploads, [])
        directory = self.package()
        (directory / "other.zip").write_bytes(b"x")
        with self.assertRaises(mr.Refused):
            mr.publish(FakeGh(), TAG, SOURCE, directory)

    def test_duplicate_or_unsafe_zip_entries_are_refused_even_with_good_sums(self):
        body = b"binary"
        sums = f"{sha(body)}  rc2fx\n".encode()
        for entries in [
            [("rc2fx/rc2fx", body), ("rc2fx/rc2fx", body), ("rc2fx/SHA256SUMS", sums)],
            [("rc2fx/rc2fx", body), ("other/x", b"x"), ("rc2fx/SHA256SUMS", sums)],
            [("rc2fx/rc2fx", body), ("rc2fx/../x", b"x"), ("rc2fx/SHA256SUMS", sums)],
        ]:
            gh = FakeGh()
            import warnings
            with warnings.catch_warnings():
                warnings.simplefilter("ignore")  # zipfile warns on the duplicate
                data = raw_zip(entries)
            with self.assertRaises(mr.Refused, msg=entries):
                mr.publish(gh, TAG, SOURCE, self.package(data))
            self.assertEqual(gh.uploads, [])
        # The real layout passes.
        good = raw_zip([("rc2fx/rc2fx", body), ("rc2fx/SHA256SUMS", sums)])
        mr.publish(FakeGh(), TAG, SOURCE, self.package(good))

    def test_repeated_release_asset_names_are_refused(self):
        gh = FakeGh()
        original = gh._release
        def doubled():
            release = original()
            release["assets"].append(dict(release["assets"][0]))
            return release
        gh._release = doubled
        with self.assertRaises(mr.Refused):
            mr.publish(gh, TAG, SOURCE, self.package())

    def test_unexpected_or_missing_release_assets_are_refused(self):
        for change in [
            lambda gh: gh.assets.update({"surprise.txt": b"x"}),
            lambda gh: gh.assets.pop(f"rc2fx-{VERSION}-windows-x64.zip"),
        ]:
            gh = FakeGh()
            change(gh)
            with self.assertRaises(mr.Refused):
                mr.publish(gh, TAG, SOURCE, self.package())
            # Refused before anything was uploaded.
            self.assertEqual(gh.uploads, [])
            self.assertEqual(gh.assets["SHA256SUMS"], b"old\n")
        with self.assertRaises(mr.Refused):
            mr.publish(FakeGh(), TAG, "not-a-commit", self.package())

    def test_an_unpublished_or_mismatched_release_is_refused(self):
        gh = FakeGh()
        gh.published = False
        with self.assertRaises(mr.Refused):
            mr.publish(gh, TAG, SOURCE, self.package())
        with self.assertRaises(mr.Refused):
            mr.publish(FakeGh(), TAG, "c" * 40, self.package())
        with self.assertRaises(mr.Refused):
            mr.publish(FakeGh(), "mixer-v0.2.0", SOURCE, self.package())


if __name__ == "__main__":
    unittest.main()
