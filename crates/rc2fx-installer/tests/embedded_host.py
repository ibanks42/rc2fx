#!/usr/bin/env python3
"""Host qualification of the installer payload the desktop app embeds.

Builds the installer crate's `embedded-probe` example, copies the executable
into an empty folder with no runtime, catalog or scripts beside it, and checks
that it installs from its own bytes only:

* its identity is exactly the payload this checkout would embed;
* a wrong payload sitting beside it changes nothing;
* it verifies a work folder's prepared firmware against that identity, and
  refuses a tampered archive, a foreign runtime and a missing record.

No window, no USB, no network, no mixer, no app build.
"""

import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

PAYLOAD_NAMES = [
    "librc2fx_native.so",
    "rc2fxd",
    "rc2fx-firmware",
    "firmware-profiles.json",
    "librc2fx_gui.so",
    "librc2fx_jackerr.so",
    "rc2fx_jackerr_snapshot",
    "mixer-release.json",
    "SHA256SUMS",
    "rc2fx.sh",
    "rc2fx-shim.sh",
    "rc2fx-gui-shim.sh",
    "install.sh",
]
PINNED = PAYLOAD_NAMES[:8]
SCRIPTS = {"rc2fx.sh", "rc2fx-shim.sh", "rc2fx-gui-shim.sh", "install.sh"}
PRO_II = "rodecaster-pro-ii"


