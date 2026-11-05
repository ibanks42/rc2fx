import hashlib
import importlib.util
import json
import os
from pathlib import Path
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

    def gh(self, *args):
        args = tuple(map(str, args))
        self.calls.append(args)
        if args[0] == "api":
            return json.dumps([[{"tag_name": tag} for tag in self.releases]])
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

    def publish(self):
        with patch.object(publisher, "gh", self.gh), patch.dict(os.environ, RELEASE_COMMIT="commit"):
            publisher.publish(self.package, self.metadata)

    def test_verifies_assets_then_publishes_payload_before_timestamp(self):
        self.publish()
        payload_publish = next(i for i, c in enumerate(self.calls) if c[:3] == ("release", "edit", self.tag))
        payload_verification = [i for i, c in enumerate(self.calls) if c[:3] == ("release", "download", self.tag)]
        self.assertTrue(all(i < payload_publish for i in payload_verification))
        uploads = [(i, c) for i, c in enumerate(self.calls) if c[:3] == ("release", "upload", "mixer-metadata")]
        self.assertTrue(all(i > payload_publish for i, _ in uploads))
        self.assertTrue(uploads[-1][1][3].endswith("timestamp.json"))
        self.assertFalse(self.releases[self.tag]["isDraft"])
        self.assertFalse(self.releases["mixer-metadata"]["isDraft"])

    def test_changed_signed_payload_fails_before_any_remote_mutation(self):
        (self.package / "rc2fx-mixer-1.2.3.payload").write_bytes(b"replaced")
        with self.assertRaisesRegex(ValueError, "differs"):
            self.publish()
        self.assertEqual(self.calls, [])

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

    def test_refresh_preserves_targets_and_publishes_timestamp_last(self):
        self.publish()
        targets = self.releases["mixer-metadata"]["assets"]["targets.json"]
        for name in ["snapshot.json", "timestamp.json"]:
            (self.metadata / name).write_text(json.dumps({"signed": {"version": 3}}))
        self.calls.clear()
        with patch.object(publisher, "gh", self.gh):
            publisher.refresh(self.metadata)
        uploads = [Path(c[3]).name for c in self.calls if c[:2] == ("release", "upload")]
        self.assertEqual(uploads, ["snapshot.json", "timestamp.json"])
        self.assertEqual(self.releases["mixer-metadata"]["assets"]["targets.json"], targets)
        with patch.object(publisher, "gh", self.gh), self.assertRaisesRegex(ValueError, "increase"):
            publisher.refresh(self.metadata)

    def test_refresh_refuses_changed_targets(self):
        self.publish()
        (self.metadata / "targets.json").write_text("replaced")
        self.calls.clear()
        with patch.object(publisher, "gh", self.gh), self.assertRaisesRegex(ValueError, "Targets changed"):
            publisher.refresh(self.metadata)
        self.assertFalse(any(c[:2] == ("release", "upload") for c in self.calls))

    def renewed_authorization(self):
        self.publish()
        for name in ["targets.json", "snapshot.json", "timestamp.json"]:
            path = self.metadata / name
            document = json.loads(path.read_text())
            document["signed"]["version"] += 1
            path.write_text(json.dumps(document))
        self.calls.clear()

    def test_reauthorization_preserves_assets_and_publishes_timestamp_last(self):
        self.renewed_authorization()
        assets = dict(self.releases[self.tag]["assets"])
        with patch.object(publisher, "gh", self.gh):
            publisher.reauthorize(self.package, self.metadata)
        uploads = [c for c in self.calls if c[:2] == ("release", "upload")]
        self.assertEqual([Path(c[3]).name for c in uploads], ["targets.json", "snapshot.json", "timestamp.json"])
        self.assertTrue(all(c[2] == "mixer-metadata" for c in uploads))
        self.assertEqual(self.releases[self.tag]["assets"], assets)

    def test_reauthorization_refuses_changed_compatibility(self):
        self.renewed_authorization()
        path = self.metadata / "targets.json"
        document = json.loads(path.read_text())
        target = document["signed"]["targets"][self.tag + "/rc2fx-mixer-1.2.3.payload"]
        target["custom"]["rc2fxMixer"]["minimumAppVersion"] = "9.0.0"
        path.write_text(json.dumps(document))
        with patch.object(publisher, "gh", self.gh), self.assertRaisesRegex(ValueError, "cannot change"):
            publisher.reauthorize(self.package, self.metadata)
        self.assertFalse(any(c[:2] == ("release", "upload") for c in self.calls))

    def test_reauthorization_refuses_stale_metadata_or_changed_remote_payload(self):
        self.publish()
        self.calls.clear()
        with patch.object(publisher, "gh", self.gh), self.assertRaisesRegex(ValueError, "increase"):
            publisher.reauthorize(self.package, self.metadata)
        self.assertFalse(any(c[:2] == ("release", "upload") for c in self.calls))
        for name in ["targets.json", "snapshot.json", "timestamp.json"]:
            path = self.metadata / name
            document = json.loads(path.read_text())
            document["signed"]["version"] += 1
            path.write_text(json.dumps(document))
        self.releases[self.tag]["assets"]["rc2fx-mixer-1.2.3.payload"] = b"tampered"
        self.calls.clear()
        with patch.object(publisher, "gh", self.gh), self.assertRaisesRegex(ValueError, "differs"):
            publisher.reauthorize(self.package, self.metadata)
        self.assertFalse(any(c[:2] == ("release", "upload") for c in self.calls))


if __name__ == "__main__":
    unittest.main()
