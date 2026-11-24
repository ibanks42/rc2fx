"""Tests for retire_usb_node() and usb_quiesce() against fake /dev and /proc dirs.

  python3 mixer/scripts/tests/usb_restart.py

Overrides `test -c` for the fake HID node so the fixture works without mknod/root.
"""

import os
import shlex
import subprocess
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from _common import RC2FX_SH, source_rc2fx  # noqa: E402

clean_env = {k: v for k, v in os.environ.items() if not k.startswith("RC2FX_")}
defaults = subprocess.run(
    [
        "bash",
        "-c",
        f'. {shlex.quote(str(RC2FX_SH))}\nprintf "%s\\n%s\\n" "$hidg0" "$proc"',
    ],
    env=clean_env,
    capture_output=True,
    text=True,
    timeout=30,
)
assert defaults.returncode == 0 and defaults.stdout.splitlines() == [
    "/dev/hidg0",
    "/proc",
], defaults.stdout + defaults.stderr

with tempfile.TemporaryDirectory(prefix="rc2fx-usb-restart-") as directory:
    root = Path(directory)
    dev = root / "dev"
    proc = root / "proc"
    dev.mkdir()
    proc.mkdir()
    node = dev / "hidg0"
    env = {"RC2FX_HIDG0": str(node), "RC2FX_PROC": str(proc)}
    prefix = """
say() { printf '%s\\n' "$*"; }
test() {
  if [[ "${1:-}" == -c && "${2:-}" == "$hidg0" ]]; then
    builtin test -f "$2"
  else
    builtin test "$@"
  fi
}
"""

    def run(script, success=True):
        result = source_rc2fx(prefix + script, env=env)
        assert (result.returncode == 0) == success, result.stdout + result.stderr
        return result.stdout + result.stderr

    run("retire_usb_node 100")  # Already absent is harmless.
    node.write_text("fixture")
    fd = proc / "100" / "fd"
    fd.mkdir(parents=True)
    (fd / "8").symlink_to(node)
    assert "another process (100)" in run("retire_usb_node 200", success=False)
    assert node.exists()
    run('pid=999; retire_usb_node 100; test "$pid" = 999')
    assert not node.exists()  # Allowed owner; caller's PID variable is preserved.
    (fd / "8").unlink()
    if Path("/dev/full").exists():
        # Log write failure after unlinking /dev/hidg0 shouldn't fail the call.
        node.write_text("fixture")
        run("retire_usb_node > /dev/full")
        assert not node.exists()
        node.write_text("fixture")
        run("retire_usb_node > /dev/full || exit 9")
        assert not node.exists()
    other = dev / "other"
    other.write_text("untouched")
    node.symlink_to(other)
    assert "not the expected character device" in run(
        "retire_usb_node 100", success=False
    )
    assert other.read_text() == "untouched"
    node.unlink()
    node.mkdir()
    run("retire_usb_node 100", success=False)
    assert node.is_dir()
    node.rmdir()
    state = root / "run"
    state.mkdir()
    (state / "native.pid").write_text("101")
    env["RC2FX_RUN"] = str(state)
    prefix += "supervisor() { echo 200; }\npgrep() { echo 100; }\n"
    node.write_text("fixture")
    (fd / "8").symlink_to(node)
    run(f"sleep() {{ rm -f {fd}/8; }}; usb_quiesce")
    assert not node.exists() and not (fd / "8").is_symlink()
    node.write_text("fixture")
    (fd / "8").symlink_to(node)
    assert "controller still holds" in run("sleep() { :; }; usb_quiesce", success=False)
    assert not node.exists() and (fd / "8").is_symlink()
print(
    "USB restart guard: absent, foreign owner, allowed owner, PID scope, symlink and wrong type passed"
)
print(
    "USB mode guard: waits for writer close and refuses teardown while writer remains"
)
