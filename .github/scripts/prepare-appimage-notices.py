#!/usr/bin/env python3
"""Notices and corresponding source for the AppImage type 2 runtime.

The Linux AppImage starts with the AppImage type 2 runtime, a static
executable that links a modified libfuse (LGPL-2.1) with squashfuse, zstd,
zlib, musl and mimalloc. This script:

- fetches the runtime source at the pinned commit (checked by git),
- fetches libfuse and squashfuse at the versions and SHA-256 checksums that
  the runtime's own build recipe pins,
- fetches the Alpine aports build recipes and sources of zstd, zlib, musl and
  mimalloc at the aports 3.21-stable commit current when the runtime was
  built, checked against Alpine's SHA-512 sums,
- writes target/appimage-legal/notices.json with the full licence texts, and
- writes target/package/rc2fx-appimage-runtime-sources.tar.gz with all of the
  above, the build scripts and patches, and a README on rebuilding and
  relinking.

The runtime's recipe installs those four from `alpine:3.21` without pinning
package revisions or the image digest, and its build log has expired. The
aports commit used here is the last 3.21-stable commit before the runtime
release was built (2025-12-08). The revisions are therefore inferred, and are
recorded as such; zlib 1.3.1 matches the version string in the runtime binary.

The script fails if any source or licence text is unavailable or
does not match its checksum.

Usage: prepare-appimage-notices.py [--root DIR] [--cache DIR]
"""

from __future__ import annotations

import argparse
import gzip
import hashlib
import io
import json
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import time
import urllib.request
from pathlib import Path

RUNTIME_REPO = "https://github.com/AppImage/type2-runtime"
RUNTIME_COMMIT = "dd6cebedcbddde9c82f89b011e8e1d40b6e43868"
# The released runtime binary built from RUNTIME_COMMIT, as pinned in
# setup-release-tools.sh. The script checks the release targets the commit
# and the binary matches, and records the hash in notices.json so packaging
# can refuse any other runtime.
RUNTIME_RELEASE = "20251108"
RUNTIME_SHA256 = "2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d"
RUNTIME_RELEASE_API = "https://api.github.com/repos/AppImage/type2-runtime/releases/tags/{tag}"
RUNTIME_BINARY = "https://github.com/AppImage/type2-runtime/releases/download/{tag}/runtime-x86_64"
ALPINE_BRANCH = "v3.21"
# The last aports 3.21-stable commit before the runtime release at
# RUNTIME_COMMIT was built (2025-12-08 11:23 +01:00). Inferred, not proven.
APORTS_COMMIT = "590fd1489813473277cf110fd88b1c3888d19367"
ALPINE_DISTFILES = "https://distfiles.alpinelinux.org/distfiles/{branch}/{name}"
APORTS_ARCHIVE = (
    "https://gitlab.alpinelinux.org/alpine/aports/-/archive/{commit}/aports-{commit}.tar.gz?path={path}"
)
# The aports that build the -dev/-static packages the runtime's Dockerfile
# installs for the libraries it links (src/runtime/Makefile LIBS).
# mimalloc-dev is provided by community/mimalloc2.
ALPINE_PACKAGES = {
    "zstd": "main/zstd",
    "zlib": "main/zlib",
    "musl": "main/musl",
    "mimalloc": "community/mimalloc2",
}
# Licence files inside each verified source, in the order they are printed.
LICENCE_FILES = {
    "zstd": ["LICENSE"],
    "zlib": ["LICENSE"],
    "musl": ["COPYRIGHT"],
    "mimalloc": ["LICENSE"],
}
USER_AGENT = "rc2fx-release-notices"
SOURCE_DATE = 0


class Failure(Exception):
    pass


