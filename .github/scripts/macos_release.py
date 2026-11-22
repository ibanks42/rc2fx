#!/usr/bin/env python3
"""Attach macOS packages to an existing app release draft and publish it.

The Release workflow opens a draft release with Linux and Windows assets, then
calls this script with the macOS packages built by Velopack (universal portable
zip and .pkg installer, legacy arm64 aliases, update nupkg, and releases.osx.json).
It uploads them, regenerates SHA256SUMS, strips the pending notice, and publishes
the release as latest. Already-published releases are left untouched once complete.

  publish --tag TAG --source SHA --package-dir DIR
      Complete and publish the draft release. If the release was manually
      published early, finish missing assets without changing `latest`. Refuses
      drafts older than the newest published release so `latest` never moves
      backwards.

Requires `gh` with GH_TOKEN and GH_REPO (or GITHUB_REPOSITORY). Exits 1 on refusal.
"""

import argparse
import json
import re
import subprocess
import sys
import tempfile
import zipfile
from pathlib import Path

import land_release
from release_common import Refused, STABLE, gh_api_get, gh_repo, require, sha256

PENDING = re.compile(
    r"\n*<!-- rc2fx:macos-pending -->\n.*?\n<!-- /rc2fx:macos-pending -->\n*", re.S
)


class Gh:
    """Thin wrapper over GitHub's release REST API (by ID so drafts are visible).
    Replaced with an in-memory fake in tests."""

    def __init__(self, repo):
        self.repo = repo

    def _gh(self, *args, data=None, binary=False):
        result = subprocess.run(["gh", *args], input=data, capture_output=True, text=not binary)
        if result.returncode != 0:
            error = result.stderr.decode() if binary else result.stderr
            raise Refused(f"gh {' '.join(args[:4])} failed: {error.strip()}")
        return result.stdout

    def api(self, path):
        """Return parsed JSON for repos/REPO/PATH, or None on 404."""
        return gh_api_get(self.repo, path, run=subprocess.run)

    def releases(self):
        """Return all releases including drafts, flattened across paginated responses."""
        pages = json.loads(self._gh("api", "--paginate", "--slurp",
                                    f"repos/{self.repo}/releases?per_page=100") or "[]")
        require(isinstance(pages, list) and all(isinstance(p, list) for p in pages),
                "unexpected release list from GitHub")
        releases = [release for page in pages for release in page]
        require(all(isinstance(r, dict) for r in releases), "unexpected release list from GitHub")
        return releases

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
        """Return one release by ID (including drafts), or None."""
        return self.api(f"releases/{release_id}")

    # Always include tag_name when PATCHing a draft, otherwise GitHub clears it.
    def set_notes(self, release_id, tag, notes):
        self._gh("api", "--method", "PATCH", f"repos/{self.repo}/releases/{release_id}",
                 "-f", f"tag_name={tag}", "-f", f"body={notes}")

    def publish(self, release_id, tag):
        self._gh("api", "--method", "PATCH", f"repos/{self.repo}/releases/{release_id}",
                 "-f", f"tag_name={tag}", "-F", "draft=false", "-f", "make_latest=true")


def safe_relative(name):
    parts = name.split("/")
    require(
        name and not name.startswith("/") and "\\" not in name
        and all(part not in ("", ".", "..") for part in parts),
        f"unsafe path {name!r}",
    )
    return name


def find_release(gh, tag):
    """Return the release (draft or published) matching TAG, or None."""
    found = [r for r in gh.releases() if r.get("tag_name") == tag]
    require(len(found) <= 1, f"more than one release uses tag {tag}")
    return found[0] if found else None


def app_version(tag):
    """Parse a stable app tag into a (major, minor, patch) tuple, or None."""
    match = re.fullmatch("v" + STABLE, tag) if isinstance(tag, str) else None
    return tuple(int(part) for part in match.groups()) if match else None


def refuse_if_behind(gh, tag):
    """Refuse to publish TAG if a newer stable app release is already published."""
    published = [r.get("tag_name") for r in gh.releases()
                 if not r.get("draft") and not r.get("prerelease")]
    versions = [(app_version(t), t) for t in published if app_version(t) is not None]
    if versions:
        newest, newest_tag = max(versions)
        require(app_version(tag) >= newest,
                f"draft {tag} is older than published release {newest_tag}; publishing it as "
                "latest would move latest backwards, so it is not changed or published")


