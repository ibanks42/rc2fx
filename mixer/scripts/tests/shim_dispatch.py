#!/usr/bin/env python3
"""Run the real launcher shims and install.sh's pre_gui check against the
real rc2fx.sh, so a change to either side that breaks how the shims find
rc2fx.sh fails here. Only the shims' paths point into a temporary folder,
and their final `exec /bin/bash` runs a stub that prints what it was handed.
"""

import re
import shutil
import subprocess
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from _common import RC2FX_SH, SCRIPTS_DIR, TempDirTestCase, write_executable


class ShimDispatch(TempDirTestCase):
    def shim(self, name, stock):
        """rc2fx-shim.sh or rc2fx-gui-shim.sh with its paths in self.root."""
        text = (SCRIPTS_DIR / name).read_text()
        for real, fake in [
            ("/run/rc2fx-dev/", f"{self.root}/dev/"),
            ("/run/rc2fx-update/", f"{self.root}/update/"),
            ("/opt/", f"{self.root}/opt/"),
            (stock, f"{self.root}/stock"),
            ("exec /bin/bash ", f"exec {self.root}/handoff "),
        ]:
            self.assertIn(real, text, f"{name} no longer mentions {real}")
            text = text.replace(real, fake)
        path = self.root / name
        path.write_text(text)
        path.chmod(0o755)
        write_executable(
            self.root / "handoff", 'printf "%s %s\\n" "$1" "$2"', shebang="#!/bin/sh"
        )
        write_executable(self.root / "stock", 'printf "stock\\n"', shebang="#!/bin/sh")
        return path

    def install(self, directory):
        target = self.root / directory / "rc2fx.sh"
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(RC2FX_SH, target)
        return target

    def run_shim(self, shim):
        result = subprocess.run(
            [str(shim)], capture_output=True, text=True, timeout=30, check=False
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        return result.stdout.strip()

    def test_the_audio_shim_starts_the_installed_rc2fx_sh(self):
        installed = self.install("opt/rc2fx")
        shim = self.shim("rc2fx-shim.sh", "/usr/bin/rc_audio_mixer")
        self.assertEqual(self.run_shim(shim), f"{installed} launch")

    def test_the_audio_shim_prefers_the_build_in_ram(self):
        self.install("opt/rc2fx")
        ram = self.install("dev")
        shim = self.shim("rc2fx-shim.sh", "/usr/bin/rc_audio_mixer")
        self.assertEqual(self.run_shim(shim), f"{ram} launch")

    def test_the_gui_shim_starts_the_installed_rc2fx_sh(self):
        installed = self.install("opt/rc2fx")
        shim = self.shim("rc2fx-gui-shim.sh", "/usr/bin/rc_gui_juce")
        self.assertEqual(self.run_shim(shim), f"{installed} gui-launch")

    def test_install_sh_sees_that_rc2fx_sh_has_gui_launch(self):
        install = (SCRIPTS_DIR / "install.sh").read_text()
        pre_gui = re.search(r"^pre_gui\(\) \{.*\}$", install, re.MULTILINE)
        self.assertIsNotNone(pre_gui, "install.sh has no one-line pre_gui")
        bundle = self.install("bundle").parent
        result = subprocess.run(
            ["bash", "-c", f'{pre_gui.group(0)}\npre_gui "$1"', "bash", str(bundle)],
            capture_output=True,
            text=True,
            timeout=30,
            check=False,
        )
        # pre_gui succeeds only for a bundle from before gui-launch existed.
        self.assertNotEqual(
            result.returncode,
            0,
            "pre_gui thinks the current rc2fx.sh has no gui-launch",
        )


if __name__ == "__main__":
    unittest.main()
