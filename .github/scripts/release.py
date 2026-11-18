"""Release preparation shared by the native build jobs. Run from the repo root."""

import argparse
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tarfile
import tomllib

from update_history import parse_changelog


STABLE = r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)"
OUTPUT = Path("target/release-input")


def git(*args):
    return subprocess.check_output(["git", *args], text=True).strip()


def version_tuple(version):
    if not re.fullmatch(STABLE, version):
        raise ValueError(f"Expected a stable version, got {version!r}")
    return tuple(map(int, version.split(".")))


# The Go desktop's copies of the workspace version: the app version
# (installer.AppVersion, which packages are named with and the app reports;
# package model refers to it rather than keeping a copy) and the mock
# mixer's runtime version.
GO_VERSIONS = {
    Path("desktop/installer/installer.go"): "const AppVersion",
    Path("desktop/transport/mock.go"): "const mockRuntime",
}


def replace_version(text, section, version):
    pattern = rf'(^\[{re.escape(section)}\]\s*\n(?:(?!\[)[\s\S])*?^version\s*=\s*)"[^"\n]+"'
    result, count = re.subn(pattern, lambda m: m[1] + json.dumps(version), text, flags=re.M)
    if count != 1:
        raise ValueError(f"Expected one version in [{section}], got {count}")
    return result


def update_versions(version):
    root = Path("Cargo.toml")
    cargo = tomllib.loads(root.read_text())
    current = cargo["workspace"]["package"]["version"]
    version_tuple(version)
    manifests = [Path(member) / "Cargo.toml" for member in cargo["workspace"]["members"]]
    packages = {tomllib.loads(path.read_text())["package"]["name"]: path for path in manifests}
    updates = {root: replace_version(root.read_text(), "workspace.package", version)}
    for name, path in packages.items():
        package = tomllib.loads(path.read_text())["package"]
        declared = package["version"]
        if isinstance(declared, str):
            if declared != current:
                raise ValueError(f"{name} version differs from workspace version")
            updates[path] = replace_version(path.read_text(), "package", version)
        elif declared != {"workspace": True}:
            raise ValueError(f"Unexpected version declaration for {name}")

    # Separate mixer/benchmark workspaces lock some of these same path packages.
    for path in [Path("Cargo.lock"), Path("crates/rc2fx-native/Cargo.lock"), Path("crates/rc2fx-bench/Cargo.lock")]:
        text = path.read_text()
        found = set()

        def update(match):
            block = match[0]
            package = tomllib.loads(block)["package"][0]
            if package["name"] not in packages or "source" in package:
                return block
            if package["version"] != current:
                raise ValueError(f"{path}: stale version for {package['name']}")
            found.add(package["name"])
            return re.sub(r'^version = "[^"\n]+"', f'version = "{version}"', block, count=1, flags=re.M)

        updated = re.sub(r'\[\[package\]\][\s\S]*?(?=\n\[\[package\]\]|\Z)', update, text)
        if path == Path("Cargo.lock") and found != set(packages):
            raise ValueError("Workspace lockfile is missing local packages")
        if found:
            updates[path] = updated
    for path, declaration in GO_VERSIONS.items():
        text = path.read_text()
        pattern = rf'^({re.escape(declaration)} = )"([^"\n]*)"$'
        found = re.findall(pattern, text, flags=re.M)
        if len(found) != 1:
            raise ValueError(f"Expected one {declaration} in {path}, got {len(found)}")
        if found[0][1] != current:
            raise ValueError(f"{path}: {declaration} differs from workspace version")
        updates[path] = re.sub(pattern, lambda m: m[1] + json.dumps(version), text, flags=re.M)
    for path, text in updates.items():
        path.write_text(text)
    return [str(path) for path in updates]


def prepare(bump):
    if not os.environ.get("ACT") and git("status", "--porcelain", "--untracked-files=no"):
        raise ValueError("Release checkout must be clean")
    current = tomllib.loads(Path("Cargo.toml").read_text())["workspace"]["package"]["version"]
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
    # Release packages never bundle the mixer software; the firmware installer
    # downloads it.
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
        # act copies the host index and any locks; use a container-only index.
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
