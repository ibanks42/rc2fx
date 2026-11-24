#!/usr/bin/env python3
"""Sign a published app release into the `app-metadata` TUF branch, or renew it.

App updates use their own orphan branch (`app-metadata`) served via
raw.githubusercontent.com, sharing the mixer's TUF root and release keys. Every
release asset is recorded as target `vVERSION/ASSET`.

  prepare --tag TAG --dir DIR [--built DIR]...
      Download all assets of TAG into DIR and verify them against GitHub's
      digests. With --built, also require the release assets to match the build
      outputs byte for byte. Checks the current branch to skip already-signed
      identical assets and refuse downgrades or conflicting re-signs. Writes
      `version` and `sign` to GITHUB_OUTPUT when set.

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
import json
import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path

from release_common import (
    MetadataRepo,
    Refused,
    SIGNED_ROLES,
    STABLE,
    gh_api_get,
    gh_repo,
    require,
    require_newer_versions,
    sha256_file,
)

BRANCH = "app-metadata"
README = """# RC2FX app update metadata

Signed TUF metadata for RC2FX desktop app updates, read by the app to check a
downloaded update before applying it. This branch is written only by the release
tooling; see docs/developer/releases.md on main.
"""
PLAIN = re.compile(r"[A-Za-z0-9_+-][A-Za-z0-9._+-]*")
# Mixer metadata versions are Unix timestamps (>= 2026); keep app versions below them.
MIXER_VERSIONS_FROM = 1_700_000_000
META = MetadataRepo(BRANCH, README, error_cls=Refused)


class Gh:
    """Minimal GitHub CLI wrapper (replaced by a fake in tests)."""

    def __init__(self, repo):
        self.repo = repo

    def release(self, tag):
        """Return the published release for TAG, or None (drafts aren't returned by tag)."""
        return gh_api_get(
            self.repo,
            f"releases/tags/{tag}",
            run=subprocess.run,
            label=f"releases/tags/{tag}",
        )

    def download(self, asset_id, path):
        with open(path, "xb") as out:
            result = subprocess.run(
                [
                    "gh",
                    "api",
                    "-H",
                    "Accept: application/octet-stream",
                    f"repos/{self.repo}/releases/assets/{asset_id}",
                ],
                stdout=out,
                stderr=subprocess.PIPE,
                text=False,
            )
        if result.returncode != 0:
            raise Refused(
                f"downloading asset {asset_id} failed: {result.stderr.decode().strip()}"
            )


def version_of(tag):
    match = re.fullmatch("v" + STABLE, tag or "")
    require(match, f"{tag!r} is not a stable app tag")
    return tag[1:]


def version_key(version):
    return tuple(map(int, version.split(".")))


def asset_files(directory):
    """Validate all files in DIRECTORY and return {name: (size, sha256)}."""
    directory = Path(directory)
    files = {}
    for path in sorted(directory.iterdir()):
        require(PLAIN.fullmatch(path.name), f"unsafe asset name {path.name!r}")
        require(
            path.is_file() and not path.is_symlink(),
            f"{path.name} is not a regular file",
        )
        files[path.name] = (path.stat().st_size, sha256_file(path))
    require(files, "the release has no assets to sign")
    return files


def download_release(gh, tag, directory):
    """Download all assets of TAG into DIRECTORY, verifying sizes and GitHub SHA-256 digests."""
    release = gh.release(tag)
    require(release is not None, f"release {tag} is not published")
    require(
        release.get("tag_name") == tag,
        f"release {tag} carries tag {release.get('tag_name')!r}",
    )
    require(not release.get("draft"), f"release {tag} is a draft")
    require(not release.get("prerelease"), f"release {tag} is a prerelease")
    assets = release.get("assets") or []
    names = [a.get("name") for a in assets]
    require(len(set(names)) == len(names), "the release repeats an asset name")
    directory = Path(directory)
    directory.mkdir(parents=True)
    for asset in assets:
        name = asset["name"]
        require(
            isinstance(name, str) and PLAIN.fullmatch(name),
            f"unsafe asset name {name!r}",
        )
        require(asset.get("state") == "uploaded", f"{name} is not fully uploaded")
        digest = asset.get("digest") or ""
        require(
            re.fullmatch(r"sha256:[0-9a-f]{64}", digest),
            f"GitHub gives no SHA-256 digest for {name}",
        )
        path = directory / name
        gh.download(asset["id"], path)
        require(
            path.stat().st_size == asset.get("size"),
            f"{name} does not have GitHub's size",
        )
        require(
            sha256_file(path) == digest.removeprefix("sha256:"),
            f"{name} does not match GitHub's digest",
        )
    return asset_files(directory)


def require_built(downloaded, built_dirs):
    """Ensure downloaded release assets match the run's build outputs byte for byte."""
    built = {}
    for directory in map(Path, built_dirs):
        for path in sorted(directory.iterdir()):
            require(
                path.is_file() and not path.is_symlink(),
                f"built {path.name} is not a regular file",
            )
            require(path.name not in built, f"{path.name} was built more than once")
            built[path.name] = path
    published = {path.name: path for path in Path(downloaded).iterdir()}
    extra = sorted(set(published) - set(built))
    require(
        not extra, f"the release holds {', '.join(extra)}, which this run did not build"
    )
    missing = sorted(set(built) - set(published))
    require(
        not missing, f"the release lacks {', '.join(missing)}, which this run built"
    )
    for name, path in sorted(published.items()):
        require(
            path.stat().st_size == built[name].stat().st_size
            and sha256_file(path) == sha256_file(built[name]),
            f"the release's {name} is not the one this run built",
        )


def expected_targets(version, files):
    return {
        f"v{version}/{name}": (length, digest, version)
        for name, (length, digest) in files.items()
    }


def signed_targets(targets_json):
    """Parse targets.json into {name: (length, sha256, version)}."""
    targets = json.loads(targets_json)["signed"]["targets"]
    return {
        name: (t["length"], t["hashes"]["sha256"], t.get("custom", {}).get("version"))
        for name, t in targets.items()
    }


def published_version(targets):
    """Return the single app version referenced by all signed targets."""
    versions = {version for _, _, version in targets.values()}
    require(
        len(versions) == 1,
        f"published metadata describes versions {sorted(map(str, versions))}",
    )
    version = versions.pop()
    require(
        isinstance(version, str) and re.fullmatch(STABLE, version),
        f"published metadata has version {version!r}",
    )
    return version


def branch_targets(branch):
    """Return signed targets from the branch, or None before the first release."""
    path = branch.path / "targets.json"
    return signed_targets(path.read_text()) if branch.tip and path.exists() else None


def decide(branch, version, expected):
    """Return True if VERSION should be signed, False if already signed identically.
    Refuses rollbacks and conflicting assets for the same version."""
    current = branch_targets(branch)
    if current is None:
        return True
    published = published_version(current)
    require(
        version_key(version) >= version_key(published),
        f"app {published} is already signed; refusing to roll back to {version}",
    )
    if version != published:
        return True
    require(
        current == expected,
        f"app {version} is already signed with different assets; publish a new version",
    )
    return False


def require_newer(branch, metadata, names):
    """Ensure new metadata versions strictly increase and stay below mixer versions."""
    require_newer_versions(
        branch,
        metadata,
        names,
        max_version=MIXER_VERSIONS_FROM,
        allow_missing=True,
        error_cls=Refused,
    )


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
        sign = decide(
            META.fetch(Path(temporary)), version, expected_targets(version, files)
        )
    write_outputs(version=version, sign=str(sign).lower())
    return (
        f"Signing app {version}: {len(files)} assets\n"
        if sign
        else f"App {version} is already signed with these assets\n"
    )


def publish(tag, directory, metadata):
    version = version_of(tag)
    expected = expected_targets(version, asset_files(directory))
    metadata = Path(metadata)
    require(
        signed_targets((metadata / "targets.json").read_text()) == expected,
        "signed targets do not describe exactly the downloaded assets",
    )
    roles = [metadata / name for name in SIGNED_ROLES]
    roots = sorted(metadata.glob("*.root.json"))
    require(
        all(re.fullmatch(r"[1-9][0-9]*\.root\.json", p.name) for p in roots),
        "unexpected root file name",
    )
    require(
        set(metadata.iterdir()) == set(roles) | set(roots),
        "unexpected file in signed metadata",
    )
    # Re-fetch right before pushing in case another release was signed since prepare.
    with tempfile.TemporaryDirectory() as temporary:
        branch = META.fetch(Path(temporary))
        if not decide(branch, version, expected):
            return f"App {version} is already signed with these assets\n"
        require_newer(branch, metadata, SIGNED_ROLES)
        # Numbered roots are immutable once published.
        for root in roots:
            old = branch.path / root.name
            require(
                not old.exists() or old.read_bytes() == root.read_bytes(),
                f"{root.name} on {BRANCH} differs from the signed root",
            )
        META.update(branch, roles + roots, f"Sign app {version}")
    return f"Signed app {version} on {BRANCH}\n"


def refresh(metadata):
    META.refresh(
        metadata,
        missing_message="Only published app metadata can be renewed",
        commit_message="Renew app discovery metadata",
        check_newer=require_newer,
        require_targets_file=True,
    )
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
            print(prepare(Gh(gh_repo()), args.tag, args.dir, args.built), end="")
        elif args.command == "publish":
            print(publish(args.tag, args.assets, args.metadata), end="")
        else:
            print(refresh(args.metadata), end="")
        return 0
    except (
        Refused,
        json.JSONDecodeError,
        KeyError,
        OSError,
        subprocess.CalledProcessError,
    ) as error:
        print(f"publish_app: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
