"""Publish an already signed mixer release; make it discoverable only afterward."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile


def gh(*args):
    return subprocess.check_output(["gh", *map(str, args)], text=True)


def state(tag):
    return json.loads(gh("release", "view", tag, "--json", "id,tagName,isDraft,targetCommitish"))


def verify_upload(tag, files):
    with tempfile.TemporaryDirectory() as temporary:
        for file in files:
            gh("release", "download", tag, "--pattern", file.name, "--dir", temporary)
            actual = Path(temporary, file.name).read_bytes()
            if hashlib.sha256(actual).digest() != hashlib.sha256(file.read_bytes()).digest():
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
        if len(data) != pin["length"] or hashlib.sha256(data).hexdigest() != pin["hashes"]["sha256"]:
            raise ValueError("Packaged asset differs from signed metadata")
        files.append(file)
    return tag, version, files


def publish(package, metadata):
    tag, version, files = package_targets(package, metadata)
    metadata_tag = "mixer-metadata"
    releases = json.loads(gh("api", "repos/{owner}/{repo}/releases", "--paginate", "--slurp"))
    exists = any(r["tag_name"] == metadata_tag for page in releases for r in page)
    if exists:
        # Check before creating a payload release: a stale signing artifact
        # must not roll the discovery service back after a renewal or release.
        with tempfile.TemporaryDirectory() as temporary:
            for name in ["targets.json", "snapshot.json", "timestamp.json"]:
                gh("release", "download", metadata_tag, "--pattern", name, "--dir", temporary)
                old = json.loads(Path(temporary, name).read_text())["signed"]["version"]
                new = json.loads((metadata / name).read_text())["signed"]["version"]
                if new <= old:
                    raise ValueError("Publication metadata version must increase")
    # A new version is required for any correction. Existing releases are not
    # silently overwritten, even when they are still drafts.
    gh("release", "create", tag, "--draft", "--latest=false", "--target", os.environ["RELEASE_COMMIT"],
       "--title", f"RC2FX mixer {version}", "--notes", f"RC2FX mixer software {version}. Install through the RC2FX desktop app's USB firmware installer.")
    gh("release", "upload", tag, *files, package / "SHA256SUMS")
    verify_upload(tag, files + [package / "SHA256SUMS"])
    release = state(tag)
    if not release["isDraft"] or release["tagName"] != tag:
        raise ValueError("Release state changed before publication")
    gh("release", "edit", tag, "--draft=false", "--latest=false")
    # Discovery metadata may be refreshed, but its authenticity comes from TUF.
    # A transient mixture during upload fails verification; timestamp goes last.
    if not exists:
        gh("release", "create", metadata_tag, "--draft", "--latest=false", "--target", os.environ["RELEASE_COMMIT"],
           "--title", "Signed mixer update metadata", "--notes", "Machine-readable TUF metadata for the RC2FX installer.")
    ordered = sorted(p for p in metadata.glob("*.json") if p.name != "timestamp.json")
    ordered.append(metadata / "timestamp.json")
    for file in ordered:
        gh("release", "upload", metadata_tag, file, "--clobber")
        verify_upload(metadata_tag, [file])
    gh("release", "edit", metadata_tag, "--draft=false", "--latest=false")


def reauthorize(package, metadata):
    """Extend target authorization without changing any published target bytes."""
    tag, _, files = package_targets(package, metadata)
    release = state(tag)
    discovery = state("mixer-metadata")
    if release["isDraft"] or release["tagName"] != tag or discovery["isDraft"] or discovery["tagName"] != "mixer-metadata":
        raise ValueError("Reauthorization requires published payload and metadata releases")
    with tempfile.TemporaryDirectory() as temporary:
        for name in ["targets.json", "snapshot.json", "timestamp.json"]:
            gh("release", "download", "mixer-metadata", "--pattern", name, "--dir", temporary)
            old = json.loads(Path(temporary, name).read_text())["signed"]
            new = json.loads((metadata / name).read_text())["signed"]
            if new["version"] <= old["version"]:
                raise ValueError("Reauthorization version must increase")
            if name == "targets.json" and new["targets"] != old["targets"]:
                raise ValueError("Reauthorization cannot change target identities, pins or compatibility")
    verify_upload(tag, files)
    # Only authorization changes. In particular, never overwrite a payload,
    # catalog, notice or root during this operation.
    for name in ["targets.json", "snapshot.json", "timestamp.json"]:
        file = metadata / name
        gh("release", "upload", "mixer-metadata", file, "--clobber")
        verify_upload("mixer-metadata", [file])


def refresh(metadata):
    """Publish renewed snapshot/timestamp only; never change targets or assets."""
    tag = "mixer-metadata"
    current = state(tag)
    if current["isDraft"] or current["tagName"] != tag:
        raise ValueError("Only published mixer metadata can be renewed")
    with tempfile.TemporaryDirectory() as temporary:
        for name in ["targets.json", "snapshot.json", "timestamp.json"]:
            gh("release", "download", tag, "--pattern", name, "--dir", temporary)
        if Path(temporary, "targets.json").read_bytes() != (metadata / "targets.json").read_bytes():
            raise ValueError("Targets changed since renewal; refresh from current metadata")
        for name in ["snapshot.json", "timestamp.json"]:
            old = json.loads(Path(temporary, name).read_text())["signed"]["version"]
            new = json.loads((metadata / name).read_text())["signed"]["version"]
            if new <= old:
                raise ValueError("Renewal version must increase")
    for name in ["snapshot.json", "timestamp.json"]:
        file = metadata / name
        gh("release", "upload", tag, file, "--clobber")
        verify_upload(tag, [file])


if __name__ == "__main__":
    if sys.argv[1] == "--refresh":
        refresh(Path(sys.argv[2]))
    elif sys.argv[1] == "--reauthorize":
        reauthorize(Path(sys.argv[2]), Path(sys.argv[3]))
    else:
        publish(Path(sys.argv[1]), Path(sys.argv[2]))
