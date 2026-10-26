#!/usr/bin/env python3
"""Portable harness contracts. Stubs are not evidence of native acceptance."""
import importlib.util
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

HERE = Path(__file__).resolve().parent
HARNESS = HERE / "interrupt-native-test.py"
CLEANUP = HERE / "cleanup-native-fixtures.py"
RUNNER = HERE / "run-native-test.sh"


class InterruptionTest(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.parent = Path(self.directory.name).resolve()
        self.bin = self.parent / "bin"
        self.bin.mkdir()
        runtime = self.bin / "runtime"
        source = (HERE / "fixtures/interruption-runtime.py").read_text().split("\n", 1)[1]
        runtime.write_text(f"#!{sys.executable}\n" + source)
        runtime.chmod(0o755)
        for name in ["native", "podman", "cat", "readlink", "fusermount3", "git"]:
            (self.bin / name).symlink_to(runtime)

    def run_case(self, scenario, expected):
        parent = self.parent / scenario
        parent.mkdir()
        self.root = parent / "fixture"
        self.output = parent / "evidence"
        environment = dict(os.environ, PATH=str(self.bin) + os.pathsep + os.environ["PATH"], WRIX_FAKE_NATIVE_ROOT=str(self.root), WRIX_FAKE_SCENARIO=scenario, WRIX_TEST_POOL_PYTHON=sys.executable, WRIX_TEST_POOL_CLEANUP_SCRIPT=str(CLEANUP), WRIX_TEST_POOL_CLEANUP_TOOLS=str(self.bin))
        command = [sys.executable, str(HARNESS), "--cleanup", str(CLEANUP), "--tools", str(self.bin), "--source-revision", "a" * 40, "--output", str(self.output), "--ready-timeout", "2" if scenario == "timeout" else "8", "--stop-timeout", "10", "--", shutil.which("bash"), str(RUNNER), str(self.bin / "native")]
        completed = subprocess.run(command, env=environment, capture_output=True, text=True, timeout=30, umask=0o022)
        native_log = (self.output / "native.log").read_text()
        if completed.returncode:
            print(f"scenario={scenario}\n{completed.stdout}{completed.stderr}{native_log}", file=sys.stderr)
        self.assertEqual(completed.returncode, expected, completed.stdout + completed.stderr + native_log)
        result = json.loads((self.output / "result.json").read_text())
        self.assertEqual((self.output / "exit").read_text(), f"{expected}\n")
        self.assertTrue((self.output / "native.log").exists())
        self.assertTrue((self.output / "registry/root").exists())
        self.assertTrue(self.root.is_dir())
        return result, completed.stdout

    def test_interrupts_after_checkpoint_and_checks_original_namespace(self):
        result, output = self.run_case("pass", 0)
        self.assertEqual(result["outcome"], "passed")
        self.assertEqual(result["native_exit"], 143)
        self.assertEqual(result["revision"], "a" * 40)
        before = json.loads((self.output / "namespace-before.json").read_text())
        after = json.loads((self.output / "namespace-after.json").read_text())
        self.assertEqual(before["namespace"], after["namespace"])
        self.assertEqual(len(before["running"]), 3)
        self.assertEqual(after["containers"], [])
        self.assertEqual(after["mountinfo"], "")
        self.assertIn("Verified native readiness", output)
        self.assertIn("PASS:", output)
        self.assertTrue((self.root / ".wrix-native-test-resources-stopped").exists())

    def test_preserves_native_failure_before_readiness(self):
        result, _ = self.run_case("early-exit", 1)
        self.assertEqual(result["native_exit"], 7)
        self.assertIn("before verified readiness", result["error"])

    def test_unexpected_native_success_is_not_interruption_acceptance(self):
        result, _ = self.run_case("early-success", 1)
        self.assertEqual(result["native_exit"], 0)
        self.assertIn("before verified readiness", result["error"])

    def test_foreign_checkpoint_does_not_authorize_signal(self):
        result, _ = self.run_case("foreign", 1)
        self.assertIn("foreign", result["error"])
        self.assertNotIn("signal", result)

    def test_world_readable_checkpoint_is_rejected(self):
        result, _ = self.run_case("unsafe-ready", 1)
        self.assertIn("unsafe native readiness", result["error"])

    def test_empty_native_fixture_cannot_claim_worker_readiness(self):
        result, _ = self.run_case("empty-ready", 1)
        self.assertIn("not all running", result["error"])
        self.assertNotIn("signal", result)

    def test_cleanup_failure_overrides_signal_exit(self):
        result, _ = self.run_case("cleanup-failure", 1)
        self.assertEqual(result["native_exit"], 1)
        self.assertIn("expected intentional SIGTERM", result["error"])

    def test_replacement_empty_namespace_is_not_absence_proof(self):
        result, _ = self.run_case("changed-namespace", 1)
        self.assertEqual(result["native_exit"], 143)
        self.assertIn("namespace changed", result["error"])

    def test_receipt_does_not_replace_fresh_export_inventory(self):
        result, _ = self.run_case("post-mount", 1)
        self.assertEqual(result["native_exit"], 143)
        self.assertTrue((self.root / ".wrix-native-test-resources-stopped").exists())
        self.assertIn("namespace mounts", result["error"])

    def test_receipt_does_not_replace_fresh_container_inventory(self):
        result, _ = self.run_case("post-container", 1)
        self.assertEqual(result["native_exit"], 143)
        self.assertIn("still owns containers", result["error"])

    def test_hung_cleanup_is_bounded_and_never_passes(self):
        result, _ = self.run_case("hung-cleanup", 1)
        self.assertEqual(result["native_exit"], -9)
        self.assertIn("graceful native teardown exceeded", result["error"])
        self.assertIn("deadline", result["supervisor_cleanup_error"])

    def test_readiness_timeout_is_not_interruption_coverage(self):
        result, output = self.run_case("timeout", 1)
        self.assertNotIn("signal", result)
        self.assertIn("deadline", result["error"])
        self.assertIn("Waiting for pool", output)
        self.assertTrue((self.root / ".wrix-native-test-resources-stopped").exists())

    def test_escaped_helper_is_reaped_and_unrelated_process_is_not_signalled(self):
        unrelated = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(60)"], start_new_session=True)
        try:
            result, output = self.run_case("escaped-helper", 0)
            self.assertTrue(result["assertion_helpers_terminated"])
            self.assertIn("assertion helpers", output)
            self.assertIsNone(unrelated.poll())
            pid = int((self.root / "escaped-pid").read_text())
            stat_path = Path(f"/proc/{pid}/stat")
            if stat_path.exists():
                self.assertEqual(stat_path.read_text().rsplit(")", 1)[1].split()[0], "Z")
        finally:
            unrelated.terminate()
            unrelated.wait(timeout=5)

    def test_registry_is_validated_before_native_work_starts(self):
        target = self.parent / "private"
        target.mkdir(mode=0o700)
        alias = self.parent / "alias"
        alias.symlink_to(target)
        root = self.parent / "fixture"
        environment = dict(os.environ, WRIX_FAKE_NATIVE_ROOT=str(root), WRIX_TEST_POOL_PYTHON=sys.executable, WRIX_TEST_POOL_CLEANUP_SCRIPT=str(CLEANUP))
        command = [shutil.which("bash"), str(RUNNER), "--registry", str(alias), str(self.bin / "native")]
        completed = subprocess.run(command, env=environment, capture_output=True, timeout=5)
        self.assertNotEqual(completed.returncode, 0)
        self.assertFalse(root.exists())


class PidfdBoundaryTest(unittest.TestCase):
    def setUp(self):
        spec = importlib.util.spec_from_file_location("interruption_harness", HARNESS)
        self.harness = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.harness)

    def test_zombie_leader_cannot_hide_live_helper_threads(self):
        proc = Path("/proc/222")
        with patch.object(Path, "iterdir", side_effect=[[proc], [proc / "task/222", proc / "task/223"]]), patch.object(Path, "read_text", side_effect=["222 (owned) Z 1 222 111", "222 (owned) Z 1 222 111", "223 (worker) S 1 222 111"]):
            self.assertEqual(self.harness.session_members(111), [222])

    def test_fully_dead_thread_group_does_not_block_teardown(self):
        proc = Path("/proc/222")
        with patch.object(Path, "iterdir", side_effect=[[proc], [proc / "task/222"]]), patch.object(Path, "read_text", return_value="222 (owned) Z 1 222 111"):
            self.assertEqual(self.harness.session_members(111), [])

    def test_pid_reuse_into_an_unrelated_session_cannot_receive_signal(self):
        descriptor = os.open("/dev/null", os.O_RDONLY)
        try:
            with patch.object(Path, "iterdir", return_value=[Path("/proc/222")]), patch.object(Path, "read_text", side_effect=["222 (owned) S 1 222 111", "222 (unrelated) S 1 222 999"]), patch.object(os, "pidfd_open", return_value=descriptor), patch.object(os, "close") as close, patch.object(signal, "pidfd_send_signal") as send:
                self.assertEqual(self.harness.signal_session_members(111, signal.SIGTERM), [])
                send.assert_not_called()
                close.assert_called_once_with(descriptor)
        finally:
            os.close(descriptor)

    def test_exited_pidfd_cannot_signal_a_new_process_at_the_same_number(self):
        child = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(60)"], start_new_session=True)
        original = os.pidfd_open
        def open_then_exit(pid):
            descriptor = original(pid)
            child.terminate()
            child.wait(timeout=5)
            return descriptor
        try:
            record = f"{child.pid} (owned) S 1 {child.pid} 111"
            with patch.object(Path, "iterdir", return_value=[Path(f"/proc/{child.pid}")]), patch.object(Path, "read_text", return_value=record), patch.object(os, "pidfd_open", side_effect=open_then_exit):
                self.assertEqual(self.harness.signal_session_members(111, signal.SIGTERM), [])
        finally:
            if child.poll() is None:
                child.terminate()
            child.wait(timeout=5)


if __name__ == "__main__":
    unittest.main()
