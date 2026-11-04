#!/usr/bin/env python3
"""Tests for `rc2fx.sh developer-ssh` with a fake root: RC2FX_SSH_DIR,
RC2FX_SSH_HOST_DIR and RC2FX_FIRST_BOOT point into a temporary directory.
Nothing outside it is read or written."""

import os
import stat
import subprocess
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).parents[1] / "rc2fx.sh"
KEY = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIEPzD7gW9u5+SC5iq8gzujWEOTBQO1CbVCLO8fAOGkip"
OTHER = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIMS4sKxaX6YqY9tZ3lJgYHL8WbK9F0jUqk1E1h0a2G7K"
MARK = "rc2fx-managed-developer-key"
HOST = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIBhostkeyhostkeyhostkeyhostkeyhostkeyhostkey0"


class DeveloperSsh(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        root = Path(self.tmp.name)
        self.ssh = root / "home" / "root" / ".ssh"
        self.ssh.mkdir(parents=True, mode=0o700)
        self.keys = self.ssh / "authorized_keys"
        self.hosts = root / "etc" / "ssh"
        self.hosts.mkdir(parents=True)
        (self.hosts / "ssh_host_ed25519_key.pub").write_text(HOST + " root@rodecaster\n")
        (self.hosts / "ssh_host_ed25519_key").write_text("PRIVATE, never read\n")
        self.first_boot = root / "first-boot"
        self.env = dict(
            os.environ,
            RC2FX_SSH_DIR=str(self.ssh),
            RC2FX_SSH_HOST_DIR=str(self.hosts),
            RC2FX_FIRST_BOOT=str(self.first_boot),
            RC2FX_DATA=str(root / "data"),
            RC2FX_LOGS=str(root / "logs"),
        )

    def tearDown(self):
        self.tmp.cleanup()

    def run_ssh(self, *args, stdin=""):
        return subprocess.run(
            ["bash", str(SCRIPT), "developer-ssh", *args],
            input=stdin,
            capture_output=True,
            text=True,
            env=self.env,
            timeout=30,
        )

    def lines(self):
        return self.keys.read_text().splitlines() if self.keys.exists() else []

    def test_status_reports_counts_and_public_host_keys_only(self):
        self.keys.write_text(f"{OTHER}\n# a comment\n\n")
        out = self.run_ssh("status")
        self.assertEqual(out.returncode, 0, out.stderr)
        self.assertIn("unmanaged 1", out.stdout)
        self.assertNotIn("managed ssh", out.stdout)
        self.assertIn(f"host {HOST}", out.stdout)
        self.assertNotIn("PRIVATE", out.stdout)
        self.assertRegex(out.stdout, r"server (running|stopped|unknown)")

    def test_enable_adds_one_marked_line_and_keeps_every_other_line(self):
        original = f"{OTHER}\n# keep me\n"
        self.keys.write_text(original)
        out = self.run_ssh("enable", stdin=KEY + "\n")
        self.assertEqual(out.returncode, 0, out.stderr)
        self.assertEqual(self.lines(), [OTHER, "# keep me", f"{KEY} {MARK}"])
        self.assertEqual(stat.S_IMODE(self.keys.stat().st_mode), 0o600)
        self.assertIn(f"managed {KEY}", out.stdout)
        self.assertIn("unmanaged 1", out.stdout)
        # Enabling again replaces the managed line rather than adding another.
        again = self.run_ssh("enable", stdin=KEY + "\n")
        self.assertEqual(again.returncode, 0, again.stderr)
        self.assertEqual(self.lines().count(f"{KEY} {MARK}"), 1)
        self.assertEqual(len(self.lines()), 3)

    def test_disable_removes_only_the_managed_line(self):
        self.keys.write_text(f"{OTHER}\n{KEY} {MARK}\n")
        out = self.run_ssh("disable")
        self.assertEqual(out.returncode, 0, out.stderr)
        self.assertEqual(self.lines(), [OTHER])
        self.assertIn("unmanaged 1", out.stdout)
        self.assertNotIn("managed ssh", out.stdout)

    def test_disable_without_a_managed_line_writes_nothing(self):
        self.keys.write_text(f"{OTHER}\n")
        os.utime(self.keys, (1, 1))
        out = self.run_ssh("disable")
        self.assertEqual(out.returncode, 0, out.stderr)
        self.assertEqual(self.keys.stat().st_mtime, 1)

    def test_lines_that_only_look_managed_are_never_removed(self):
        lookalikes = [
            f'command="sh" {KEY} {MARK}',
            f"{KEY} {MARK} extra",
            f"ssh-rsa AAAAB3NzaC1yc2E {MARK}",
        ]
        self.keys.write_text("\n".join(lookalikes) + "\n")
        out = self.run_ssh("disable")
        self.assertEqual(out.returncode, 0, out.stderr)
        self.assertEqual(self.lines(), lookalikes)
        self.assertIn("unmanaged 3", out.stdout)

    def test_invalid_keys_are_refused_without_side_effects(self):
        self.keys.write_text(f"{OTHER}\n")
        for bad in [
            "",
            f"ssh-rsa {KEY[12:]}",
            f'command="id" {KEY}',
            KEY[:-1],
            f"{KEY} comment",
            f"{KEY}\n{OTHER}",
        ]:
            out = self.run_ssh("enable", stdin=bad + "\n")
            self.assertEqual(out.returncode, 2, bad)
            self.assertEqual(self.lines(), [OTHER], bad)
        self.assertEqual(sorted(p.name for p in self.ssh.iterdir()), ["authorized_keys"])

    def test_first_boot_refuses_changes(self):
        self.first_boot.write_text("")
        self.keys.write_text(f"{KEY} {MARK}\n")
        for args, stdin in [(("enable",), OTHER + "\n"), (("disable",), "")]:
            out = self.run_ssh(*args, stdin=stdin)
            self.assertEqual(out.returncode, 4, out.stderr)
            self.assertEqual(self.lines(), [f"{KEY} {MARK}"])

    def test_unsafe_paths_are_refused(self):
        target = Path(self.tmp.name) / "elsewhere"
        target.write_text("")
        self.keys.symlink_to(target)
        out = self.run_ssh("enable", stdin=KEY + "\n")
        self.assertEqual(out.returncode, 3, out.stderr)
        self.assertEqual(target.read_text(), "")
        self.keys.unlink()
        os.link(target, self.keys)
        self.assertEqual(self.run_ssh("enable", stdin=KEY + "\n").returncode, 3)
        self.assertEqual(target.read_text(), "")
        self.keys.unlink()
        self.ssh.rmdir()
        self.ssh.symlink_to(Path(self.tmp.name))
        self.assertEqual(self.run_ssh("status").returncode, 3)

    def test_a_missing_ssh_directory_is_created_private(self):
        self.ssh.rmdir()
        out = self.run_ssh("enable", stdin=KEY + "\n")
        self.assertEqual(out.returncode, 0, out.stderr)
        self.assertEqual(stat.S_IMODE(self.ssh.stat().st_mode), 0o700)
        self.assertEqual(self.lines(), [f"{KEY} {MARK}"])

    def test_status_changes_nothing_even_when_the_directory_is_missing(self):
        self.ssh.rmdir()
        out = self.run_ssh("status")
        self.assertEqual(out.returncode, 0, out.stderr)
        self.assertFalse(self.ssh.exists())
        self.assertIn("unmanaged 0", out.stdout)

    def test_every_other_byte_is_kept_including_a_missing_final_newline(self):
        original = f"{OTHER}\r\n  \n# c\n{KEY} {MARK}\nlast-line-without-newline"
        self.keys.write_bytes(original.encode())
        out = self.run_ssh("disable")
        self.assertEqual(out.returncode, 0, out.stderr)
        self.assertEqual(
            self.keys.read_bytes(),
            f"{OTHER}\r\n  \n# c\nlast-line-without-newline".encode(),
        )
        # Enabling adds the one newline needed before the managed line.
        out = self.run_ssh("enable", stdin=KEY)
        self.assertEqual(out.returncode, 0, out.stderr)
        self.assertEqual(
            self.keys.read_bytes(),
            f"{OTHER}\r\n  \n# c\nlast-line-without-newline\n{KEY} {MARK}\n".encode(),
        )

    def test_writable_by_others_or_oversized_is_refused(self):
        self.keys.write_text(f"{OTHER}\n")
        self.ssh.chmod(0o770)
        self.assertEqual(self.run_ssh("enable", stdin=KEY).returncode, 3)
        self.ssh.chmod(0o700)
        self.keys.chmod(0o666)
        self.assertEqual(self.run_ssh("status").returncode, 3)
        self.keys.chmod(0o600)
        self.keys.write_bytes(b"#" * (1024 * 1024 + 1))
        self.assertEqual(self.run_ssh("disable").returncode, 3)
        self.assertEqual(self.keys.stat().st_size, 1024 * 1024 + 1)

    def test_oversized_or_extra_input_is_refused_not_truncated(self):
        for bad in [KEY + "\n\n", KEY + "\n" + OTHER, KEY + " " * 600, " " + KEY]:
            out = self.run_ssh("enable", stdin=bad)
            self.assertEqual(out.returncode, 2, repr(bad))
            self.assertFalse(self.keys.exists(), repr(bad))

    def test_nul_bytes_in_input_or_file_are_refused(self):
        out = subprocess.run(
            ["bash", str(SCRIPT), "developer-ssh", "enable"],
            input=(KEY + "\0garbage\n").encode(),
            capture_output=True,
            env=self.env,
            timeout=30,
        )
        self.assertEqual(out.returncode, 2)
        self.assertFalse(self.keys.exists())
        self.keys.write_bytes(f"{OTHER}\n\0hidden\n{KEY} {MARK}\n".encode())
        before = self.keys.read_bytes()
        self.assertEqual(self.run_ssh("disable").returncode, 3)
        self.assertEqual(self.run_ssh("status").returncode, 3)
        self.assertEqual(self.keys.read_bytes(), before)

    def test_an_ancestor_writable_by_others_is_refused(self):
        home = self.ssh.parent
        home.chmod(0o777)
        try:
            self.assertEqual(self.run_ssh("enable", stdin=KEY).returncode, 3)
            self.assertFalse(self.keys.exists())
            home.chmod(0o1777)  # sticky but not root's: still refused
            self.assertEqual(self.run_ssh("status").returncode, 3)
        finally:
            home.chmod(0o755)
        self.assertEqual(self.run_ssh("status").returncode, 0)

    def test_unknown_actions_are_usage_errors(self):
        self.assertEqual(self.run_ssh("open").returncode, 2)
        self.assertEqual(self.run_ssh().returncode, 2)


if __name__ == "__main__":
    unittest.main()
