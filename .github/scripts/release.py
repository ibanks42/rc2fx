"""Release preparation shared by the native build jobs. Run from the repo root."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tarfile
import zipfile

from update_history import parse_changelog


STABLE = r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)"
PACK_ID = "io.github.ibanks42.rc2fx"
OUTPUT = Path("target/release-input")
APP_RELEASE = Path("config/app-release.json")
WINDOWS_NUPKG_MEMBERS = ("lib/app/rc2fx.exe", "lib/app/velopack_libc.dll")
MACOS_NUPKG_MEMBERS = (
    "lib/app/Contents/MacOS/rc2fx",
    "lib/app/Contents/MacOS/UpdateMac",
    "lib/app/Contents/Info.plist",
)


def git(*args):
    return subprocess.check_output(["git", *args], text=True).strip()


def version_tuple(version):
    if not re.fullmatch(STABLE, version):
        raise ValueError(f"Expected a stable version, got {version!r}")
    return tuple(map(int, version.split(".")))


def current_app_version(root=Path(".")):
    data = json.loads((root / APP_RELEASE).read_text())
    version = data.get("version")
    if not isinstance(version, str):
        raise ValueError(f"{APP_RELEASE} has no version")
    version_tuple(version)
    return version


# Go desktop version constant kept in sync with config/app-release.json.
# The mock mixer tracks config/mixer-release.json separately.
GO_VERSIONS = {
    Path("desktop/installer/installer.go"): "const AppVersion",
}


def update_versions(version):
    current = current_app_version()
    version_tuple(version)
    release = json.loads(APP_RELEASE.read_text())
    release["version"] = version
    updates = {APP_RELEASE: json.dumps(release, indent=2) + "\n"}
    for path, declaration in GO_VERSIONS.items():
        text = path.read_text()
        pattern = rf'^({re.escape(declaration)} = )"([^"\n]*)"$'
        found = re.findall(pattern, text, flags=re.M)
        if len(found) != 1:
            raise ValueError(f"Expected one {declaration} in {path}, got {len(found)}")
        if found[0][1] != current:
            raise ValueError(f"{path}: {declaration} differs from {APP_RELEASE} version")
        updates[path] = re.sub(pattern, lambda m: m[1] + json.dumps(version), text, flags=re.M)
    for path, text in updates.items():
        path.write_text(text)
    return [str(path) for path in updates]


def prepare(bump):
    if not os.environ.get("ACT") and git("status", "--porcelain", "--untracked-files=no"):
        raise ValueError("Release checkout must be clean")
    current = current_app_version()
    version_tuple(current)
    previous = "v" + current
    cliff = ["git-cliff", "--config", "cliff.toml", "--topo-order", "--use-branch-tags"]
    if not git("tag", "--list", previous):
        raise ValueError(f"Tag {previous} for the current version is missing")
    git("merge-base", "--is-ancestor", previous, "HEAD")
    if not git("rev-list", f"{previous}..HEAD"):
        raise ValueError("No new commits to release")
    history = [f"{previous}..HEAD"]
    tag = subprocess.check_output(cliff + history + ["--bump", bump, "--bumped-version"], text=True).strip()
    if not tag.startswith("v"):
        tag = "v" + tag
    version = tag[1:]
    if version_tuple(version) <= version_tuple(current):
        raise ValueError("Version must increase")
    if git("tag", "--list", tag):
        raise ValueError(f"Tag {tag} already exists")
    files = update_versions(version)
    if OUTPUT.exists():
        shutil.rmtree(OUTPUT)
    OUTPUT.mkdir(parents=True)
    notes = OUTPUT / "release-notes.md"
    subprocess.run(cliff + history + ["--tag", tag, "--strip", "all", "--output", str(notes)], check=True)
    if not notes.read_text().strip():
        raise ValueError("Release notes are empty")
    # Release packages don't bundle the mixer software; the firmware installer downloads it.
    with notes.open("a") as output:
        output.write("\nMixer software is downloaded separately by the firmware installer.\n")
    changelog = Path("CHANGELOG.md")
    destination = "--prepend" if changelog.exists() else "--output"
    subprocess.run(cliff + history + ["--unreleased", "--tag", tag, destination, str(changelog)], check=True)
    if version not in parse_changelog(changelog):
        raise ValueError(f"Release {tag} has no valid changelog section")
    files.append(str(changelog))
    plan = {"tag": tag, "previous": previous, "source": git("rev-parse", "HEAD"), "files": files}
    (OUTPUT / "plan.json").write_text(json.dumps(plan, indent=2) + "\n")
    with tarfile.open(OUTPUT / "versioned.tar", "w") as archive:
        for filename in files:
            archive.add(filename, arcname=filename)
    if os.environ.get("GITHUB_OUTPUT"):
        with open(os.environ["GITHUB_OUTPUT"], "a") as output:
            output.write(f"tag={tag}\n")
    print(f"v{current} -> {tag}")


def commit():
    if os.environ.get("ACT"):
        # act copies the host index and lock files; use an isolated index in the container.
        os.environ["GIT_INDEX_FILE"] = str((OUTPUT / "git-index").resolve())
        git("read-tree", "HEAD")
    plan = json.loads((OUTPUT / "plan.json").read_text())
    if git("rev-parse", "HEAD") != plan["source"]:
        raise ValueError("Source commit changed")
    tag = plan["tag"]
    version_tuple(tag.removeprefix("v"))
    if git("tag", "--list", tag):
        raise ValueError(f"Tag {tag} already exists")
    git("config", "user.name", "github-actions[bot]")
    git("config", "user.email", "41898282+github-actions[bot]@users.noreply.github.com")
    git("add", "--", *plan["files"])
    changed = subprocess.run(["git", "diff", "--cached", "--quiet", "--", *plan["files"]]).returncode
    if changed == 1:
        print(git("commit", "--only", "-m", f"chore(release): {tag}", "--", *plan["files"]))
    elif changed != 0:
        raise ValueError("Could not check prepared release changes")
    git("tag", tag)
    print(f"Created local tag {tag}")


def _require(condition, message):
    if not condition:
        raise ValueError(message)


def _safe_relative(name):
    parts = name.split("/")
    _require(
        name and not name.startswith("/") and "\\" not in name
        and all(part not in ("", ".", "..") for part in parts),
        f"unsafe path {name!r}",
    )


def _sha256(data):
    return hashlib.sha256(data).hexdigest()


def _sound_zip(path):
    _require(zipfile.is_zipfile(path), f"{path.name} is not a zip")
    with zipfile.ZipFile(path) as package:
        _require(package.testzip() is None, f"{path.name} is damaged")
        entries = [i.filename for i in package.infolist()]
    _require(len(set(entries)) == len(entries), f"{path.name} repeats an entry name")
    for entry in entries:
        _safe_relative(entry.rstrip("/"))
    return entries


def _check_feed(path, version, package_name, digest):
    try:
        feed = json.loads(path.read_bytes())
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ValueError(f"{path.name} is not JSON: {error}") from None
    assets = feed.get("Assets") if isinstance(feed, dict) else None
    _require(isinstance(assets, list) and len(assets) == 1,
             f"{path.name} does not list exactly one package")
    asset = assets[0]
    _require(isinstance(asset, dict), f"{path.name} lists a package that is not an object")
    field = lambda name: asset.get(name) if isinstance(asset.get(name), str) else ""
    _require(
        field("PackageId") == PACK_ID and field("Version") == version
        and field("Type") == "Full" and field("FileName") == package_name
        and field("SHA256").lower() == digest,
        f"{path.name} does not describe {package_name} {version} with SHA-256 {digest}",
    )


def expected_assets(version):
    """Return the expected app release assets."""
    return {
        "rc2fx-linux-x64.AppImage",
        f"rc2fx-{version}-appimage-runtime-sources.tar.gz",
        f"{PACK_ID}-{version}-linux-full.nupkg",
        "releases.linux.json",
        "rc2fx-windows-x64-setup.exe",
        f"{PACK_ID}-{version}-full.nupkg",
        "releases.win.json",
        "rc2fx-macos-universal.pkg",
        f"{PACK_ID}-{version}-osx-full.nupkg",
        "releases.osx.json",
    }


def verify_assets(directory, version=None):
    """Validate every platform package in DIRECTORY and return their SHA-256 listing."""
    directory = Path(directory)
    version = version or current_app_version()
    version_tuple(version)
    sources_name = f"rc2fx-{version}-appimage-runtime-sources.tar.gz"
    appimage = "rc2fx-linux-x64.AppImage"
    feeds = {
        "releases.win.json": f"{PACK_ID}-{version}-full.nupkg",
        "releases.linux.json": f"{PACK_ID}-{version}-linux-full.nupkg",
        "releases.osx.json": f"{PACK_ID}-{version}-osx-full.nupkg",
    }
    archives = sorted(directory.iterdir())
    names = {p.name for p in archives}
    expected = expected_assets(version)
    _require(names == expected, f"Missing or unexpected release packages: {sorted(names ^ expected)}")
    for archive in archives:
        _require(archive.is_file() and not archive.is_symlink(), f"{archive.name} is not a regular file")
    for archive in archives:
        if archive.name == sources_name:
            with tarfile.open(archive, "r:gz") as sources:
                _require(bool(sources.getmembers()), "Empty runtime source archive")
        elif archive.suffix == ".AppImage":
            with archive.open("rb") as stream:
                header = stream.read(11)
            _require(header[:4] == b"\x7fELF" and header[8:11] == b"AI\x02", "Invalid AppImage")
        elif archive.suffix == ".exe":
            with archive.open("rb") as stream:
                _require(stream.read(2) == b"MZ", "Invalid Windows installer")
        elif archive.suffix == ".pkg":
            with archive.open("rb") as stream:
                _require(stream.read(4) == b"xar!", "Invalid macOS installer")
        elif archive.suffix == ".json":
            package_name = feeds[archive.name]
            _check_feed(archive, version, package_name, _sha256((directory / package_name).read_bytes()))
        else:
            _require(archive.suffix == ".nupkg", f"Unexpected archive {archive.name}")
            entries = _sound_zip(archive)
            members = set(entries)
            if archive.name == feeds["releases.linux.json"]:
                with zipfile.ZipFile(archive) as package:
                    inside = package.read(f"lib/app/{PACK_ID}.AppImage")
                _require(inside == (directory / appimage).read_bytes(),
                         f"{archive.name} holds another AppImage")
            elif archive.name == feeds["releases.win.json"]:
                missing = sorted(set(WINDOWS_NUPKG_MEMBERS) - members)
                _require(not missing, f"{archive.name} lacks {missing}")
            else:
                _require(any("/" not in e and e.endswith(".nuspec") for e in entries),
                         f"{archive.name} has no .nuspec at its root")
                missing = sorted(set(MACOS_NUPKG_MEMBERS) - members)
                _require(not missing, f"{archive.name} lacks {missing}")
    return "".join(f"{_sha256(p.read_bytes())}  {p.name}\n" for p in archives)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=["prepare", "commit", "verify"])
    parser.add_argument("--bump", choices=["patch", "minor", "major"], default="patch")
    parser.add_argument("--dir", type=Path, default=Path("target/release-assets"))
    parser.add_argument("--version")
    args = parser.parse_args()
    if args.command == "prepare":
        prepare(args.bump)
    elif args.command == "commit":
        commit()
    else:
        print(verify_assets(args.dir, args.version), end="")