def check_tag(gh, tag, source):
    """Verify TAG points to SOURCE or to the chore(release) commit on top of it.

    land_release.py may have moved the release commit on top of mixer version
    commits that landed during the build; those are the only commits allowed
    between SOURCE and the release commit.
    """
    commit = gh.api(f"commits/{tag}")
    require(commit is not None, f"tag {tag} has no commit")
    # Initial releases don't create a version-bump commit.
    if commit.get("sha") == source:
        return
    message = (commit.get("commit") or {}).get("message", "")
    require(message.split("\n", 1)[0] == f"chore(release): {tag}",
            f"tag {tag} is not on the release commit")
    parents = [p.get("sha") for p in commit.get("parents", [])]
    on_top = f"release commit for {tag} is not on top of {source}"
    require(len(parents) == 1, on_top)
    if parents[0] == source:
        return
    between = gh.api(f"compare/{source}...{parents[0]}")
    require(between is not None and between.get("status") == "ahead", on_top)
    commits = between.get("commits") or []
    require(len(commits) == between.get("total_commits"), on_top)
    files = [f.get("filename") for f in between.get("files") or []]
    for landed in commits:
        subject = ((landed.get("commit") or {}).get("message") or "").split("\n", 1)[0]
        problem = land_release.foreign_commit_problem("app", subject, files)
        require(problem is None, f"{on_top}: {landed.get('sha', '')[:12]} {problem}")


PACK_ID = "io.github.ibanks42.rc2fx"
APP = "RC2FX.app/"
SEQUESTERED = "__MACOSX/"
APP_MEMBERS = (
    "RC2FX.app/Contents/MacOS/rc2fx",
    "RC2FX.app/Contents/MacOS/UpdateMac",
    "RC2FX.app/Contents/Info.plist",
)


# Releases <= 0.1.7 used arm64-specific filenames; keep publishing identical
# copies under those names so existing releases/latest/download/ links still work.
LEGACY_ALIASES = {
    "rc2fx-macos-arm64.zip": "rc2fx-macos-universal.zip",
    "rc2fx-macos-arm64.pkg": "rc2fx-macos-universal.pkg",
}


def mac_names(version):
    """Return macOS asset names in upload order (Velopack feed last)."""
    return ("rc2fx-macos-universal.zip", "rc2fx-macos-universal.pkg", *LEGACY_ALIASES,
            f"{PACK_ID}-{version}-osx-full.nupkg", "releases.osx.json")


def expected_assets(version):
    """Return the full set of expected release assets (excluding SHA256SUMS)."""
    return {
        "rc2fx-linux-x64.AppImage", f"{PACK_ID}-{version}-linux-full.nupkg",
        "releases.linux.json", f"rc2fx-{version}-appimage-runtime-sources.tar.gz",
        "rc2fx-windows-x64.zip", "rc2fx-windows-x64-setup.exe",
        f"{PACK_ID}-{version}-full.nupkg", "releases.win.json", *mac_names(version),
    }


def sound_zip(path):
    """Validate a zip archive and return its entry names, rejecting duplicates."""
    require(zipfile.is_zipfile(path), f"{path.name} is not a zip")
    with zipfile.ZipFile(path) as package:
        require(package.testzip() is None, f"{path.name} is damaged")
        entries = [i.filename for i in package.infolist()]
    require(len(set(entries)) == len(entries), f"{path.name} repeats an entry name")
    return entries


def check_portable(path):
    """Validate Velopack's portable zip (RC2FX.app plus ditto AppleDouble metadata)."""
    entries = sound_zip(path)
    for entry in entries:
        require(entry.startswith((APP, SEQUESTERED + APP))
                or entry in (SEQUESTERED, SEQUESTERED + "._" + APP.rstrip("/")),
                f"{path.name} has {entry!r} outside {APP} and {SEQUESTERED + APP}")
        safe_relative(entry.rstrip("/"))
    for member in APP_MEMBERS:
        require(member in entries, f"{path.name} has no {member}")


def check_installer(path):
    """Validate that a macOS .pkg installer is a non-empty xar archive."""
    with path.open("rb") as package:
        magic = package.read(4)
    require(magic, f"{path.name} is empty")
    require(magic == b"xar!", f"{path.name} is not a xar archive; it is not an installer package")


def check_update_package(path):
    """Validate a NuGet update package (valid zip with a root .nuspec)."""
    entries = sound_zip(path)
    require(any("/" not in e and e.endswith(".nuspec") for e in entries),
            f"{path.name} has no .nuspec at its root")


def check_feed(path, version, package, digest):
    """Verify releases.osx.json points to this exact nupkg and SHA-256."""
    try:
        feed = json.loads(path.read_bytes())
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise Refused(f"{path.name} is not JSON: {error}") from None
    assets = feed.get("Assets") if isinstance(feed, dict) else None
    require(isinstance(assets, list) and len(assets) == 1,
            f"{path.name} does not list exactly one package")
    asset = assets[0]
    require(isinstance(asset, dict), f"{path.name} lists a package that is not an object")
    field = lambda name: asset.get(name) if isinstance(asset.get(name), str) else ""
    require(field("PackageId") == PACK_ID and field("Version") == version
            and field("Type") == "Full" and field("FileName") == package
            and field("SHA256").lower() == digest,
            f"{path.name} does not describe {package} {version} with SHA-256 {digest}")


