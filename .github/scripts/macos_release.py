#!/usr/bin/env python3
"""Finish an app release by adding the macOS package, then publish it.

The Release workflow creates the release as a DRAFT with the Linux and Windows
packages and never publishes it. The Release macOS workflow builds the macOS
package from exactly the same source and version files and calls this script,
which adds it, rewrites SHA256SUMS, removes the pending notice and, as the
very last step after every check, publishes the release as latest. A
published release is never changed afterwards by this script.

  prepare --run-id N [--automatic]
      Check that run N is a successful Release run on main, download its
      release-input artifact to .release-input, and check that its tag is the
      source itself or the release commit on top of it and has a draft release. Writes
      source, tag and ready to GITHUB_OUTPUT. With --automatic, a run that
      created no release (a dry run) gives ready=false. A release that is
      already published and complete also gives ready=false. One published by
      hand before it was complete is finished like a draft, unless it is
      immutable, which is refused. A draft older than the newest published
      release is refused.
  stage --source SHA --tag TAG
      In the checkout of SHA, apply the version files from
      .release-input/versioned.tar, extracting only version files.
  publish --tag TAG --source SHA --package-dir DIR
      Finish the draft and publish it as latest, last. A release someone
      published by hand before it was complete is finished but not published
      again, so which release is latest is left as they chose. On a complete
      published release, only check that it holds this exact macOS zip. A
      draft older than the newest published release is refused before any
      change and again just before publishing: publishing it as latest would
      move latest backwards.

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
PACKAGE_ARTIFACT = "macos-package"
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
    """The GitHub operations this script uses, by release ID through the REST
    API (which, unlike lookups by tag, also sees drafts). Tests replace it
    with an in-memory fake."""

    def __init__(self, repo):
        self.repo = repo

    def _gh(self, *args, data=None, binary=False):
        result = subprocess.run(["gh", *args], input=data, capture_output=True, text=not binary)
        if result.returncode != 0:
            error = result.stderr.decode() if binary else result.stderr
            raise Refused(f"gh {' '.join(args[:4])} failed: {error.strip()}")
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

    def releases(self):
        """Every release, drafts included. `--slurp` gives a list of pages,
        each a list of releases; they are flattened here."""
        pages = json.loads(self._gh("api", "--paginate", "--slurp",
                                    f"repos/{self.repo}/releases?per_page=100") or "[]")
        require(isinstance(pages, list) and all(isinstance(p, list) for p in pages),
                "unexpected release list from GitHub")
        releases = [release for page in pages for release in page]
        require(all(isinstance(r, dict) for r in releases), "unexpected release list from GitHub")
        return releases

    def download_artifact(self, run_id, name, directory):
        self._gh("run", "download", str(run_id), "-R", self.repo, "-n", name, "-D", str(directory))

    def asset_bytes(self, asset_id):
        return self._gh("api", "-H", "Accept: application/octet-stream",
                        f"repos/{self.repo}/releases/assets/{asset_id}", binary=True)

    def upload_asset(self, release_id, path):
        url = (f"https://uploads.github.com/repos/{self.repo}/releases/{release_id}/assets"
               f"?name={Path(path).name}")
        self._gh("api", "--method", "POST", "-H", "Content-Type: application/octet-stream",
                 url, "--input", str(path))

    def delete_asset(self, asset_id):
        self._gh("api", "--method", "DELETE", f"repos/{self.repo}/releases/assets/{asset_id}")

    def release(self, release_id):
        """One release by ID, drafts included, or None."""
        return self.api(f"releases/{release_id}")

    # A PATCH on a draft that omits tag_name leaves the draft untagged, so
    # both updates always send it.
    def set_notes(self, release_id, tag, notes):
        self._gh("api", "--method", "PATCH", f"repos/{self.repo}/releases/{release_id}",
                 "-f", f"tag_name={tag}", "-f", f"body={notes}")

    def publish(self, release_id, tag):
        self._gh("api", "--method", "PATCH", f"repos/{self.repo}/releases/{release_id}",
                 "-f", f"tag_name={tag}", "-F", "draft=false", "-f", "make_latest=true")


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


def find_release(gh, tag):
    """The one release (draft or published) for TAG, or None."""
    found = [r for r in gh.releases() if r.get("tag_name") == tag]
    require(len(found) <= 1, f"more than one release uses tag {tag}")
    return found[0] if found else None


def app_version(tag):
    """The numbers of a stable app tag; None for anything else."""
    match = re.fullmatch("v" + STABLE, tag) if isinstance(tag, str) else None
    return tuple(int(part) for part in match.groups()) if match else None


def refuse_if_behind(gh, tag):
    """Refuse a draft of TAG when a newer stable app release is published.
    Other drafts, prereleases and non-app tags are not releases it could fall
    behind. The list is read fresh on every call."""
    published = [r.get("tag_name") for r in gh.releases()
                 if not r.get("draft") and not r.get("prerelease")]
    versions = [(app_version(t), t) for t in published if app_version(t) is not None]
    if versions:
        newest, newest_tag = max(versions)
        require(app_version(tag) >= newest,
                f"draft {tag} is older than published release {newest_tag}; publishing it as "
                "latest would move latest backwards, so it is not changed or published")


def check_tag(gh, tag, source):
    """TAG must point at SOURCE or its version-bump release commit."""
    commit = gh.api(f"commits/{tag}")
    require(commit is not None, f"tag {tag} has no commit")
    # An already-versioned initial release needs no additional commit. The
    # successful parent run and its immutable input artifact pin this source.
    if commit.get("sha") == source:
        return
    message = (commit.get("commit") or {}).get("message", "")
    require(message.split("\n", 1)[0] == f"chore(release): {tag}",
            f"tag {tag} is not on the release commit")
    parents = [p.get("sha") for p in commit.get("parents", [])]
    require(parents == [source], f"release commit for {tag} is not on top of {source}")


def release_state(gh, tag, source):
    """(release, state) with state "missing", "draft", "complete", "open"
    (published early, still changeable) or "locked" (published, incomplete
    and immutable). The tag is checked whenever a release exists."""
    release = find_release(gh, tag)
    if release is None:
        return None, "missing"
    check_tag(gh, tag, source)
    if release.get("draft"):
        return release, "draft"
    if complete(gh, release, tag[1:]):
        return release, "complete"
    return release, ("locked" if release.get("immutable") else "open")


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
    release, state = release_state(gh, plan["tag"], plan["source"])
    if state == "missing":
        # A dry run pushes no tag. If the tag is there, the release exists
        # but is a draft this token cannot see.
        if gh.api(f"commits/{plan['tag']}") is not None:
            check_tag(gh, plan["tag"], plan["source"])
            raise Refused(
                f"tag {plan['tag']} is pushed but its release is not visible: it is probably a "
                "draft, and listing drafts needs a token with contents: write"
            )
        require(automatic, f"run {run_id} created no release for {plan['tag']} (a dry run?)")
        ready, why = "false", f"Skipped: Release run {run_id} created no release (a dry run)."
    elif state == "draft":
        refuse_if_behind(gh, plan["tag"])
        ready, why = "true", (f"Finishing draft release {plan['tag']} from Release run {run_id} "
                              f"(source `{plan['source'][:12]}`).")
    elif state == "complete":
        ready, why = "false", f"Release {plan['tag']} is already published and complete."
    elif state == "open":
        ready, why = "true", (f"Finishing release {plan['tag']}, which was published before it "
                              "was complete. It stays published as it is.")
    else:
        raise Refused(f"release {plan['tag']} is published, incomplete and immutable; "
                      "it can't be finished")
    packages = gh.api(f"actions/runs/{run_id}/artifacts?name={PACKAGE_ARTIFACT}") or {}
    prebuilt = any(a.get("name") == PACKAGE_ARTIFACT and not a.get("expired")
                   for a in packages.get("artifacts", []))
    lines = [f"source={plan['source']}", f"tag={plan['tag']}", f"ready={ready}",
             f"prebuilt={str(prebuilt).lower()}"]
    for line in lines:
        output(line)
    summary(f"### Release macOS for {plan['tag']}\n\n{why}\n")
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


APP_MEMBERS = ("rc2fx/RC2FX.app/Contents/MacOS/rc2fx", "rc2fx/RC2FX.app/Contents/Info.plist")


def expected_assets(version):
    prefix = f"rc2fx-{version}"
    return {
        f"{prefix}-linux-x64.AppImage", f"{prefix}-linux-x64.deb", f"{prefix}-linux-x64.rpm",
        f"{prefix}-appimage-runtime-sources.tar.gz", f"{prefix}-windows-x64.zip",
        f"{prefix}-macos-arm64.zip",
    }


def check_package(path):
    """A sound macOS zip holding the app bundle under rc2fx/."""
    require(zipfile.is_zipfile(path), f"{path.name} is not a zip")
    with zipfile.ZipFile(path) as package:
        require(package.testzip() is None, f"{path.name} is damaged")
        entries = [i.filename for i in package.infolist()]
        require(len(set(entries)) == len(entries), f"{path.name} repeats an entry name")
        for entry in entries:
            require(entry.startswith("rc2fx/"), f"{path.name} has {entry!r} outside rc2fx/")
            safe_relative(entry.rstrip("/"))
        for member in APP_MEMBERS:
            require(member in entries, f"{path.name} has no {member}")


def asset_map(release):
    """The release's assets by name; a repeated name is refused."""
    names = [a["name"] for a in release.get("assets", [])]
    require(len(set(names)) == len(names), "the release repeats an asset name")
    return {a["name"]: a for a in release.get("assets", [])}


