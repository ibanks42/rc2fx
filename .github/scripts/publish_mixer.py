"""Publish an already signed mixer release; make it discoverable only afterward."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time
import urllib.request

# Discovery metadata changes, so it lives on a branch; releases are immutable.
BRANCH = "mixer-metadata"
REMOTE = "https://github.com/ibanks42/rc2fx.git"
RAW_URL = f"https://raw.githubusercontent.com/ibanks42/rc2fx/{BRANCH}/"
SIGNED_ROLES = ["targets.json", "snapshot.json", "timestamp.json"]
COMMITTER = "RC2FX release tooling"
COMMITTER_EMAIL = "rc2fx-release@users.noreply.github.com"
README = """# RC2FX mixer update metadata

Signed TUF metadata for RC2FX mixer updates, read by the desktop app. This branch
is written only by the release tooling; see docs/developer/mixer-releases.md on main.
"""
# Longer than raw.githubusercontent.com's five-minute cache.
SERVED_WAIT = 420
SERVED_POLL = 15


def gh(*args):
    return subprocess.check_output(["gh", *map(str, args)], text=True)


def git(*args, cwd=None):
    # gh supplies the credential, so a token never appears in a URL or argument.
    helper = ["-c", "credential.helper=", "-c", "credential.helper=!gh auth git-credential"]
    return subprocess.check_output(["git", *helper, *map(str, args)], text=True, cwd=cwd)


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


class Branch:
    """A checkout of the discovery metadata branch in `path`; `tip` is None until it exists."""

    def __init__(self, path, tip):
        self.path, self.tip = path, tip

    def version(self, name):
        return json.loads((self.path / name).read_text())["signed"]["version"]


def fetch_branch(path):
    git("init", "-q", path)
    git("remote", "add", "origin", REMOTE, cwd=path)
    listed = git("ls-remote", "origin", f"refs/heads/{BRANCH}", cwd=path).split()
    if not listed:
        git("symbolic-ref", "HEAD", f"refs/heads/{BRANCH}", cwd=path)
        return Branch(path, None)
    tip = listed[0]
    git("fetch", "-q", "--depth=1", "origin", tip, cwd=path)
    git("checkout", "-q", "-B", BRANCH, tip, cwd=path)
    return Branch(path, tip)


def require_newer(branch, metadata, names):
    """Refuse a stale signing artifact: it must not roll discovery back."""
    if branch.tip is None:
        return
    for name in names:
        new = json.loads((metadata / name).read_text())["signed"]["version"]
        if new <= branch.version(name):
            raise ValueError("Publication metadata version must increase")


def read_raw(name):
    request = urllib.request.Request(RAW_URL + name, headers={"Cache-Control": "no-cache"})
    with urllib.request.urlopen(request, timeout=30) as response:
        return response.read()


def verify_served(files):
    """raw.githubusercontent.com caches each file for minutes; wait for the push to appear."""
    deadline = time.monotonic() + SERVED_WAIT
    waiting = list(files)
    while True:
        remaining = []
        for file in waiting:
            try:
                served = read_raw(file.name)
            except OSError:
                served = None
            if served != file.read_bytes():
                remaining.append(file)
        waiting = remaining
        if not waiting:
            return
        if time.monotonic() >= deadline:
            names = ", ".join(file.name for file in waiting)
            raise ValueError(f"Pushed to {BRANCH}, but {RAW_URL} still does not serve the new {names}")
        time.sleep(SERVED_POLL)


def update_branch(branch, files, message):
    """One commit, so the branch never holds a partial set; a concurrent writer fails the push."""
    for file in files:
        shutil.copyfile(file, branch.path / file.name)
    readme = branch.path / "README.md"
    if not readme.exists():
        readme.write_text(README)
    git("add", "--all", cwd=branch.path)
    git("-c", f"user.name={COMMITTER}", "-c", f"user.email={COMMITTER_EMAIL}", "commit", "-q", "-m", message, cwd=branch.path)
    lease = f"--force-with-lease=refs/heads/{BRANCH}:{branch.tip or ''}"
    git("push", "-q", lease, "origin", f"HEAD:refs/heads/{BRANCH}", cwd=branch.path)
    verify_served(files)


def signed_json(metadata):
    return sorted(metadata.glob("*.json"))


def publish_discovery(metadata, version):
    # Fetch again: payload publication takes minutes, and only a fresh tip can
    # tell a renewal that ran meanwhile from a stale artifact.
    with tempfile.TemporaryDirectory() as temporary:
        branch = fetch_branch(Path(temporary))
        require_newer(branch, metadata, SIGNED_ROLES)
        update_branch(branch, signed_json(metadata), f"Publish mixer {version}")


def publish(package, metadata):
    tag, version, files = package_targets(package, metadata)
    # Check before creating a payload release: it can't be replaced afterward.
    with tempfile.TemporaryDirectory() as temporary:
        require_newer(fetch_branch(Path(temporary)), metadata, SIGNED_ROLES)
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
    publish_discovery(metadata, version)


def resume(package, metadata):
    """Finish a publication that stopped after its payload release was published."""
    tag, version, files = package_targets(package, metadata)
    release = state(tag)
    if release["isDraft"] or release["tagName"] != tag:
        raise ValueError("Resuming requires a published payload release")
    verify_upload(tag, files)
    publish_discovery(metadata, version)


def reauthorize(package, metadata):
    """Extend target authorization without changing any published target bytes."""
    tag, version, files = package_targets(package, metadata)
    release = state(tag)
    if release["isDraft"] or release["tagName"] != tag:
        raise ValueError("Reauthorization requires a published payload release")
    with tempfile.TemporaryDirectory() as temporary:
        branch = fetch_branch(Path(temporary))
        if branch.tip is None:
            raise ValueError("Reauthorization requires published metadata")
        require_newer(branch, metadata, SIGNED_ROLES)
        old = json.loads((branch.path / "targets.json").read_text())["signed"]
        new = json.loads((metadata / "targets.json").read_text())["signed"]
        if new["targets"] != old["targets"]:
            raise ValueError("Reauthorization cannot change target identities, pins or compatibility")
        verify_upload(tag, files)
        # Only authorization changes. In particular, never overwrite a payload,
        # catalog, notice or root during this operation.
        update_branch(branch, [metadata / name for name in SIGNED_ROLES], f"Reauthorize mixer {version}")


def refresh(metadata):
    """Publish renewed snapshot/timestamp only; never change targets or assets."""
    with tempfile.TemporaryDirectory() as temporary:
        branch = fetch_branch(Path(temporary))
        if branch.tip is None:
            raise ValueError("Only published mixer metadata can be renewed")
        if (branch.path / "targets.json").read_bytes() != (metadata / "targets.json").read_bytes():
            raise ValueError("Targets changed since renewal; refresh from current metadata")
        names = ["snapshot.json", "timestamp.json"]
        require_newer(branch, metadata, names)
        update_branch(branch, [metadata / name for name in names], "Renew discovery metadata")


if __name__ == "__main__":
    if sys.argv[1] == "--refresh":
        refresh(Path(sys.argv[2]))
    elif sys.argv[1] == "--reauthorize":
        reauthorize(Path(sys.argv[2]), Path(sys.argv[3]))
    elif sys.argv[1] == "--resume":
        resume(Path(sys.argv[2]), Path(sys.argv[3]))
    else:
        publish(Path(sys.argv[1]), Path(sys.argv[2]))
