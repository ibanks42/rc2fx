"""Resolve docs source at execution time, including when an old run is retried."""

import json
import os
from pathlib import Path
import re
import subprocess


STABLE_TAG = re.compile(r"v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)")


def source_ref(source, latest):
    if source == "main":
        return "refs/heads/main"
    if source != "latest-release":
        raise ValueError(f"unsupported documentation source: {source!r}")
    release = latest()
    if (not isinstance(release, dict) or release.get("draft") is not False
            or release.get("prerelease") is not False or not release.get("published_at")):
        raise ValueError("latest release is not a published stable release")
    tag = release.get("tag_name")
    if not isinstance(tag, str) or not STABLE_TAG.fullmatch(tag):
        raise ValueError("latest release does not have a stable app tag")
    return f"refs/tags/{tag}"


def latest_release():
    repo = os.environ["GH_REPO"]
    return json.loads(subprocess.check_output(
        ["gh", "api", f"repos/{repo}/releases/latest"], text=True
    ))


if __name__ == "__main__":
    ref = source_ref(os.environ.get("DOCS_SOURCE", "main"), latest_release)
    with Path(os.environ["GITHUB_OUTPUT"]).open("a") as output:
        output.write(f"ref={ref}\n")
    print(f"Documentation source: {ref}")