def asset_digest(asset):
    digest = asset.get("digest") or ""
    return digest.removeprefix("sha256:") if digest.startswith("sha256:") else None


def verified_bytes(gh, asset):
    """An asset's bytes, which must match GitHub's digest when it has one."""
    data = gh.asset_bytes(asset["id"])
    claimed = asset_digest(asset)
    require(claimed in (None, sha256(data)), f"{asset['name']} does not match GitHub's digest")
    return data


def sums_for(gh, assets, version):
    """SHA256SUMS text for the six packages, from their actual bytes."""
    names = set(assets) - {"SHA256SUMS"}
    require(names == expected_assets(version),
            f"release assets are not exactly the expected packages: {sorted(names)}")
    return "".join(f"{sha256(verified_bytes(gh, assets[n]))}  {n}\n" for n in sorted(names))


def complete(gh, release, version, mac_sha=None):
    """Whether a release holds exactly the six packages and a SHA256SUMS that
    matches them, has no pending notice, and (if given) this macOS zip."""
    assets = asset_map(release)
    if set(assets) != expected_assets(version) | {"SHA256SUMS"}:
        return False
    mac = assets[f"rc2fx-{version}-macos-arm64.zip"]
    if mac_sha is not None and sha256(verified_bytes(gh, mac)) != mac_sha:
        return False
    if PENDING.search(release.get("body") or ""):
        return False
    return verified_bytes(gh, assets["SHA256SUMS"]).decode() == sums_for(gh, assets, version)


