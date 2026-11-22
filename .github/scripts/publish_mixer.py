"""Publish a signed mixer release, then update discovery metadata on `mixer-metadata`."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

from release_common import MetadataRepo, SIGNED_ROLES, require_newer_versions, sha256

# Discovery metadata is mutable so it lives on its own branch while releases stay immutable.
BRANCH = "mixer-metadata"
README = """# RC2FX mixer update metadata

Signed TUF metadata for RC2FX mixer updates, read by the desktop app. This branch
is written only by the release tooling; see docs/developer/mixer-releases.md on main.
"""
META = MetadataRepo(BRANCH, README, error_cls=ValueError)


def gh(*args):
    return subprocess.check_output(["gh", *map(str, args)], text=True)


def state(tag):
    return json.loads(gh("release", "view", tag, "--json", "id,tagName,isDraft,targetCommitish"))


def verify_upload(tag, files):
    with tempfile.TemporaryDirectory() as temporary:
        for file in files:
            gh("release", "download", tag, "--pattern", file.name, "--dir", temporary)
            actual = Path(temporary, file.name).read_bytes()
            if sha256(actual) != sha256(file.read_bytes()):
                raise ValueError(f"Uploaded {tag}/{file.name} differs from signed local bytes")


def package_targets(package, metadata):
    targets = json.loads((metadata / "targets.json").read_text())["signed"]["targets"]
    mixers = [(name, target) for name, target in targets.items() if "rc2fxMixer" in target.get("custom", {})]
    if len(mixers) != 1:
        raise ValueError("Expected exactly one mixer release")
    name, target = mixers[0]
    version = target["custom"]["rc2fxMixer"]["version"]
    tag = f"mixer-v{version}"
    if name != f"{tag}/rc2fx-mixer-{version}.payload":
        raise ValueError("Signed target path differs from mixer release identity")
    files = []
    for name, pin in targets.items():
        if not name.startswith(tag + "/") or Path(name).name != name.split("/", 1)[1]:
            raise ValueError("Unexpected target path")
        file = package / Path(name).name
        data = file.read_bytes()
        if len(data) != pin["length"] or sha256(data) != pin["hashes"]["sha256"]:
            raise ValueError("Packaged asset differs from signed metadata")
        files.append(file)
    return tag, version, files


def require_newer(branch, metadata, names):
    """Ensure new metadata versions strictly increase over the current branch."""
    require_newer_versions(branch, metadata, names, error_cls=ValueError)


def publish_discovery(metadata, version):
    # Re-fetch after uploading the payload in case a scheduled renewal ran concurrently.
    with tempfile.TemporaryDirectory() as temporary:
        branch = META.fetch(Path(temporary))
        require_newer(branch, metadata, SIGNED_ROLES)
        META.update(branch, sorted(metadata.glob("*.json")), f"Publish mixer {version}")


def publish(package, metadata):
    tag, version, files = package_targets(package, metadata)
    # Check before creating the release since published releases are immutable.
    with tempfile.TemporaryDirectory() as temporary:
        require_newer(META.fetch(Path(temporary)), metadata, SIGNED_ROLES)
    # Never overwrite an existing release (even a draft); fixes require a new version.
    gh("release", "create", tag, "--draft", "--latest=false", "--target", os.environ["RELEASE_COMMIT"],
       "--title", f"RC2FX mixer {version}", "--notes", f"RC2FX mixer software {version}. Install through the RC2FX desktop app's USB firmware installer.")
    gh("release", "upload", tag, *files, package / "SHA256SUMS")
    verify_upload(tag, files + [package / "SHA256SUMS"])
    release = state(tag)
    if not release["isDraft"] or release["tagName"] != tag:
        raise ValueError("Release state changed before publication")
    gh("release", "edit", tag, "--draft=false", "--latest=false")
    publish_discovery(metadata, version)


def resume(package, metadata):
    """Resume metadata publication after the payload release was already published."""
    tag, version, files = package_targets(package, metadata)
    release = state(tag)
    if release["isDraft"] or release["tagName"] != tag:
        raise ValueError("Resuming requires a published payload release")
    verify_upload(tag, files)
    publish_discovery(metadata, version)


def reauthorize(package, metadata):
    """Re-sign target authorization without changing any published target bytes."""
    tag, version, files = package_targets(package, metadata)
    release = state(tag)
    if release["isDraft"] or release["tagName"] != tag:
        raise ValueError("Reauthorization requires a published payload release")
    with tempfile.TemporaryDirectory() as temporary:
        branch = META.fetch(Path(temporary))
        if branch.tip is None:
            raise ValueError("Reauthorization requires published metadata")
        require_newer(branch, metadata, SIGNED_ROLES)
        old = json.loads((branch.path / "targets.json").read_text())["signed"]
        new = json.loads((metadata / "targets.json").read_text())["signed"]
        if new["targets"] != old["targets"]:
            raise ValueError("Reauthorization cannot change target identities, pins or compatibility")
        verify_upload(tag, files)
        # Only update the signed role files (leave roots/payloads untouched).
        META.update(branch, [metadata / name for name in SIGNED_ROLES], f"Reauthorize mixer {version}")


def refresh(metadata):
    """Push renewed snapshot and timestamp metadata without changing targets."""
    META.refresh(
        metadata,
        missing_message="Only published mixer metadata can be renewed",
        commit_message="Renew discovery metadata",
        check_newer=require_newer,
    )


if __name__ == "__main__":
    if sys.argv[1] == "--refresh":
        refresh(Path(sys.argv[2]))
    elif sys.argv[1] == "--reauthorize":
        reauthorize(Path(sys.argv[2]), Path(sys.argv[3]))
    elif sys.argv[1] == "--resume":
        resume(Path(sys.argv[2]), Path(sys.argv[3]))
    else:
        publish(Path(sys.argv[1]), Path(sys.argv[2]))
