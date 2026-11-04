#!/usr/bin/env python3
"""Add the macOS package to an app release that is already published.

The Release workflow publishes Linux and Windows without waiting for a macOS
runner. The Release macOS workflow then builds the macOS package from exactly
the same source and version files and attaches it to the same release:

  prepare --run-id N [--automatic]
      Check that run N is a successful Release run on main, download its
      release-input artifact to .release-input, and check that its tag is
      published as a release commit on top of its source. Writes source, tag
      and ready to GITHUB_OUTPUT. With --automatic, a run that published
      nothing (a dry run) gives ready=false instead of an error.
  stage --source SHA --tag TAG
      In the checkout of SHA, apply the version files from
      .release-input/versioned.tar, extracting only the files the plan lists.
  publish --tag TAG --source SHA --package-dir DIR
      Upload DIR's macOS zip to the release (never replacing a different
      one), rebuild the release's SHA256SUMS from its actual assets, and
      remove the pending-macOS notice. Which release is latest is not touched.

Needs `gh` with GH_TOKEN, and GH_REPO (or GITHUB_REPOSITORY). Every refusal
exits 1.
"""

import argparse
import hashlib
import json
import os
import re
import subprocess
import sys
import tarfile
import tempfile
import tomllib
import zipfile
from pathlib import Path

STABLE = r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)"
WORKFLOW = ".github/workflows/release.yml"
ARTIFACT = "release-input"
INPUT = Path(".release-input")
PLAN_KEYS = {"tag", "previous", "source", "files"}
PENDING = re.compile(
    r"\n*<!-- rc2fx:macos-pending -->\n.*?\n<!-- /rc2fx:macos-pending -->\n*", re.S
)


class Refused(Exception):
    pass


def require(condition, message):
    if not condition:
        raise Refused(message)


class Gh:
    """The GitHub operations this script uses, through the `gh` CLI. Tests
    replace it with an in-memory fake."""

    def __init__(self, repo):
        self.repo = repo

    def _run(self, *args, capture=True):
        result = subprocess.run(["gh", *args], capture_output=capture, text=True)
        if result.returncode != 0:
            raise Refused(f"gh {' '.join(args[:3])} failed: {(result.stderr or '').strip()}")
        return result.stdout

    def api(self, path):
        """The JSON at repos/REPO/PATH, or None if it does not exist."""
        result = subprocess.run(
            ["gh", "api", f"repos/{self.repo}/{path}"], capture_output=True, text=True
        )
        if result.returncode != 0:
            if "HTTP 404" in result.stderr or "Not Found" in result.stderr:
                return None
            raise Refused(f"GitHub API repos/{self.repo}/{path}: {result.stderr.strip()}")
        return json.loads(result.stdout)

    def download_artifact(self, run_id, name, directory):
        self._run("run", "download", str(run_id), "-R", self.repo, "-n", name, "-D", str(directory))

    def download_asset(self, tag, name, directory):
        self._run("release", "download", tag, "-R", self.repo, "-p", name, "-D", str(directory))
        return Path(directory) / name

    def upload_asset(self, tag, path, clobber=False):
        args = ["release", "upload", tag, str(path), "-R", self.repo]
        self._run(*(args + ["--clobber"] if clobber else args))

    def edit_notes(self, tag, notes):
        """Replace the notes only. No --latest: the latest release stays as it is."""
        with tempfile.NamedTemporaryFile("w", suffix=".md", delete=False) as handle:
            handle.write(notes)
        try:
            self._run("release", "edit", tag, "-R", self.repo, "--notes-file", handle.name)
        finally:
            os.unlink(handle.name)


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def safe_relative(name):
    parts = name.split("/")
    require(
        name and not name.startswith("/") and "\\" not in name
        and all(part not in ("", ".", "..") for part in parts),
        f"unsafe path {name!r}",
    )
    return name


