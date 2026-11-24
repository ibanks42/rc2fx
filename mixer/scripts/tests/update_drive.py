"""Tests for `rc2fx.sh update-drive` using a fake configfs USB gadget.

Run directly with:
  python3 mixer/scripts/tests/update_drive.py

Stubs for mkfs.vfat, mount, and umount are placed first on PATH: the virtual
disk is backed by a sibling directory that the mount stub copies into the mount
point.
"""

import os
import shlex
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from _common import source_rc2fx, write_fake_bin  # noqa: E402

# Confirm update_dir is fixed to /run/rc2fx-update (the shims read it literally).
default_check = source_rc2fx(
    'printf "%s\\n" "$update_dir"',
    env={"RC2FX_UPDATE_DIR": "/ignored/override"},
)
assert (
    default_check.returncode == 0 and default_check.stdout == "/run/rc2fx-update\n"
), default_check.stdout + default_check.stderr

with tempfile.TemporaryDirectory(prefix="rc2fx-update-drive-") as directory:
    root = Path(directory)
    gadget = root / "gadget" / "g1"
    lun = gadget / "functions" / "mass_storage.usb0" / "lun.0"
    lun.mkdir(parents=True)
    (gadget / "UDC").write_text("ff400000.usb\n")
    (lun / "file").write_text("\n")
    update = root / "run-update"
    session = update / "0123456789abcdef"
    session.mkdir(parents=True)
    image = session / "drive.img"
    proc = root / "proc"
    proc.mkdir()
    mounts = proc / "mounts"
    mounts.write_text("")
    bin_dir = write_fake_bin(
        root / "bin",
        {
            "mkfs.vfat": 'set -eu\ntest "$1" = -n && test "$2" = RC2FX && test -f "$3"; mkdir -p "$3.disk"',
            "mount": f'set -eu\nimg=${{@: -2:1}}; mnt=${{@: -1}}; cp -r "$img.disk/." "$mnt"; echo "x $mnt x" >> {mounts}',
            "umount": f'set -eu\nrm -rf "$1"/*; : > {mounts}',
        },
    )
    env = {
        "PATH": f"{bin_dir}:{os.environ['PATH']}",
        "RC2FX_GADGET_ROOT": str(root / "gadget"),
        "RC2FX_PROC": str(proc),
        "RC2FX_HOME": str(root),
        "RC2FX_LOGS": str(root / "logs"),
        "RC2FX_DATA": str(root / "data"),
    }

    def run(*args, success=True):
        body = (
            f"update_dir={shlex.quote(str(update))}\nupdate_drive {shlex.join(args)}\n"
        )
        result = source_rc2fx(body, env=env)
        assert (result.returncode == 0) == success, (
            args,
            result.stdout + result.stderr,
        )
        return result.stdout + result.stderr

    def attached():
        return (lun / "file").read_text().strip()

    # Reject disk paths outside the update session directory.
    assert "not an update disk path" in run(
        "attach", str(root / "elsewhere.img"), "8388608", success=False
    )
    assert "not an update disk path" in run(
        "attach", f"{update}/../x/drive.img", "8388608", success=False
    )
    # If RØDE is already using the LUN or the gadget isn't bound, don't create an image.
    (lun / "file").write_text("/mnt/sd.img\n")
    assert "in use" in run("attach", str(image), "8388608", success=False)
    assert not image.exists()
    (lun / "file").write_text("\n")
    (gadget / "UDC").write_text("\n")
    assert "not bound" in run("attach", str(image), "8388608", success=False)
    (gadget / "UDC").write_text("ff400000.usb\n")

    # Normal flow: attach the drive, write the payload, eject (clearing the LUN), and take it.
    run("attach", str(image), "8388608")
    assert image.stat().st_size == 8388608
    assert attached() == str(image)
    (Path(f"{image}.disk") / "RC2FX.UPD").write_bytes(b"payload!")
    (lun / "file").write_text("\n")
    dest = session / "payload"
    run("take", str(image), str(dest), "8")
    assert dest.read_bytes() == b"payload!"
    assert not image.exists()
    assert not (session / "drive").exists()

    # If the host hasn't ejected yet, take should detach the LUN before reading the image.
    run("attach", str(image), "8388608")
    (Path(f"{image}.disk") / "RC2FX.UPD").write_bytes(b"payload!")
    dest.unlink()
    run("take", str(image), str(dest), "8")
    assert attached() == "" and dest.read_bytes() == b"payload!"

    # Reject a wrong file size, a missing payload file, or a LUN claimed by RØDE.
    for case in ("size", "missing", "taken"):
        run("attach", str(image), "8388608")
        if case != "missing":
            (Path(f"{image}.disk") / "RC2FX.UPD").write_bytes(b"short")
        if case == "taken":
            (lun / "file").write_text("/mnt/sd.img\n")
        dest.unlink(missing_ok=True)
        out = run("take", str(image), str(dest), "8", success=False)
        assert {
            "size": "not 8 bytes",
            "missing": "has no RC2FX.UPD",
            "taken": "taken for something else",
        }[case] in out, out
        if case == "taken":
            assert attached() == "/mnt/sd.img"  # Leave RØDE's backing file untouched.
            run("drop", str(image))
            assert attached() == "/mnt/sd.img" and not image.exists()
            (lun / "file").write_text("\n")
        else:
            assert not image.exists()
        (Path(f"{image}.disk") / "RC2FX.UPD").unlink(missing_ok=True)

    # drop cleans up a specific image; drop-stale cleans up a drive left behind by a crashed controller.
    run("attach", str(image), "8388608")
    run("drop", str(image))
    assert attached() == "" and not image.exists()
    run("attach", str(image), "8388608")
    run("drop-stale")
    assert attached() == "" and not image.exists()
    (lun / "file").write_text("/mnt/sd.img\n")
    run("drop-stale")
    assert attached() == "/mnt/sd.img"
print("update_drive: OK")
