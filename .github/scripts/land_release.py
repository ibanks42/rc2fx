"""Push a release's version commit and tags to main.

Usage: python3 .github/scripts/land_release.py --kind app|mixer --source SHA
           [--remote REMOTE] [--tag TAG]...

The app and mixer release workflows each build from the main commit they were
started on (SOURCE) and then push one version commit on top of it. When both
run at once, whichever finishes second finds main moved by the other's
version commit. That commit can't change what this release built, so the
version commit is moved on top of it instead of failing the run. Any other
change to main still stops the release.

Writes `sha=` (the commit the release points at) to $GITHUB_OUTPUT if set.
"""

import argparse
import os
import re
import subprocess

# What each workflow's version commit looks like. A release accepts only the
# other workflow's commits between its source and main.
APP_SUBJECT = re.compile(r"chore\(release\): v\d+\.\d+\.\d+")
APP_FILES = frozenset(
    {"CHANGELOG.md", "config/app-release.json", "desktop/installer/installer.go"}
)
MIXER_SUBJECT = re.compile(r"chore\(mixer-release\): \d+\.\d+\.\d+")
MIXER_FILES = frozenset({"config/mixer-release.json"})

OTHER = {"app": (MIXER_SUBJECT, MIXER_FILES), "mixer": (APP_SUBJECT, APP_FILES)}


class Refused(Exception):
    pass


def git(*args):
    done = subprocess.run(["git", *args], capture_output=True, text=True, check=False)
    if done.returncode != 0:
        # Don't echo the arguments: a remote URL can carry a token.
        raise Refused(f"git {args[0]} failed: {done.stderr.strip()}")
    return done.stdout.strip()


def foreign_commit_problem(kind, subject, files):
    """Return why a commit between source and main stops a `kind` release, or None."""
    pattern, allowed = OTHER[kind]
    if not pattern.fullmatch(subject):
        return f"it is not a {'mixer' if kind == 'app' else 'app'} version commit"
    extra = sorted(set(files) - allowed)
    if extra:
        return f"it changes {', '.join(extra)}"
    return None


def check_moved(kind, source, head):
    if git("merge-base", source, head) != source:
        raise Refused(
            f"main no longer contains {source}. Start a new {kind} release run."
        )
    for commit in git("rev-list", "--reverse", f"{source}..{head}").split():
        subject = git("show", "-s", "--format=%s", commit)
        files = git("diff-tree", "--no-commit-id", "--name-only", "-r", commit).split()
        problem = foreign_commit_problem(kind, subject, files)
        if problem:
            raise Refused(
                f"main changed during the build: {commit[:12]} ({subject}) can't be "
                f"released over because {problem}. Start a new {kind} release run."
            )


def land(kind, source, remote, tags):
    git("fetch", "-q", remote, "main")
    head = git("rev-parse", "FETCH_HEAD")
    ours = git("rev-parse", "HEAD")
    own = git("rev-list", f"{source}..{ours}").split()
    if len(own) > 1 or git("merge-base", ours, source) != source:
        raise Refused("expected at most one version commit on top of the source")
    if head != source:
        check_moved(kind, source, head)
        if own:
            git("rebase", "-q", "--onto", head, source)
            ours = git("rev-parse", "HEAD")
            for tag in tags:
                git("tag", "-f", tag, ours)
            print(f"main moved to {head[:12]}; moved the version commit on top of it")
    refs = [f"refs/tags/{tag}" for tag in tags]
    if own:
        refs.insert(0, "HEAD:refs/heads/main")
    if refs:
        git("push", "-q", "--atomic", remote, *refs)
    return ours


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--kind", choices=sorted(OTHER), required=True)
    parser.add_argument("--source", required=True)
    parser.add_argument("--remote", default="origin")
    parser.add_argument("--tag", action="append", default=[])
    args = parser.parse_args()
    try:
        sha = land(args.kind, args.source, args.remote, args.tag)
    except Refused as error:
        raise SystemExit(str(error))
    print(f"sha={sha}")
    output = os.environ.get("GITHUB_OUTPUT")
    if output:
        with open(output, "a", encoding="utf-8") as stream:
            stream.write(f"sha={sha}\n")


if __name__ == "__main__":
    main()
