#!/usr/bin/env python3
"""Collect license notices and corresponding source for the AppImage type 2 runtime.

Velopack's vpk prepends its vendored AppImage type 2 runtime
(vendor/appimagekit-runtime-x86_64) to the Linux AppImage. That static binary
links a patched libfuse (LGPL-2.1) alongside squashfuse, zstd, zlib, musl, and
mimalloc. This script verifies the vendored runtime against its pinned SHA-256,
fetches the matching runtime commit plus libfuse, squashfuse, and Alpine 3.21
aports sources/recipes (checked against their pinned hashes), writes
target/appimage-legal/notices.json, and bundles the source archive in
target/package.

Note: the vendored runtime is a continuous build of type2-runtime commit
3d17002 (differs from 20251108 only in CI workflows). Because its Dockerfile
installs Alpine packages without pinning revisions and the build log expired,
the aports commit here is the last 3.21-stable commit before that build
(recorded as inferred; zlib 1.3.1 and zstd 1.5.6 match strings in the binary).

Usage: prepare-appimage-notices.py [--root DIR] [--cache DIR] [--vpk-package FILE]
"""

from __future__ import annotations

import argparse
import gzip
import hashlib
import io
import json
import re
import subprocess
import sys
import tarfile
import tempfile
import time
import zipfile
import urllib.request
from pathlib import Path

RUNTIME_REPO = "https://github.com/AppImage/type2-runtime"
# Commit reported by `--appimage-version` on the vendored runtime.
RUNTIME_COMMIT = "3d17002ee2a519a288e28aeb7bc41af5f5adaf4e"
RUNTIME_SHA256 = "a2419dce47568395ae79c01ffa9a5a341dd339581352ff104d073527543177e5"
# Pinned vpk version/hash shared with go -C tools/xtask run . fetch-vpk via config/velopack.json.
_VELOPACK_CONFIG = json.loads(
    (Path(__file__).resolve().parents[2] / "config" / "velopack.json").read_text()
)
VPK_VERSION = _VELOPACK_CONFIG["version"]
VPK_SHA256 = _VELOPACK_CONFIG["vpkSha256"]
VPK_PACKAGE = "https://api.nuget.org/v3-flatcontainer/vpk/{version}/vpk.{version}.nupkg"
VPK_RUNTIME = "vendor/appimagekit-runtime-x86_64"
ALPINE_BRANCH = "v3.21"
# Last aports 3.21-stable commit before the 2026-03-07 runtime build (inferred;
# the four recipes below have been unchanged since Feb 2025).
APORTS_COMMIT = "a1652b510b484befbf2e3f613dca969ba2abbdb7"
ALPINE_DISTFILES = "https://distfiles.alpinelinux.org/distfiles/{branch}/{name}"
APORTS_REPO = "https://github.com/alpinelinux/aports"