def read_plan(directory=INPUT):
    path = directory / "plan.json"
    require(path.is_file() and not path.is_symlink(), "release-input has no plan.json")
    plan = json.loads(path.read_text())
    require(isinstance(plan, dict) and set(plan) == PLAN_KEYS, "plan.json has unexpected keys")
    require(isinstance(plan["tag"], str) and re.fullmatch("v" + STABLE, plan["tag"]),
            f"plan tag {plan['tag']!r} is not a stable app tag")
    require(isinstance(plan["source"], str) and re.fullmatch("[0-9a-f]{40}", plan["source"]),
            "plan source is not a commit id")
    files = plan["files"]
    require(isinstance(files, list) and files and all(isinstance(f, str) for f in files),
            "plan files are missing")
    require(len(set(files)) == len(files), "plan files repeat")
    for name in files:
        safe_relative(name)
    return plan


def published(gh, tag, source):
    """Whether TAG is a published release whose commit is the release commit
    on top of SOURCE. Returns the release, or None if it is not published."""
    release = gh.api(f"releases/tags/{tag}")
    if release is None:
        return None
    require(release.get("tag_name") == tag, "release tag differs")
    require(release.get("draft") is False, f"release {tag} is a draft")
    commit = gh.api(f"commits/{tag}")
    require(commit is not None, f"tag {tag} has no commit")
    message = (commit.get("commit") or {}).get("message", "")
    require(message.split("\n", 1)[0] == f"chore(release): {tag}",
            f"tag {tag} is not on the release commit")
    parents = [p.get("sha") for p in commit.get("parents", [])]
    require(parents == [source], f"release commit for {tag} is not on top of {source}")
    return release


def prepare(gh, run_id, automatic, output=print):
    run = gh.api(f"actions/runs/{run_id}")
    require(run is not None, f"workflow run {run_id} not found")
    require((run.get("repository") or {}).get("full_name") == gh.repo, "run is from another repository")
    require(run.get("path") == WORKFLOW, f"run {run_id} is not the Release workflow")
    require(run.get("event") == "workflow_dispatch", f"run {run_id} was not started by hand")
    require(run.get("head_branch") == "main", f"run {run_id} is not on main")
    require(run.get("status") == "completed" and run.get("conclusion") == "success",
            f"run {run_id} did not succeed")
    artifacts = gh.api(f"actions/runs/{run_id}/artifacts?name={ARTIFACT}") or {}
    found = [a for a in artifacts.get("artifacts", []) if a.get("name") == ARTIFACT]
    require(len(found) == 1, f"run {run_id} has no {ARTIFACT} artifact")
    require(not found[0].get("expired"),
            f"run {run_id}'s {ARTIFACT} artifact has expired; start a new release instead")
    require(not INPUT.exists() or not any(INPUT.iterdir()), f"{INPUT} is not empty")
    gh.download_artifact(run_id, ARTIFACT, INPUT)
    plan = read_plan()
    require(plan["source"] == run.get("head_sha"), "plan source is not the run's commit")
    release = published(gh, plan["tag"], plan["source"])
    if release is None:
        require(automatic, f"release {plan['tag']} is not published (was run {run_id} a dry run?)")
        ready = "false"
    else:
        ready = "true"
    lines = [f"source={plan['source']}", f"tag={plan['tag']}", f"ready={ready}"]
    for line in lines:
        output(line)
    summary(
        f"### Release macOS for {plan['tag']}\n\n"
        + (f"Building the macOS package for {plan['tag']} from Release run {run_id} "
           f"(source `{plan['source'][:12]}`).\n"
           if ready == "true" else
           f"Skipped: Release run {run_id} published no release (a dry run).\n")
    )
    return dict(line.split("=", 1) for line in lines)


def summary(text):
    """Append to the job summary when running in GitHub Actions."""
    path = os.environ.get("GITHUB_STEP_SUMMARY")
    if path:
        with open(path, "a") as handle:
            handle.write(text)


# Files the Release workflow's version bump may change (release.py
# update_versions and the changelog); workspace member manifests are added
# from the checkout's own Cargo.toml.
FIXED_VERSION_FILES = {
    "Cargo.toml", "Cargo.lock", "CHANGELOG.md",
    "crates/rc2fx-native/Cargo.lock", "crates/rc2fx-bench/Cargo.lock",
}


