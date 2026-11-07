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
    files = {"RC2FX.app/Contents/MacOS/rc2fx": b"binary", "RC2FX.app/Contents/Info.plist": b"plist",
             "LICENSES.html": b"notices"}
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


MAC = f"rc2fx-{VERSION}-macos-arm64.zip"


def other(tag, draft=False, prerelease=False):
    """Another release in the repository."""
    return {"id": 1, "tag_name": tag, "draft": draft, "prerelease": prerelease, "assets": []}


class FakeGh:
    """An in-memory repository. `ops` records every mutating call in order."""

    def __init__(self, input_files=None):
        self.repo = REPO
        self.run = {
            "repository": {"full_name": REPO}, "path": ".github/workflows/release.yml",
            "event": "workflow_dispatch", "head_branch": "main", "status": "completed",
            "conclusion": "success", "head_sha": SOURCE,
        }
        self.artifact = {"name": "release-input", "expired": False}
        self.exists = True
        self.tag_pushed = None  # defaults to whether the release exists
        self.draft = True
        self.body = "Notes.\n\n" + PENDING
        self.commit = {"sha": RELEASE_COMMIT, "commit": {"message": f"chore(release): {TAG}\n"},
                       "parents": [{"sha": SOURCE}]}
        self.input_files = input_files if input_files is not None else {
            "plan.json": json.dumps(plan()).encode()}
        self.assets = {}
        for name in mr.expected_assets(VERSION) - {MAC}:
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
        if path == "actions/runs/7":
            return self.run
        if path == "actions/runs/7/artifacts?name=release-input":
            return {"artifacts": [self.artifact] if self.artifact else []}
        if path == f"commits/{TAG}":
            pushed = self.exists if self.tag_pushed is None else self.tag_pushed
            return self.commit if pushed else None
        return None

    def download_artifact(self, run_id, name, directory):
        directory = Path(directory)
        directory.mkdir(parents=True, exist_ok=True)
        for file, data in self.input_files.items():
            (directory / file).write_bytes(data)

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


def published_complete(zip_bytes):
    """A fake whose release is already published and complete."""
    gh = FakeGh()
    gh.assets[MAC] = zip_bytes
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