def sha(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def repo() -> Path:
    """The workspace root, from this file: crates/rc2fx-installer/tests/."""
    root = Path(__file__).resolve().parents[3]
    assert (root / "Cargo.toml").is_file() and (root / "crates").is_dir(), root
    return root


def payload(root: Path) -> dict[str, bytes]:
    """The payload these sources embed, read as the crate reads it."""
    source = root / "runtime"
    if not source.is_dir():
        source = root / "target" / "mixer"
    files = {}
    for name in PAYLOAD_NAMES:
        if name in SCRIPTS:
            data = (root / "xtask" / "mixer" / name).read_bytes()
            data = data.replace(b"\r\n", b"\n")
        else:
            data = (source / name).read_bytes()
        files[name] = data
    return files


def expected_identity(files: dict[str, bytes]) -> dict:
    # The runtime digest covers every payload file, as the image build does.
    manifest = "".join(f"{sha(files[name])}  {name}\n" for name in PAYLOAD_NAMES)
    return {
        "kind": "embedded",
        "build": "development",
        "catalog": sha(files["firmware-profiles.json"]),
        "release": json.loads(files["mixer-release.json"])["version"],
        "runtime": sha(manifest.encode()),
        "files": [
            {"name": name, "sha256": sha(files[name]), "size": len(files[name])}
            for name in PAYLOAD_NAMES
        ],
    }


def build_probe(root: Path) -> Path:
    subprocess.run(
        [
            "cargo",
            "build",
            "--locked",
            "--example",
            "embedded-probe",
            "-p",
            "rc2fx-installer",
            "--features",
            "embedded",
        ],
        cwd=root,
        check=True,
    )
    probe = root / "target" / "debug" / "examples" / "embedded-probe"
    assert probe.is_file(), probe
    return probe


def run(exe: Path, args: list[str], cwd: Path) -> subprocess.CompletedProcess:
    env = {"PATH": os.environ.get("PATH", "/usr/bin:/bin"), "HOME": str(cwd)}
    return subprocess.run(
        [str(exe), *args], cwd=cwd, env=env, capture_output=True, text=True
    )


class Checks:
    def __init__(self) -> None:
        self.failures: list[str] = []

    def ok(self, condition: bool, what: str, detail: str = "") -> None:
        if condition:
            print(f"PASS: {what}")
        else:
            print(f"FAIL: {what}{': ' + detail if detail else ''}")
            self.failures.append(what)

    def done(self) -> int:
        if self.failures:
            print(f"\n{len(self.failures)} check(s) failed")
            return 1
        print("\nall embedded-payload checks passed")
        return 0


def main() -> int:
    root = repo()
    files = payload(root)
    want = expected_identity(files)
    probe = build_probe(root)
    checks = Checks()

    with tempfile.TemporaryDirectory() as tmp:
        tmp = Path(tmp)
        # 1. An empty folder: the executable and nothing else.
        empty = tmp / "empty"
        empty.mkdir()
        alone = empty / probe.name
        shutil.copy2(probe, alone)
        out = run(alone, ["identity"], empty)
        checks.ok(out.returncode == 0, "identity runs with nothing beside it", out.stderr.strip())
        report = json.loads(out.stdout) if out.returncode == 0 else {}
        identity = report.get("identity", {})
        checks.ok(
            report.get("envelope", {}).get("valid") == 1,
            "the executable carries exactly one complete envelope",
            json.dumps(report.get("envelope")),
        )
        for key in ("kind", "build", "catalog", "runtime", "files"):
            checks.ok(
                identity.get(key) == want[key],
                f"embedded {key} is this checkout's",
                f"got {identity.get(key)!r}",
            )
        profiles = identity.get("profiles", [])
        checks.ok(bool(profiles), "the embedded catalog pins firmware profiles")
        pro_ii = [
            p for p in profiles if p.get("supported") and p.get("model") == PRO_II
        ]
        checks.ok(bool(pro_ii), "a supported Pro II profile is embedded")

        # 2. Beside a deliberately wrong payload: still the embedded bytes.
        wrong = tmp / "wrong"
        for name in PAYLOAD_NAMES:
            path = (
                wrong / "xtask" / "mixer" / name
                if name in SCRIPTS
                else wrong / "runtime" / name
            )
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(b"not the payload")
        (wrong / "config").mkdir()
        (wrong / "config" / "firmware-profiles.json").write_bytes(
            b'{"schemaVersion":1,"profiles":[]}'
        )
        beside = wrong / probe.name
        shutil.copy2(probe, beside)
        other = run(beside, ["identity"], wrong)
        checks.ok(
            other.returncode == 0 and other.stdout == out.stdout,
            "a wrong payload beside it changes nothing",
            other.stderr.strip(),
        )

        # 3. Prepared firmware in a work folder, judged by the embedded bytes.
        work = tmp / "work"
        work.mkdir()
        archive = b"stand-in prepared firmware\n"
        md5 = hashlib.md5(archive).hexdigest()
        prepared = {
            "version": pro_ii[0]["version"],
            "channel": "beta",
            "profile": pro_ii[0]["id"],
            "archive_sha": sha(archive),
            "archive_md5": md5,
            "runtime": identity["runtime"],
            "stock": False,
        }

        def write_work() -> None:
            (work / "archive.tar.gz").write_bytes(archive)
            (work / "archive.md5").write_text(md5)
            (work / "installer.json").write_text(json.dumps({"prepared": prepared}))

        write_work()
        good = run(alone, ["verify", str(work)], empty)
        checks.ok(
            good.returncode == 0,
            "a prepared firmware matching the embedded runtime verifies",
            good.stderr.strip(),
        )

        (work / "archive.tar.gz").write_bytes(archive + b"tampered")
        bad = run(alone, ["verify", str(work)], empty)
        checks.ok(
            bad.returncode != 0 and "does not match the prepared firmware" in bad.stderr,
            "a tampered archive fails closed",
            bad.stderr.strip(),
        )

        prepared["runtime"] = "0" * 64
        write_work()
        foreign = run(alone, ["verify", str(work)], empty)
        checks.ok(
            foreign.returncode != 0 and "different RC2FX build" in foreign.stderr,
            "firmware prepared by another build fails closed",
            foreign.stderr.strip(),
        )

        (work / "installer.json").unlink()
        absent = run(alone, ["verify", str(work)], empty)
        checks.ok(
            absent.returncode != 0 and "no prepared firmware" in absent.stderr,
            "a work folder with no prepared firmware fails closed",
            absent.stderr.strip(),
        )

        # 4. A negative control: two copies of the payload are ambiguous.
        doubled = run(alone, ["duplicate"], empty)
        checks.ok(
            doubled.returncode == 0 and "2 installer payloads" in doubled.stdout,
            "a payload embedded twice is refused, not guessed",
            (doubled.stdout + doubled.stderr).strip(),
        )

        # 5. Nothing was written beside the executable.
        checks.ok(
            sorted(p.name for p in empty.iterdir()) == [probe.name],
            "the executable created no files beside itself",
            ", ".join(sorted(p.name for p in empty.iterdir())),
        )

    return checks.done()


if __name__ == "__main__":
    sys.exit(main())