def version_files(root):
    """Every file a version bump may write, from the checkout at ROOT."""
    cargo = tomllib.loads((root / "Cargo.toml").read_text())
    members = cargo["workspace"]["members"]
    require(all(isinstance(m, str) and "*" not in m for m in members), "unexpected workspace members")
    return FIXED_VERSION_FILES | {f"{safe_relative(m)}/Cargo.toml" for m in members}


def stage(source, tag, head, root=Path(".")):
    """Apply the plan's version files to the checkout at ROOT. Only files a
    version bump may write are accepted, whatever the plan lists."""
    plan = read_plan(root / INPUT)
    allowed = version_files(root)
    outside = sorted(set(plan["files"]) - allowed)
    require(not outside, f"plan lists files a version bump never writes: {outside}")
    require(plan["source"] == source and plan["tag"] == tag, "plan does not match --source/--tag")
    require(head == source, f"checkout is at {head}, not {source}")
    archive = root / INPUT / "versioned.tar"
    require(archive.is_file() and not archive.is_symlink(), "release-input has no versioned.tar")
    with tarfile.open(archive) as tar:
        members = tar.getmembers()
        names = [m.name for m in members]
        require(len(set(names)) == len(names), "versioned.tar repeats a file")
        require(set(names) == set(plan["files"]), "versioned.tar does not hold exactly the plan's files")
        for member in members:
            safe_relative(member.name)
            require(member.isreg(), f"{member.name} in versioned.tar is not a regular file")
        for member in members:
            data = tar.extractfile(member).read()
            target = root / member.name
            require(not target.is_symlink(), f"{member.name} is a link in the checkout")
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(data)
    return sorted(names)


def expected_assets(version):
    prefix = f"rc2fx-{version}"
    return {
        f"{prefix}-linux-x64.AppImage", f"{prefix}-linux-x64.deb", f"{prefix}-linux-x64.rpm",
        f"{prefix}-appimage-runtime-sources.tar.gz", f"{prefix}-windows-x64.zip",
        f"{prefix}-macos-arm64.zip",
    }


def check_package(path):
    """A macOS zip whose rc2fx/SHA256SUMS lists every other member exactly."""
    require(zipfile.is_zipfile(path), f"{path.name} is not a zip")
    with zipfile.ZipFile(path) as package:
        require(package.testzip() is None, f"{path.name} is damaged")
        entries = [i.filename for i in package.infolist()]
        require(len(set(entries)) == len(entries), f"{path.name} repeats an entry name")
        for entry in entries:
            require(entry.startswith("rc2fx/"), f"{path.name} has {entry!r} outside rc2fx/")
            safe_relative(entry.rstrip("/"))
        names = [i.filename for i in package.infolist() if not i.is_dir()]
        require("rc2fx/SHA256SUMS" in names, f"{path.name} has no rc2fx/SHA256SUMS")
        listed = {}
        for line in package.read("rc2fx/SHA256SUMS").decode().splitlines():
            digest, sep, name = line.partition("  ")
            require(sep and re.fullmatch("[0-9a-f]{64}", digest) and name not in listed,
                    f"malformed SHA256SUMS line {line!r}")
            listed[name] = digest
        members = {n.removeprefix("rc2fx/") for n in names if n != "rc2fx/SHA256SUMS"}
        require(set(listed) == members, f"{path.name}'s SHA256SUMS does not list exactly its files")
        for name, digest in listed.items():
            require(sha256(package.read("rc2fx/" + name)) == digest, f"{name} does not match its checksum")


def asset_map(release):
    """The release's assets by name; a repeated name is refused."""
    names = [a["name"] for a in release.get("assets", [])]
    require(len(set(names)) == len(names), "the release repeats an asset name")
    return {a["name"]: a for a in release.get("assets", [])}


def asset_digest(asset):
    digest = asset.get("digest") or ""
    return digest.removeprefix("sha256:") if digest.startswith("sha256:") else None


