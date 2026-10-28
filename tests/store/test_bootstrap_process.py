#!/usr/bin/env python3
"""Execute the pool supervisor's actual kernel-process classifier against proc fixtures."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(os.environ.get(
    "WRIX_TEST_BOOTSTRAP_PROCESS_SCRIPT",
    Path(__file__).resolve().parents[2] / "lib/services/nix-pool/bootstrap-process.sh",
))


class BootstrapProcessTest(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.proc = Path(self.directory.name)

    def process(self, pid, parent, uid=0, kernel=0):
        directory = self.proc / str(pid)
        directory.mkdir(exist_ok=True)
        (directory / "status").write_text(
            f"Name:\ttrusted-name-is-not-authority\nPPid:\t{parent}\n"
            f"Uid:\t{uid}\t{uid}\t{uid}\t{uid}\nKthread:\t{kernel}\n"
        )

    def invoke(self, function, pid):
        return subprocess.run(
            ["bash", "-c", 'source "$1"; "$2" "$3" "$4"',
             "bootstrap-test", str(SCRIPT), function, str(pid), str(self.proc)],
            text=True, capture_output=True, check=False,
            env={**os.environ, "LC_ALL": "C"},
        )

    def chain(self, pid):
        return self.invoke("nix_pool_bootstrap_chain", pid)

    def test_kernel_classification_requires_positive_kernel_status_not_root_uid_or_name(self):
        self.process(2, 0, kernel=1)
        self.assertEqual(self.invoke("nix_pool_bootstrap_kernel", 2).returncode, 0)
        self.process(2, 0, kernel=0)
        self.assertNotEqual(self.invoke("nix_pool_bootstrap_kernel", 2).returncode, 0)
        self.process(2, 0, uid=1000, kernel=1)
        self.assertNotEqual(self.invoke("nix_pool_bootstrap_kernel", 2).returncode, 0)

    def test_missing_kernel_type_evidence_does_not_exempt_an_unknown_process(self):
        self.process(2, 0, kernel=1)
        status = self.proc / "2/status"
        status.write_text(status.read_text().replace("Kthread:\t1\n", ""))
        self.assertNotEqual(self.invoke("nix_pool_bootstrap_kernel", 2).returncode, 0)

    def test_root_group_may_retain_kernel_infrastructure_but_no_unaccounted_user_process(self):
        cgroup = self.proc / "cgroup"
        cgroup.mkdir()
        members = cgroup / "cgroup.procs"
        self.process(2, 0, kernel=1)
        self.process(42, 1)
        members.write_text("2\n")
        self.assertEqual(self.invoke("nix_pool_bootstrap_root_kernel", cgroup).returncode, 0)
        for value in ["2\n42\n", "0\n"]:
            members.write_text(value)
            self.assertNotEqual(self.invoke("nix_pool_bootstrap_root_kernel", cgroup).returncode, 0)

    def test_native_vm_init_is_admitted_only_as_a_root_startup_ancestor(self):
        self.process(1, 0)
        self.process(10, 1)
        self.process(11, 10)
        self.process(12, 1)
        result = self.chain(11)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.splitlines(), ["11", "10", "1"])
        self.assertNotIn("12", result.stdout.splitlines())

    def test_direct_pid_one_bootstrap_needs_no_extra_process_exception(self):
        self.process(1, 0)
        result = self.chain(1)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "1\n")

    def test_nonroot_ancestor_cannot_be_admitted_by_its_name_or_pid(self):
        self.process(1, 0)
        self.process(10, 1, uid=1000)
        self.process(11, 10)
        self.assertNotEqual(self.chain(11).returncode, 0)

    def test_missing_and_changed_parent_are_not_assumed_to_be_native_init(self):
        self.process(11, 10)
        self.assertNotEqual(self.chain(11).returncode, 0)
        self.process(10, 0)
        self.assertNotEqual(self.chain(11).returncode, 0)

    def test_cycles_and_oversized_ancestry_fail_closed(self):
        self.process(10, 11)
        self.process(11, 10)
        self.assertNotEqual(self.chain(11).returncode, 0)
        self.process(1, 0)
        for pid in range(2, 11):
            self.process(pid, pid - 1)
        self.assertEqual(self.chain(8).returncode, 0)
        self.assertNotEqual(self.chain(10).returncode, 0)

    def test_malformed_pid_cannot_escape_proc_or_select_process_groups(self):
        for pid in ["0", "-1", "../1", "1/..", "01", "999999999999999999999"]:
            self.assertNotEqual(self.chain(pid).returncode, 0)


if __name__ == "__main__":
    unittest.main()