def fetch(url: str, cache: Path | None) -> bytes:
    if cache is not None:
        cached = cache / hashlib.sha256(url.encode()).hexdigest()
        if cached.is_file():
            return cached.read_bytes()
    last = None
    for attempt in range(3):
        try:
            request = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
            with urllib.request.urlopen(request, timeout=120) as response:
                data = response.read()
            break
        except OSError as error:
            last = error
            time.sleep(2 * (attempt + 1))
    else:
        raise Failure(f"could not fetch {url}: {last}")
    if cache is not None:
        cache.mkdir(parents=True, exist_ok=True)
        cached.write_bytes(data)
    return data


def safe_relative(name: str) -> str:
    """A bundle-relative path that stays inside the bundle."""
    parts = name.split("/")
    if (
        not name
        or name.startswith("/")
        or "\\" in name
        or any(part in ("", ".", "..") for part in parts)
    ):
        raise Failure(f"unsafe path in an archive: {name!r}")
    return name


def digest(algorithm: str, data: bytes) -> str:
    return hashlib.new(algorithm, data).hexdigest()


def require(algorithm: str, data: bytes, expected: str, what: str) -> None:
    actual = digest(algorithm, data)
    if actual != expected:
        raise Failure(f"{what}: {algorithm} {actual}, expected {expected}")


def apkbuild_field(apkbuild: str, name: str) -> str:
    """A simple `name=value` or `name="value"` line of an APKBUILD."""
    match = re.search(rf'^{name}="?([^"\n]*)"?$', apkbuild, re.M)
    if not match:
        raise Failure(f"APKBUILD has no {name}")
    return match.group(1)


def parse_sums(apkbuild: str) -> list[tuple[str, str]]:
    """The (sha512, filename) pairs of an APKBUILD's sha512sums block."""
    match = re.search(r'^sha512sums="(.*?)"', apkbuild, re.S | re.M)
    if not match:
        raise Failure("APKBUILD has no sha512sums")
    pairs = []
    for line in match.group(1).strip().splitlines():
        parts = line.split()
        if len(parts) != 2 or not re.fullmatch(r"[0-9a-f]{128}", parts[0]):
            raise Failure(f"malformed sha512sums line: {line!r}")
        pairs.append((parts[0], parts[1]))
    return pairs


def recipe_pins(script: str) -> dict[str, str]:
    """`sha256  filename` pins from the runtime's install-dependencies.sh."""
    return {
        name: sha
        for sha, name in re.findall(r'echo "([0-9a-f]{64})  ([^"\s]+)" \| sha256sum -c', script)
    }


def recipe_url(script: str, filename: str) -> str:
    for url in re.findall(r'wget "?(https://[^"\s]+)"?', script):
        if url.rsplit("/", 1)[-1] == filename:
            return url
    raise Failure(f"install-dependencies.sh has no download for {filename}")


def member(archive: bytes, suffix: str) -> bytes:
    """The one file in a tarball whose path ends with /suffix."""
    with tarfile.open(fileobj=io.BytesIO(archive)) as tar:
        found = [
            m for m in tar.getmembers()
            if m.isfile() and (m.name == suffix or m.name.endswith("/" + suffix))
            and m.name.count("/") <= 1 + suffix.count("/")
        ]
        if len(found) != 1:
            raise Failure(f"expected one {suffix} in the archive, found {len(found)}")
        return tar.extractfile(found[0]).read()


def aport_files(archive: bytes, path: str) -> dict[str, bytes]:
    """The regular files under `path` in an aports archive, by safe relative name.
    Links and other entries are refused, not followed."""
    files = {}
    with tarfile.open(fileobj=io.BytesIO(archive)) as tar:
        for m in tar.getmembers():
            if f"/{path}/" not in f"/{m.name}":
                continue
            relative = safe_relative(m.name.split(f"/{path}/", 1)[1])
            if m.isdir():
                continue
            if not m.isfile():
                raise Failure(f"{path}/{relative} is not a regular file")
            files[relative] = tar.extractfile(m).read()
    return files


