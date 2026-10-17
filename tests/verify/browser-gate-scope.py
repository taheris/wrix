"""Exercise production runners and the consumed planner with a cold browser boundary."""

from concurrent.futures import ThreadPoolExecutor
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import unittest
import uuid


SOURCE, LOOM, SYSTEM = sys.argv[1:4]
SOURCE_STORE = json.loads(subprocess.check_output(
    ["nix", "flake", "metadata", "--json", "git+file://" + SOURCE], text=True
))["path"]


class BrowserGateScope(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="wrix-browser-scope-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        runtime = self.root / "runtime"
        runtime.mkdir()
        self.addCleanup(self.stop_runtime, runtime)
        self.env = dict(os.environ, LOOM_INSIDE="1", REPO_ROOT=str(self.root),
                        GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_NOSYSTEM="1", TMPDIR=str(runtime))
        for key in subprocess.check_output(["git", "rev-parse", "--local-env-vars"], text=True).splitlines():
            self.env.pop(key, None)
        for key in tuple(self.env):
            if key.startswith("PLAYWRIGHT_") or key == "WRIX_MCP_MANIFEST":
                self.env.pop(key)
        self.sentinel = "browser-runtime-" + uuid.uuid4().hex
        nixpkgs = subprocess.check_output(["nix", "eval", "--raw", "--impure", "--expr",
            '(builtins.getFlake "path:' + SOURCE_STORE + '").inputs.nixpkgs.outPath'], text=True)
        self.write("browser-nixpkgs/flake.nix", '''{
          inputs.upstream.url = UPSTREAM;
          outputs = { upstream, ... }: {
            inherit (upstream) lib;
            legacyPackages.SYSTEM = upstream.legacyPackages.SYSTEM.extend (final: prev: {
              playwright-mcp = final.runCommand "playwright-mcp" { boundary = SENTINEL; } ''
                echo BROWSER_RUNTIME_REALIZED >&2
                exit 1
              '';
              playwright-driver = prev.playwright-driver // {
                browsers = final.runCommand SENTINEL {} ''
                  echo BROWSER_RUNTIME_REALIZED >&2
                  exit 1
                '';
              };
            });
          };
        }'''.replace("UPSTREAM", json.dumps("path:" + nixpkgs))
             .replace("SYSTEM", SYSTEM).replace("SENTINEL", json.dumps(self.sentinel)))
        self.write("flake.nix", '''{
          inputs.wrix.url = SOURCE;
          inputs.wrix.inputs.nixpkgs.follows = "nixpkgs";
          inputs.nixpkgs.url = "path:./browser-nixpkgs";
          outputs = { self, wrix, ... }: let
            system = SYSTEM;
            base = wrix.inputs.nixpkgs.legacyPackages.${system};
            pkgs = base;
            test = import (wrix.outPath + "/tests") {
              inherit pkgs system;
              loomTests = import (wrix.outPath + "/tests/loom.nix") {
                inherit pkgs;
                linuxPkgs = pkgs;
                wrix = throw "discovery forced Loom image construction";
                loomCli = throw "discovery forced Loom tooling";
                linuxLoomCli = throw "discovery forced Linux Loom tooling";
                packages = throw "discovery forced profile images";
                devShell = throw "discovery forced a devshell";
                lock = throw "discovery forced lock assertions";
              };
              linuxPkgs = pkgs;
              treefmt = base.hello;
              src = self;
              inherit (wrix.inputs) crane fenix;
              wrix = import (wrix.outPath + "/lib") {
                inherit pkgs system;
                linuxPkgs = pkgs;
                inherit (wrix.inputs) crane fenix;
                treefmt = base.hello;
              };
            };
          in {
            apps.${system} = {
              verify = test.verify.app;
              test-ci = test.apps.ci;
            };
            legacyPackages.${system} = {
              browserBoundaries = [ pkgs.playwright-driver.browsers.drvPath pkgs.playwright-mcp.drvPath ];
              verifyScripts = test.verify.scripts;
              ciApps = test.ciAppDerivations;
              testApps = test.testAppDerivations;
            };
          };
        }'''.replace("SOURCE", json.dumps("path:" + SOURCE_STORE))
             .replace("SYSTEM", json.dumps(SYSTEM)).replace("SENTINEL", json.dumps(self.sentinel)))
        config = (Path(SOURCE) / "loom.toml").read_text()
        for name, command in (("verify", "nix run .#verify --"), ("test-ci", "bin/test-ci-verifiers")):
            config = config.replace(f'inputs = "{command} {{print_inputs}} {{targets}}"',
                                    f'inputs = "bash query-provider.sh {name} {{print_inputs}} {{targets}}"')
        self.write("loom.toml", config)
        self.write("query-provider.sh", '''set -euo pipefail
printf '%s\\n' "$*" >> "$REPO_ROOT/query-calls"
kind="$1"
shift
if [[ "$kind" == verify ]]; then
  exec nix run .#verify -- "$@"
fi
exec bin/test-ci-verifiers "$@"
''')
        shutil.copytree(Path(SOURCE) / "bin", self.root / "bin")
        shutil.copytree(Path(SOURCE) / "tests/mcp/playwright", self.root / "tests/mcp/playwright")
        shutil.copytree(Path(SOURCE) / "lib/mcp", self.root / "lib/mcp")
        self.write(".pre-commit-config.yaml", json.dumps({"repos": [{
            "repo": "local", "hooks": [{"id": "fixture", "name": "fixture", "entry": "true",
            "language": "system", "always_run": True, "pass_filenames": False,
            "stages": ["pre-commit"]}],
        }]}))
        self.write("specs/fixture.md", "# Fixture\n\n## Success Criteria\n\n" + "".join(
            f"- Browser boundary [{tier}]({target})\n"
            for tier in ("check", "system")
            for target in ("verify:playwright-mcp.screenshot", "test-ci:test-playwright-chromium-executable-path")
        ) + "".join(f"- Opaque information [{tier}](verify:beads.tracked-files)\n" for tier in ("check", "system")))
        for path in ("docs/README.md", "specs/playwright-mcp.md", "tests/mcp/playwright/screenshot-test.sh",
                     "tests/sandbox/pi-mcp-native.mjs", "lib/sandbox/image.nix", "tests/verify/default.nix"):
            self.write(path, "# Overview\n\n| Spec | Code | Beads | Purpose |\n| --- | --- | --- | --- |\n| [fixture](../specs/fixture.md) | tests/ | — | Scope fixture |\n| [playwright-mcp](../specs/playwright-mcp.md) | lib/mcp/ | — | Browser |\n" if path == "docs/README.md" else (Path(SOURCE) / path).read_text())
        self.write("specs/playwright-mcp.md", "# Browser owning contract fixture\n")
        for path in (".beads/.gitignore", ".beads/config.yaml", ".beads/metadata.json"):
            self.write(path, (Path(SOURCE) / path).read_text())
        self.run_ok("git", "init", "-q")
        self.run_ok("git", "add", "-f", ".")
        self.run_ok("nix", "flake", "lock")
        self.run_ok("git", "add", "-f", ".")
        self.commit()

    @staticmethod
    def stop_runtime(runtime):
        processes = subprocess.check_output(["ps", "-eo", "pid=,args="], text=True)
        for row in processes.splitlines():
            pid, command = row.strip().split(maxsplit=1)
            if str(runtime) + "/" in command:
                try:
                    os.kill(int(pid), signal.SIGTERM)
                except ProcessLookupError:
                    pass  # A fixture child may exit between process discovery and termination.

    def write(self, path, text):
        destination = self.root / path
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_text(text)

    def run_command(self, *argv):
        with tempfile.TemporaryFile(mode="w+") as stdout, tempfile.TemporaryFile(mode="w+") as stderr:
            process = subprocess.Popen(argv, cwd=self.root, env=self.env,
                                       stdout=stdout, stderr=stderr, start_new_session=True)
            try:
                status = process.wait(timeout=180)
            finally:
                try:
                    os.killpg(process.pid, signal.SIGTERM)
                except ProcessLookupError:
                    pass  # The owned process group has already exited without background children.
            stdout.seek(0)
            stderr.seek(0)
            return subprocess.CompletedProcess(argv, status, stdout.read(), stderr.read())

    def run_ok(self, *argv):
        result = self.run_command(*argv)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        return result

    def commit(self):
        self.run_ok("git", "add", "-u")
        self.run_ok("git", "-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid",
                    "-c", "core.hooksPath=/dev/null", "-c", "commit.gpgsign=false",
                    "commit", "-qm", "Record scoped subject")

    def gate(self, lane, path):
        changed = self.root / path
        changed.write_text(changed.read_text() + "\n")
        self.commit()
        receipts = self.root / ".loom/logs/gate/verifier-results.jsonl"
        receipts.unlink(missing_ok=True)
        self.write("query-calls", "")
        result = self.run_command(LOOM, "gate", lane, "--diff", "HEAD^..HEAD")
        queries = (self.root / "query-calls").read_text().splitlines()
        groups = 2 if lane == "verify" else 1
        self.assertEqual(len(queries), 2 * groups, result.stdout + result.stderr + repr(queries))
        for kind in ("verify", "test-ci"):
            self.assertEqual(sum(query.startswith(kind + " ") for query in queries), groups, queries)
        records = [json.loads(line) for line in receipts.read_text().splitlines()] if receipts.exists() else []
        return result, records

    def boundary_logs(self):
        paths = json.loads(self.run_ok("nix", "eval", "--json", f".#legacyPackages.{SYSTEM}.browserBoundaries").stdout)
        logs = []
        for path in paths:
            result = self.run_command("nix", "log", path)
            if result.returncode != 0:
                self.assertIn("is not available", result.stderr)
            logs.append(result.stdout)
        return "\n".join(logs)

    def assert_cold(self):
        self.assertNotIn("BROWSER_RUNTIME_REALIZED", self.boundary_logs())

    def assert_realized(self, count=1):
        self.assertGreaterEqual(self.boundary_logs().count("BROWSER_RUNTIME_REALIZED"), count)

    def test_runtime_cleanup_terminates_owned_fixture_children(self):
        script = Path(self.env["TMPDIR"]) / "sleeper.py"
        script.write_text("import time\ntime.sleep(30)\n")
        child = subprocess.Popen([sys.executable, str(script)])
        try:
            self.stop_runtime(script.parent)
            self.assertEqual(child.wait(timeout=5), -signal.SIGTERM)
        finally:
            if child.poll() is None:
                child.terminate()
                child.wait(timeout=5)

    def test_boundary_fixture_fails_only_when_realized(self):
        self.assert_cold()
        paths = json.loads(self.run_ok("nix", "eval", "--json", f".#legacyPackages.{SYSTEM}.browserBoundaries").stdout)
        for path in paths:
            result = self.run_command("nix", "build", "--no-link", path + "^*")
            self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assert_realized(2)

    def test_inventory_and_description_do_not_realize_browser_boundary(self):
        self.assert_cold()
        for app in ("verify", "test-ci"):
            for mode in ("--list", "--print-inputs"):
                result = self.run_ok("nix", "run", ".#" + app, "--", mode)
                self.assertNotIn(self.sentinel, result.stderr)
                self.assertNotIn("BROWSER_RUNTIME_REALIZED", result.stdout + result.stderr)
        self.assert_cold()

    def test_unrelated_documentation_excludes_browser_in_both_lanes(self):
        known = []
        for app, prefix in (("verify", "verify:"), ("test-ci", "test-ci:")):
            result = self.run_ok("nix", "run", ".#" + app, "--", "--print-inputs")
            known.extend(prefix + target for target in json.loads(result.stdout)["inputs"])
        self.write("specs/fixture.md", "# Fixture\n\n## Success Criteria\n\n" + "".join(
            f"- Declared resource [{tier}]({target})\n"
            for tier in ("check", "system") for target in known + ["verify:beads.tracked-files"]
        ))
        self.commit()
        self.assert_cold()
        for lane in ("verify", "system"):
            result, records = self.gate(lane, "docs/README.md")
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertFalse(any(record["target"] in known for record in records), records)
            self.assertNotIn(self.sentinel, result.stdout + result.stderr)
            self.assertTrue(any(record["target"] == "verify:beads.tracked-files" for record in records))
        self.assert_cold()

    def test_relevant_changes_reach_cold_browser_runtime_in_both_lanes(self):
        paths = ("tests/mcp/playwright/screenshot-test.sh", "tests/sandbox/pi-mcp-native.mjs",
                 "specs/playwright-mcp.md", "flake.lock", "lib/sandbox/image.nix", "tests/verify/default.nix")
        def check_subject(lane, path):
            case = BrowserGateScope()
            try:
                case.setUp()
                case.assert_cold()
                result, records = case.gate(lane, path)
                case.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                targets = {record["target"] for record in records}
                case.assertIn("verify:playwright-mcp.screenshot", targets, result.stdout + result.stderr)
                case.assertIn("test-ci:test-playwright-chromium-executable-path", targets)
                case.assert_realized(2)
                for record in records:
                    if record["target"] in ("verify:playwright-mcp.screenshot", "test-ci:test-playwright-chromium-executable-path"):
                        case.assertEqual(record["outcome"], "fail", record)
                case.assertFalse((case.root / ".loom/marker.json").exists())
            finally:
                self.assertTrue(case.doCleanups(), "Scoped fixture cleanup failed")

        with ThreadPoolExecutor(max_workers=3) as pool:
            futures = [(lane, path, pool.submit(check_subject, lane, path))
                       for lane in ("verify", "system") for path in paths]
            for lane, path, future in futures:
                with self.subTest(lane=lane, path=path):
                    future.result()

    def test_explicit_and_full_verification_keep_browser_coverage(self):
        for lane in ("verify", "system"):
            for scope in (("--tree",), ("--target", "test-ci:test-playwright-chromium-executable-path")):
                with self.subTest(lane=lane, scope=scope):
                    self.setUp()
                    self.assert_cold()
                    command_lane = "check" if lane == "verify" and scope[0] == "--target" else lane
                    result = self.run_command(LOOM, "gate", command_lane, *scope)
                    self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                    self.assert_realized(2 if scope[0] == "--tree" else 1)
                    self.assertFalse((self.root / ".loom/marker.json").exists())

    def test_bad_discovery_blocks_both_lanes(self):
        for response in ('printf \'%s\\n\' \'{"inputs":{"playwright-mcp.screenshot":"malformed"}}\'', 'exit 9'):
            self.write("query-provider.sh", "set -euo pipefail\n" + response + "\n")
            self.commit()
            for lane in ("verify", "system"):
                result = self.run_command(LOOM, "gate", lane, "--diff", "HEAD^..HEAD")
                self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertIn("input-query errored", result.stdout + result.stderr)
                self.assertNotIn("BROWSER_RUNTIME_REALIZED", result.stdout + result.stderr)


unittest.main(argv=[sys.argv[0]] + sys.argv[4:])