def changeable(gh, tag, release_id):
    """The release as it is now, by ID, which must still carry TAG and accept
    changes: a draft, or a published release that is not immutable
    (published early by hand)."""
    release = gh.release(release_id)
    require(release is not None, f"release {tag} disappeared")
    require(release.get("tag_name") == tag,
            f"release {release_id} no longer carries tag {tag} "
            f"(now {release.get('tag_name')!r}); not changing or publishing it")
    require(release.get("draft") or not release.get("immutable"),
            f"release {tag} became immutable before it was complete; it can't be finished")
    return release


def publish(gh, tag, source, package_dir):
    require(re.fullmatch("v" + STABLE, tag), f"{tag!r} is not a stable app tag")
    require(re.fullmatch("[0-9a-f]{40}", source), f"{source!r} is not a commit id")
    version = tag[1:]
    name = f"rc2fx-{version}-macos-arm64.zip"
    files = sorted(p.name for p in Path(package_dir).iterdir())
    require(files == [name], f"{package_dir} must hold exactly {name}, not {files}")
    package = Path(package_dir) / name
    require(package.is_file() and not package.is_symlink(), f"{name} is not a regular file")
    check_package(package)
    ours = sha256(package.read_bytes())
    release = find_release(gh, tag)
    require(release is not None, f"release {tag} does not exist")
    check_tag(gh, tag, source)
    if not release.get("draft") and complete(gh, release, version, mac_sha=ours):
        return "already complete\n"  # read only: nothing to change
    if release.get("draft"):
        refuse_if_behind(gh, tag)
    # The release is re-read before every change: someone may publish the
    # draft by hand meanwhile, which is fine while it stays changeable.
    release_id = release["id"]
    release = changeable(gh, tag, release_id)
    assets = asset_map(release)
    present = set(assets) - {"SHA256SUMS", name}
    require(present == expected_assets(version) - {name},
            f"release {tag} does not hold exactly the other packages: {sorted(present)}")
    if name in assets:
        require(sha256(verified_bytes(gh, assets[name])) == ours,
                f"release {tag} already has a different {name}; not replacing it")
    else:
        gh.upload_asset(release_id, package)
    release = changeable(gh, tag, release_id)
    assets = asset_map(release)
    require(name in assets and sha256(verified_bytes(gh, assets[name])) == ours,
            f"uploaded {name} does not match")
    sums = sums_for(gh, assets, version)
    if "SHA256SUMS" not in assets or verified_bytes(gh, assets["SHA256SUMS"]).decode() != sums:
        with tempfile.TemporaryDirectory() as work:
            path = Path(work) / "SHA256SUMS"
            path.write_text(sums)
            release = changeable(gh, tag, release_id)
            old = asset_map(release).get("SHA256SUMS")
            if old is not None:
                gh.delete_asset(old["id"])
            gh.upload_asset(release_id, path)
    release = changeable(gh, tag, release_id)
    notes = release.get("body") or ""
    if PENDING.search(notes):
        gh.set_notes(release_id, tag, PENDING.sub("\n", notes).strip("\n") + "\n")
    # Last: everything checked again on the release as it now is. A draft is
    # published as latest; a release already published by hand is left so.
    release = changeable(gh, tag, release_id)
    require(complete(gh, release, version, mac_sha=ours), f"release {tag} is not complete")
    if release.get("draft"):
        # A newer release may have been published during this run.
        refuse_if_behind(gh, tag)
        gh.publish(release_id, tag)
    return sums


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