def runtime_source() -> bytes:
    """`git archive` of the runtime at the pinned commit; git checks the id.
    Not cached, so the commit is checked on every run."""
    with tempfile.TemporaryDirectory() as work:
        git = ["git", "-C", work, "-c", "advice.detachedHead=false"]
        subprocess.run(["git", "init", "-q", work], check=True)
        subprocess.run(git + ["fetch", "-q", "--depth", "1", RUNTIME_REPO, RUNTIME_COMMIT], check=True)
        head = subprocess.run(
            git + ["rev-parse", "FETCH_HEAD"], check=True, capture_output=True, text=True
        ).stdout.strip()
        if head != RUNTIME_COMMIT:
            raise Failure(f"fetched runtime commit {head}, expected {RUNTIME_COMMIT}")
        data = subprocess.run(
            git + ["archive", "--format=tar", f"--prefix=type2-runtime-{RUNTIME_COMMIT}/", head],
            check=True,
            capture_output=True,
        ).stdout
    return data


def collect(cache: Path | None) -> tuple[dict[str, bytes], list[dict], list[dict]]:
    """Bundle files by path, the source manifest, and notice sections."""
    files: dict[str, bytes] = {}
    manifest: list[dict] = []
    sections: list[dict] = []

    release = json.loads(fetch(RUNTIME_RELEASE_API.format(tag=RUNTIME_RELEASE), None))
    if release.get("target_commitish") != RUNTIME_COMMIT:
        raise Failure(
            f"runtime release {RUNTIME_RELEASE} targets {release.get('target_commitish')}, "
            f"expected {RUNTIME_COMMIT}"
        )
    binary_url = RUNTIME_BINARY.format(tag=RUNTIME_RELEASE)
    require("sha256", fetch(binary_url, cache), RUNTIME_SHA256, binary_url)
    runtime = runtime_source()
    files[f"type2-runtime-{RUNTIME_COMMIT}.tar"] = runtime
    script = member(runtime, "scripts/common/install-dependencies.sh").decode()
    dockerfile = member(runtime, "scripts/docker/Dockerfile").decode()
    if f"FROM alpine:{ALPINE_BRANCH[1:]}" not in dockerfile:
        raise Failure(f"the runtime's Dockerfile no longer builds on alpine:{ALPINE_BRANCH[1:]}")
    manifest.append({
        "component": "AppImage type 2 runtime",
        "source": f"{RUNTIME_REPO}/tree/{RUNTIME_COMMIT}",
        "file": f"type2-runtime-{RUNTIME_COMMIT}.tar",
        "sha256": digest("sha256", runtime),
        "verification": (
            f"git commit id; release {RUNTIME_RELEASE} targets it, and its runtime-x86_64 "
            f"has sha256 {RUNTIME_SHA256}"
        ),
    })
    sections.append({
        "title": "AppImage type 2 runtime",
        "text": member(runtime, "LICENSE").decode(),
    })

    pins = recipe_pins(script)
    for name, title, licences in [
        ("fuse-3.15.0.tar.xz", "libfuse 3.15.0 (modified)", ["LGPL2.txt"]),
        ("0.5.2.tar.gz", "squashfuse 0.5.2", ["LICENSE"]),
    ]:
        if name not in pins:
            raise Failure(f"install-dependencies.sh no longer pins {name}")
        url = recipe_url(script, name)
        data = fetch(url, cache)
        require("sha256", data, pins[name], url)
        stored = "squashfuse-0.5.2.tar.gz" if name == "0.5.2.tar.gz" else name
        files[stored] = data
        manifest.append({
            "component": title,
            "source": url,
            "file": stored,
            "sha256": pins[name],
            "verification": "sha256 pinned by the runtime's install-dependencies.sh",
        })
        text = "\n\n".join(member(data, f).decode() for f in licences)
        if name.startswith("fuse-"):
            text = (
                "The AppImage runtime links the libfuse library statically, modified by "
                "patches/libfuse/mount.c.diff in the runtime source (it reads the fusermount "
                "program from $FUSERMOUNT_PROG). The library is licensed under the GNU Lesser "
                "General Public License 2.1; its source, the patch and build scripts are in "
                "rc2fx-appimage-runtime-sources.tar.gz, published with each RC2FX release.\n\n"
                + text
            )
        sections.append({"title": title, "text": text})

    for component, path in ALPINE_PACKAGES.items():
        commit = APORTS_COMMIT
        aport = fetch(APORTS_ARCHIVE.format(commit=commit, path=path), cache)
        recipe: dict[str, bytes] = {}
        recipe.update(aport_files(aport, path))
        if "APKBUILD" not in recipe:
            raise Failure(f"aports {commit} has no {path}/APKBUILD")
        apkbuild = recipe["APKBUILD"].decode()
        origin = path.split("/")[1]
        version = f"{apkbuild_field(apkbuild, 'pkgver')}-r{apkbuild_field(apkbuild, 'pkgrel')}"
        licence = apkbuild_field(apkbuild, "license")
        prefix = f"alpine/{origin}-{version}"
        for name, data in recipe.items():
            files[f"{prefix}/aports/{name}"] = data
        licence_text = None
        for sha512, name in parse_sums(apkbuild):
            if name in recipe:
                require("sha512", recipe[name], sha512, f"{path}/{name}")
                continue
            url = ALPINE_DISTFILES.format(branch=ALPINE_BRANCH, name=name)
            data = fetch(url, cache)
            require("sha512", data, sha512, url)
            files[f"{prefix}/{safe_relative(name)}"] = data
            manifest.append({
                "component": f"{component} (Alpine {origin} {version})",
                "source": url,
                "file": f"{prefix}/{name}",
                "sha512": sha512,
                "verification": (
                    f"sha512 from Alpine aports {commit} ({path}/APKBUILD); Alpine "
                    f"{ALPINE_BRANCH} at the runtime's build time, inferred, NOT verified to be "
                    "the revision linked into the runtime"
                ),
            })
            if licence_text is None and re.search(r"\.tar\.(gz|xz|bz2|zst)$", name):
                licence_text = "\n\n".join(
                    member(data, f).decode() for f in LICENCE_FILES[component]
                )
        if licence_text is None:
            raise Failure(f"no source archive with a licence for {component}")
        sections.append({
            "title": f"{component} {version.rsplit('-r', 1)[0]} (AppImage runtime)",
            "text": f"Licence: {licence}\n\n{licence_text}",
        })
    return files, manifest, sections


