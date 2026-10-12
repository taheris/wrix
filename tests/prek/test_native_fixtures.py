import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


SCRIPT = Path(sys.argv.pop(1)) if len(sys.argv) > 1 else Path(__file__).parents[1] / "prepare-native-fixtures.sh"
MEMBERS = {
    "WRIX_TEST_PREK_HOOKS": "hooks",
    "WRIX_TEST_PREK_RUNNER": "runner",
    "WRIX_TEST_BROKEN_PREK_RUNNER": "broken-runner",
    "WRIX_TEST_PREK_BUILD_SPEC": "builder-spec",
    "WRIX_TEST_PACKAGED_WRIX": "packaged-wrix",
    "WRIX_TEST_DEVSHELL_HOOK": "devshell-hook",
}


class Preparation(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="wrix fixtures ")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.bundle = self.root / "bundle"
        self.bundle.mkdir()
        self.outputs = self.root / "outputs"
        self.outputs.mkdir()
        for name, member in MEMBERS.items():
            path = self.outputs / member
            (self.bundle / member).symlink_to(path)
            if member == "hooks":
                path.mkdir()
            else:
                path.write_text("fixture\n")
                if "RUNNER" in name or name == "WRIX_TEST_PACKAGED_WRIX":
                    path.chmod(0o755)
        self.env_file = self.root / "environment"
        self.env_file.write_text("")
        self.calls = self.root / "calls"
        self.bin = self.root / "bin"
        self.bin.mkdir()
        nix = self.bin / "nix"
        nix.write_text(f'''#!{shutil.which("bash")}
set -euo pipefail
printf 'call\\n' >>"$WRIX_FIXTURE_CALLS"
printf '%s\\n' "$*" >>"$WRIX_FIXTURE_CALLS.args"
if [[ "$WRIX_FIXTURE_STATUS" != 0 ]]; then
  echo fixture-build-failed >&2
  exit "$WRIX_FIXTURE_STATUS"
fi
printf '%s\\n' "$WRIX_FIXTURE_BUNDLE"
''')
        nix.chmod(0o755)
        self.env = {name: value for name, value in os.environ.items() if name not in MEMBERS}
        self.env.update(
            PATH=str(self.bin) + os.pathsep + os.environ["PATH"],
            NEXTEST_ENV=str(self.env_file),
            WRIX_FIXTURE_CALLS=str(self.calls),
            WRIX_FIXTURE_STATUS="0",
            WRIX_FIXTURE_BUNDLE=str(self.bundle),
        )

    def run_script(self):
        return subprocess.run(["bash", str(SCRIPT)], env=self.env, text=True, capture_output=True, check=False)

    def exported(self):
        return dict(line.split("=", 1) for line in self.env_file.read_text().splitlines())

    def test_one_build_supplies_all_packaging_fixtures(self):
        result = self.run_script()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.exported(), {name: str(self.outputs / member) for name, member in MEMBERS.items()})
        self.assertEqual(len(self.calls.read_text().splitlines()), 1)
        self.assertIn("--out-link " + str(self.env_file) + ".fixtures", self.calls.with_suffix(".args").read_text())

    def test_existing_nix_packaging_inputs_need_no_nix_frontend(self):
        expected = {name: str(self.outputs / member) for name, member in MEMBERS.items()}
        self.env.update(expected)
        result = self.run_script()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.exported(), expected)
        self.assertFalse(self.calls.exists())

    def test_partial_inputs_keep_the_explicit_override(self):
        custom = self.root / "custom-runner"
        custom.write_text("custom\n")
        custom.chmod(0o755)
        self.env["WRIX_TEST_PREK_RUNNER"] = str(custom)
        result = self.run_script()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.exported()["WRIX_TEST_PREK_RUNNER"], str(custom))
        self.assertEqual(len(self.calls.read_text().splitlines()), 1)

    def test_build_failure_stops_before_exporting_any_fixture(self):
        self.env["WRIX_FIXTURE_STATUS"] = "23"
        result = self.run_script()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("fixture-build-failed", result.stderr)
        self.assertEqual(self.env_file.read_text(), "")

    def test_non_executable_runner_is_rejected_before_export(self):
        (self.bundle / "runner").chmod(0o644)
        result = self.run_script()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("WRIX_TEST_PREK_RUNNER", result.stderr)
        self.assertEqual(self.env_file.read_text(), "")

    def test_newline_in_an_override_cannot_inject_an_environment_variable(self):
        path = self.root / "runner\nINJECTED=value"
        path.write_text("fixture\n")
        path.chmod(0o755)
        self.env["WRIX_TEST_PREK_RUNNER"] = str(path)
        result = self.run_script()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.env_file.read_text(), "")

    def test_missing_nextest_environment_is_an_error(self):
        del self.env["NEXTEST_ENV"]
        result = self.run_script()
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(self.calls.exists())


if __name__ == "__main__":
    unittest.main()
