#!/usr/bin/env python3
"""Offline tests for publish_app.py: a fake replaces `gh`, and a local bare
repository stands in for GitHub's metadata branch."""

import hashlib
import importlib.util
import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("publish_app", Path(__file__).with_name("publish_app.py"))
app = importlib.util.module_from_spec(spec)
spec.loader.exec_module(app)


def sha(data):
    return hashlib.sha256(data).hexdigest()


def assets(version):
    return {
        f"io.github.ibanks42.rc2fx-{version}-full.nupkg": f"package {version}".encode(),
        "releases.linux.json": f"feed {version}".encode(),
        "rc2fx-linux-x64.AppImage": f"appimage {version}".encode(),
    }


class FakeGh:
    """Published releases by tag, with GitHub's digests computed on upload."""

    def __init__(self):
        self.releases = {}
        self.downloads = []
        self.ids = iter(range(1, 10**6))

    def add(self, tag, files, sums=None, **fields):
        listing = sums if sums is not None else "".join(f"{sha(d)}  {n}\n" for n, d in sorted(files.items()))
        files = {**files, "SHA256SUMS": listing.encode()}
        self.releases[tag] = {
            "tag_name": tag, "draft": False, "prerelease": False,
            "assets": [{"id": next(self.ids), "name": n, "size": len(d), "state": "uploaded",
                        "digest": f"sha256:{sha(d)}", "data": d}
                       for n, d in sorted(files.items())],
            **fields,
        }
        return self.releases[tag]

    def release(self, tag):
        release = self.releases.get(tag)
        return None if release is None or release["draft"] else release

    def download(self, asset_id, path):
        self.downloads.append(asset_id)
        for release in self.releases.values():
            for asset in release["assets"]:
                if asset["id"] == asset_id:
                    Path(path).write_bytes(asset["data"])
                    return
        raise app.Refused("no such asset")