README = """# AppImage runtime sources

`rc2fx-linux-x64.AppImage` starts with the AppImage type 2 runtime
({repo}, commit {commit}). The runtime is a static executable. It links a
modified libfuse 3.15.0, which is licensed under the GNU Lesser General Public
License 2.1, together with squashfuse, zstd, zlib, musl and mimalloc. This
archive holds their source so that you can rebuild the runtime, with a
modified libfuse if you wish, and put it in front of RC2FX's application
image.

## Contents

- `type2-runtime-{commit}.tar`: the runtime source at that commit, including
  its build scripts (`scripts/`), its Alpine build container
  (`scripts/docker/Dockerfile`) and its libfuse patch
  (`patches/libfuse/mount.c.diff`).
- `fuse-3.15.0.tar.xz` and `squashfuse-0.5.2.tar.gz`: exactly the files the
  runtime's `scripts/common/install-dependencies.sh` downloads, matching the
  SHA-256 sums it pins.
- `alpine/`: the Alpine {alpine} build recipes (aports {aports}) and sources
  of zstd, zlib, musl and mimalloc, including Alpine's patches.
- `SOURCES.json`: every file's origin, checksum and how it was verified.
- `LICENSES.json`: the licence texts, as shown in RC2FX's licence notices.

## What is and isn't verified

The runtime, libfuse and squashfuse are pinned: the runtime by its git commit,
and libfuse and squashfuse by the checksums in the runtime's own recipe.

The runtime's Dockerfile installs zstd, zlib, musl and mimalloc from the
`alpine:{alpine}` image without pinning the image digest or the package
revisions, and the log of the build that produced the released runtime has
expired. The `alpine/` recipes are from aports commit {aports}, the last
{alpine}-stable commit before that build, and the sources are checked against
their SHA-512 sums. These revisions are inferred: they are not verified to be
the exact ones linked into the runtime in the AppImage, although zlib 1.3.1
matches the version string in the runtime binary. `SOURCES.json` says so for
each file. Whether the runtime links mimalloc's secure or insecure build is
not verified either.

Rebuilding is best effort: this archive is not claimed to reproduce a
byte-identical runtime. Rebuilding the runtime with a modified libfuse needs
only the runtime source and libfuse, which are pinned.

## Rebuilding the runtime

With Docker, from the extracted runtime source:

```sh
tar -xf type2-runtime-{commit}.tar
cd type2-runtime-{commit}
# To change libfuse, edit patches/libfuse/ or scripts/common/install-dependencies.sh.
scripts/docker/build-with-docker.sh
```

`BUILD.md` in the runtime source describes this and the other supported build
methods. The result is `out/runtime-x86_64`.

## Using a rebuilt runtime with RC2FX

An AppImage is the runtime followed by a SquashFS image of the application.
To combine your runtime with RC2FX's application image:

```sh
offset=$(./rc2fx-linux-x64.AppImage --appimage-offset)
tail -c +$((offset + 1)) rc2fx-linux-x64.AppImage > rc2fx.squashfs
cat out/runtime-x86_64 rc2fx.squashfs > rc2fx-relinked.AppImage
chmod +x rc2fx-relinked.AppImage
```
"""


