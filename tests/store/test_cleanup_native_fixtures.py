#!/usr/bin/env python3
"""Conformance checks for the disposable native runtime's teardown boundary."""

import importlib.util
import os
import signal
import time
import shutil
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

SCRIPT = Path(__file__).with_name("cleanup-native-fixtures.py")
sys.dont_write_bytecode = True
SPEC = importlib.util.spec_from_file_location("cleanup", SCRIPT)
cleanup = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(cleanup)


def line(target, source="wrix-nix-" + "a" * 32, kind="fuse.sshfs"):
    escaped = str(target).replace("\\", "\\134").replace(" ", "\\040")
    return f"90 2 0:99 / {escaped} ro,nosuid,nodev - {kind} {source} ro\n"


class CleanupTest(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name).resolve()
        (self.root / cleanup.MARKER).write_text("1\n")
        self.view = self.root / "home/.local/state/wrix/control/nix" / ("b" * 32) / "x86_64-linux/executions" / ("c" * 64) / "store"

    def test_mountinfo_framing_matches_native_export_records(self):
        target = self.root / "view with spaces"
        self.assertEqual(cleanup.mounts(line(target)), [(target, "fuse.sshfs", "wrix-nix-" + "a" * 32)])
        self.assertEqual(cleanup.Cleanup(self.root).views(cleanup.mounts(line(self.view))), [self.view])

    def test_teardown_is_scoped_ordered_and_checks_both_namespaces(self):
        (self.root / "home/.local/share/containers/storage").mkdir(parents=True)
        peer = Path("/home/operator/.local/state/wrix/control/nix/real/store")
        host = cleanup.mounts(line(self.view) + line(peer))
        native = cleanup.mounts(line(self.view))
        calls = []
        subject = cleanup.Cleanup(self.root)

        def run(args):
            calls.append(args)
            if Path(args[0]).name == "fusermount3":
                self.assertEqual(args[1:], ["-u", "--", str(self.view)])
                host.remove((self.view, "fuse.sshfs", "wrix-nix-" + "a" * 32))
            elif "unshare" in args and "fusermount3" in args:
                self.assertEqual(args[-3:], ["-u", "--", str(self.view)])
                native.clear()
            elif args[-1] == "/proc/self/mountinfo":
                return line(self.view) if native else ""
            return ""

        with patch.object(cleanup, "host_mounts", side_effect=lambda: list(host)), patch.object(subject, "run", side_effect=run):
            subject.finish()
        self.assertEqual(calls[0], subject.podman + ["stop", "--all", "--time=10"])
        self.assertEqual(calls[-1], subject.podman + ["system", "migrate"])
        self.assertIn(subject.podman + ["rm", "--all"], calls)
        self.assertIn(subject.podman + ["rmi", "--all"], calls)
        self.assertIn(subject.podman + ["unshare", "chown", "--recursive", "--no-dereference", "0:0", str(subject.storage)], calls)
        self.assertIn(subject.podman + ["unshare", "chmod", "--recursive", "u+rwX", str(subject.storage)], calls)
        self.assertTrue(subject.is_stopped())
        self.assertLess(next(i for i, call in enumerate(calls) if Path(call[0]).name == "fusermount3"), next(i for i, call in enumerate(calls) if "rm" in call))
        self.assertEqual(host, [(peer, "fuse.sshfs", "wrix-nix-" + "a" * 32)])
        self.assertEqual(subject.environment["HOME"], str(self.root / "home"))
        self.assertEqual(subject.environment["XDG_RUNTIME_DIR"], str(self.root / "runtime"))
        self.assertEqual(subject.podman[subject.podman.index("--root") + 1], str(subject.storage))
        self.assertNotIn("-z", [arg for call in calls for arg in call])
        self.assertNotIn("--force", [arg for call in calls for arg in call])

    def test_failed_unmount_does_not_remove_native_owner_records(self):
        (self.root / "home/.local/share/containers/storage").mkdir(parents=True)
        subject = cleanup.Cleanup(self.root)
        calls = []

        def run(args):
            calls.append(args)
            if Path(args[0]).name == "fusermount3":
                raise RuntimeError("Device or resource busy")
            return ""

        with patch.object(cleanup, "host_mounts", return_value=cleanup.mounts(line(self.view))), patch.object(subject, "run", side_effect=run):
            with self.assertRaisesRegex(RuntimeError, "resource busy"):
                subject.finish()
        self.assertFalse(any("rm" in call or "migrate" in call for call in calls))
        self.assertTrue(self.root.exists())

    def test_podman_administrative_bind_mounts_do_not_block_namespace_release(self):
        (self.root / "home/.local/share/containers/storage").mkdir(parents=True)
        subject = cleanup.Cleanup(self.root)
        calls = []

        def run(args):
            calls.append(args)
            if args[-1] == "/proc/self/mountinfo":
                return line(subject.storage, source="/dev/root", kind="ext4") + line(self.root / "runtime/containers", source="tmpfs", kind="tmpfs")
            return ""

        with patch.object(cleanup, "host_mounts", return_value=[]), patch.object(subject, "run", side_effect=run):
            subject.finish()
        self.assertEqual(calls[-1], subject.podman + ["system", "migrate"])

    def test_unexpected_namespace_mounts_still_prevent_file_cleanup(self):
        (self.root / "home/.local/share/containers/storage").mkdir(parents=True)
        subject = cleanup.Cleanup(self.root)
        with patch.object(cleanup, "host_mounts", return_value=[]), patch.object(subject, "run", return_value=line(self.root / "unknown", kind="ext4")):
            with self.assertRaisesRegex(RuntimeError, "non-administrative"):
                subject.finish()
        self.assertTrue(self.root.exists())

    def test_leftover_container_storage_mount_prevents_ownership_changes(self):
        (self.root / "home/.local/share/containers/storage").mkdir(parents=True)
        subject = cleanup.Cleanup(self.root)
        table = line(subject.storage / "overlay/layer/merged", source="overlay", kind="overlay")
        with patch.object(cleanup, "host_mounts", return_value=[]), patch.object(subject, "run", return_value=table) as run:
            with self.assertRaisesRegex(RuntimeError, "non-administrative"):
                subject.finish()
        self.assertFalse(any("chown" in call.args[0] for call in run.call_args_list))

    def test_partial_file_deletion_retains_runtime_marker_and_retry_receipt(self):
        storage = self.root / "home/.local/share/containers/storage"
        storage.mkdir(parents=True)
        runtime = self.root / "runtime"
        runtime.mkdir(mode=0o700)
        subject = cleanup.Cleanup(self.root)
        with patch.object(cleanup, "host_mounts", return_value=[]), patch.object(subject, "run", return_value=""):
            subject.finish()
        with patch.object(cleanup.shutil, "rmtree", side_effect=PermissionError("mapped layer owner")):
            with self.assertRaises(PermissionError):
                subject.remove_files()
        self.assertTrue(runtime.is_dir())
        self.assertTrue((self.root / cleanup.MARKER).is_file())
        self.assertTrue(subject.is_stopped())
        shutil.rmtree(runtime)
        retry = cleanup.Cleanup(self.root)
        with patch.object(retry, "run", side_effect=AssertionError("completed teardown must not restart Podman")):
            retry.finish()
            retry.remove_files()
        self.assertFalse(self.root.exists())

    def test_failure_diagnostics_capture_precedes_stop_and_uses_private_files(self):
        (self.root / "home/.local/share/containers/storage").mkdir(parents=True)
        subject = cleanup.Cleanup(self.root)
        identifier = "d" * 64
        calls = []

        def run(args, timeout=45):
            calls.append((args, timeout))
            if "ps" in args:
                return identifier + "\n"
            if "inspect" in args:
                return '{"Running":true}'
            if "logs" in args:
                return "native bootstrap phase\n"
            return ""

        with patch.object(cleanup, "host_mounts", return_value=[]), patch.object(subject, "run", side_effect=run):
            subject.finish(diagnostics=True)
        self.assertIn("ps", calls[0][0])
        self.assertTrue(all(timeout == 3 for _, timeout in calls[:3]))
        files = list((self.root / "native-cleanup-diagnostics").iterdir())
        self.assertEqual({path.read_text() for path in files}, {'{"Running":true}', "native bootstrap phase\n"})
        self.assertTrue(all(path.stat().st_mode & 0o777 == 0o600 for path in files))
        self.assertTrue(subject.is_stopped())

    def test_unavailable_diagnostics_do_not_prevent_resource_teardown(self):
        (self.root / "home/.local/share/containers/storage").mkdir(parents=True)
        subject = cleanup.Cleanup(self.root)

        def run(args, timeout=45):
            if "ps" in args:
                raise subprocess.TimeoutExpired(args, timeout)
            return ""

        with patch.object(cleanup, "host_mounts", return_value=[]), patch.object(subject, "run", side_effect=run), patch("sys.stderr") as errors:
            subject.finish(diagnostics=True)
        self.assertTrue(subject.is_stopped())
        self.assertTrue(any("continuing resource teardown" in str(call) for call in errors.write.call_args_list))

    def test_symlinked_teardown_receipt_is_not_trusted(self):
        peer = self.root / "peer-receipt"
        peer.write_text("{}")
        (self.root / cleanup.STOPPED).symlink_to(peer)
        with self.assertRaises(OSError):
            cleanup.Cleanup(self.root).finish()
        self.assertTrue(self.root.exists())

    def test_legacy_partial_deletion_recreates_private_runtime_before_podman(self):
        (self.root / "home/.local/share/containers/storage").mkdir(parents=True)
        subject = cleanup.Cleanup(self.root)

        def run(args):
            self.assertTrue((self.root / "runtime").is_dir())
            self.assertEqual((self.root / "runtime").stat().st_mode & 0o777, 0o700)
            return ""

        with patch.object(cleanup, "host_mounts", return_value=[]), patch.object(subject, "run", side_effect=run):
            subject.finish()

    def test_teardown_receipt_cannot_authorize_another_fixture(self):
        subject = cleanup.Cleanup(self.root)
        subject.finish()
        (self.root / cleanup.STOPPED).write_text('{"version":1,"device":0,"inode":0}')
        with self.assertRaisesRegex(RuntimeError, "another root"):
            subject.finish()
        with self.assertRaisesRegex(RuntimeError, "another root"):
            subject.remove_files()
        self.assertTrue(self.root.exists())

    def test_registered_root_can_recover_a_marker_removed_by_old_cleanup(self):
        (self.root / cleanup.MARKER).unlink()
        with tempfile.TemporaryDirectory() as registry:
            entry = Path(registry) / self.root.name
            entry.write_bytes(bytes(self.root))
            output = subprocess.run([sys.executable, str(SCRIPT), "--registry", registry, "--remove-files"], capture_output=True, text=True)
            self.assertEqual(output.returncode, 0, output.stderr)
        self.assertFalse(self.root.exists())

    def test_unknown_mounts_prevent_file_cleanup(self):
        with patch.object(cleanup, "host_mounts", return_value=cleanup.mounts(line(self.root / "unknown", kind="ext4"))):
            with self.assertRaisesRegex(RuntimeError, "still has host mounts"):
                cleanup.Cleanup(self.root).finish()
        self.assertTrue(self.root.exists())

    def test_owned_nonwritable_0755_test_root_is_supported(self):
        self.root.chmod(0o755)
        cleanup.Cleanup(self.root).finish()

    def test_group_or_world_writable_test_roots_are_refused(self):
        for mode in [0o775, 0o777]:
            self.root.chmod(mode)
            with self.assertRaisesRegex(RuntimeError, "unsafe fixture root"):
                cleanup.Cleanup(self.root)
        self.root.chmod(0o700)

    def test_symlinked_storage_cannot_redirect_cleanup_to_real_home(self):
        with tempfile.TemporaryDirectory() as other:
            (self.root / "home").symlink_to(other, target_is_directory=True)
            with self.assertRaisesRegex(RuntimeError, "escapes"):
                cleanup.Cleanup(self.root)

    def test_unmarked_nonfixture_root_is_refused(self):
        (self.root / cleanup.MARKER).unlink()
        with self.assertRaisesRegex(RuntimeError, "not a marked"):
            cleanup.Cleanup(self.root)

    def test_cli_removes_only_confirmed_empty_fixture(self):
        sibling = self.root.parent / (self.root.name + "-peer")
        sibling.mkdir()
        self.addCleanup(sibling.rmdir)
        output = subprocess.run([sys.executable, str(SCRIPT), "--remove-files", str(self.root)], capture_output=True, text=True)
        self.assertEqual(output.returncode, 0, output.stderr)
        self.assertFalse(self.root.exists())
        self.assertTrue(sibling.is_dir())

    def test_registry_cleans_fixture_left_by_an_abruptly_exited_test(self):
        with tempfile.TemporaryDirectory() as registry:
            entry = Path(registry) / self.root.name
            entry.write_bytes(bytes(self.root))
            output = subprocess.run([sys.executable, str(SCRIPT), "--registry", registry, "--remove-files"], capture_output=True, text=True)
            self.assertEqual(output.returncode, 0, output.stderr)
            self.assertFalse(self.root.exists())
            self.assertEqual(list(Path(registry).iterdir()), [])

    def test_empty_registry_is_a_successful_idempotent_cleanup(self):
        with tempfile.TemporaryDirectory() as registry:
            output = subprocess.run([sys.executable, str(SCRIPT), "--registry", registry, "--remove-files"], capture_output=True, text=True)
            self.assertEqual(output.returncode, 0, output.stderr)
            self.assertTrue(self.root.exists())

    def run_supervised(self, action, timeout="15m"):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        environment = dict(os.environ)
        environment.update(
            TMPDIR=temporary.name,
            WRIX_TEST_POOL_PYTHON=sys.executable,
            WRIX_TEST_POOL_CLEANUP_SCRIPT=str(SCRIPT),
            WRIX_TEST_POOL_TEST_TIMEOUT=timeout,
        )
        environment.pop("WRIX_TEST_POOL_CLEANUP_TOOLS", None)
        child = (
            "import os, pathlib, signal, sys, time; "
            "root = pathlib.Path(sys.argv[1]); "
            "registry = pathlib.Path(os.environ['WRIX_TEST_POOL_FIXTURE_REGISTRY']); "
            "(registry / root.name).write_bytes(os.fsencode(root)); "
            "(root / 'ready').touch(); "
        ) + action
        return subprocess.Popen(
            [shutil.which("bash"), str(SCRIPT.with_name("run-native-test.sh")), sys.executable, "-c", child, str(self.root)],
            env=environment, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, start_new_session=True,
        )

    def assert_supervised_cleanup(self, process, expected):
        output, errors = process.communicate(timeout=15)
        self.assertEqual(process.returncode, expected, (output, errors))
        if expected == 0:
            self.assertFalse(self.root.exists(), (output, errors))
        else:
            self.assertTrue(cleanup.Cleanup(self.root).is_stopped(), (output, errors))
            self.assertTrue((self.root / "ready").is_file())
            self.assertIn("diagnostics and registry retained", errors)

    def test_runner_cleans_on_success(self):
        self.assert_supervised_cleanup(self.run_supervised("sys.exit(0)"), 0)

    def test_runner_cleans_after_test_assertion_failure(self):
        self.assert_supervised_cleanup(self.run_supervised("raise AssertionError('fixture failure')"), 1)

    def test_runner_cleans_after_test_process_is_killed(self):
        self.assert_supervised_cleanup(self.run_supervised("os.kill(os.getpid(), signal.SIGKILL)"), 137)

    def test_runner_cleans_on_timeout(self):
        self.assert_supervised_cleanup(self.run_supervised("time.sleep(60)", timeout="0.5s"), 124)

    def test_runner_cleans_on_termination_signal(self):
        process = self.run_supervised("time.sleep(60)")
        try:
            deadline = time.monotonic() + 10
            while not (self.root / "ready").exists():
                if time.monotonic() > deadline or process.poll() is not None:
                    self.fail("supervised test did not register its fixture")
                time.sleep(0.01)
            os.killpg(process.pid, signal.SIGTERM)
            self.assert_supervised_cleanup(process, 143)
        finally:
            if process.poll() is None:
                os.killpg(process.pid, signal.SIGKILL)
                process.communicate()

    def test_external_runtime_failure_retains_files_even_when_removal_requested(self):
        (self.root / "home/.local/share/containers/storage").mkdir(parents=True)
        tools = self.root / "tools"
        tools.mkdir()
        podman = tools / "podman"
        podman.write_text(f"#!{shutil.which('sh')}\nset -eu\nprintf '%s\\n' 'fixture runtime unavailable' >&2\nexit 42\n")
        podman.chmod(0o700)
        output = subprocess.run([sys.executable, str(SCRIPT), "--remove-files", "--tools", str(tools), str(self.root)], capture_output=True, text=True)
        self.assertEqual(output.returncode, 1)
        self.assertIn("fixture runtime unavailable", output.stderr)
        self.assertTrue(self.root.exists())


if __name__ == "__main__":
    unittest.main()
