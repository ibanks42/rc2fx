import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("publish_mixer", Path(__file__).with_name("publish_mixer.py"))
publisher = importlib.util.module_from_spec(spec)
spec.loader.exec_module(publisher)


class Publication(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.package = Path(self.temp.name, "package")
        self.metadata = Path(self.temp.name, "metadata")
        self.package.mkdir()
        self.metadata.mkdir()
        self.tag = "mixer-v1.2.3"
        targets = {}
        for name, data in [("rc2fx-mixer-1.2.3.payload", b"verified payload"), ("THIRD-PARTY-NOTICES.html", b"notices")]:
            (self.package / name).write_bytes(data)
            pin = {"length": len(data), "hashes": {"sha256": hashlib.sha256(data).hexdigest()}}
            if name.endswith("payload"):
                pin["custom"] = {"rc2fxMixer": {"version": "1.2.3"}}
            targets[self.tag + "/" + name] = pin
        (self.package / "SHA256SUMS").write_text("checksums")
        (self.metadata / "targets.json").write_text(json.dumps({"signed": {"version": 2, "targets": targets}}))
        for name in ["1.root.json", "snapshot.json", "timestamp.json"]:
            (self.metadata / name).write_text(json.dumps({"signed": {"version": 2}}))
        self.calls = []
        self.releases = {}
        # Real git against a local bare repository stands in for GitHub's branch.
        self.remote = Path(self.temp.name, "remote.git")
        subprocess.check_call(["git", "init", "-q", "--bare", self.remote])
        self.stale_reads = 0
        self.reads = []
        for patcher in [
            patch.object(publisher, "gh", self.gh),
            patch.object(publisher, "REMOTE", str(self.remote)),
            patch.object(publisher, "read_raw", self.read_raw),
            patch.object(publisher.time, "sleep", lambda _: None),
            patch.dict(os.environ, RELEASE_COMMIT="commit"),
        ]:
            patcher.start()
            self.addCleanup(patcher.stop)

    def gh(self, *args):
        args = tuple(map(str, args))
        self.calls.append(args)
        operation, tag = args[1:3]
        if operation == "create":
            if tag in self.releases:
                raise RuntimeError("release already exists")
            self.releases[tag] = {"assets": {}, "isDraft": True}
        elif operation == "upload":
            for file in args[3:]:
                if file == "--clobber":
                    continue
                path = Path(file)
                self.releases[tag]["assets"][path.name] = path.read_bytes()
        elif operation == "download":
            name = args[args.index("--pattern") + 1]
            directory = args[args.index("--dir") + 1]
            Path(directory, name).write_bytes(self.releases[tag]["assets"][name])
        elif operation == "edit":
            self.releases[tag]["isDraft"] = False
        elif operation == "view":
            return json.dumps({"id": "1", "tagName": tag, "isDraft": self.releases[tag]["isDraft"], "targetCommitish": "commit"})
        return ""

    def branch(self, *args):
        return subprocess.check_output(["git", "-C", self.remote, *args], text=True)

    def commits(self):
        if not self.branch("for-each-ref", "refs/heads/mixer-metadata"):
            return []
        return self.branch("log", "--format=%s", "mixer-metadata").splitlines()

    def served(self, name):
        return subprocess.check_output(["git", "-C", self.remote, "show", f"mixer-metadata:{name}"])

    def read_raw(self, name):
        self.reads.append(name)
        if self.stale_reads:
            self.stale_reads -= 1
            return b"stale"
        return self.served(name)

    def publish(self):
        publisher.publish(self.package, self.metadata)

    def bump(self, names):
        for name in names:
            path = self.metadata / name
            document = json.loads(path.read_text())
            document["signed"]["version"] += 1
            path.write_text(json.dumps(document))

    def test_publishes_the_verified_payload_then_one_branch_commit(self):
        order = []
        real = publisher.update_branch
        with patch.object(publisher, "update_branch", lambda *a: (order.append(list(self.calls)), real(*a))):
            self.publish()
        before_branch = order[0]
        payload_publish = next(i for i, c in enumerate(before_branch) if c[:3] == ("release", "edit", self.tag))
        verification = [i for i, c in enumerate(before_branch) if c[:3] == ("release", "download", self.tag)]
        self.assertTrue(verification and all(i < payload_publish for i in verification))
        self.assertFalse(self.releases[self.tag]["isDraft"])
        self.assertEqual(self.commits(), ["Publish mixer 1.2.3"])
        names = self.branch("ls-tree", "--name-only", "mixer-metadata").split()
        self.assertEqual(names, ["1.root.json", "README.md", "snapshot.json", "targets.json", "timestamp.json"])
        self.assertEqual(self.served("targets.json"), (self.metadata / "targets.json").read_bytes())
        self.assertFalse("mixer-metadata" in self.releases, "no release is created for metadata")

    def test_a_failed_payload_check_stops_before_the_branch_is_written(self):
        real = self.gh

        def corrupting(*args):
            result = real(*args)
            if args[:2] == ("release", "upload"):
                self.releases[self.tag]["assets"]["rc2fx-mixer-1.2.3.payload"] = b"damaged"
            return result

        with patch.object(publisher, "gh", corrupting), self.assertRaisesRegex(ValueError, "differs"):
            self.publish()
        self.assertTrue(self.releases[self.tag]["isDraft"])
        self.assertEqual(self.commits(), [])

    def test_stale_raw_content_is_retried_until_it_appears(self):
        self.stale_reads = 5
        self.publish()
        self.assertEqual(self.stale_reads, 0)
        self.assertGreater(len(self.reads), 4)

    def test_raw_content_that_never_appears_is_an_error_naming_the_files(self):
        self.stale_reads = 10**6
        with patch.object(publisher, "SERVED_WAIT", 0), self.assertRaisesRegex(ValueError, "still does not serve"):
            self.publish()
        self.assertEqual(self.commits(), ["Publish mixer 1.2.3"], "the push itself stands")

    def test_a_concurrent_writer_fails_the_push_without_a_retry(self):
        self.publish()
        self.bump(["snapshot.json", "timestamp.json"])
        real = publisher.fetch_branch

        def raced(path):
            branch = real(path)
            other = Path(self.temp.name, "other")
            subprocess.check_call(["git", "clone", "-q", "--branch", "mixer-metadata", self.remote, other])
            subprocess.check_call(["git", "-C", other, "-c", "user.name=x", "-c", "user.email=x@example.com",
                                   "commit", "-q", "--allow-empty", "-m", "concurrent"])
            subprocess.check_call(["git", "-C", other, "push", "-q", "origin", "mixer-metadata"])
            return branch

        pushes = []
        real_git = publisher.git

        def counting(*args, cwd=None):
            pushes.extend(a for a in args if a == "push")
            return real_git(*args, cwd=cwd)

        with patch.object(publisher, "fetch_branch", raced), patch.object(publisher, "git", counting), \
                self.assertRaises(subprocess.CalledProcessError):
            publisher.refresh(self.metadata)
        self.assertEqual(len(pushes), 1)
        self.assertEqual(self.commits()[0], "concurrent", "the other writer's commit survives")

    def test_a_publication_stopped_after_the_payload_can_be_resumed(self):
        with patch.object(publisher, "publish_discovery", side_effect=RuntimeError("stopped")), \
                self.assertRaises(RuntimeError):
            self.publish()
        self.assertEqual(self.commits(), [])
        with self.assertRaisesRegex(RuntimeError, "already exists"):
            self.publish()
        publisher.resume(self.package, self.metadata)
        self.assertEqual(self.commits(), ["Publish mixer 1.2.3"])

    def test_changed_signed_payload_fails_before_any_remote_mutation(self):
        (self.package / "rc2fx-mixer-1.2.3.payload").write_bytes(b"replaced")
        with self.assertRaisesRegex(ValueError, "differs"):
            self.publish()
        self.assertEqual(self.calls, [])
        self.assertEqual(self.commits(), [])

    def test_existing_release_is_not_overwritten(self):
        self.releases[self.tag] = {"assets": {}, "isDraft": False}
        with self.assertRaisesRegex(RuntimeError, "already exists"):
            self.publish()
        self.assertFalse(any(c[:2] in [("release", "upload"), ("release", "edit")] for c in self.calls))

    def test_stale_publication_does_not_create_or_modify_any_release(self):
        self.publish()
        # Leave metadata published, but simulate an unpublished new payload.
        del self.releases[self.tag]
        self.calls.clear()
        with self.assertRaisesRegex(ValueError, "must increase"):
            self.publish()
        self.assertFalse(any(c[:2] in [("release", "create"), ("release", "upload"), ("release", "edit")] for c in self.calls))

    def test_refresh_commits_snapshot_and_timestamp_and_keeps_targets(self):
        self.publish()
        targets = self.served("targets.json")
        self.bump(["snapshot.json", "timestamp.json"])
        self.calls.clear()
        publisher.refresh(self.metadata)
        self.assertEqual(self.commits(), ["Renew discovery metadata", "Publish mixer 1.2.3"])
        self.assertEqual(self.served("timestamp.json"), (self.metadata / "timestamp.json").read_bytes())
        self.assertEqual(self.served("targets.json"), targets)
        self.assertEqual(self.calls, [], "renewal touches no release")
        with self.assertRaisesRegex(ValueError, "increase"):
            publisher.refresh(self.metadata)

    def test_refresh_refuses_changed_targets(self):
        self.publish()
        (self.metadata / "targets.json").write_text("replaced")
        with self.assertRaisesRegex(ValueError, "Targets changed"):
            publisher.refresh(self.metadata)
        self.assertEqual(len(self.commits()), 1)

    def test_refresh_needs_published_metadata(self):
        with self.assertRaisesRegex(ValueError, "published"):
            publisher.refresh(self.metadata)

    def renewed_authorization(self):
        self.publish()
        self.bump(["targets.json", "snapshot.json", "timestamp.json"])
        self.calls.clear()

    def test_reauthorization_commits_once_and_preserves_assets(self):
        self.renewed_authorization()
        assets = dict(self.releases[self.tag]["assets"])
        publisher.reauthorize(self.package, self.metadata)
        self.assertEqual(self.commits(), ["Reauthorize mixer 1.2.3", "Publish mixer 1.2.3"])
        self.assertEqual(self.served("targets.json"), (self.metadata / "targets.json").read_bytes())
        self.assertFalse(any(c[:2] == ("release", "upload") for c in self.calls))
        self.assertEqual(self.releases[self.tag]["assets"], assets)

    def test_reauthorization_refuses_changed_compatibility(self):
        self.renewed_authorization()
        path = self.metadata / "targets.json"
        document = json.loads(path.read_text())
        target = document["signed"]["targets"][self.tag + "/rc2fx-mixer-1.2.3.payload"]
        target["custom"]["rc2fxMixer"]["minimumAppVersion"] = "9.0.0"
        path.write_text(json.dumps(document))
        with self.assertRaisesRegex(ValueError, "cannot change"):
            publisher.reauthorize(self.package, self.metadata)
        self.assertEqual(len(self.commits()), 1)

    def test_reauthorization_refuses_stale_metadata_or_changed_remote_payload(self):
        self.publish()
        with self.assertRaisesRegex(ValueError, "increase"):
            publisher.reauthorize(self.package, self.metadata)
        self.bump(["targets.json", "snapshot.json", "timestamp.json"])
        self.releases[self.tag]["assets"]["rc2fx-mixer-1.2.3.payload"] = b"tampered"
        with self.assertRaisesRegex(ValueError, "differs"):
            publisher.reauthorize(self.package, self.metadata)
        self.assertEqual(len(self.commits()), 1)


if __name__ == "__main__":
    unittest.main()
