import json
import os
from pathlib import Path
import shlex
import subprocess
import sys
import tempfile
import unittest


VERIFY, LOOM, LIBRARY = sys.argv[1:]
PLATFORM = subprocess.check_output(
    ["bash", "-c", 'source "$1"; verifier_platform', "fixture", LIBRARY], text=True
).strip()
FOREIGN = (
    "notifications.container-transport-darwin"
    if PLATFORM.endswith("-linux")
    else "notifications.container-transport-linux"
)
PASS = "notifications.client-envelope"
FAIL = "notifications.client-non-blocking"
UNEXPECTED = "notifications.client-tcp-endpoint-override"
EMPTY = {"platforms": [], "capabilities": []}


class VerifierResults(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="wrix-verifier-results-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        script = self.root / "tests/standalone/notify-test.sh"
        script.parent.mkdir(parents=True)
        script.write_text('''#!/usr/bin/env bash
set -euo pipefail
printf '%s\\n' "$1" >>"$REPO_ROOT/calls"
case "$1" in
  test_client_envelope) printf 'fixture passed\\n' ;;
  test_client_non_blocking) printf 'fixture assertion failed\\n' >&2; false; printf 'concealed failure\\n' ;;
  test_client_tcp_endpoint_override) printf 'unreported fixture prerequisite\\n' >&2; exit 77 ;;
  *) printf 'preflight should not execute this fixture\\n' >&2; exit 24 ;;
esac
''')
        self.calls = self.root / "calls"
        self.calls.write_text("")
        self.env = dict(os.environ, REPO_ROOT=str(self.root))

    def run_targets(self, *targets):
        result = subprocess.run(
            [VERIFY, *targets], env=self.env, capture_output=True, text=True, check=False
        )
        records = [json.loads(line) for line in result.stdout.splitlines()]
        return result, records

    def test_pass_reports_execution_and_omits_skip_reason(self):
        result, records = self.run_targets(PASS)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(len(records), 1)
        self.assertEqual(records[0], {
            "target": PASS, "outcome": "passed", "evidence": "passed",
            "execution": dict(EMPTY, platform=PLATFORM),
        })

    def test_target_prefixes_and_duplicate_invocations_are_preserved(self):
        result, records = self.run_targets(PASS, "verify:" + PASS)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual([record["target"] for record in records], [PASS, PASS])
        self.assertEqual(self.calls.read_text().splitlines(), ["test_client_envelope"] * 2)

    def test_intermediate_assertion_failure_cannot_be_hidden_by_later_success(self):
        result, records = self.run_targets(FAIL, PASS)
        self.assertEqual(result.returncode, 1)
        self.assertEqual([record["outcome"] for record in records], ["failed", "passed"])
        self.assertIn("fixture assertion failed", records[0]["evidence"])
        self.assertNotIn("concealed failure", records[0]["evidence"])
        self.assertNotIn("skip_reason", records[0])

    def test_foreign_platform_skips_before_invocation(self):
        result, records = self.run_targets(FOREIGN)
        self.assertEqual(result.returncode, 77, result.stderr)
        self.assertEqual(records[0]["outcome"], "skipped")
        self.assertEqual(records[0]["execution"]["platform"], PLATFORM)
        self.assertNotIn(PLATFORM, records[0]["execution"]["platforms"])
        self.assertEqual(records[0]["skip_reason"]["kind"], "foreign-platform")
        self.assertTrue(records[0]["skip_reason"]["reason"])
        self.assertEqual(self.calls.read_text(), "")

    def test_pass_does_not_conceal_skip(self):
        result, records = self.run_targets(PASS, FOREIGN)
        self.assertEqual(result.returncode, 77, result.stderr)
        self.assertEqual([record["outcome"] for record in records], ["passed", "skipped"])

    def test_skip_does_not_conceal_failure(self):
        result, records = self.run_targets(FOREIGN, FAIL)
        self.assertEqual(result.returncode, 1)
        self.assertEqual([record["outcome"] for record in records], ["skipped", "failed"])

    def test_unreported_exit_77_stays_unauthorized_legacy_skip(self):
        result, records = self.run_targets(PASS, UNEXPECTED)
        self.assertEqual(result.returncode, 77)
        self.assertEqual(records[1]["pass"], False)
        self.assertEqual(records[1]["skipped"], True)
        self.assertNotIn("skip_reason", records[1])
        self.assertEqual(records[1]["execution"]["capabilities"], [])
        self.assertIn("unreported fixture prerequisite", records[1]["evidence"])

    def test_unknown_target_fails_before_any_invocation(self):
        result, records = self.run_targets(PASS, "verify:cli.not-registered")
        self.assertEqual(result.returncode, 64)
        self.assertEqual(records, [])
        self.assertEqual(self.calls.read_text(), "")
        self.assertIn("Unknown verify target: verify:cli.not-registered", result.stderr)
        self.assertIn("nix run .#verify -- --list", result.stderr)
        self.assertIn("verify:cli.package-surface", result.stderr)

    def library_result(self, requirements, path=None):
        env = dict(self.env)
        if path is not None:
            env["PATH"] = str(path) + os.pathsep + env["PATH"]
        result = subprocess.run(
            ["bash", "-c", 'source "$1"; verifier_run fixture "$2" bash -c \'echo ran >"$REPO_ROOT/executed"\'',
             "fixture", LIBRARY, json.dumps(requirements)],
            env=env, capture_output=True, text=True, check=False,
        )
        return result, json.loads(result.stdout)

    def test_missing_runtime_skips_before_execution_on_applicable_platform(self):
        bin_dir = self.root / "bin"
        bin_dir.mkdir()
        for tool in ("bash", "uname", "jq", "mktemp", "head", "cat", "rm"):
            executable = subprocess.check_output(["bash", "-c", 'command -v "$1"', "fixture", tool], text=True).strip()
            (bin_dir / tool).symlink_to(executable)
        result = subprocess.run(
            [str(bin_dir / "bash"), "-c", 'source "$1"; verifier_run fixture "$2" bash -c \'exit 24\'',
             "fixture", LIBRARY, json.dumps({"platforms": [PLATFORM], "capabilities": ["container-runtime"]})],
            env=dict(self.env, PATH=str(bin_dir)), capture_output=True, text=True, check=False,
        )
        self.assertEqual(result.returncode, 77, result.stderr)
        record = json.loads(result.stdout)
        self.assertEqual(record["outcome"], "skipped")
        self.assertEqual(record["execution"]["platforms"], [PLATFORM])
        self.assertEqual(record["skip_reason"]["kind"], "missing-capability")
        self.assertEqual(record["skip_reason"]["capability"], "container-runtime")
        self.assertIn("not on PATH", record["skip_reason"]["reason"])

    def test_kvm_preflight_retains_declared_requirement_and_device_reason(self):
        result, record = self.library_result(dict(EMPTY, capabilities=["kvm"]))
        available = os.access("/dev/kvm", os.R_OK | os.W_OK) and Path("/dev/kvm").is_char_device()
        self.assertEqual(result.returncode, 0 if available else 77, result.stderr)
        self.assertEqual(record["execution"]["capabilities"], ["kvm"])
        self.assertEqual((self.root / "executed").exists(), available)
        if not available:
            self.assertEqual(record["skip_reason"]["capability"], "kvm")
            self.assertIn("/dev/kvm", record["skip_reason"]["reason"])

    def test_namespace_preflight_checks_external_capability_before_execution(self):
        bin_dir = self.root / "bin"
        bin_dir.mkdir()
        probe = bin_dir / "unshare"
        probe.write_text('#!/usr/bin/env bash\nset -euo pipefail\necho fixture-namespace-unavailable >&2\nexit 1\n')
        probe.chmod(0o755)
        result, record = self.library_result(dict(EMPTY, capabilities=["user-network-namespace"]), bin_dir)
        self.assertEqual(result.returncode, 77)
        self.assertEqual(record["skip_reason"]["capability"], "user-network-namespace")
        self.assertIn("fixture-namespace-unavailable", result.stderr)
        self.assertFalse((self.root / "executed").exists())

    def test_unknown_capability_is_a_preflight_failure_not_a_skip(self):
        result, record = self.library_result(dict(EMPTY, capabilities=["undeclared-probe"]))
        self.assertEqual(result.returncode, 1)
        self.assertEqual(record["outcome"], "failed")
        self.assertIn("unknown verifier capability", record["evidence"])
        self.assertFalse((self.root / "executed").exists())

    def gate_targets(self, targets, worker=True):
        (self.root / "specs").mkdir(exist_ok=True)
        (self.root / "specs/fixture.md").write_text(
            "# Fixture\n\n## Success Criteria\n\n" + "".join(
                f"- Target {index} [check](verify:{target})\n" for index, target in enumerate(targets)
            )
        )
        (self.root / "loom.toml").write_text(
            "[runner.check.verify]\nmatch = '^verify:(.+)$'\n"
            f"command = {json.dumps(shlex.quote(VERIFY) + ' {targets}')}\n"
            "target = '{capture_1}'\njoin = ' '\nparse = 'json-lines'\ncwd = '.'\n"
            "skip_policy = 'sandbox-capability'\nskip_capabilities = ['container-runtime', 'kvm', 'user-network-namespace']\n"
        )
        env = dict(self.env)
        env.pop("LOOM_INSIDE", None)
        if worker:
            env["LOOM_INSIDE"] = "1"
        for name in subprocess.check_output(["git", "rev-parse", "--local-env-vars"], text=True).splitlines():
            env.pop(name, None)
        for args in [("init", "-q"), ("add", "."),
                     ("-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid",
                      "-c", "core.hooksPath=/dev/null", "-c", "commit.gpgsign=false", "commit", "-qm", "Create fixture")]:
            subprocess.run(["git", *args], cwd=self.root, env=env, capture_output=True, check=True)
        return subprocess.run(
            [LOOM, "gate", "verify", "--workspace", str(self.root), "--tree"],
            cwd=self.root, env=env, capture_output=True, text=True, check=False,
        )

    def test_worker_accepts_exported_foreign_skip_without_claiming_coverage(self):
        result = self.gate_targets([PASS, FOREIGN])
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("unverified", result.stdout + result.stderr)
        self.assertFalse((self.root / ".loom/marker.json").exists())
        records = [json.loads(line) for line in (self.root / ".loom/logs/gate/verifier-results.jsonl").read_text().splitlines()]
        self.assertTrue(records)
        self.assertIn("skipped", json.dumps(records))

    def test_host_acceptance_retains_skip_exit_77(self):
        result = self.gate_targets([PASS, FOREIGN], worker=False)
        self.assertEqual(result.returncode, 77, result.stdout + result.stderr)

    def test_worker_failure_remains_blocking_alongside_authorized_skip(self):
        result = self.gate_targets([FOREIGN, FAIL])
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)

    def test_worker_unreported_skip_remains_blocking(self):
        result = self.gate_targets([UNEXPECTED])
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)


unittest.main(argv=[sys.argv[0]])
