"""Exercise the actual retirement function against an isolated proc/dev fixture.

Run on Linux: python3 xtask/mixer/tests/usb_restart.py
No real device, process, or mixer is opened or modified. Only the character-node
predicate is replaced with a regular-file predicate for unprivileged fixtures.
"""
from pathlib import Path
import subprocess
import tempfile

source = (Path(__file__).parents[1] / "rc2fx.sh").read_text()
function = source.split("retire_usb_node() {", 1)[1].split("\n}\n", 1)[0]
with tempfile.TemporaryDirectory(prefix="rc2fx-usb-restart-") as directory:
    root = Path(directory)
    dev = root / "dev"
    proc = root / "proc"
    dev.mkdir()
    proc.mkdir()
    node = dev / "hidg0"
    function = "retire_usb_node() {" + function + "\n}\n"
    function = function.replace("/dev/hidg0", str(node)).replace("/proc/", str(proc) + "/")
    function = function.replace('test -c ', 'test -f ')
    prefix = "set -euo pipefail\nsay() { printf '%s\\n' \"$*\"; }\n" + function

    def run(script, success=True):
        result = subprocess.run(["bash", "-c", prefix + script], capture_output=True, text=True)
        assert (result.returncode == 0) == success, result.stdout + result.stderr
        return result.stdout + result.stderr

    run("retire_usb_node 100")  # Already absent is harmless.
    node.write_text("fixture")
    fd = proc / "100" / "fd"
    fd.mkdir(parents=True)
    (fd / "8").symlink_to(node)
    assert "another process (100)" in run("retire_usb_node 200", success=False)
    assert node.exists()
    run("pid=999; retire_usb_node 100; test \"$pid\" = 999")
    assert not node.exists()  # Allowed owner; caller's PID variable is preserved.
    (fd / "8").unlink()
    other = dev / "other"
    other.write_text("untouched")
    node.symlink_to(other)
    assert "not the expected character device" in run("retire_usb_node 100", success=False)
    assert other.read_text() == "untouched"
    node.unlink()
    node.mkdir()
    run("retire_usb_node 100", success=False)
    assert node.is_dir()
    node.rmdir()
    quiesce = 'usb_quiesce() {' + source.split('usb_quiesce() {', 1)[1].split('\n}\n', 1)[0] + '\n}\n'
    quiesce = quiesce.replace('/dev/hidg0', str(node)).replace('/proc/', str(proc) + '/')
    state = root / 'run'
    state.mkdir()
    (state / 'native.pid').write_text('101')
    prefix += f'run={state}\nsupervisor() {{ echo 200; }}\npgrep() {{ echo 100; }}\n' + quiesce
    node.write_text('fixture')
    (fd / '8').symlink_to(node)
    run(f'sleep() {{ rm -f {fd}/8; }}; usb_quiesce')
    assert not node.exists() and not (fd / '8').is_symlink()
    node.write_text('fixture')
    (fd / '8').symlink_to(node)
    assert 'controller still holds' in run('sleep() { :; }; usb_quiesce', success=False)
    assert not node.exists() and (fd / '8').is_symlink()
print("USB restart guard: absent, foreign owner, allowed owner, PID scope, symlink and wrong type passed")
print('USB mode guard: waits for writer close and refuses teardown while writer remains')
