import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys
import tempfile
import unittest


VERIFY, CI, LOOM, SOURCE, NIXPKGS, SYSTEM = sys.argv[1:]
KNOWN = "devshell.no-prek-install"
OPAQUE = "notifications.focus-target-envelope"
CI_KNOWN = "test-linux-builder-source-kind-load-transport"
CI_OPAQUE = "test-security-audit-trail-anchor"
GIT_ENV = subprocess.check_output(["git", "rev-parse", "--local-env-vars"], text=True).splitlines()


class VerifierInputs(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="wrix-verifier-inputs-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.env = dict(os.environ, REPO_ROOT=str(self.root), LOOM_INSIDE="1",
                        GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_NOSYSTEM="1")
        for name in GIT_ENV:
            self.env.pop(name, None)
        self.calls = self.root / "calls"
        self.calls.write_text("")

    def run_command(self, *args):
        return subprocess.run(args, cwd=self.root, env=self.env,
                              capture_output=True, text=True, check=False)

    def query(self, program, *targets):
        result = self.run_command(program, "--print-inputs", *targets)
        self.assertEqual(result.returncode, 0, result.stderr)
        document = json.loads(result.stdout)
        self.assertEqual(set(document), {"inputs"})
        self.assertIsInstance(document["inputs"], dict)
        self.assertEqual(self.calls.read_text(), "")
        return document["inputs"]

    def test_verify_batch_normalizes_ids_and_omits_opaque_definitions(self):
        inputs = self.query(VERIFY, "verify:" + KNOWN, KNOWN, OPAQUE)
        self.assertEqual(set(inputs), {KNOWN})
        self.assertIn("/lib/devshell/default.nix", inputs[KNOWN])

    def test_ci_batch_projects_recipe_resources_without_realizing_apps(self):
        inputs = self.query(CI, CI_KNOWN, CI_OPAQUE, CI_KNOWN)
        self.assertEqual(set(inputs), {CI_KNOWN})
        self.assertIn("/lib/", inputs[CI_KNOWN])
        self.assertIn("/tests/builder/", inputs[CI_KNOWN])
        # This directory has no flake or image runtime: execution/build would fail.
        self.assertFalse((self.root / "flake.nix").exists())

    def test_single_opaque_target_stays_unknown_instead_of_empty_known(self):
        for program, target in [(VERIFY, OPAQUE), (CI, CI_OPAQUE)]:
            self.assertEqual(self.query(program, target), {})

    def test_no_target_query_returns_only_definition_derived_known_entries(self):
        for program, target in [(VERIFY, KNOWN), (CI, CI_KNOWN)]:
            inputs = self.query(program)
            self.assertIn(target, inputs)
            for paths in inputs.values():
                self.assertIsInstance(paths, list)
                self.assertTrue(all(isinstance(path, str) for path in paths))
                self.assertIn("/**/*.nix", paths)
                self.assertIn("/flake.lock", paths)
                self.assertIn("/tests/lib/verifier.sh", paths)

    def test_unknown_id_rejects_whole_description_before_output(self):
        for program, target in [(VERIFY, KNOWN), (CI, CI_KNOWN)]:
            result = self.run_command(program, "--print-inputs", target, "not-registered")
            self.assertEqual(result.returncode, 64, result.stderr)
            self.assertEqual(result.stdout, "")
            self.assertIn("not-registered", result.stderr)

    def fixture(self, malformed=False, system=False):
        for relative in ["tests/verify/profiles-eval.nix", "tests/verify/agent-defaults.nix",
                         "tests/lib/inputs.nix"]:
            destination = self.root / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(Path(SOURCE) / relative, destination)
        source = self.root / "lib/devshell/default.nix"
        source.parent.mkdir(parents=True)
        source.write_text("# source fixture with no forbidden hook mutation\n")
        (self.root / "flake.nix").write_text(f'''{{
          inputs.nixpkgs.url = {json.dumps('path:' + NIXPKGS)};
          outputs = {{ ... }}: {{ }};
        }}''')
        script = self.root / "tests/standalone/notify-test.sh"
        script.parent.mkdir(parents=True)
        # The opaque external verifier is a counted boundary fixture. The
        # input provider, dispatch, Nix source assertion and audit are real.
        script.write_text('#!/usr/bin/env bash\nset -euo pipefail\nprintf "opaque\\n" >>"$REPO_ROOT/calls"\n')
        wrapper = self.root / "runner"
        wrapper.write_text(f'''#!/usr/bin/env bash
set -euo pipefail
if [[ "$1" == --print-inputs ]]; then
  printf 'query\\n' >>"$REPO_ROOT/calls"
  {'printf \'%s\\n\' \'{"inputs":{"devshell.no-prek-install":"malformed"}}\'; exit 0' if malformed else ':'}
else
  printf 'execute %s\\n' "$*" >>"$REPO_ROOT/calls"
fi
exec {shlex.quote(VERIFY)} "$@"
''')
        wrapper.chmod(0o755)
        command = shlex.quote(str(wrapper))
        config = (
            "[runner.check.verify]\nmatch = '^verify:(.+)$'\n"
            f"command = {json.dumps(command + ' {targets}')}\n"
            f"inputs = {json.dumps(command + ' {print_inputs} {targets}')}\n"
            "target = '{capture_1}'\njoin = ' '\nparse = 'json-lines'\ncwd = '.'\n"
        )
        if system:
            config += (
                "[runner.system.verify]\nmatch = '^verify:(.+)$'\n"
                f"command = {json.dumps(command + ' {targets}')}\n"
                "target = '{capture_1}'\njoin = ' '\nparse = 'json-lines'\ncwd = '.'\n"
            )
        (self.root / "loom.toml").write_text(config)
        (self.root / ".pre-commit-config.yaml").write_text(json.dumps({"repos": [{
            "repo": "local", "hooks": [{
                "id": "fixture", "name": "fixture", "entry": "true", "language": "system",
                "always_run": True, "pass_filenames": False, "stages": ["pre-commit"],
            }],
        }]}))
        specs = self.root / "specs"
        specs.mkdir()
        annotations = (
            f"- Source hook assertion [check](verify:{KNOWN})\n"
            f"- Independent opaque assertion [check](verify:{OPAQUE})\n"
        )
        if system:
            annotations += f"- System assertion [system](verify:{KNOWN})\n" * 2
        (specs / "fixture.md").write_text("# Fixture\n\n## Success Criteria\n\n" + annotations)
        (self.root / "README.md").write_text("Fixture documentation\n")
        for args in [("init", "-q"), ("add", "."),
                     ("-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid",
                      "-c", "core.hooksPath=/dev/null", "-c", "commit.gpgsign=false",
                      "commit", "-qm", "Create input fixture")]:
            result = self.run_command("git", *args)
            self.assertEqual(result.returncode, 0, result.stderr)

    def gate(self, path, tier="verify", commit=True):
        # Exercise the actual pre-push range path. Explicit --files currently
        # drops runner-owned logical IDs upstream (wx-1x5ns), independently
        # of these input declarations; do not disguise that with a bypass.
        if commit:
            changed = self.root / path
            changed.write_text(changed.read_text() + "\n")
            for args in [("add", path),
                         ("-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid",
                          "-c", "core.hooksPath=/dev/null", "-c", "commit.gpgsign=false",
                          "commit", "-qm", "Change fixture subject")]:
                result = self.run_command("git", *args)
                self.assertEqual(result.returncode, 0, result.stderr)
        self.calls.write_text("")
        result = self.run_command(LOOM, "gate", tier, "--workspace", str(self.root),
                                  "--diff", "HEAD^..HEAD")
        return result, self.calls.read_text().splitlines()

    def test_independent_change_runs_opaque_and_queries_group_once_per_session(self):
        self.fixture()
        for index in range(2):
            result, calls = self.gate("README.md", commit=index == 0)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertEqual(calls.count("query"), 1, calls)
            self.assertIn("execute " + OPAQUE, calls)
            self.assertNotIn("execute " + KNOWN, calls)
            self.assertEqual(calls.count("opaque"), 1)

    def test_changed_source_selects_real_assertion_and_retains_failure(self):
        self.fixture()
        (self.root / "lib/devshell/default.nix").write_text('"prek install"\n')
        result, calls = self.gate("lib/devshell/default.nix")
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertEqual(calls.count("query"), 1, calls)
        self.assertTrue(any(KNOWN in call and call.startswith("execute ") for call in calls))
        evidence = (self.root / ".loom/logs/gate/verifier-results.jsonl").read_text()
        self.assertIn("mkDevShell invokes prek install", evidence)
        self.assertIn("opaque", calls)

    def test_changed_owner_spec_selects_known_assertion(self):
        self.fixture()
        result, calls = self.gate("specs/fixture.md")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertTrue(any(KNOWN in call and call.startswith("execute ") for call in calls))
        self.assertEqual(calls.count("query"), 1, calls)

    def test_new_nix_definition_invalidates_known_assertion(self):
        self.fixture()
        path = self.root / "new/definition.nix"
        path.parent.mkdir()
        path.write_text("{}\n")
        result, calls = self.gate("new/definition.nix")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertTrue(any(KNOWN in call and call.startswith("execute ") for call in calls))

    def test_malformed_provider_document_is_a_blocking_protocol_finding(self):
        self.fixture(malformed=True)
        result, calls = self.gate("README.md")
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("input-query errored / emitted a malformed inputs document",
                      result.stdout + result.stderr)
        self.assertEqual(calls.count("query"), 1, calls)

    def test_system_checks_remain_always_run_and_share_only_within_invocation(self):
        self.fixture(system=True)
        for index in range(2):
            result, calls = self.gate("README.md", tier="system", commit=index == 0)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertEqual(calls.count("execute " + KNOWN), 1, calls)
            self.assertEqual(calls.count("query"), 0, calls)
            records = [json.loads(line) for line in
                       (self.root / ".loom/logs/gate/verifier-results.jsonl").read_text().splitlines()]
            self.assertEqual([record["target"] for record in records[-2:]],
                             ["verify:" + KNOWN] * 2)
            self.assertFalse((self.root / ".loom/marker.json").exists())


unittest.main(argv=[sys.argv[0]])
