#!/usr/bin/env python3
"""Global identity bootstrap must not discover or mutate the mounted repository."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(os.environ.get(
    "WRIX_TEST_GIT_BOOTSTRAP_SCRIPT",
    Path(__file__).resolve().parents[2] / "lib/util/git-ssh-setup.sh",
))


class GlobalBootstrapTest(unittest.TestCase):
    def test_global_bootstrap_works_before_workspace_repository_discovery(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            home = root / "home"
            workspace = root / "workspace"
            home.mkdir()
            workspace.mkdir()
            marker = workspace / ".git"
            original = f"gitdir: {root / 'unavailable-host-git-directory'}\n"
            marker.write_text(original)
            environment = {
                "HOME": str(home), "PATH": os.environ["PATH"],
                "GIT_CONFIG_NOSYSTEM": "1", "LC_ALL": "C",
            }
            result = subprocess.run(
                ["bash", "-c", 'source "$1" 0', "bootstrap", str(SCRIPT)],
                cwd=workspace, env=environment, text=True, capture_output=True, check=False,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(marker.read_text(), original)
            for key, expected in [("user.name", "Wrix Sandbox"), ("user.email", "sandbox@wrix.dev")]:
                value = subprocess.check_output(
                    ["git", "-C", "/", "config", "--global", "--get", key],
                    env=environment, text=True,
                )
                self.assertEqual(value.strip(), expected)
            dependent = subprocess.run(
                ["git", "config", "--global", "--get", "user.name"],
                cwd=workspace, env=environment, text=True, capture_output=True, check=False,
            )
            self.assertNotEqual(dependent.returncode, 0)
            self.assertIn("not a git repository", dependent.stderr)


if __name__ == "__main__":
    unittest.main()