class Prepare(InTemp):
    def outputs(self, gh, automatic=False):
        lines = []
        return mr.prepare(gh, 7, automatic, lines.append)

    def test_a_draft_release_is_ready(self):
        self.assertEqual(self.outputs(FakeGh()), {"source": SOURCE, "tag": TAG, "ready": "true"})

    def test_published_complete_is_nothing_to_do_and_incomplete_is_refused(self):
        self.assertEqual(self.outputs(published_complete(mac_zip()))["ready"], "false")
        for path in Path(".release-input").iterdir():
            path.unlink()
        early = FakeGh()
        early.draft = False  # published by hand before macOS: finished
        self.assertEqual(self.outputs(early, automatic=True)["ready"], "true")
        for path in Path(".release-input").iterdir():
            path.unlink()
        locked = FakeGh()
        locked.draft = False
        locked.immutable = True  # incomplete and immutable: can't be finished
        with self.assertRaises(mr.Refused):
            self.outputs(locked, automatic=True)
        self.assertEqual(locked.ops, [])

    def test_a_draft_behind_a_published_release_is_refused_before_building(self):
        gh = FakeGh()
        gh.others = [other("v0.10.0")]
        with self.assertRaisesRegex(mr.Refused, "older than published release v0.10.0"):
            self.outputs(gh)

    def test_a_complete_release_behind_a_newer_one_is_still_nothing_to_do(self):
        gh = published_complete(mac_zip())
        gh.others = [other("v0.10.0")]
        self.assertEqual(self.outputs(gh)["ready"], "false")

    def test_a_pushed_tag_without_a_visible_release_is_a_loud_error(self):
        # The draft exists, but this token cannot list drafts.
        gh = FakeGh()
        gh.exists = False
        gh.tag_pushed = True
        with self.assertRaises(mr.Refused) as caught:
            self.outputs(gh, automatic=True)
        self.assertIn("contents: write", str(caught.exception))
        for path in Path(".release-input").iterdir():
            path.unlink()
        # A pushed tag that is not the release commit on this source.
        gh.commit["parents"] = [{"sha": "c" * 40}]
        with self.assertRaises(mr.Refused) as caught:
            self.outputs(gh, automatic=True)
        self.assertIn("not on top of", str(caught.exception))

    def test_a_dry_run_is_skipped_automatically_and_refused_by_hand(self):
        gh = FakeGh()
        gh.exists = False
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

    def test_already_versioned_release_can_tag_the_exact_source(self):
        gh = FakeGh()
        gh.commit.update(sha=SOURCE, commit={"message": "fix(release): initial notes"},
                         parents=[{"sha": "c" * 40}])
        self.assertEqual(self.outputs(gh)["ready"], "true")

    def test_a_tag_that_is_not_the_release_commit_on_the_source_is_refused(self):
        for change in [
            lambda gh: gh.commit.update(sha="c" * 40, commit={"message": "fix: unrelated"}),
            lambda gh: gh.commit.update(parents=[{"sha": "c" * 40}]),
            lambda gh: gh.commit.update(parents=[{"sha": SOURCE}, {"sha": "d" * 40}]),
            lambda gh: gh.commit.update(commit={"message": "feat: something"}),
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
        (directory / MAC).write_bytes(data or mac_zip())
        return directory

    def test_the_draft_is_finished_and_published_last(self):
        gh = FakeGh()
        sums = mr.publish(gh, TAG, SOURCE, self.package())
        self.assertEqual(gh.ops[-1], ("publish", 99), "publishing is the last call")
        self.assertEqual([op for op, _ in gh.ops].count("publish"), 1)
        self.assertEqual(gh.ops[0], ("upload", MAC))
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
        gh.release_after = (1, other("v0.3.0"))  # right after the macOS upload
        with self.assertRaisesRegex(mr.Refused, "older than published release v0.3.0"):
            mr.publish(gh, TAG, SOURCE, self.package())
        self.assertNotIn("publish", [op for op, _ in gh.ops])
        self.assertTrue(gh.draft)

    def test_an_older_complete_release_stays_a_read_only_success(self):
        gh = published_complete(mac_zip())
        gh.others = [other("v0.3.0")]
        self.assertEqual(mr.publish(gh, TAG, SOURCE, self.package()), "already complete\n")
        self.assertEqual(gh.ops, [])

    def test_an_older_release_published_early_by_hand_is_still_finished(self):
        gh = FakeGh()
        gh.draft = False
        gh.others = [other("v0.3.0")]
        mr.publish(gh, TAG, SOURCE, self.package())
        self.assertNotIn("publish", [op for op, _ in gh.ops], "latest is left as chosen")
        self.assertTrue(mr.complete(gh, gh._release(), VERSION))

    def test_a_published_retry_is_read_only(self):
        gh = published_complete(mac_zip())
        self.assertEqual(mr.publish(gh, TAG, SOURCE, self.package()), "already complete\n")
        self.assertEqual(gh.ops, [])

    def test_a_release_published_early_by_hand_is_finished_but_not_republished(self):
        gh = FakeGh()
        gh.draft = False
        mr.publish(gh, TAG, SOURCE, self.package())
        self.assertIn(("upload", MAC), gh.ops)
        self.assertNotIn("publish", [op for op, _ in gh.ops], "latest is left as chosen")
        self.assertEqual(gh.assets["SHA256SUMS"].decode(), complete_sums(gh))
        self.assertNotIn("rc2fx:macos-pending", gh.body)

    def test_a_draft_published_by_hand_mid_run_is_finished_without_publishing_again(self):
        gh = FakeGh()
        gh.publish_by_hand_after = 1  # right after the macOS upload
        mr.publish(gh, TAG, SOURCE, self.package())
        self.assertFalse(gh.draft)
        self.assertNotIn("publish", [op for op, _ in gh.ops])
        self.assertEqual(gh.assets["SHA256SUMS"].decode(), complete_sums(gh))

    def test_immutable_incomplete_releases_and_different_zips_are_refused(self):
        changes = [
            lambda gh: (setattr(gh, "draft", False), setattr(gh, "immutable", True)),
            lambda gh: (setattr(gh, "draft", False), gh.assets.update({MAC: b"another build"})),
            lambda gh: gh.assets.pop(f"rc2fx-{VERSION}-windows-x64.zip"),
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
            "digest mismatch": lambda gh: gh.bad_digest.add(f"rc2fx-{VERSION}-linux-x64.deb"),
            "missing package": lambda gh: gh.assets.pop(f"rc2fx-{VERSION}-linux-x64.rpm"),
            "unexpected asset": lambda gh: gh.assets.update({"surprise.txt": b"x"}),
            "different mac zip": lambda gh: gh.assets.update({MAC: b"another build"}),
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
        mr.publish(gh, TAG, SOURCE, self.package())
        self.assertNotIn(("upload", MAC), gh.ops)
        self.assertEqual(gh.ops[-1], ("publish", 99))

    def test_bad_packages_and_package_directories_are_refused(self):
        for data in [mac_zip(tamper=True), mac_zip(unlisted=True), b"not a zip"]:
            gh = FakeGh()
            with self.assertRaises(mr.Refused):
                mr.publish(gh, TAG, SOURCE, self.package(data))
            self.assertEqual(gh.ops, [])
        directory = self.package()
        (directory / "other.zip").write_bytes(b"x")
        with self.assertRaises(mr.Refused):
            mr.publish(FakeGh(), TAG, SOURCE, directory)

    def test_duplicate_or_unsafe_zip_entries_are_refused_even_with_good_sums(self):
        import warnings
        body = b"binary"
        exe, info = "RC2FX.app/Contents/MacOS/rc2fx", "RC2FX.app/Contents/Info.plist"
        app = [("rc2fx/" + exe, body), ("rc2fx/" + info, body)]
        sums = f"{sha(body)}  {exe}\n{sha(body)}  {info}\n".encode()
        for entries in [
            app + [("rc2fx/" + exe, body), ("rc2fx/SHA256SUMS", sums)],
            app + [("other/x", b"x"), ("rc2fx/SHA256SUMS", sums)],
            app + [("rc2fx/../x", b"x"), ("rc2fx/SHA256SUMS", sums)],
        ]:
            gh = FakeGh()
            with warnings.catch_warnings():
                warnings.simplefilter("ignore")
                data = raw_zip(entries)
            with self.assertRaises(mr.Refused):
                mr.publish(gh, TAG, SOURCE, self.package(data))
            self.assertEqual(gh.ops, [])
        good = raw_zip(app + [("rc2fx/SHA256SUMS", sums)])
        mr.publish(FakeGh(), TAG, SOURCE, self.package(good))

    def test_a_bare_executable_without_the_app_is_refused(self):
        body = b"binary"
        bare = raw_zip([("rc2fx/rc2fx", body), ("rc2fx/SHA256SUMS", f"{sha(body)}  rc2fx\n".encode())])
        gh = FakeGh()
        with self.assertRaisesRegex(mr.Refused, "RC2FX.app"):
            mr.publish(gh, TAG, SOURCE, self.package(bare))
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
            self.gh.upload_asset(5, Path("rc2fx-1.0.0-macos-arm64.zip"))
        self.assertTrue(self.calls[-1][-3].startswith("https://uploads.github.com/repos/"))


if __name__ == "__main__":
    unittest.main()
