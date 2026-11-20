#!/usr/bin/env python3
"""Sign a published app release into the `app-metadata` TUF branch, or renew it.

App updates use their own orphan branch (`app-metadata`) served via
raw.githubusercontent.com, sharing the mixer's TUF root and release keys. Every
release asset except SHA256SUMS is recorded as target `vVERSION/ASSET`.

  prepare --tag TAG --dir DIR [--built DIR]...
      Download all assets of TAG into DIR and verify them against GitHub's
      digests and SHA256SUMS. With --built, also require the release assets to
      match the build outputs byte for byte (excluding SHA256SUMS). Checks the
      current branch to skip already-signed identical assets and refuse
      downgrades or conflicting re-signs. Writes `version` and `sign` to
      GITHUB_OUTPUT when set.

  publish --tag TAG DIR METADATA
      Verify METADATA matches DIR, push it in a single commit using
      --force-with-lease, and wait until raw.githubusercontent.com serves it.

  refresh METADATA
      Push renewed snapshot and timestamp metadata without changing targets.

App metadata versions use `unix_time - 1e9`, keeping them strictly below mixer
metadata versions (raw Unix timestamps) so app metadata can't be replayed at the
mixer metadata URL.

Requires `gh` with GH_TOKEN and GH_REPO (or GITHUB_REPOSITORY). Exits 1 on refusal.
"""

import argparse
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
import urllib.request
from pathlib import Path

BRANCH = "app-metadata"
REMOTE = "https://github.com/ibanks42/rc2fx.git"
RAW_URL = f"https://raw.githubusercontent.com/ibanks42/rc2fx/{BRANCH}/"
SUMS = "SHA256SUMS"
SIGNED_ROLES = ["targets.json", "snapshot.json", "timestamp.json"]
COMMITTER = "RC2FX release tooling"
COMMITTER_EMAIL = "rc2fx-release@users.noreply.github.com"
README = """# RC2FX app update metadata

Signed TUF metadata for RC2FX desktop app updates, read by the app to check a
downloaded update before applying it. This branch is written only by the release
tooling; see docs/developer/releases.md on main.
"""
STABLE = r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)"
PLAIN = re.compile(r"[A-Za-z0-9_+-][A-Za-z0-9._+-]*")
# Mixer metadata versions are Unix timestamps (>= 2026); keep app versions below them.
MIXER_VERSIONS_FROM = 1_700_000_000
# Exceeds raw.githubusercontent.com's 5-minute CDN cache TTL.
SERVED_WAIT = 420
SERVED_POLL = 15


class Refused(Exception):
    pass


def require(condition, message):
    if not condition:
        raise Refused(message)


class Gh:
    """Minimal GitHub CLI wrapper (replaced by a fake in tests)."""

    def __init__(self, repo):
        self.repo = repo

    def release(self, tag):
        """Return the published release for TAG, or None (drafts aren't returned by tag)."""
        result = subprocess.run(["gh", "api", f"repos/{self.repo}/releases/tags/{tag}"],
                                capture_output=True, text=True)
        if result.returncode != 0:
            if "HTTP 404" in result.stderr or "Not Found" in result.stderr:
                return None
            raise Refused(f"GitHub API releases/tags/{tag}: {result.stderr.strip()}")
        return json.loads(result.stdout)

    def download(self, asset_id, path):
        with open(path, "xb") as out:
            result = subprocess.run(
                ["gh", "api", "-H", "Accept: application/octet-stream",
                 f"repos/{self.repo}/releases/assets/{asset_id}"],
                stdout=out, stderr=subprocess.PIPE, text=False)
        if result.returncode != 0:
            raise Refused(f"downloading asset {asset_id} failed: {result.stderr.decode().strip()}")


def git(*args, cwd=None):
    # Use gh as credential helper so tokens never appear in URLs or argv.
    helper = ["-c", "credential.helper=", "-c", "credential.helper=!gh auth git-credential"]
    return subprocess.check_output(["git", *helper, *map(str, args)], text=True, cwd=cwd)