def asset_map(release):
    """Return {name: asset} for a release, rejecting duplicate names."""
    names = [a["name"] for a in release.get("assets", [])]
    require(len(set(names)) == len(names), "the release repeats an asset name")
    return {a["name"]: a for a in release.get("assets", [])}


def asset_digest(asset):
    digest = asset.get("digest") or ""
    return digest.removeprefix("sha256:") if digest.startswith("sha256:") else None


def verified_bytes(gh, asset):
    """Download an asset and verify it matches GitHub's reported SHA-256 if present."""
    data = gh.asset_bytes(asset["id"])
    claimed = asset_digest(asset)
    require(claimed in (None, sha256(data)), f"{asset['name']} does not match GitHub's digest")
    return data


def sums_for(gh, assets, version):
    """Build SHA256SUMS contents from the downloaded release assets."""
    names = set(assets) - {"SHA256SUMS"}
    require(names == expected_assets(version),
            f"release assets are not exactly the expected packages: {sorted(names)}")
    return "".join(f"{sha256(verified_bytes(gh, assets[n]))}  {n}\n" for n in sorted(names))


def complete(gh, release, version, mac_shas=None):
    """Return True if the release has all expected assets, matching SHA256SUMS,
    no pending notice, and (when provided) matching macOS asset digests."""
    assets = asset_map(release)
    names = set(assets) - {"SHA256SUMS"}
    if "SHA256SUMS" not in assets or names != expected_assets(version):
        return False
    for name, digest in sorted((mac_shas or {}).items()):
        if name not in assets or sha256(verified_bytes(gh, assets[name])) != digest:
            return False
    if PENDING.search(release.get("body") or ""):
        return False
    return verified_bytes(gh, assets["SHA256SUMS"]).decode() == sums_for(gh, assets, version)


def changeable(gh, tag, release_id):
    """Re-fetch a release by ID and verify it still has TAG and can be modified."""
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
    names = mac_names(version)
    zip_name, pkg_name, nupkg_name, feed_name = names[0], names[1], names[-2], names[-1]
    files = sorted(p.name for p in Path(package_dir).iterdir())
    require(files == sorted(names),
            f"{package_dir} must hold exactly {', '.join(sorted(names))}, not {files}")
    packages = {name: Path(package_dir) / name for name in names}
    for name, package in packages.items():
        require(package.is_file() and not package.is_symlink(), f"{name} is not a regular file")
    check_portable(packages[zip_name])
    check_installer(packages[pkg_name])
    for alias, original in LEGACY_ALIASES.items():
        require(packages[alias].read_bytes() == packages[original].read_bytes(),
                f"{alias} is not the same file as {original}")
    check_update_package(packages[nupkg_name])
    check_feed(packages[feed_name], version, nupkg_name,
               sha256(packages[nupkg_name].read_bytes()))
    ours = {name: sha256(package.read_bytes()) for name, package in packages.items()}
    release = find_release(gh, tag)
    require(release is not None, f"release {tag} does not exist")
    check_tag(gh, tag, source)
    if not release.get("draft") and complete(gh, release, version, mac_shas=ours):
        return "already complete\n"
    if release.get("draft"):
        refuse_if_behind(gh, tag)
    # Re-fetch before every mutation in case someone manually published the draft mid-run.
    release_id = release["id"]
    release = changeable(gh, tag, release_id)
    assets = asset_map(release)
    present = set(assets) - {"SHA256SUMS", *names}
    require(present == expected_assets(version) - set(names),
            f"release {tag} does not hold exactly the other packages: {sorted(present)}")
    # Verify any already-uploaded macOS assets before uploading the rest (feed goes last).
    for name in names:
        if name in assets:
            require(sha256(verified_bytes(gh, assets[name])) == ours[name],
                    f"release {tag} already has a different {name}; not replacing it")
    for name in names:
        release = changeable(gh, tag, release_id)
        if name not in asset_map(release):
            gh.upload_asset(release_id, packages[name])
    release = changeable(gh, tag, release_id)
    assets = asset_map(release)
    for name in names:
        require(name in assets and sha256(verified_bytes(gh, assets[name])) == ours[name],
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
    # Final verification before publishing; leave manually published releases alone.
    release = changeable(gh, tag, release_id)
    require(complete(gh, release, version, mac_shas=ours), f"release {tag} is not complete")
    if release.get("draft"):
        # Re-check in case a newer release was published while we were uploading.
        refuse_if_behind(gh, tag)
        gh.publish(release_id, tag)
    return sums


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    commands = parser.add_subparsers(dest="command", required=True)
    u = commands.add_parser("publish")
    u.add_argument("--source", required=True)
    u.add_argument("--tag", required=True)
    u.add_argument("--package-dir", required=True, type=Path)
    args = parser.parse_args()
    try:
        gh = Gh(gh_repo())
        print(publish(gh, args.tag, args.source, args.package_dir), end="")
        return 0
    except (Refused, json.JSONDecodeError, KeyError, zipfile.BadZipFile, OSError) as error:
        print(f"macos_release: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
