#!/usr/bin/env python3
"""Finish an app release by adding the macOS packages, then publish it.

The Release workflow creates the release as a DRAFT with the Linux and Windows
packages; its last job calls this script with the macOS packages Velopack
built in the same run: the portable zip, the installer, and the update package
and feed. It adds them, rewrites SHA256SUMS, removes the pending notice and, as
the very last step after every check, publishes the release as latest.
A published release is never changed afterwards by this script.

  publish --tag TAG --source SHA --package-dir DIR
      Finish the draft and publish it as latest, last. A release someone
      published by hand before it was complete is finished but not published
      again, so which release is latest is left as they chose. On a complete
      published release, only check that it holds these exact macOS packages. A
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
import tempfile
import zipfile
from pathlib import Path

STABLE = r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)"
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


PACK_ID = "io.github.ibanks42.rc2fx"
APP = "RC2FX.app/"
SEQUESTERED = "__MACOSX/"
APP_MEMBERS = (
    "RC2FX.app/Contents/MacOS/rc2fx",
    "RC2FX.app/Contents/MacOS/UpdateMac",
    "RC2FX.app/Contents/Info.plist",
)


def mac_names(version):
    """The macOS packages, which this script adds: the portable zip, the
    installer, and the update package and its feed (which keep vpk's names,
    since the feed names the package). The feed comes last."""
    return ("rc2fx-macos-arm64.zip", "rc2fx-macos-arm64.pkg",
            f"{PACK_ID}-{version}-osx-full.nupkg", "releases.osx.json")


def expected_assets(version):
    """The twelve packages, without SHA256SUMS. Published names have no
    version, so releases/latest/download/NAME always links the newest one;
    the update packages and feeds keep vpk's names, which do."""
    return {
        "rc2fx-linux-x64.AppImage", f"{PACK_ID}-{version}-linux-full.nupkg",
        "releases.linux.json", f"rc2fx-{version}-appimage-runtime-sources.tar.gz",
        "rc2fx-windows-x64.zip", "rc2fx-windows-x64-setup.exe",
        f"{PACK_ID}-{version}-full.nupkg", "releases.win.json", *mac_names(version),
    }


def sound_zip(path):
    """The entry names of a sound zip; a repeated name is refused."""
    require(zipfile.is_zipfile(path), f"{path.name} is not a zip")
    with zipfile.ZipFile(path) as package:
        require(package.testzip() is None, f"{path.name} is damaged")
        entries = [i.filename for i in package.infolist()]
    require(len(set(entries)) == len(entries), f"{path.name} repeats an entry name")
    return entries


def check_portable(path):
    """Velopack's portable zip: the app bundle (`ditto --keepParent`), with
    its updater, and nothing outside it but the bundle's sequestered
    AppleDouble entries (`--sequesterRsrc`): __MACOSX/._RC2FX.app for the
    bundle folder itself, and those under __MACOSX/RC2FX.app/."""
    entries = sound_zip(path)
    for entry in entries:
        require(entry.startswith((APP, SEQUESTERED + APP))
                or entry in (SEQUESTERED, SEQUESTERED + "._" + APP.rstrip("/")),
                f"{path.name} has {entry!r} outside {APP} and {SEQUESTERED + APP}")
        safe_relative(entry.rstrip("/"))
    for member in APP_MEMBERS:
        require(member in entries, f"{path.name} has no {member}")


def check_installer(path):
    """A macOS installer package: a non-empty xar archive."""
    with path.open("rb") as package:
        magic = package.read(4)
    require(magic, f"{path.name} is empty")
    require(magic == b"xar!", f"{path.name} is not a xar archive; it is not an installer package")


def check_update_package(path):
    """A NuGet package: a sound zip with its .nuspec at the root."""
    entries = sound_zip(path)
    require(any("/" not in e and e.endswith(".nuspec") for e in entries),
            f"{path.name} has no .nuspec at its root")


def check_feed(path, version, package, digest):
    """The feed must offer exactly this release's package, with its digest:
    that digest is what the app checks the download against."""
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
    """SHA256SUMS text for the packages, from their actual bytes."""
    names = set(assets) - {"SHA256SUMS"}
    require(names == expected_assets(version),
            f"release assets are not exactly the expected packages: {sorted(names)}")
    return "".join(f"{sha256(verified_bytes(gh, assets[n]))}  {n}\n" for n in sorted(names))


def complete(gh, release, version, mac_shas=None):
    """Whether a release holds exactly the packages and a SHA256SUMS that
    matches them, has no pending notice, and (if given, as {name: sha256})
    these macOS packages."""
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
    zip_name, pkg_name, nupkg_name, feed_name = names = mac_names(version)
    files = sorted(p.name for p in Path(package_dir).iterdir())
    require(files == sorted(names),
            f"{package_dir} must hold exactly {', '.join(sorted(names))}, not {files}")
    packages = {name: Path(package_dir) / name for name in names}
    for name, package in packages.items():
        require(package.is_file() and not package.is_symlink(), f"{name} is not a regular file")
    check_portable(packages[zip_name])
    check_installer(packages[pkg_name])
    check_update_package(packages[nupkg_name])
    check_feed(packages[feed_name], version, nupkg_name,
               sha256(packages[nupkg_name].read_bytes()))
    ours = {name: sha256(package.read_bytes()) for name, package in packages.items()}
    release = find_release(gh, tag)
    require(release is not None, f"release {tag} does not exist")
    check_tag(gh, tag, source)
    if not release.get("draft") and complete(gh, release, version, mac_shas=ours):
        return "already complete\n"  # read only: nothing to change
    if release.get("draft"):
        refuse_if_behind(gh, tag)
    # The release is re-read before every change: someone may publish the
    # draft by hand meanwhile, which is fine while it stays changeable.
    release_id = release["id"]
    release = changeable(gh, tag, release_id)
    assets = asset_map(release)
    present = set(assets) - {"SHA256SUMS", *names}
    require(present == expected_assets(version) - set(names),
            f"release {tag} does not hold exactly the other packages: {sorted(present)}")
    # Every existing copy is checked before any upload. The feed is uploaded
    # last, after the package it names.
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
    # Last: everything checked again on the release as it now is. A draft is
    # published as latest; a release already published by hand is left so.
    release = changeable(gh, tag, release_id)
    require(complete(gh, release, version, mac_shas=ours), f"release {tag} is not complete")
    if release.get("draft"):
        # A newer release may have been published during this run.
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
        repo = os.environ.get("GH_REPO") or os.environ.get("GITHUB_REPOSITORY", "")
        require(re.fullmatch(r"[\w.-]+/[\w.-]+", repo), "GH_REPO or GITHUB_REPOSITORY is not set")
        gh = Gh(repo)
        print(publish(gh, args.tag, args.source, args.package_dir), end="")
        return 0
    except (Refused, json.JSONDecodeError, KeyError, zipfile.BadZipFile, OSError) as error:
        print(f"macos_release: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
