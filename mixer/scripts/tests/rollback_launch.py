#!/usr/bin/env python3
"""Verify that launching from /opt/.rc2fx-previous passes that copy's release
manifest to rc2fxd (so mixer_update::installed_bundle recognizes it and triggers
recover()), and still honors /opt/rc2fx/experimental-duo.
"""

import os
import shlex
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from _common import RC2FX_SH, SCRIPTS_DIR, TempDirTestCase

SHIM = (SCRIPTS_DIR / "rc2fx-shim.sh").read_text()


class RollbackLaunch(TempDirTestCase):
    def run_shim(self, body, *, pending, setup_bundle):
        case = Path(tempfile.mkdtemp(dir=self.root))
        opt, run = case / "opt", case / "run"
        run.mkdir(parents=True)
        for name in ["rc2fx", ".rc2fx-previous"]:
            bundle = opt / name
            bundle.mkdir(parents=True)
            setup_bundle(bundle, name)
            (bundle / "rc2fx.sh").write_text(
                f"#!/bin/bash\n. {shlex.quote(str(RC2FX_SH))}\n"
                f'case "${{1:-}}" in\n  launch) {body} ;;\nesac\n'
            )
        if pending:
            (opt / ".rc2fx-update-pending").write_text("{}")
        shim = (
            SHIM.replace("/opt/", f"{opt}/")
            .replace("/run/rc2fx-update/", f"{run}/")
            .replace("/run/rc2fx-dev/", f"{case}/no-dev/")
            .replace("/usr/bin/rc_audio_mixer", "false")
        )
        env = {k: v for k, v in os.environ.items() if not k.startswith("RC2FX_")}
        env["RC2FX_DUO_MARKER"] = str(opt / "rc2fx" / "experimental-duo")
        result = subprocess.run(
            ["bash", "-c", shim], env=env, capture_output=True, text=True, check=False
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        return opt, result.stdout

    def launch(self, *, pending):
        """Run rc2fx-shim.sh against a fake /opt and return the --release path."""
        opt, stdout = self.run_shim(
            "data=/data run=/run/rc2fx show= profile_id=; controller_arguments; printf '%s\\n' \"${controller[@]}\"",
            pending=pending,
            setup_bundle=lambda bundle, _: (bundle / "mixer-release.json").write_text(
                "{}"
            ),
        )
        arguments = stdout.splitlines()
        return opt, arguments[arguments.index("--release") + 1]

    def duo(self, *, live=None, previous=None, pending=True):
        """Run rc2fx-shim.sh against a fake /opt and return (bundle_dir, RC2FX_EXPERIMENTAL_DUO)."""
        markers = {"rc2fx": live, ".rc2fx-previous": previous}

        def setup(bundle, name):
            if markers[name] is not None:
                (bundle / "experimental-duo").write_text(markers[name])

        opt, stdout = self.run_shim(
            'printf \'%s %s\\n\' "$home" "$RC2FX_EXPERIMENTAL_DUO"',
            pending=pending,
            setup_bundle=setup,
        )
        home, opted_in = stdout.split()
        return Path(home).relative_to(opt).as_posix(), opted_in

    def test_an_opted_in_duo_keeps_its_opt_in_in_the_rollback_copy(self):
        # Rollback copy has no marker of its own; it should pick up /opt/rc2fx's.
        self.assertEqual(self.duo(live="1\n"), (".rc2fx-previous", "1"))

    def test_the_installed_bundle_reads_its_own_marker(self):
        self.assertEqual(self.duo(live="1\n", pending=False), ("rc2fx", "1"))

    def test_a_marker_beside_the_script_still_counts(self):
        self.assertEqual(self.duo(previous="1\n"), (".rc2fx-previous", "1"))

    def test_no_marker_or_another_value_is_no_opt_in(self):
        self.assertEqual(self.duo(), (".rc2fx-previous", "0"))
        self.assertEqual(
            self.duo(live="0\n", previous="yes\n"), (".rc2fx-previous", "0")
        )

    def test_pending_update_launches_the_rollback_copy_with_its_release(self):
        opt, release = self.launch(pending=True)
        self.assertEqual(release, f"{opt}/.rc2fx-previous/mixer-release.json")

    def test_committed_bundle_launches_with_the_live_release(self):
        opt, release = self.launch(pending=False)
        self.assertEqual(release, f"{opt}/rc2fx/mixer-release.json")

    def test_default_duo_marker_path(self):
        env = {k: v for k, v in os.environ.items() if not k.startswith("RC2FX_")}
        result = subprocess.run(
            [
                "bash",
                "-c",
                f'. {shlex.quote(str(RC2FX_SH))}\nprintf "%s\\n" "$duo_marker"',
            ],
            env=env,
            capture_output=True,
            text=True,
            timeout=30,
            check=False,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "/opt/rc2fx/experimental-duo\n")


if __name__ == "__main__":
    unittest.main()
