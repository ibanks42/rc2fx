#!/usr/bin/env python3
"""Write the licence report for Velopack's bundled programs.

The Windows and macOS packages include Velopack's launcher, updater and
installer, and the AppImage its updater (UpdateNix), all built by Velopack
from its own Rust workspace. Their dependencies' notices must ship with RC2FX
like the app's own, so this runs cargo-about on that workspace at the pinned
tag, for the targets Velopack builds them for (Windows, macOS, and the static
musl Linux build UpdateNix comes from), under the licences RC2FX's about.toml
accepts, and keeps only what the licence document uses: each licence's name,
text and the crates using it.

  velopack-licenses.py VERSION
      Writes licenses/velopack-VERSION.json. Needs git, cargo and
      cargo-about. Run it whenever VPK_VERSION in
      xtask/src/package/velopack.rs changes.
"""

import json
import subprocess
import sys
import tempfile
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


def main():
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    version = sys.argv[1]
    accepted = tomllib.loads((ROOT / "about.toml").read_text())["accepted"]
    with tempfile.TemporaryDirectory() as work:
        source = Path(work) / "velopack"
        subprocess.run(
            ["git", "clone", "--quiet", "--depth", "1", "--branch", version,
             "https://github.com/velopack/velopack", str(source)],
            check=True,
        )
        config = Path(work) / "about.toml"
        config.write_text(
            # Velopack's CI builds UpdateNix for musl, not glibc.
            'targets = ["x86_64-pc-windows-msvc", "aarch64-apple-darwin", "x86_64-unknown-linux-musl"]\n'
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
        sys.exit("cargo-about found no licences")
    out = ROOT / "licenses" / f"velopack-{version}.json"
    out.write_text(json.dumps({"velopack": version, "licenses": licenses}, indent=1) + "\n")
    print(f"wrote {out.relative_to(ROOT)}: {len(licenses)} licence texts")


if __name__ == "__main__":
    main()