def publish(gh, tag, source, package_dir):
    require(re.fullmatch("v" + STABLE, tag), f"{tag!r} is not a stable app tag")
    require(re.fullmatch("[0-9a-f]{40}", source), f"{source!r} is not a commit id")
    release = published(gh, tag, source)
    require(release is not None, f"release {tag} is not published")
    version = tag[1:]
    name = f"rc2fx-{version}-macos-arm64.zip"
    files = sorted(p.name for p in Path(package_dir).iterdir())
    require(files == [name], f"{package_dir} must hold exactly {name}, not {files}")
    package = Path(package_dir) / name
    require(package.is_file() and not package.is_symlink(), f"{name} is not a regular file")
    check_package(package)
    ours = sha256(package.read_bytes())
    assets = asset_map(release)
    # Before uploading anything: the release holds the other packages and at
    # most this macOS zip and SHA256SUMS, nothing else.
    present = set(assets) - {"SHA256SUMS", name}
    require(present == expected_assets(version) - {name},
            f"release {tag} does not hold exactly the other packages: {sorted(present)}")
    with tempfile.TemporaryDirectory() as work:
        if name in assets:
            theirs = asset_digest(assets[name])
            if theirs is None:
                theirs = sha256(gh.download_asset(tag, name, Path(work) / "existing").read_bytes())
            require(theirs == ours, f"release {tag} already has a different {name}; not replacing it")
        else:
            gh.upload_asset(tag, package)
        release = gh.api(f"releases/tags/{tag}")
        assets = asset_map(release)
        names = set(assets) - {"SHA256SUMS"}
        require(names == expected_assets(version),
                f"release {tag} assets are not exactly the expected packages: {sorted(names)}")
        lines = []
        for asset_name in sorted(names):
            data = gh.download_asset(tag, asset_name, Path(work) / "assets").read_bytes()
            digest = sha256(data)
            claimed = asset_digest(assets[asset_name])
            require(claimed in (None, digest), f"{asset_name} does not match GitHub's digest")
            lines.append(f"{digest}  {asset_name}\n")
        sums = Path(work) / "SHA256SUMS"
        sums.write_text("".join(lines))
        gh.upload_asset(tag, sums, clobber=True)
    notes = release.get("body") or ""
    cleaned = PENDING.sub("\n", notes).strip("\n") + "\n" if PENDING.search(notes) else None
    if cleaned is not None:
        gh.edit_notes(tag, cleaned)
    return "".join(lines)


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    commands = parser.add_subparsers(dest="command", required=True)
    p = commands.add_parser("prepare")
    p.add_argument("--run-id", required=True, type=int)
    p.add_argument("--automatic", action="store_true")
    s = commands.add_parser("stage")
    s.add_argument("--source", required=True)
    s.add_argument("--tag", required=True)
    u = commands.add_parser("publish")
    u.add_argument("--source", required=True)
    u.add_argument("--tag", required=True)
    u.add_argument("--package-dir", required=True, type=Path)
    args = parser.parse_args()
    try:
        if args.command == "stage":
            head = subprocess.run(["git", "rev-parse", "HEAD"], capture_output=True, text=True,
                                  check=True).stdout.strip()
            for name in stage(args.source, args.tag, head):
                print(f"staged {name}")
            return 0
        repo = os.environ.get("GH_REPO") or os.environ.get("GITHUB_REPOSITORY", "")
        require(re.fullmatch(r"[\w.-]+/[\w.-]+", repo), "GH_REPO or GITHUB_REPOSITORY is not set")
        gh = Gh(repo)
        if args.command == "prepare":
            outputs = []
            prepare(gh, args.run_id, args.automatic, outputs.append)
            if os.environ.get("GITHUB_OUTPUT"):
                with open(os.environ["GITHUB_OUTPUT"], "a") as handle:
                    handle.write("".join(f"{line}\n" for line in outputs))
            print("\n".join(outputs))
        else:
            print(publish(gh, args.tag, args.source, args.package_dir), end="")
        return 0
    except (Refused, json.JSONDecodeError, tomllib.TOMLDecodeError, KeyError, tarfile.TarError,
            zipfile.BadZipFile, OSError) as error:
        print(f"macos_release: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
