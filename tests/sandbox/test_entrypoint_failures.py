"""Exercise real entrypoints and aggregate reporting with a failing hook runner."""

import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


ROOT = Path(os.environ.get("REPO_ROOT", Path(__file__).resolve().parents[2])).resolve()
CASES = (
    "test_linux_core_hooks_path",
    "test_darwin_core_hooks_path",
    "test_linked_worktree_core_hooks_path_both",
)


class HookFailurePropagation(unittest.TestCase):
    def run_cases(self, cases):
        with tempfile.TemporaryDirectory(prefix="wrix-hook-failure-") as temp:
            root = Path(temp)
            runner = root / "wrix-prek"
            bash = shutil.which("bash")
            self.assertIsNotNone(bash, "Bash is required for the entrypoint fixture")
            runner.write_text(
                f'#!{bash}\nset -euo pipefail\n'
                'printf "fixture hook binding failed\\n" >&2\nexit 23\n'
            )
            runner.chmod(0o755)
            driver = root / "driver.sh"
            driver.write_text(
                'review_hook_cases() {\n'
                '  unset BASH_ENV\n'
                f'  ALL_TESTS=({" ".join(cases)})\n'
                '  run_all\n'
                '}\n'
            )
            env = dict(os.environ, BASH_ENV=str(driver), REPO_ROOT=str(ROOT))
            env["PATH"] = str(root) + os.pathsep + env["PATH"]
            for name in subprocess.check_output(
                ["git", "rev-parse", "--local-env-vars"], cwd=ROOT, text=True
            ).splitlines():
                env.pop(name, None)
            env.update(GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_NOSYSTEM="1")
            result = subprocess.run(
                [bash, str(ROOT / "tests/sandbox/entrypoint-contract.sh"), "review_hook_cases"],
                env=env, cwd=ROOT, capture_output=True, text=True, check=False,
            )
            self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertNotEqual(result.returncode, 77, result.stderr)
            self.assertIn("fixture hook binding failed", result.stderr)
            self.assertNotIn("PASS:", result.stdout + result.stderr)
            for case in cases:
                self.assertIn(f"FAIL: {case}", result.stderr)

    def test_linux_failure_propagates(self):
        self.run_cases(CASES[:1])

    def test_darwin_failure_propagates(self):
        self.run_cases(CASES[1:2])

    def test_linked_worktree_failure_propagates(self):
        self.run_cases(CASES[2:])

    def test_aggregate_reports_all_owner_failures(self):
        self.run_cases(CASES)


if __name__ == "__main__":
    unittest.main()
