import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


LIFECYCLE, CACHE, LIBRARY, SYSTEM, BASH, SETPRIV = sys.argv[1:]
REQUIREMENTS = {
    "platforms": ["aarch64-linux", "x86_64-linux"],
    "capabilities": ["virtiofsd-capabilities"],
}
CAPABILITIES = {
    "CAP_CHOWN": 0, "CAP_DAC_OVERRIDE": 1, "CAP_DAC_READ_SEARCH": 2,
    "CAP_FOWNER": 3, "CAP_FSETID": 4, "CAP_SETGID": 6, "CAP_SETUID": 7,
    "CAP_MKNOD": 27, "CAP_SETFCAP": 31,
}
REQUIRED_BITS = sum(1 << bit for bit in CAPABILITIES.values())


class VmPrerequisites(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="wrix-vm-prerequisite-", dir="/tmp")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.root.chmod(0o777)
        self.calls = self.root / "calls"
        self.builds = self.root / "builds"
        for path in (self.calls, self.builds):
            path.touch(mode=0o666)
            path.chmod(0o666)
        self.env = dict(
            os.environ, WRIX_TEST_VM_ROOT=str(self.root),
            WRIX_TEST_VM_CALLS=str(self.calls), WRIX_TEST_VM_BUILDS=str(self.builds),
            WRIX_TEST_VM_STATUS="pass", WRIX_TEST_VM_BUILD_STATUS="pass",
            GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_NOSYSTEM="1",
            TMPDIR="/tmp",
        )
        for name in subprocess.check_output(["git", "rev-parse", "--local-env-vars"], text=True).splitlines():
            self.env.pop(name, None)
        self.env.update(GIT_CONFIG_COUNT="1", GIT_CONFIG_KEY_0="safe.directory", GIT_CONFIG_VALUE_0=str(self.root))
        subprocess.run(["git", "init", "-q", str(self.root)], env=self.env, check=True)

    def helper(self, uid, bounding, permitted="0000000000000000", inheritable="0000000000000000"):
        return subprocess.run(
            [BASH + "/bin/bash", "-c", 'source "$1"; verifier_virtiofsd_capability_set "$2" "$3" "$4" "$5"',
             "fixture", LIBRARY, str(uid), bounding, permitted, inheritable],
            capture_output=True, text=True, check=False,
        )

    def test_each_missing_root_capability_has_a_specific_supported_reason(self):
        for name, bit in CAPABILITIES.items():
            with self.subTest(capability=name):
                result = self.helper(0, f"{REQUIRED_BITS & ~(1 << bit):016x}")
                self.assertEqual(result.returncode, 77, result.stderr)
                reason = json.loads(result.stdout)
                self.assertEqual(reason["kind"], "missing-capability")
                self.assertEqual(reason["capability"], "virtiofsd-capabilities")
                self.assertIn(name, reason["reason"])

    def test_complete_root_capability_set_remains_enabled_without_kvm(self):
        result = self.helper(0, f"{REQUIRED_BITS:016x}")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "")

    def test_permitted_or_inheritable_capabilities_prevent_a_false_bounding_skip(self):
        for permitted, inheritable in ((f"{REQUIRED_BITS:016x}", "0000000000000000"),
                                       ("0000000000000000", f"{REQUIRED_BITS:016x}")):
            with self.subTest(permitted=permitted, inheritable=inheritable):
                result = self.helper(0, "0000000000000000", permitted, inheritable)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(result.stdout, "")

    def test_unprivileged_virtiofsd_does_not_install_root_capabilities(self):
        result = self.helper(65534, "0000000000000000")
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_invalid_kernel_capability_data_fails_instead_of_skipping(self):
        for invalid in ("", "not-hex", "00000000800405fb extra", "0"):
            for index in range(3):
                with self.subTest(invalid=invalid, index=index):
                    masks = [f"{REQUIRED_BITS:016x}"] * 3
                    masks[index] = invalid
                    result = self.helper(0, *masks)
                    self.assertEqual(result.returncode, 1)
                    self.assertIn("invalid Linux capability", result.stderr)
                    self.assertEqual(result.stdout, "")

    def test_kernel_preflight_retains_metadata_and_never_executes_when_unavailable(self):
        result = subprocess.run(
            [BASH + "/bin/bash", "-c", 'source "$1"; verifier_run fixture "$2" touch "$3"',
             "fixture", LIBRARY, json.dumps(REQUIREMENTS), str(self.root / "executed")],
            env=self.env, capture_output=True, text=True, check=False,
        )
        masks = {line.split()[0]: int(line.split()[1], 16)
                 for line in Path("/proc/self/status").read_text().splitlines()
                 if line.startswith(("CapBnd:", "CapPrm:", "CapInh:"))}
        available_bits = masks["CapBnd:"] | masks["CapPrm:"] | masks["CapInh:"]
        available = os.geteuid() != 0 or available_bits & REQUIRED_BITS == REQUIRED_BITS
        self.assertEqual(result.returncode, 0 if available else 77, result.stderr)
        record = json.loads(result.stdout)
        self.assertEqual(record["execution"], dict(REQUIREMENTS, platform=SYSTEM))
        self.assertEqual(record["outcome"], "passed" if available else "skipped")
        self.assertEqual((self.root / "executed").exists(), available)
        if not available:
            self.assertEqual(record["skip_reason"]["capability"], "virtiofsd-capabilities")
            for name, mask in masks.items():
                self.assertIn(f"{name[:-1]}={mask:016x}", record["skip_reason"]["reason"])

    def test_direct_app_checks_prerequisites_before_driver_preparation(self):
        probe = subprocess.run(
            [BASH + "/bin/bash", "-c", 'source "$1"; verifier_preflight "$2" "$3"',
             "fixture", LIBRARY, SYSTEM, json.dumps(REQUIREMENTS)],
            capture_output=True, text=True, check=False,
        )
        self.assertIn(probe.returncode, (0, 77), probe.stderr)
        for package in (LIFECYCLE, CACHE):
            with self.subTest(package=package):
                self.builds.write_text("")
                result = subprocess.run(
                    [package + "/bin/" + Path(package).name.split("-", 1)[1]],
                    cwd=self.root, env=dict(self.env, WRIX_TEST_VM_BUILD_STATUS="19"),
                    capture_output=True, text=True, check=False,
                )
                self.assertEqual(result.returncode, 77 if probe.returncode == 77 else 1, result.stderr)
                self.assertEqual(bool(self.builds.read_text()), probe.returncode == 0)
                self.assertEqual(self.calls.read_text(), "")
                if probe.returncode == 77:
                    self.assertEqual(json.loads(result.stderr), json.loads(probe.stdout))

    def execute_wrapper(self, package, guest="pass", build="pass"):
        prefix = [] if os.geteuid() != 0 else [SETPRIV, "--reuid=65534", "--regid=65534", "--clear-groups"]
        env = dict(self.env, WRIX_TEST_VM_STATUS=guest, WRIX_TEST_VM_BUILD_STATUS=build)
        return subprocess.run(
            [*prefix, BASH + "/bin/bash", "-c", 'source "$1"; verifier_run fixture "$2" "$3"',
             "fixture", LIBRARY, json.dumps(REQUIREMENTS),
             package + "/bin/" + Path(package).name.split("-", 1)[1]],
            cwd=self.root, env=env, capture_output=True, text=True, check=False,
        )

    def test_capable_execution_builds_then_runs_the_driver_in_isolated_scratch(self):
        for package, test in ((LIFECYCLE, "services-devshell-start-independent"),
                              (CACHE, "services-limit-mode-cache-endpoint")):
            with self.subTest(test=test):
                self.calls.write_text("")
                self.builds.write_text("")
                result = self.execute_wrapper(package)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(json.loads(result.stdout)["outcome"], "passed")
                self.assertEqual(self.builds.read_text().splitlines(),
                                 [f"{self.root}#legacyPackages.{SYSTEM}.systemTests.{test}.driver"])
                directories = self.calls.read_text().splitlines()
                self.assertEqual(len(directories), 1)
                self.assertNotEqual(directories[0], str(self.root))
                self.assertFalse(Path(directories[0]).exists())

    def test_guest_assertion_failure_is_not_classified_from_its_log(self):
        result = self.execute_wrapper(LIFECYCLE, guest="fail")
        self.assertEqual(result.returncode, 1, result.stderr)
        record = json.loads(result.stdout)
        self.assertEqual(record["outcome"], "failed")
        self.assertIn("can't apply the child capabilities", record["evidence"])
        self.assertNotIn("skip_reason", record)
        self.assertEqual(len(self.calls.read_text().splitlines()), 1)
        self.assertFalse(Path(self.calls.read_text().strip()).exists())

    def test_guest_exit_77_is_a_failure_not_a_prerequisite_skip(self):
        result = self.execute_wrapper(LIFECYCLE, guest="skip")
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertEqual(json.loads(result.stdout)["outcome"], "failed")

    def test_driver_build_failures_never_execute_or_report_a_skip(self):
        for status in (19, 77):
            with self.subTest(status=status):
                result = self.execute_wrapper(LIFECYCLE, build=str(status))
                self.assertEqual(result.returncode, 1, result.stderr)
                record = json.loads(result.stdout)
                self.assertEqual(record["outcome"], "failed")
                self.assertIn("fixture driver build", record["evidence"])
                self.assertNotIn("skip_reason", record)
                self.assertEqual(self.calls.read_text(), "")


unittest.main(argv=[sys.argv[0]])
