#!/usr/bin/env python3
"""Tests for rc2fx.sh's start_time() and alive() helpers against fake /proc entries.

Supervisor-side process checks are tested in crates/rc2fx-supervisor.
"""

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from _common import TempDirTestCase, source_rc2fx  # noqa: E402


class Identity(TempDirTestCase):
    def run_fixture(self, state="S", start="123", comm="rc_audio_mixer", present=True):
        proc = self.root / "proc"
        task = proc / "4242"
        task.mkdir(parents=True, exist_ok=True)
        stat_file = task / "stat"
        fields = [state] + ["0"] * 39
        fields[19] = start
        if present:
            stat_file.write_text("4242 (" + comm + ") " + " ".join(fields) + "\n")
        else:
            stat_file.unlink(missing_ok=True)
        result = source_rc2fx(
            "alive 4242 && echo alive; start_time 4242 || true\n",
            env={"RC2FX_PROC": str(proc)},
            timeout=5,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        return result.stdout.splitlines()

    def test_matching_live_process(self):
        self.assertEqual(self.run_fixture(), ["alive", "123"])

    def test_zombie_cannot_pass_liveness(self):
        self.assertEqual(self.run_fixture(state="Z"), ["123"])

    def test_vanished_process_is_not_alive(self):
        self.assertEqual(self.run_fixture(present=False), [])

    def test_parentheses_and_z_in_comm_are_not_process_state(self):
        self.assertEqual(self.run_fixture(comm="odd ) Z ) ( name"), ["alive", "123"])


if __name__ == "__main__":
    unittest.main()
