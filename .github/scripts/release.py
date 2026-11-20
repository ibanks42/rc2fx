"""Release preparation shared by the native build jobs. Run from the repo root."""

import argparse
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tarfile

from update_history import parse_changelog


STABLE = r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)"
OUTPUT = Path("target/release-input")
APP_RELEASE = Path("config/app-release.json")


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


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=["prepare", "commit"])
    parser.add_argument("--bump", choices=["patch", "minor", "major"], default="patch")
    args = parser.parse_args()
    if args.command == "prepare":
        prepare(args.bump)
    else:
        commit()
