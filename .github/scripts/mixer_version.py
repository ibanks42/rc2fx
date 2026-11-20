"""Compute and apply the next mixer release version for the Mixer release workflow.

Usage: python3 .github/scripts/mixer_version.py --bump patch|minor|major

Bumps the highest existing `mixer-vX.Y.Z` tag and updates config/mixer-release.json.
If config/mixer-release.json already matches the target version (e.g. bumped manually),
it is kept; any other mismatch is rejected. If no mixer tags exist yet, the checked-in
version is used as-is. Writes `version=` and `changed=` to $GITHUB_OUTPUT if set.
"""
import argparse
import json
import os
import re
import subprocess
from pathlib import Path

MANIFEST = Path("config/mixer-release.json")
STABLE = re.compile(r"mixer-v(\d+)\.(\d+)\.(\d+)")


def parse(version):
    match = re.fullmatch(r"(\d+)\.(\d+)\.(\d+)", version)
    if not match:
        raise ValueError(f"not a release version: {version}")
    return tuple(int(part) for part in match.groups())


def bumped(version, bump):
    major, minor, patch = version
    return {
        "major": (major + 1, 0, 0),
        "minor": (major, minor + 1, 0),
        "patch": (major, minor, patch + 1),
    }[bump]


def latest(tags):
    versions = [tuple(int(p) for p in m.groups()) for m in map(STABLE.fullmatch, tags) if m]
    return max(versions, default=None)


def next_version(checked_in, published, bump):
    """Return (target_version, needs_manifest_update)."""
    if published is None:
        return checked_in, False
    target = bumped(published, bump)
    if checked_in == target:
        return target, False
    if checked_in == published:
        return target, True
    show = ".".join(map(str, checked_in))
    raise ValueError(
        f"config/mixer-release.json says {show}, but a {bump} bump of the latest mixer "
        f"release {'.'.join(map(str, published))} is {'.'.join(map(str, target))}. "
        "Choose the bump that gives the checked-in version, or set it back."
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bump", choices=["patch", "minor", "major"], required=True)
    args = parser.parse_args()
    tags = subprocess.check_output(["git", "tag", "--list", "mixer-v*"], text=True).split()
    manifest = json.loads(MANIFEST.read_text())
    version, changed = next_version(parse(manifest["version"]), latest(tags), args.bump)
    text = ".".join(map(str, version))
    if f"mixer-v{text}" in tags:
        raise ValueError(f"mixer-v{text} already exists")
    if changed:
        manifest["version"] = text
        MANIFEST.write_text(json.dumps(manifest, indent=2) + "\n")
    if os.environ.get("GITHUB_OUTPUT"):
        with open(os.environ["GITHUB_OUTPUT"], "a") as output:
            output.write(f"version={text}\nchanged={'true' if changed else 'false'}\n")
    print(f"mixer {text}" + (" (bumped)" if changed else " (checked in)"))


if __name__ == "__main__":
    main()
