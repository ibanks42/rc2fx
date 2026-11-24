"""Shared helpers for host tests of mixer/scripts/rc2fx.sh and its companions."""

import hashlib
import os
import shlex
import subprocess
import tempfile
import unittest
from pathlib import Path

SCRIPTS_DIR = Path(__file__).resolve().parents[1]
RC2FX_SH = SCRIPTS_DIR / "rc2fx.sh"


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def write_executable(
    path: Path, body: str, *, shebang: str = "#!/bin/bash", mode: int = 0o755
) -> Path:
    if not body.endswith("\n"):
        body += "\n"
    path.write_text(f"{shebang}\n{body}")
    path.chmod(mode)
    return path


def write_fake_bin(
    bin_dir: Path,
    stubs: dict[str, str],
    *,
    shebang: str = "#!/bin/bash",
    mode: int = 0o755,
) -> Path:
    bin_dir.mkdir(parents=True, exist_ok=True)
    for name, body in stubs.items():
        write_executable(bin_dir / name, body, shebang=shebang, mode=mode)
    return bin_dir


def source_rc2fx(
    body: str,
    *,
    env: dict[str, str] | None = None,
    cwd: Path | None = None,
    strict: bool = True,
    timeout: int = 30,
) -> subprocess.CompletedProcess:
    """Source the real rc2fx.sh in bash, then run `body`."""
    prelude = f". {shlex.quote(str(RC2FX_SH))}\n"
    if not strict:
        prelude += "set +e\n"
    full_env = dict(os.environ, **(env or {}))
    return subprocess.run(
        ["bash", "-c", prelude + body],
        capture_output=True,
        text=True,
        cwd=cwd,
        env=full_env,
        timeout=timeout,
    )


def run_rc2fx(
    *args: str,
    env: dict[str, str] | None = None,
    stdin: str | bytes | None = None,
    cwd: Path | None = None,
    timeout: int = 30,
) -> subprocess.CompletedProcess:
    """Execute the real rc2fx.sh directly with the given subcommand arguments."""
    full_env = dict(os.environ, **(env or {}))
    return subprocess.run(
        ["/bin/bash", str(RC2FX_SH), *args],
        input=stdin,
        capture_output=True,
        text=not isinstance(stdin, bytes),
        cwd=cwd,
        env=full_env,
        timeout=timeout,
    )


def function_body(name: str, text: str | None = None) -> str:
    """Extract a top-level shell function's text from rc2fx.sh for static checks."""
    source = RC2FX_SH.read_text() if text is None else text
    start = source.index(f"\n{name}() {{\n") + 1
    return source[start : source.index("\n}\n", start) + 3]


class TempDirTestCase(unittest.TestCase):
    """TestCase base with a per-test resolved temporary directory at self.root."""

    def setUp(self):
        super().setUp()
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name).resolve()
