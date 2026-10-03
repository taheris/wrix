import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


RUNNER, SYSTEM, BASH, COREUTILS = sys.argv[1:]
PASS = "test-linux-builder-sshd-hardening"
STATUS = "test-linux-builder-image-source-kind"
BROKEN = "test-linux-builder-source-kind-load-transport"
GIT_LOCAL_ENV = subprocess.check_output(["git", "rev-parse", "--local-env-vars"], text=True).splitlines()


def command(*args, cwd=None, env=None):
    isolated = dict(os.environ if env is None else env)
    for name in GIT_LOCAL_ENV:
        isolated.pop(name, None)
    isolated.update(GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_NOSYSTEM="1")
    return subprocess.run(args, cwd=cwd, env=isolated, capture_output=True, text=True, check=False)


class CiBatching(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.temp = tempfile.TemporaryDirectory(prefix="wrix-ci-batching-", dir="/tmp")
        cls.root = Path(cls.temp.name)
        (cls.root / "flake.nix").write_text(f'''{{
          inputs.bash = {{ url = {json.dumps('path:' + BASH)}; flake = false; }};
          inputs.coreutils = {{ url = {json.dumps('path:' + COREUTILS)}; flake = false; }};
          outputs = {{ bash, coreutils, ... }}: builtins.trace "WRIX-CI-BATCH-QUERY" {{
            legacyPackages.{SYSTEM}.ciApps = let
              mkRunner = name: text: derivation {{
                inherit name;
                system = "{SYSTEM}";
                builder = "${{bash}}/bin/bash";
                PATH = "${{coreutils}}/bin";
                script = builtins.toFile name (''
                  #!${{bash}}/bin/bash
                  set -euo pipefail
                  case "$0" in /nix/store/*/bin/${{name}}) ;; *) exit 24 ;; esac
                '' + text);
                args = [ "-c" ''
                  mkdir -p "$out/bin"
                  cp "$script" "$out/bin/${{name}}"
                  chmod +x "$out/bin/${{name}}"
                '' ];
              }};
            in {{
              {PASS} = mkRunner "{PASS}" ''
                set -euo pipefail
                printf '%s\\n' '{PASS}' >>"$WRIX_TEST_CI_CALLS"
              '';
              {STATUS} = mkRunner "{STATUS}" ''
                set -euo pipefail
                printf '%s\\n' '{STATUS}' >>"$WRIX_TEST_CI_CALLS"
                case "$WRIX_TEST_CI_STATUS" in
                  pass) exit 0 ;;
                  fail) echo fixture-failure >&2; exit 23 ;;
                  skip) echo fixture-skip >&2; exit 77 ;;
                esac
              '';
              {BROKEN} = derivation {{
                name = "ci-fixture-build-failure";
                system = "{SYSTEM}";
                builder = "${{bash}}/bin/bash";
                args = [ "-c" "exit 19" ];
              }};
            }};
          }};
        }}''')
        for args in [("init", "-q"), ("add", "flake.nix"),
                     ("-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid",
                      "-c", "core.hooksPath=/dev/null", "commit", "-qm", "Create CI fixture")]:
            result = command("git", *args, cwd=cls.root)
            if result.returncode:
                raise RuntimeError(result.stderr)

    @classmethod
    def tearDownClass(cls):
        cls.temp.cleanup()

    def run_apps(self, *apps, status="pass", git_dir=None):
        calls = self.root / "calls"
        calls.write_text("")
        env = dict(os.environ, WRIX_TEST_CI_STATUS=status, WRIX_TEST_CI_CALLS=str(calls))
        env["NIX_CONFIG"] = env.get("NIX_CONFIG", "") + "\neval-cache = false\n"
        if git_dir is not None:
            env["GIT_DIR"] = str(git_dir)
        result = command(RUNNER, "--json", *apps, cwd=self.root, env=env)
        verdicts = [json.loads(line) for line in result.stdout.splitlines()]
        self.assertEqual([v["target"] for v in verdicts], list(apps), result.stderr)
        return result, verdicts, calls.read_text().splitlines()

    def test_selected_runners_batch_before_independent_execution(self):
        result, verdicts, calls = self.run_apps(PASS, STATUS, PASS)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(all(v["pass"] for v in verdicts))
        self.assertEqual(calls, [PASS, STATUS, PASS])
        self.assertIn("trace: WRIX-CI-BATCH-QUERY", result.stderr)

    def test_fixture_execution_is_independent_of_caller_git_context(self):
        result, verdicts, calls = self.run_apps(PASS, STATUS, git_dir=self.root / "not-a-repository")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(all(v["pass"] for v in verdicts))
        self.assertEqual(calls, [PASS, STATUS])

    def test_script_failure_keeps_other_app_verdicts(self):
        result, verdicts, calls = self.run_apps(PASS, STATUS, status="fail")
        self.assertEqual(result.returncode, 1)
        self.assertEqual([v["pass"] for v in verdicts], [True, False])
        self.assertIn("fixture-failure", verdicts[1]["evidence"])
        self.assertEqual(calls, [PASS, STATUS])

    def test_exit_77_remains_a_failed_json_verdict(self):
        result, verdicts, _ = self.run_apps(PASS, STATUS, status="skip")
        self.assertEqual(result.returncode, 1)
        self.assertEqual([v["pass"] for v in verdicts], [True, False])
        self.assertIn("fixture-skip", verdicts[1]["evidence"])

    def test_failed_batch_falls_back_to_individual_build_verdicts(self):
        result, verdicts, calls = self.run_apps(PASS, BROKEN)
        self.assertEqual(result.returncode, 1)
        self.assertEqual([v["pass"] for v in verdicts], [True, False])
        self.assertTrue(verdicts[1]["evidence"])
        self.assertEqual(calls, [PASS])

    def test_unknown_apps_fail_without_suppressing_valid_apps(self):
        result, verdicts, calls = self.run_apps(PASS, "not-a-ci-app", STATUS)
        self.assertEqual(result.returncode, 1)
        self.assertEqual([v["pass"] for v in verdicts], [True, False, True])
        self.assertEqual(verdicts[1]["evidence"], "unknown test-ci app")
        self.assertEqual(calls, [PASS, STATUS])

    def test_single_app_retains_individual_build_execution(self):
        result, verdicts, calls = self.run_apps(PASS)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(verdicts[0]["pass"])
        self.assertEqual(calls, [PASS])


unittest.main(argv=[sys.argv[0]])