def write_bundle(path: Path, files: dict[str, bytes]) -> None:
    """A gzip tarball with sorted entries and fixed metadata."""
    top = "rc2fx-appimage-runtime-sources"
    raw = io.BytesIO()
    with tarfile.open(fileobj=raw, mode="w", format=tarfile.PAX_FORMAT) as tar:
        for name in sorted(files):
            info = tarfile.TarInfo(f"{top}/{name}")
            info.size = len(files[name])
            info.mtime = SOURCE_DATE
            info.mode = 0o644
            info.uid = info.gid = 0
            info.uname = info.gname = ""
            tar.addfile(info, io.BytesIO(files[name]))
    path.parent.mkdir(parents=True, exist_ok=True)
    with open(path, "wb") as out, gzip.GzipFile(fileobj=out, mode="wb", mtime=SOURCE_DATE, filename="") as gz:
        gz.write(raw.getvalue())


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--cache", type=Path, help="reuse downloads across runs")
    args = parser.parse_args()
    try:
        files, manifest, sections = collect(args.cache)
    except (Failure, subprocess.CalledProcessError, tarfile.TarError, UnicodeDecodeError) as error:
        print(f"prepare-appimage-notices: {error}", file=sys.stderr)
        return 1
    notices = {"schemaVersion": 1, "runtimeSha256": RUNTIME_SHA256, "sections": sections}
    files["README.md"] = README.format(
        repo=RUNTIME_REPO, commit=RUNTIME_COMMIT, alpine=ALPINE_BRANCH[1:], aports=APORTS_COMMIT
    ).encode()
    files["SOURCES.json"] = (json.dumps(manifest, indent=2) + "\n").encode()
    files["LICENSES.json"] = (json.dumps(notices, indent=2, ensure_ascii=False) + "\n").encode()
    legal = args.root / "target" / "appimage-legal"
    legal.mkdir(parents=True, exist_ok=True)
    (legal / "notices.json").write_bytes(files["LICENSES.json"])
    bundle = args.root / "target" / "package" / "rc2fx-appimage-runtime-sources.tar.gz"
    write_bundle(bundle, files)
    print(f"wrote {legal / 'notices.json'} ({len(sections)} sections)")
    print(f"wrote {bundle} ({bundle.stat().st_size} bytes, sha256 {digest('sha256', bundle.read_bytes())})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
