"""Shared git, GitHub API, hashing, and TUF metadata branch helpers for release scripts."""

import hashlib
import json
import os
import re
import shutil
import subprocess
import tempfile
import time
import urllib.request
from pathlib import Path

REMOTE = "https://github.com/ibanks42/rc2fx.git"
SIGNED_ROLES = ["targets.json", "snapshot.json", "timestamp.json"]
COMMITTER = "RC2FX release tooling"
COMMITTER_EMAIL = "rc2fx-release@users.noreply.github.com"
STABLE = r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)"
# Exceeds raw.githubusercontent.com's 5-minute CDN cache TTL.
SERVED_WAIT = 420
SERVED_POLL = 15


class Refused(Exception):
    pass


def require(condition, message, error_cls=Refused):
    if not condition:
        raise error_cls(message)


def gh_repo():
    repo = os.environ.get("GH_REPO") or os.environ.get("GITHUB_REPOSITORY", "")
    require(re.fullmatch(r"[\w.-]+/[\w.-]+", repo), "GH_REPO or GITHUB_REPOSITORY is not set")
    return repo


def gh_api_get(repo, path, *, run=None, label=None):
    """Return parsed JSON for repos/REPO/PATH, or None on 404."""
    runner = subprocess.run if run is None else run
    endpoint = f"repos/{repo}/{path}"
    result = runner(["gh", "api", endpoint], capture_output=True, text=True)
    if result.returncode != 0:
        if "HTTP 404" in result.stderr or "Not Found" in result.stderr:
            return None
        raise Refused(f"GitHub API {label or endpoint}: {result.stderr.strip()}")
    return json.loads(result.stdout)


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def sha256_file(path):
    digest = hashlib.sha256()
    with open(path, "rb") as stream:
        for block in iter(lambda: stream.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def git(*args, cwd=None):
    # Use gh as credential helper so tokens never appear in URLs or argv.
    helper = ["-c", "credential.helper=", "-c", "credential.helper=!gh auth git-credential"]
    return subprocess.check_output(["git", *helper, *map(str, args)], text=True, cwd=cwd)


class Branch:
    """Checkout of a metadata branch at `path` (`tip` is None if the branch does not exist yet)."""

    def __init__(self, path, tip):
        self.path, self.tip = path, tip

    def version(self, name):
        return json.loads((self.path / name).read_text())["signed"]["version"]


def require_newer_versions(branch, metadata, names, *, max_version=None,
                           allow_missing=False, error_cls=ValueError):
    """Ensure new metadata versions strictly increase over the current branch."""
    if branch.tip is None and max_version is None:
        return
    for name in names:
        new = json.loads((metadata / name).read_text())["signed"]["version"]
        if max_version is not None:
            require(isinstance(new, int) and 0 < new < max_version,
                    f"{name} version {new} is not below the mixer's metadata versions", error_cls)
        if branch.tip is not None and (not allow_missing or (branch.path / name).exists()):
            require(new > branch.version(name), "Publication metadata version must increase", error_cls)


class MetadataRepo:
    """Git and raw.githubusercontent.com operations for an orphan TUF metadata branch."""

    def __init__(self, branch, readme, *, remote=REMOTE, raw_url=None, error_cls=ValueError,
                 served_wait=SERVED_WAIT, served_poll=SERVED_POLL):
        self.branch = branch
        self.readme = readme
        self.remote = remote
        self.raw_url = raw_url or f"https://raw.githubusercontent.com/ibanks42/rc2fx/{branch}/"
        self.error_cls = error_cls
        self.served_wait = served_wait
        self.served_poll = served_poll

    def git(self, *args, cwd=None):
        return git(*args, cwd=cwd)

    def sleep(self, seconds):
        time.sleep(seconds)

    def fetch(self, path):
        self.git("init", "-q", path)
        self.git("remote", "add", "origin", self.remote, cwd=path)
        listed = self.git("ls-remote", "origin", f"refs/heads/{self.branch}", cwd=path).split()
        if not listed:
            self.git("symbolic-ref", "HEAD", f"refs/heads/{self.branch}", cwd=path)
            return Branch(path, None)
        tip = listed[0]
        self.git("fetch", "-q", "--depth=1", "origin", tip, cwd=path)
        self.git("checkout", "-q", "-B", self.branch, tip, cwd=path)
        return Branch(path, tip)

    def read_raw(self, name):
        request = urllib.request.Request(self.raw_url + name, headers={"Cache-Control": "no-cache"})
        with urllib.request.urlopen(request, timeout=30) as response:
            return response.read()

    def verify_served(self, files):
        """Poll raw.githubusercontent.com until all pushed files are served (CDN caches for ~5m)."""
        deadline = time.monotonic() + self.served_wait
        waiting = list(files)
        while True:
            remaining = []
            for file in waiting:
                try:
                    served = self.read_raw(file.name)
                except OSError:
                    served = None
                if served != file.read_bytes():
                    remaining.append(file)
            waiting = remaining
            if not waiting:
                return
            if time.monotonic() >= deadline:
                names = ", ".join(file.name for file in waiting)
                raise self.error_cls(
                    f"Pushed to {self.branch}, but {self.raw_url} still does not serve the new {names}")
            self.sleep(self.served_poll)

    def update(self, branch, files, message):
        """Commit all metadata files atomically and push with --force-with-lease."""
        for file in files:
            shutil.copyfile(file, branch.path / file.name)
        readme = branch.path / "README.md"
        if not readme.exists():
            readme.write_text(self.readme)
        self.git("add", "--all", cwd=branch.path)
        self.git("-c", f"user.name={COMMITTER}", "-c", f"user.email={COMMITTER_EMAIL}",
                 "commit", "-q", "-m", message, cwd=branch.path)
        lease = f"--force-with-lease=refs/heads/{self.branch}:{branch.tip or ''}"
        self.git("push", "-q", lease, "origin", f"HEAD:refs/heads/{self.branch}", cwd=branch.path)
        self.verify_served(files)

    def refresh(self, metadata, *, missing_message, commit_message, check_newer,
                require_targets_file=False):
        """Push renewed snapshot and timestamp metadata without changing targets."""
        metadata = Path(metadata)
        with tempfile.TemporaryDirectory() as temporary:
            branch = self.fetch(Path(temporary))
            if branch.tip is None or (require_targets_file and not (branch.path / "targets.json").exists()):
                raise self.error_cls(missing_message)
            if (branch.path / "targets.json").read_bytes() != (metadata / "targets.json").read_bytes():
                raise self.error_cls("Targets changed since renewal; refresh from current metadata")
            names = ["snapshot.json", "timestamp.json"]
            check_newer(branch, metadata, names)
            self.update(branch, [metadata / name for name in names], commit_message)


class BareRemote:
    """Local bare git repository standing in for a GitHub metadata branch in tests."""

    def __init__(self, temp_dir, branch):
        self.branch_name = branch
        self.remote = Path(temp_dir) / "remote.git"
        subprocess.check_call(["git", "init", "-q", "--bare", self.remote])
        self.stale_reads = 0
        self.reads = []

    def git(self, *args):
        return subprocess.check_output(["git", "-C", self.remote, *args], text=True)

    def commits(self):
        if not self.git("for-each-ref", f"refs/heads/{self.branch_name}"):
            return []
        return self.git("log", "--format=%s", self.branch_name).splitlines()

    def served(self, name):
        return subprocess.check_output(["git", "-C", self.remote, "show", f"{self.branch_name}:{name}"])

    def read_raw(self, name):
        self.reads.append(name)
        if self.stale_reads:
            self.stale_reads -= 1
            return b"stale"
        return self.served(name)

    def push_concurrent(self, work_dir, message="concurrent"):
        subprocess.check_call(["git", "clone", "-q", "--branch", self.branch_name, self.remote, work_dir])
        subprocess.check_call(["git", "-C", work_dir, "-c", "user.name=x", "-c", "user.email=x@example.com",
                               "commit", "-q", "--allow-empty", "-m", message])
        subprocess.check_call(["git", "-C", work_dir, "push", "-q", "origin", self.branch_name])
