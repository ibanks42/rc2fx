#!/usr/bin/env python3
"""Generate the license report for Velopack's bundled helper binaries.

Runs cargo-about against Velopack's Rust workspace at the pinned tag for the
shipped targets (Windows, macOS arm64/x86_64, and static Linux musl for
UpdateNix) and writes `licenses/velopack-VERSION.json`.

Usage: velopack-licenses.py VERSION

Requires git, cargo, and cargo-about. Re-run whenever VPKVersion in
desktop/internal/xtask/licenses/licenses.go (plus setup-vpk.sh and
desktop/scripts/fetch-velopack.sh) is bumped.
"""

import json
import subprocess
import sys
import tempfile
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]

# Extra licenses allowed for Velopack's Rust deps beyond root about.toml.
VELOPACK_ACCEPTED = [
    "MPL-2.0",
    # webpki-roots (Mozilla root certs used by Velopack's updater).
    "CDLA-Permissive-2.0",
]


def main():
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    version = sys.argv[1]
    accepted = tomllib.loads((ROOT / "about.toml").read_text())["accepted"]
    accepted += [l for l in VELOPACK_ACCEPTED if l not in accepted]
    with tempfile.TemporaryDirectory() as work:
        source = Path(work) / "velopack"
        subprocess.run(
            ["git", "clone", "--quiet", "--depth", "1", "--branch", version,
             "https://github.com/velopack/velopack", str(source)],
            check=True,
        )
        config = Path(work) / "about.toml"
        config.write_text(
            # UpdateNix is built against musl; macOS ships universal (arm64 + x86_64).
            'targets = ["x86_64-pc-windows-msvc", "aarch64-apple-darwin", "x86_64-apple-darwin", '
            '"x86_64-unknown-linux-musl"]\n'
            "ignore-build-dependencies = true\n"
            "ignore-dev-dependencies = true\n"
            f"accepted = {json.dumps(accepted)}\n"
            'workarounds = ["ring"]\n'
        )
        report = json.loads(subprocess.run(
            ["cargo", "about", "generate", "--locked", "--format", "json",
             "--config", str(config), "--manifest-path", str(source / "src/bins/Cargo.toml"),
             "--features", "windows"],
            check=True, capture_output=True, text=True,
        ).stdout)
    licenses = [
        {
            "name": license["name"],
            "text": license["text"],
            "used_by": sorted(
                ({"crate": {"name": u["crate"]["name"], "version": u["crate"]["version"]}}
                 for u in license["used_by"]),
                key=lambda u: (u["crate"]["name"], u["crate"]["version"]),
            ),
        }
        for license in report["licenses"]
    ]
    licenses.sort(key=lambda l: (l["name"], l["text"]))
    if not licenses:
        sys.exit("cargo-about found no licenses")
    out = ROOT / "licenses" / f"velopack-{version}.json"
    out.write_text(json.dumps({"velopack": version, "licenses": licenses}, indent=1) + "\n")
    print(f"wrote {out.relative_to(ROOT)}: {len(licenses)} license texts")


if __name__ == "__main__":
    main()