class Publication(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.temp = Path(temp.name)
        self.gh = FakeGh()
        self.remote = self.temp / "remote.git"
        subprocess.check_call(["git", "init", "-q", "--bare", self.remote])
        self.stale_reads = 0
        self.outputs = self.temp / "outputs"
        for patcher in [
            patch.object(app, "REMOTE", str(self.remote)),
            patch.object(app, "read_raw", self.read_raw),
            patch.object(app.time, "sleep", lambda _: None),
            patch.dict(os.environ, GITHUB_OUTPUT=str(self.outputs)),
        ]:
            patcher.start()
            self.addCleanup(patcher.stop)
        self.sequence = 100

    def branch(self, *args):
        return subprocess.check_output(["git", "-C", self.remote, *args], text=True)

    def commits(self):
        if not self.branch("for-each-ref", f"refs/heads/{app.BRANCH}"):
            return []
        return self.branch("log", "--format=%s", app.BRANCH).splitlines()

    def served(self, name):
        return subprocess.check_output(["git", "-C", self.remote, "show", f"{app.BRANCH}:{name}"])

    def read_raw(self, name):
        if self.stale_reads:
            self.stale_reads -= 1
            return b"stale"
        return self.served(name)

    def prepare(self, tag, name=None):
        directory = self.temp / (name or f"assets-{tag}")
        self.outputs.write_text("")
        message = app.prepare(self.gh, tag, directory)
        return directory, message

    def sign(self, tag, directory, version=None, roots=None):
        """What `cargo xtask app-sign` writes, without real signatures:
        publish_app trusts the signer for those, and the app checks them."""
        self.sequence += 1
        version = version or tag[1:]
        out = self.temp / f"signed-{self.sequence}"
        out.mkdir()
        targets = {}
        for path in sorted(directory.iterdir()):
            if path.name != "SHA256SUMS":
                data = path.read_bytes()
                targets[f"v{version}/{path.name}"] = {
                    "length": len(data), "hashes": {"sha256": sha(data)}, "custom": {"version": version}}
        (out / "targets.json").write_text(json.dumps({"signed": {"version": self.sequence, "targets": targets}}))
        for name in ["snapshot.json", "timestamp.json"]:
            (out / name).write_text(json.dumps({"signed": {"version": self.sequence}}))
        for name, data in (roots or {"1.root.json": b'{"root": 1}'}).items():
            (out / name).write_bytes(data)
        return out

    def release(self, version):
        """Publish, prepare, sign and publish metadata for app VERSION."""
        tag = f"v{version}"
        self.gh.add(tag, assets(version))
        directory, _ = self.prepare(tag)
        self.assertIn("sign=true", self.outputs.read_text())
        signed = self.sign(tag, directory)
        app.publish(tag, directory, signed)
        return directory, signed

    def test_a_release_is_downloaded_checked_signed_and_published_in_one_commit(self):
        directory, signed = self.release("1.2.3")
        self.assertEqual(self.outputs.read_text(), "version=1.2.3\nsign=true\n")
        self.assertEqual(sorted(p.name for p in directory.iterdir()), sorted([*assets("1.2.3"), "SHA256SUMS"]))
        self.assertEqual(self.commits(), ["Sign app 1.2.3"])
        names = self.branch("ls-tree", "--name-only", app.BRANCH).split()
        self.assertEqual(names, ["1.root.json", "README.md", "snapshot.json", "targets.json", "timestamp.json"])
        self.assertEqual(self.served("targets.json"), (signed / "targets.json").read_bytes())
        targets = json.loads(self.served("targets.json"))["signed"]["targets"]
        self.assertEqual(sorted(targets), sorted(f"v1.2.3/{n}" for n in assets("1.2.3")))

    def test_running_again_for_the_same_release_changes_nothing(self):
        directory, signed = self.release("1.2.3")
        again, message = self.prepare("v1.2.3", "again")
        self.assertIn("already signed", message)
        self.assertIn("sign=false", self.outputs.read_text())
        # A publish that lost a race to an identical one is done, not an error.
        self.assertIn("already signed", app.publish("v1.2.3", again, self.sign("v1.2.3", again)))
        self.assertEqual(self.commits(), ["Sign app 1.2.3"])

    def test_a_newer_release_replaces_the_signed_targets(self):
        self.release("1.2.3")
        self.release("1.3.0")
        self.assertEqual(self.commits(), ["Sign app 1.3.0", "Sign app 1.2.3"])
        targets = json.loads(self.served("targets.json"))["signed"]["targets"]
        self.assertTrue(all(name.startswith("v1.3.0/") for name in targets))

    def test_an_older_release_is_refused_as_a_rollback(self):
        self.release("1.3.0")
        self.gh.add("v1.2.9", assets("1.2.9"))
        with self.assertRaisesRegex(app.Refused, "roll back"):
            self.prepare("v1.2.9")
        # Signed anyway, it is still refused at publication.
        directory = self.temp / "old"
        app.download_release(self.gh, "v1.2.9", directory)
        with self.assertRaisesRegex(app.Refused, "roll back"):
            app.publish("v1.2.9", directory, self.sign("v1.2.9", directory))
        self.assertEqual(self.commits(), ["Sign app 1.3.0"])

    def test_the_same_release_with_different_assets_is_refused(self):
        self.release("1.2.3")
        changed = assets("1.2.3")
        changed["rc2fx-linux-x64.AppImage"] = b"rebuilt"
        self.gh.add("v1.2.3", changed)
        with self.assertRaisesRegex(app.Refused, "different assets"):
            self.prepare("v1.2.3", "changed")

    def test_a_publication_racing_a_newer_one_is_refused(self):
        self.gh.add("v1.2.3", assets("1.2.3"))
        directory, _ = self.prepare("v1.2.3")
        signed = self.sign("v1.2.3", directory)
        self.release("1.3.0")
        with self.assertRaisesRegex(app.Refused, "roll back"):
            app.publish("v1.2.3", directory, signed)

    def test_stale_metadata_versions_are_refused(self):
        _, signed = self.release("1.2.3")
        self.gh.add("v1.3.0", assets("1.3.0"))
        directory, _ = self.prepare("v1.3.0")
        stale = self.sign("v1.3.0", directory)
        for name in app.SIGNED_ROLES:
            document = json.loads((stale / name).read_text())
            document["signed"]["version"] = 1
            (stale / name).write_text(json.dumps(document))
        with self.assertRaisesRegex(app.Refused, "must increase"):
            app.publish("v1.3.0", directory, stale)

    def test_a_published_numbered_root_is_never_replaced(self):
        self.release("1.2.3")
        self.gh.add("v1.3.0", assets("1.3.0"))
        directory, _ = self.prepare("v1.3.0")
        signed = self.sign("v1.3.0", directory, roots={"1.root.json": b"other"})
        with self.assertRaisesRegex(app.Refused, "1.root.json"):
            app.publish("v1.3.0", directory, signed)
        rotated = self.sign("v1.3.0", directory, roots={"2.root.json": b'{"root": 2}'})
        app.publish("v1.3.0", directory, rotated)
        names = self.branch("ls-tree", "--name-only", app.BRANCH).split()
        self.assertIn("1.root.json", names)
        self.assertIn("2.root.json", names)

    def test_metadata_that_does_not_describe_the_assets_is_refused(self):
        self.gh.add("v1.2.3", assets("1.2.3"))
        directory, _ = self.prepare("v1.2.3")
        for change in ["version", "target", "extra file"]:
            signed = self.sign("v1.2.3", directory, version="1.2.4" if change == "version" else None)
            if change == "target":
                document = json.loads((signed / "targets.json").read_text())
                document["signed"]["targets"].popitem()
                (signed / "targets.json").write_text(json.dumps(document))
            if change == "extra file":
                (signed / "notes.txt").write_text("x")
            with self.subTest(change), self.assertRaises(app.Refused):
                app.publish("v1.2.3", directory, signed)
        self.assertEqual(self.commits(), [])

    def test_downloads_are_checked_against_github_and_the_release_sums(self):
        files = assets("1.2.3")
        cases = {
            "digest": lambda r: r["assets"][0].update(digest="sha256:" + "0" * 64),
            "no digest": lambda r: r["assets"][0].pop("digest"),
            "size": lambda r: r["assets"][0].update(size=1),
            "partial upload": lambda r: r["assets"][0].update(state="starter"),
            "unsafe name": lambda r: r["assets"][0].update(name="../x"),
            "no sums": lambda r: r.update(assets=[a for a in r["assets"] if a["name"] != "SHA256SUMS"]),
        }
        for number, (case, damage) in enumerate(cases.items()):
            with self.subTest(case):
                damage(self.gh.add("v1.2.3", files))
                with self.assertRaises(app.Refused) as refused:
                    self.prepare("v1.2.3", f"case-{number}")
                if case == "no sums":
                    self.assertIn("has no SHA256SUMS", str(refused.exception))
        listed = "".join(f"{sha(d)}  {n}\n" for n, d in sorted(files.items()))
        for number, sums in enumerate([
            listed.replace(sha(files["releases.linux.json"]), "0" * 64),
            listed + f"{'0' * 64}  missing.deb\n",
            "".join(line + "\n" for line in listed.splitlines()[1:]),
            listed + listed.splitlines()[0] + "\n",
            listed.replace("  ", " "),
        ]):
            with self.subTest(sums=number):
                self.gh.add("v1.2.3", files, sums=sums)
                with self.assertRaises(app.Refused):
                    self.prepare("v1.2.3", f"sums-{number}")
        self.assertEqual(self.commits(), [])

    def built(self, files, name="built"):
        """The release run's artifacts: its Linux and Windows packages with the
        SHA256SUMS written before macOS, and the macOS packages."""
        assets, macos = self.temp / name / "release-assets", self.temp / name / "macos-package"
        for directory in (assets, macos):
            directory.mkdir(parents=True)
        for file, data in files.items():
            ((macos if "macos" in file else assets) / file).write_bytes(data)
        (assets / "SHA256SUMS").write_text("written before the macOS packages\n")
        return [assets, macos]

    def test_a_release_run_signs_only_the_packages_it_built(self):
        files = {**assets("1.2.3"), "rc2fx-macos-arm64.zip": b"mac"}
        self.gh.add("v1.2.3", files)
        built = self.built(files)
        directory = self.temp / "bound"
        self.outputs.write_text("")
        app.prepare(self.gh, "v1.2.3", directory, built)
        self.assertIn("sign=true", self.outputs.read_text())
        cases = {
            "replaced": ({**files, "rc2fx-linux-x64.AppImage": b"other"}, "not the one this run built"),
            "added": ({**files, "extra.deb": b"x"}, "did not build"),
            "removed": ({n: d for n, d in files.items() if "macos" not in n}, "lacks"),
        }
        for number, (case, (published, message)) in enumerate(cases.items()):
            with self.subTest(case):
                self.gh.add("v1.2.3", published)
                with self.assertRaisesRegex(app.Refused, message):
                    app.prepare(self.gh, "v1.2.3", self.temp / f"bound-{number}", built)
        self.gh.add("v1.2.3", files)
        twice = self.built(files, "twice")
        (twice[1] / "rc2fx-linux-x64.AppImage").write_bytes(files["rc2fx-linux-x64.AppImage"])
        with self.assertRaisesRegex(app.Refused, "more than once"):
            app.prepare(self.gh, "v1.2.3", self.temp / "bound-twice", twice)
        self.assertEqual(self.commits(), [])

    def test_metadata_versions_stay_below_the_mixers(self):
        self.gh.add("v1.2.3", assets("1.2.3"))
        directory, _ = self.prepare("v1.2.3", "unix-time")
        signed = self.sign("v1.2.3", directory)
        # What `date -u +%s` gave when mixer 0.1.2 was signed.
        for name in app.SIGNED_ROLES:
            document = json.loads((signed / name).read_text())
            document["signed"]["version"] = 1794082929
            (signed / name).write_text(json.dumps(document))
        with self.assertRaisesRegex(app.Refused, "below the mixer"):
            app.publish("v1.2.3", directory, signed)
        self.assertEqual(self.commits(), [])
        _, signed = self.release("1.2.3")
        renewed = self.renewed(signed)
        for name in ["snapshot.json", "timestamp.json"]:
            (renewed / name).write_text(json.dumps({"signed": {"version": app.MIXER_VERSIONS_FROM}}))
        with self.assertRaisesRegex(app.Refused, "below the mixer"):
            app.refresh(renewed)

    def test_only_a_published_stable_release_is_signed(self):
        self.gh.add("v1.2.3", assets("1.2.3"), draft=True)
        with self.assertRaisesRegex(app.Refused, "not published"):
            self.prepare("v1.2.3", "draft")
        self.gh.add("v1.2.3", assets("1.2.3"), prerelease=True)
        with self.assertRaisesRegex(app.Refused, "prerelease"):
            self.prepare("v1.2.3", "prerelease")
        for tag in ["1.2.3", "v1.2", "v1.2.3-rc1", "mixer-v1.2.3"]:
            with self.assertRaisesRegex(app.Refused, "stable app tag"):
                self.prepare(tag, "tag")
        self.assertEqual(self.gh.downloads, [])

    def test_stale_raw_content_is_waited_for_and_then_reported(self):
        self.stale_reads = 5
        self.release("1.2.3")
        self.assertEqual(self.stale_reads, 0)
        self.stale_reads = 10**6
        self.gh.add("v1.3.0", assets("1.3.0"))
        directory, _ = self.prepare("v1.3.0")
        with patch.object(app, "SERVED_WAIT", 0), self.assertRaisesRegex(app.Refused, "still does not serve"):
            app.publish("v1.3.0", directory, self.sign("v1.3.0", directory))
        self.assertEqual(self.commits()[0], "Sign app 1.3.0", "the push itself stands")

    def test_a_concurrent_writer_fails_the_push_without_a_retry(self):
        self.release("1.2.3")
        self.gh.add("v1.3.0", assets("1.3.0"))
        directory, _ = self.prepare("v1.3.0")
        signed = self.sign("v1.3.0", directory)
        real = app.fetch_branch

        def raced(path):
            branch = real(path)
            other = self.temp / "other"
            subprocess.check_call(["git", "clone", "-q", "--branch", app.BRANCH, self.remote, other])
            subprocess.check_call(["git", "-C", other, "-c", "user.name=x", "-c", "user.email=x@example.com",
                                   "commit", "-q", "--allow-empty", "-m", "concurrent"])
            subprocess.check_call(["git", "-C", other, "push", "-q", "origin", app.BRANCH])
            return branch

        with patch.object(app, "fetch_branch", raced), self.assertRaises(subprocess.CalledProcessError):
            app.publish("v1.3.0", directory, signed)
        self.assertEqual(self.commits()[0], "concurrent", "the other writer's commit survives")

    def renewed(self, signed):
        renewed = self.temp / f"renewed-{self.sequence}"
        renewed.mkdir()
        self.sequence += 1
        (renewed / "targets.json").write_bytes((signed / "targets.json").read_bytes())
        for name in ["snapshot.json", "timestamp.json"]:
            (renewed / name).write_text(json.dumps({"signed": {"version": self.sequence}}))
        return renewed

    def test_renewal_commits_snapshot_and_timestamp_and_keeps_targets(self):
        _, signed = self.release("1.2.3")
        targets = self.served("targets.json")
        renewed = self.renewed(signed)
        app.refresh(renewed)
        self.assertEqual(self.commits(), ["Renew app discovery metadata", "Sign app 1.2.3"])
        self.assertEqual(self.served("timestamp.json"), (renewed / "timestamp.json").read_bytes())
        self.assertEqual(self.served("targets.json"), targets)
        with self.assertRaisesRegex(app.Refused, "increase"):
            app.refresh(renewed)
        (renewed / "targets.json").write_text("replaced")
        with self.assertRaisesRegex(app.Refused, "Targets changed"):
            app.refresh(renewed)

    def test_renewal_needs_published_metadata(self):
        renewed = self.temp / "renewed"
        renewed.mkdir()
        for name in app.SIGNED_ROLES:
            (renewed / name).write_text(json.dumps({"signed": {"version": 1}}))
        with self.assertRaisesRegex(app.Refused, "published"):
            app.refresh(renewed)


if __name__ == "__main__":
    unittest.main()