def version_of(tag):
    match = re.fullmatch("v" + STABLE, tag or "")
    require(match, f"{tag!r} is not a stable app tag")
    return tag[1:]


def version_key(version):
    return tuple(map(int, version.split(".")))


def sha256_file(path):
    digest = hashlib.sha256()
    with open(path, "rb") as stream:
        for block in iter(lambda: stream.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def parse_sums(text):
    """Parse SHA256SUMS into {name: sha256}, rejecting malformed or duplicate entries."""
    sums = {}
    for line in text.splitlines():
        match = re.fullmatch(r"([0-9a-f]{64})  (.+)", line)
        require(match and PLAIN.fullmatch(match[2]) and match[2] != SUMS,
                f"malformed {SUMS} line {line!r}")
        require(match[2] not in sums, f"{SUMS} lists {match[2]} more than once")
        sums[match[2]] = match[1]
    return sums


def asset_files(directory):
    """Verify all files in DIRECTORY against SHA256SUMS and return {name: (size, sha256)}."""
    directory = Path(directory)
    sums = parse_sums((directory / SUMS).read_text())
    files = {}
    for path in sorted(directory.iterdir()):
        if path.name == SUMS:
            continue
        require(path.is_file() and not path.is_symlink(), f"{path.name} is not a regular file")
        require(path.name in sums, f"{path.name} is not listed in {SUMS}")
        digest = sha256_file(path)
        require(digest == sums[path.name], f"{path.name} does not match {SUMS}")
        files[path.name] = (path.stat().st_size, digest)
    missing = sorted(set(sums) - set(files))
    require(not missing, f"{SUMS} lists {', '.join(missing)}, which the release does not hold")
    require(files, "the release has no assets to sign")
    return files


def download_release(gh, tag, directory):
    """Download all assets of TAG into DIRECTORY, verifying sizes and GitHub SHA-256 digests."""
    release = gh.release(tag)
    require(release is not None, f"release {tag} is not published")
    require(release.get("tag_name") == tag, f"release {tag} carries tag {release.get('tag_name')!r}")
    require(not release.get("draft"), f"release {tag} is a draft")
    require(not release.get("prerelease"), f"release {tag} is a prerelease")
    assets = release.get("assets") or []
    names = [a.get("name") for a in assets]
    require(len(set(names)) == len(names), "the release repeats an asset name")
    require(SUMS in names, f"release {tag} has no {SUMS}")
    directory = Path(directory)
    directory.mkdir(parents=True)
    for asset in assets:
        name = asset["name"]
        require(isinstance(name, str) and PLAIN.fullmatch(name), f"unsafe asset name {name!r}")
        require(asset.get("state") == "uploaded", f"{name} is not fully uploaded")
        digest = asset.get("digest") or ""
        require(re.fullmatch(r"sha256:[0-9a-f]{64}", digest), f"GitHub gives no SHA-256 digest for {name}")
        path = directory / name
        gh.download(asset["id"], path)
        require(path.stat().st_size == asset.get("size"), f"{name} does not have GitHub's size")
        require(sha256_file(path) == digest.removeprefix("sha256:"), f"{name} does not match GitHub's digest")
    return asset_files(directory)


def require_built(downloaded, built_dirs):
    """Ensure downloaded release assets match the run's build outputs byte for byte (excluding SHA256SUMS)."""
    built = {}
    for directory in map(Path, built_dirs):
        for path in sorted(directory.iterdir()):
            if path.name == SUMS:
                continue
            require(path.is_file() and not path.is_symlink(), f"built {path.name} is not a regular file")
            require(path.name not in built, f"{path.name} was built more than once")
            built[path.name] = path
    published = {path.name: path for path in Path(downloaded).iterdir() if path.name != SUMS}
    extra = sorted(set(published) - set(built))
    require(not extra, f"the release holds {', '.join(extra)}, which this run did not build")
    missing = sorted(set(built) - set(published))
    require(not missing, f"the release lacks {', '.join(missing)}, which this run built")
    for name, path in sorted(published.items()):
        require(path.stat().st_size == built[name].stat().st_size
                and sha256_file(path) == sha256_file(built[name]),
                f"the release's {name} is not the one this run built")


def expected_targets(version, files):
    return {f"v{version}/{name}": (length, digest, version) for name, (length, digest) in files.items()}


def signed_targets(targets_json):
    """Parse targets.json into {name: (length, sha256, version)}."""
    targets = json.loads(targets_json)["signed"]["targets"]
    return {name: (t["length"], t["hashes"]["sha256"], t.get("custom", {}).get("version"))
            for name, t in targets.items()}


def published_version(targets):
    """Return the single app version referenced by all signed targets."""
    versions = {version for _, _, version in targets.values()}
    require(len(versions) == 1, f"published metadata describes versions {sorted(map(str, versions))}")
    version = versions.pop()
    require(isinstance(version, str) and re.fullmatch(STABLE, version),
            f"published metadata has version {version!r}")
    return version


class Branch:
    """Checkout of the metadata branch at `path` (`tip` is None if the branch doesn't exist yet)."""

    def __init__(self, path, tip):
        self.path, self.tip = path, tip

    def version(self, name):
        return json.loads((self.path / name).read_text())["signed"]["version"]

    def targets(self):
        """Return signed targets from the branch, or None before the first release."""
        path = self.path / "targets.json"
        return signed_targets(path.read_text()) if self.tip and path.exists() else None


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


def decide(branch, version, expected):
    """Return True if VERSION should be signed, False if already signed identically.
    Refuses rollbacks and conflicting assets for the same version."""
    current = branch.targets()
    if current is None:
        return True
    published = published_version(current)
    require(version_key(version) >= version_key(published),
            f"app {published} is already signed; refusing to roll back to {version}")
    if version != published:
        return True
    require(current == expected,
            f"app {version} is already signed with different assets; publish a new version")
    return False


def require_newer(branch, metadata, names):
    """Ensure new metadata versions strictly increase and stay below mixer versions."""
    for name in names:
        new = json.loads((metadata / name).read_text())["signed"]["version"]
        require(isinstance(new, int) and 0 < new < MIXER_VERSIONS_FROM,
                f"{name} version {new} is not below the mixer's metadata versions")
        if branch.tip is not None and (branch.path / name).exists():
            require(new > branch.version(name), "Publication metadata version must increase")


def read_raw(name):
    request = urllib.request.Request(RAW_URL + name, headers={"Cache-Control": "no-cache"})
    with urllib.request.urlopen(request, timeout=30) as response:
        return response.read()


def verify_served(files):
    """Poll raw.githubusercontent.com until all pushed files are served (CDN caches for ~5m)."""
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
            raise Refused(f"Pushed to {BRANCH}, but {RAW_URL} still does not serve the new {names}")
        time.sleep(SERVED_POLL)


def update_branch(branch, files, message):
    """Commit all metadata files atomically and push with --force-with-lease."""
    for file in files:
        shutil.copyfile(file, branch.path / file.name)
    readme = branch.path / "README.md"
    if not readme.exists():
        readme.write_text(README)
    git("add", "--all", cwd=branch.path)
    git("-c", f"user.name={COMMITTER}", "-c", f"user.email={COMMITTER_EMAIL}",
        "commit", "-q", "-m", message, cwd=branch.path)
    lease = f"--force-with-lease=refs/heads/{BRANCH}:{branch.tip or ''}"
    git("push", "-q", lease, "origin", f"HEAD:refs/heads/{BRANCH}", cwd=branch.path)
    verify_served(files)


def write_outputs(**values):
    path = os.environ.get("GITHUB_OUTPUT")
    if path:
        with open(path, "a") as out:
            out.writelines(f"{key}={value}\n" for key, value in values.items())


def prepare(gh, tag, directory, built=None):
    version = version_of(tag)
    files = download_release(gh, tag, directory)
    if built:
        require_built(directory, built)
    with tempfile.TemporaryDirectory() as temporary:
        sign = decide(fetch_branch(Path(temporary)), version, expected_targets(version, files))
    write_outputs(version=version, sign=str(sign).lower())
    return (f"Signing app {version}: {len(files)} assets\n" if sign
            else f"App {version} is already signed with these assets\n")


def publish(tag, directory, metadata):
    version = version_of(tag)
    expected = expected_targets(version, asset_files(directory))
    metadata = Path(metadata)
    require(signed_targets((metadata / "targets.json").read_text()) == expected,
            "signed targets do not describe exactly the downloaded assets")
    roles = [metadata / name for name in SIGNED_ROLES]
    roots = sorted(metadata.glob("*.root.json"))
    require(all(re.fullmatch(r"[1-9][0-9]*\.root\.json", p.name) for p in roots),
            "unexpected root file name")
    require(set(metadata.iterdir()) == set(roles) | set(roots), "unexpected file in signed metadata")
    # Re-fetch right before pushing in case another release was signed since prepare.
    with tempfile.TemporaryDirectory() as temporary:
        branch = fetch_branch(Path(temporary))
        if not decide(branch, version, expected):
            return f"App {version} is already signed with these assets\n"
        require_newer(branch, metadata, SIGNED_ROLES)
        # Numbered roots are immutable once published.
        for root in roots:
            old = branch.path / root.name
            require(not old.exists() or old.read_bytes() == root.read_bytes(),
                    f"{root.name} on {BRANCH} differs from the signed root")
        update_branch(branch, roles + roots, f"Sign app {version}")
    return f"Signed app {version} on {BRANCH}\n"


def refresh(metadata):
    metadata = Path(metadata)
    with tempfile.TemporaryDirectory() as temporary:
        branch = fetch_branch(Path(temporary))
        require(branch.tip is not None and (branch.path / "targets.json").exists(),
                "Only published app metadata can be renewed")
        require((branch.path / "targets.json").read_bytes() == (metadata / "targets.json").read_bytes(),
                "Targets changed since renewal; refresh from current metadata")
        names = ["snapshot.json", "timestamp.json"]
        require_newer(branch, metadata, names)
        update_branch(branch, [metadata / name for name in names], "Renew app discovery metadata")
    return "Renewed app discovery metadata\n"


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    commands = parser.add_subparsers(dest="command", required=True)
    p = commands.add_parser("prepare")
    p.add_argument("--tag", required=True)
    p.add_argument("--dir", required=True, type=Path)
    p.add_argument("--built", action="append", type=Path, default=[])
    u = commands.add_parser("publish")
    u.add_argument("--tag", required=True)
    u.add_argument("assets", type=Path)
    u.add_argument("metadata", type=Path)
    r = commands.add_parser("refresh")
    r.add_argument("metadata", type=Path)
    args = parser.parse_args()
    try:
        if args.command == "prepare":
            repo = os.environ.get("GH_REPO") or os.environ.get("GITHUB_REPOSITORY", "")
            require(re.fullmatch(r"[\w.-]+/[\w.-]+", repo), "GH_REPO or GITHUB_REPOSITORY is not set")
            print(prepare(Gh(repo), args.tag, args.dir, args.built), end="")
        elif args.command == "publish":
            print(publish(args.tag, args.assets, args.metadata), end="")
        else:
            print(refresh(args.metadata), end="")
        return 0
    except (Refused, json.JSONDecodeError, KeyError, OSError, subprocess.CalledProcessError) as error:
        print(f"publish_app: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
