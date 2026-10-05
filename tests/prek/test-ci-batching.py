import json
import os
from pathlib import Path
import shlex
import subprocess
import sys
import tempfile
import unittest


RUNNER, SYSTEM, BASH, COREUTILS, LIBRARY, LOOM = sys.argv[1:]
PASS = "test-linux-builder-sshd-hardening"
STATUS = "test-linux-builder-image-source-kind"
BROKEN = "test-linux-builder-source-kind-load-transport"
LIVE_BROKEN = "test-security-audit-trail-anchor"
OFFLINE_HOOK = "test-container-pre-commit"
VM = "test-services-devshell-start-independent"
VM_BROKEN = "test-services-limit-mode-cache-endpoint"
VM_REQUIREMENTS = {
    "platforms": ["aarch64-linux", "x86_64-linux"],
    "capabilities": ["virtiofsd-capabilities"],
}
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
              {OFFLINE_HOOK} = mkRunner "{OFFLINE_HOOK}" ''
                set -euo pipefail
                printf '%s\\n' '{OFFLINE_HOOK}' >>"$WRIX_TEST_CI_CALLS"
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
              {VM} = mkRunner "{VM}" ''
                set -euo pipefail
                printf '%s\\n' '{VM}' >>"$WRIX_TEST_CI_CALLS"
              '';
              {VM_BROKEN} = derivation {{
                name = "ci-fixture-vm-runner-build-failure";
                system = "{SYSTEM}";
                builder = "${{bash}}/bin/bash";
                args = [ "-c" "echo vm-runner-build-failure >&2; exit 77" ];
              }};
              {LIVE_BROKEN} = derivation {{
                name = "ci-fixture-live-build-failure";
                system = "{SYSTEM}";
                builder = "${{bash}}/bin/bash";
                args = [ "-c" "exit 77" ];
              }};
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
        self.assertTrue(all(v["outcome"] == "passed" for v in verdicts))
        self.assertEqual(calls, [PASS, STATUS, PASS])
        self.assertIn("trace: WRIX-CI-BATCH-QUERY", result.stderr)

    def test_fixture_execution_is_independent_of_caller_git_context(self):
        result, verdicts, calls = self.run_apps(PASS, STATUS, git_dir=self.root / "not-a-repository")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(all(v["outcome"] == "passed" for v in verdicts))
        self.assertEqual(calls, [PASS, STATUS])

    def test_script_failure_keeps_other_app_verdicts(self):
        result, verdicts, calls = self.run_apps(PASS, STATUS, status="fail")
        self.assertEqual(result.returncode, 1)
        self.assertEqual([v["outcome"] for v in verdicts], ["passed", "failed"])
        self.assertIn("fixture-failure", verdicts[1]["evidence"])
        self.assertEqual(calls, [PASS, STATUS])

    def test_unreported_exit_77_retains_skip_without_policy_authorization(self):
        result, verdicts, _ = self.run_apps(PASS, STATUS, status="skip")
        self.assertEqual(result.returncode, 77)
        self.assertEqual(verdicts[0]["outcome"], "passed")
        self.assertFalse(verdicts[1]["pass"])
        self.assertTrue(verdicts[1]["skipped"])
        self.assertNotIn("skip_reason", verdicts[1])
        self.assertEqual(verdicts[1]["execution"]["capabilities"], [])
        self.assertIn("fixture-skip", verdicts[1]["evidence"])

    def test_build_failure_dominates_skip_and_keeps_individual_results(self):
        result, verdicts, calls = self.run_apps(PASS, STATUS, BROKEN, status="skip")
        self.assertEqual(result.returncode, 1)
        self.assertEqual(verdicts[0]["outcome"], "passed")
        self.assertTrue(verdicts[1]["skipped"])
        self.assertEqual(verdicts[2]["outcome"], "failed")
        self.assertEqual(calls, [PASS, STATUS])

    def test_all_unreported_skips_exit_77(self):
        result, verdicts, calls = self.run_apps(STATUS, STATUS, status="skip")
        self.assertEqual(result.returncode, 77)
        self.assertTrue(all(v["skipped"] for v in verdicts))
        self.assertEqual(calls, [STATUS, STATUS])

    def test_offline_hook_executes_without_host_networking_capability(self):
        result, verdicts, calls = self.run_apps(PASS, OFFLINE_HOOK)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual([v["outcome"] for v in verdicts], ["passed", "passed"])
        self.assertEqual(verdicts[1]["execution"]["capabilities"], [])
        self.assertEqual(calls, [PASS, OFFLINE_HOOK])

    def test_missing_runtime_cannot_conceal_a_failed_live_app_build(self):
        result, verdicts, calls = self.run_apps(PASS, LIVE_BROKEN)
        self.assertEqual(result.returncode, 1)
        self.assertEqual([v["outcome"] for v in verdicts], ["passed", "failed"])
        self.assertIn("CI app build failed", verdicts[1]["evidence"])
        self.assertNotIn("skip_reason", verdicts[1])
        self.assertEqual(calls, [PASS])

    def test_failed_batch_falls_back_to_individual_build_verdicts(self):
        result, verdicts, calls = self.run_apps(PASS, BROKEN)
        self.assertEqual(result.returncode, 1)
        self.assertEqual([v["outcome"] for v in verdicts], ["passed", "failed"])
        self.assertTrue(verdicts[1]["evidence"])
        self.assertEqual(calls, [PASS])

    def test_unknown_apps_fail_without_suppressing_valid_apps(self):
        result, verdicts, calls = self.run_apps(PASS, "not-a-ci-app", STATUS)
        self.assertEqual(result.returncode, 1)
        self.assertEqual([v["outcome"] for v in verdicts], ["passed", "failed", "passed"])
        self.assertEqual(verdicts[1]["evidence"], "unknown test-ci app")
        self.assertEqual(calls, [PASS, STATUS])

    @unittest.skipUnless(SYSTEM.endswith("-linux"), "Linux VM prerequisite")
    def test_vm_prerequisite_precedes_execution_with_matching_metadata(self):
        probe = command("bash", "-c", 'source "$1"; verifier_preflight "$2" "$3"',
                        "fixture", LIBRARY, SYSTEM, json.dumps(VM_REQUIREMENTS))
        self.assertIn(probe.returncode, (0, 77), probe.stderr)
        result, verdicts, calls = self.run_apps(VM, VM)
        self.assertEqual(result.returncode, probe.returncode, result.stderr)
        self.assertEqual(calls, [VM, VM] if probe.returncode == 0 else [])
        for verdict in verdicts:
            self.assertEqual(verdict["execution"], dict(VM_REQUIREMENTS, platform=SYSTEM))
            self.assertEqual(verdict["outcome"], "passed" if probe.returncode == 0 else "skipped")
            if probe.returncode == 77:
                self.assertEqual(verdict["skip_reason"], json.loads(probe.stdout))

    @unittest.skipUnless(SYSTEM.endswith("-linux"), "Linux VM prerequisite")
    def test_vm_runner_build_failure_dominates_a_supported_capability_skip(self):
        result, verdicts, _ = self.run_apps(VM, VM_BROKEN)
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn(verdicts[0]["outcome"], ("passed", "skipped"))
        self.assertEqual(verdicts[1]["outcome"], "failed")
        self.assertEqual(verdicts[1]["execution"], dict(VM_REQUIREMENTS, platform=SYSTEM))
        self.assertIn("CI app build failed", verdicts[1]["evidence"])
        self.assertNotIn("skip_reason", verdicts[1])

    def gate_apps(self, *apps, worker=True, status="pass"):
        (self.root / "specs").mkdir(exist_ok=True)
        (self.root / "specs/fixture.md").write_text(
            "# Fixture\n\n## Success Criteria\n\n" + "".join(
                f"- Target {index} [check](test-ci:{app})\n" for index, app in enumerate(apps)
            )
        )
        (self.root / "loom.toml").write_text(
            "[runner.check.test-ci]\nmatch = '^test-ci:(.+)$'\n"
            f"command = {json.dumps(shlex.quote(RUNNER) + ' --json {targets}')}\n"
            "target = '{capture_1}'\njoin = ' '\nparse = 'json-lines'\ncwd = '.'\n"
            "skip_policy = 'sandbox-capability'\nskip_capabilities = ['virtiofsd-capabilities']\n"
        )
        env = dict(os.environ, WRIX_TEST_CI_STATUS=status, WRIX_TEST_CI_CALLS=str(self.root / "calls"))
        env.pop("LOOM_INSIDE", None)
        if worker:
            env["LOOM_INSIDE"] = "1"
        for args in [("add", "specs", "loom.toml"),
                     ("-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid",
                      "-c", "core.hooksPath=/dev/null", "-c", "commit.gpgsign=false", "commit", "--allow-empty", "-qm", "Update gate fixture")]:
            result = command("git", *args, cwd=self.root, env=env)
            self.assertEqual(result.returncode, 0, result.stderr)
        return command(LOOM, "gate", "verify", "--workspace", str(self.root), "--tree",
                       cwd=self.root, env=env)

    @unittest.skipUnless(SYSTEM.endswith("-linux"), "Linux VM prerequisite")
    def test_worker_accepts_vm_capability_gap_without_claiming_coverage(self):
        result = self.gate_apps(VM)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        probe, verdicts, _ = self.run_apps(VM)
        if probe.returncode == 77:
            self.assertIn("unverified", result.stdout + result.stderr)
            self.assertEqual(verdicts[0]["outcome"], "skipped")
            self.assertFalse((self.root / ".loom/marker.json").exists())

    @unittest.skipUnless(SYSTEM.endswith("-linux"), "Linux VM prerequisite")
    def test_host_vm_capability_skip_retains_exit_77(self):
        probe, _, _ = self.run_apps(VM)
        result = self.gate_apps(VM, worker=False)
        self.assertEqual(result.returncode, probe.returncode, result.stdout + result.stderr)

    @unittest.skipUnless(SYSTEM.endswith("-linux"), "Linux VM prerequisite")
    def test_worker_vm_skip_cannot_mask_a_runtime_failure(self):
        result = self.gate_apps(VM, STATUS, status="fail")
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)

    def test_single_app_retains_individual_build_execution(self):
        result, verdicts, calls = self.run_apps(PASS)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(verdicts[0]["outcome"], "passed")
        self.assertEqual(calls, [PASS])


unittest.main(argv=[sys.argv[0]])