# aports paths for the static libraries linked in src/runtime/Makefile LIBS.
# mimalloc-dev comes from community/mimalloc2.
ALPINE_PACKAGES = {
    "zstd": "main/zstd",
    "zlib": "main/zlib",
    "musl": "main/musl",
    "mimalloc": "community/mimalloc2",
}
# License files to extract from each verified source tarball, in output order.
LICENSE_FILES = {
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
    """Validate that an archive path stays within the bundle."""
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
    """Read a simple `name=value` or `name="value"` field from an APKBUILD."""
    match = re.search(rf'^{name}="?([^"\n]*)"?$', apkbuild, re.M)
    if not match:
        raise Failure(f"APKBUILD has no {name}")
    return match.group(1)


def parse_sums(apkbuild: str) -> list[tuple[str, str]]:
    """Parse (sha512, filename) pairs from an APKBUILD sha512sums block."""
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
    """Extract `sha256  filename` pins from install-dependencies.sh."""
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
    """Read the unique top-level file in a tarball ending with `suffix`."""
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
    """Extract regular files under `path` from an aports tarball; reject links."""
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


def git_source(repo: str, commit: str, prefix: str, paths: tuple[str, ...] = ()) -> bytes:
    """Fetch `commit` from `repo` and return its `git archive` tarball.

    Uncached so git verifies the commit object on every run.
    """
    with tempfile.TemporaryDirectory() as work:
        git = ["git", "-C", work, "-c", "advice.detachedHead=false"]
        subprocess.run(["git", "init", "-q", work], check=True)
        subprocess.run(git + ["fetch", "-q", "--depth", "1", repo, commit], check=True)
        head = subprocess.run(
            git + ["rev-parse", "FETCH_HEAD"], check=True, capture_output=True, text=True
        ).stdout.strip()
        if head != commit:
            raise Failure(f"fetched commit {head}, expected {commit}")
        data = subprocess.run(
            git + ["archive", "--format=tar", f"--prefix={prefix}/", head, *paths],
            check=True,
            capture_output=True,
        ).stdout
    return data


def vendored_runtime(vpk_package: Path | None, cache: Path | None) -> bytes:
    """Extract and verify the AppImage runtime binary from the pinned vpk package."""
    url = VPK_PACKAGE.format(version=VPK_VERSION)
    package = vpk_package.read_bytes() if vpk_package else fetch(url, cache)
    require("sha256", package, VPK_SHA256, str(vpk_package or url))
    try:
        with zipfile.ZipFile(io.BytesIO(package)) as nupkg:
            binary = nupkg.read(VPK_RUNTIME)
    except (zipfile.BadZipFile, KeyError) as error:
        raise Failure(f"vpk {VPK_VERSION} has no {VPK_RUNTIME}: {error}") from None
    require("sha256", binary, RUNTIME_SHA256, f"vpk {VPK_VERSION} {VPK_RUNTIME}")
    check_runtime_commit(binary)
    return binary


def check_runtime_commit(binary: bytes) -> None:
    """Verify the runtime binary's embedded version string matches RUNTIME_COMMIT."""
    found = set(re.findall(rb"github\.com/AppImage/type2-runtime/commit/([0-9a-f]{7,40})", binary))
    if len(found) != 1 or not RUNTIME_COMMIT.startswith(found.pop().decode()):
        raise Failure(f"the vendored runtime does not report commit {RUNTIME_COMMIT}")


def collect(
    cache: Path | None, vpk_package: Path | None = None
) -> tuple[dict[str, bytes], list[dict], list[dict], int]:
    """Return (bundle files, source manifest, notice sections, runtime size)."""
    files: dict[str, bytes] = {}
    manifest: list[dict] = []
    sections: list[dict] = []

    binary = vendored_runtime(vpk_package, cache)
    runtime = git_source(RUNTIME_REPO, RUNTIME_COMMIT, f"type2-runtime-{RUNTIME_COMMIT}")
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
            f"git commit id, the one the runtime binary reports; vpk {VPK_VERSION} vendors that "
            f"binary as {VPK_RUNTIME}, sha256 {RUNTIME_SHA256}, in its package with sha256 "
            f"{VPK_SHA256}"
        ),
    })
    sections.append({
        "title": "AppImage type 2 runtime",
        "text": member(runtime, "LICENSE").decode(),
    })

    pins = recipe_pins(script)
    for name, title, licenses in [
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
        text = "\n\n".join(member(data, f).decode() for f in licenses)
        if name.startswith("fuse-"):
            text = (
                "The AppImage runtime links the libfuse library statically, modified by "
                "patches/libfuse/mount.c.diff in the runtime source (it reads the fusermount "
                "program from $FUSERMOUNT_PROG). The library is licensed under the GNU Lesser "
                "General Public License 2.1; its source, the patch and build scripts are in "
                "the AppImage runtime source archive published with each RC2FX release.\n\n"
                + text
            )
        sections.append({"title": title, "text": text})

    aport = git_source(
        APORTS_REPO, APORTS_COMMIT, f"aports-{APORTS_COMMIT}", tuple(ALPINE_PACKAGES.values())
    )
    for component, path in ALPINE_PACKAGES.items():
        commit = APORTS_COMMIT
        recipe: dict[str, bytes] = {}
        recipe.update(aport_files(aport, path))
        if "APKBUILD" not in recipe:
            raise Failure(f"aports {commit} has no {path}/APKBUILD")
        apkbuild = recipe["APKBUILD"].decode()
        origin = path.split("/")[1]
        version = f"{apkbuild_field(apkbuild, 'pkgver')}-r{apkbuild_field(apkbuild, 'pkgrel')}"
        license = apkbuild_field(apkbuild, "license")
        prefix = f"alpine/{origin}-{version}"
        for name, data in recipe.items():
            files[f"{prefix}/aports/{name}"] = data
        license_text = None
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
            if license_text is None and re.search(r"\.tar\.(gz|xz|bz2|zst)$", name):
                license_text = "\n\n".join(
                    member(data, f).decode() for f in LICENSE_FILES[component]
                )
        if license_text is None:
            raise Failure(f"no source archive with a license for {component}")
        sections.append({
            "title": f"{component} {version.rsplit('-r', 1)[0]} (AppImage runtime)",
            "text": f"License: {license}\n\n{license_text}",
        })
    return files, manifest, sections, len(binary)


README = """# AppImage runtime sources

The RC2FX Linux AppImage is made by Velopack's vpk {vpk}, which starts it
with the AppImage type 2 runtime it vendors ({repo}, commit {commit}). The
runtime is a static executable. It links a modified libfuse 3.15.0, which is
licensed under the GNU Lesser General Public License 2.1, together with
squashfuse, zstd, zlib, musl and mimalloc. This archive holds their source so
that you can rebuild the runtime, with a modified libfuse if you wish, and put
it in front of RC2FX's application image.

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
- `LICENSES.json`: the license texts, as shown in RC2FX's license notices.

## What is and isn't verified

The runtime, libfuse and squashfuse are pinned: the runtime by its git commit,
which the runtime binary reports and which is checked against the binary in
vpk's package (SHA-256 {runtime_sha}), and libfuse and squashfuse by the
checksums in the runtime's own recipe. The binary is a continuous build of
that commit; that commit changes only CI files since the 20251108 release.

The runtime's Dockerfile installs zstd, zlib, musl and mimalloc from the
`alpine:{alpine}` image without pinning the image digest or the package
revisions, and the log of the build that produced the vendored runtime has
expired. The `alpine/` recipes are from aports commit {aports}, the last
{alpine}-stable commit before that build, and the sources are checked against
their SHA-512 sums. These revisions are inferred: they are not verified to be
the exact ones linked into the runtime in the AppImage, although zlib 1.3.1
and zstd 1.5.6 match the version strings in the runtime binary.
`SOURCES.json` says so for each file. Whether the runtime links mimalloc's
secure or insecure build is not verified either.

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
appimage=./rc2fx-linux-x64.AppImage   # the RC2FX AppImage you downloaded
offset=$("$appimage" --appimage-offset)
tail -c +$((offset + 1)) "$appimage" > rc2fx.squashfs
cat out/runtime-x86_64 rc2fx.squashfs > rc2fx-relinked.AppImage
chmod +x rc2fx-relinked.AppImage
```

RC2FX's in-app updates replace the whole AppImage with the next official one,
runtime included. Turn updates off, or relink each new version, to keep your
runtime.
"""


def write_bundle(path: Path, files: dict[str, bytes]) -> None:
    """Write a deterministic gzip tarball with sorted entries and fixed metadata."""
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
    parser.add_argument(
        "--vpk-package", type=Path, help="the vpk NuGet package already downloaded by go -C tools/xtask run . fetch-vpk"
    )
    args = parser.parse_args()
    try:
        files, manifest, sections, size = collect(args.cache, args.vpk_package)
    except (
        Failure, OSError, subprocess.CalledProcessError, tarfile.TarError, UnicodeDecodeError
    ) as error:
        print(f"prepare-appimage-notices: {error}", file=sys.stderr)
        return 1
    # Packaging checks that the AppImage starts with exactly this runtime.
    notices = {
        "schemaVersion": 2,
        "runtimeSha256": RUNTIME_SHA256,
        "runtimeSize": size,
        "runtimeCommit": RUNTIME_COMMIT,
        "sections": sections,
    }
    files["README.md"] = README.format(
        repo=RUNTIME_REPO, commit=RUNTIME_COMMIT, alpine=ALPINE_BRANCH[1:], aports=APORTS_COMMIT,
        vpk=VPK_VERSION, runtime_sha=RUNTIME_SHA256,
    ).encode()
    files["SOURCES.json"] = (json.dumps(manifest, indent=2) + "\n").encode()
    files["LICENSES.json"] = (json.dumps(notices, indent=2, ensure_ascii=False) + "\n").encode()
    legal = args.root / "target" / "appimage-legal"
    legal.mkdir(parents=True, exist_ok=True)
    (legal / "notices.json").write_bytes(files["LICENSES.json"])
    version = json.loads((args.root / "config" / "app-release.json").read_text())["version"]
    bundle = args.root / "target" / "package" / f"rc2fx-{version}-appimage-runtime-sources.tar.gz"
    write_bundle(bundle, files)
    print(f"wrote {legal / 'notices.json'} ({len(sections)} sections)")
    print(f"wrote {bundle} ({bundle.stat().st_size} bytes, sha256 {digest('sha256', bundle.read_bytes())})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
