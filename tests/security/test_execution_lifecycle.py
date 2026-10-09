"""Packaged host/container evidence; journal state-machine tests remain in Rust."""

import json
import os
from pathlib import Path
import platform
import shlex
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import unittest


HERE = Path(__file__).resolve().parent
MODE = sys.argv.pop(1)
RUNNER = sys.argv.pop(1) if MODE == "fixtures" else None
LIMIT = 120


def wait_for(path, process):
    deadline = time.monotonic() + LIMIT
    while not path.exists():
        if process.poll() is not None:
            raise AssertionError(f"process exited {process.returncode} before {path}")
        if time.monotonic() >= deadline:
            raise AssertionError(f"timed out waiting for {path}")
        time.sleep(0.05)


def stop_group(process):
    if getattr(process, "execution_group_stopped", False):
        return
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass  # The whole process group has already exited.
    process.wait(timeout=30)
    process.execution_group_stopped = True


class FixtureConformance(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="wrix-execution-fixture-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)

    def test_runtime_non_run_calls_preserve_real_command_stdio_and_status(self):
        args = ["-c", 'IFS= read -r line; printf "%s:%s\\n" "$1" "$line"; echo diagnostic >&2; exit 37',
                "fixture", "argument with spaces"]
        expected = subprocess.run(["bash", *args], input="input\n", text=True, capture_output=True)
        actual = subprocess.run(["bash", str(HERE / "execution-runtime.sh"), *args],
                                env=dict(os.environ, WRIX_TEST_REAL_RUNTIME=shutil.which("bash")),
                                input="input\n", text=True, capture_output=True, timeout=LIMIT)
        self.assertEqual((actual.returncode, actual.stdout, actual.stderr),
                         (expected.returncode, expected.stdout, expected.stderr))

    def test_runtime_run_gate_names_and_execs_external_runtime_without_other_mutation(self):
        delegate = self.root / "external-runtime"
        delegate.write_text(f"#!{sys.executable}\nimport json, sys\n"
                            "print(json.dumps(sys.argv[1:]))\n"
                            "print(sys.stdin.read(), end='')\n"
                            "print('runtime diagnostic', file=sys.stderr)\nsys.exit(37)\n")
        delegate.chmod(0o755)
        args = ["run", "--rm", "image:tag", "space argument", ""]
        expected = subprocess.run([str(delegate), "run", "--name", "fixture-name", *args[1:]],
                                  input="stream\n", capture_output=True, text=True)
        process = subprocess.Popen(["bash", str(HERE / "execution-runtime.sh"), *args],
                                   env=dict(os.environ, WRIX_TEST_REAL_RUNTIME=str(delegate),
                                            WRIX_TEST_CONTROL=str(self.root),
                                            WRIX_TEST_CONTAINER_NAME="fixture-name"),
                                   stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                   stderr=subprocess.PIPE, text=True, start_new_session=True)
        self.addCleanup(stop_group, process)
        wait_for(self.root / "ready", process)
        self.assertIsNone(process.poll())
        self.assertEqual(int((self.root / "host-pid").read_text()), os.getpid())
        self.assertEqual(int((self.root / "runtime-pid").read_text()), process.pid)
        (self.root / "release").touch()
        stdout, stderr = process.communicate("stream\n", timeout=LIMIT)
        self.assertEqual((process.returncode, stdout, stderr),
                         (expected.returncode, expected.stdout, expected.stderr))

    def test_packaged_direct_runner_preserves_consumer_argv_stdio_and_status(self):
        command = ["bash", "-c",
                   'IFS= read -r line; printf "%s|%s|%s\\n" "$1" "$2" "$line"; echo diagnostic >&2; exit 37',
                   "consumer", "two words", ""]
        expected = subprocess.run(command, input="input\n", text=True, capture_output=True)
        actual = subprocess.run([RUNNER, *command], input="input\n", text=True,
                                capture_output=True, timeout=LIMIT)
        self.assertEqual((actual.returncode, actual.stdout, actual.stderr),
                         (expected.returncode, expected.stdout, expected.stderr))

    def test_packaged_direct_runner_preserves_probe_gate_output_and_exit(self):
        for status in (0, 37):
            member = f"member-{status}"
            command = ["bash", str(HERE / "execution-probe.sh"), str(self.root), member, str(status)]
            process = subprocess.Popen([RUNNER, *command], stdout=subprocess.PIPE,
                                       stderr=subprocess.PIPE, text=True, start_new_session=True)
            self.addCleanup(stop_group, process)
            wait_for(self.root / f"ready-{member}", process)
            self.assertIsNone(process.poll())
            (self.root / f"exit-{member}").touch()
            stdout, stderr = process.communicate(timeout=LIMIT)
            expected = subprocess.run(command, capture_output=True, text=True, timeout=LIMIT)
            self.assertEqual((process.returncode, stdout, stderr),
                             (expected.returncode, expected.stdout, expected.stderr))


class LiveExecution(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.system = subprocess.check_output(["nix", "eval", "--raw", "--impure", "--expr",
                                              "builtins.currentSystem"], text=True).strip()
        fixtures = f".#legacyPackages.{cls.system}.testFixtures.execution"
        def build(target):
            return Path(subprocess.check_output([
                "nix", "build", "--no-link", "--print-out-paths", "--no-warn-dirty", target],
                text=True, timeout=1800).strip())

        cls.launcher = build(".#wrix") / "bin/wrix"
        cls.packaged_profiles = {
            "direct": build(f"{fixtures}.profileConfig"),
            "claude": build(".#sandbox-claude.profileConfig"),
            "pi": build(".#sandbox-pi.profileConfig"),
        }
        cls.runtime_name = "podman" if platform.system() == "Linux" else "container"
        cls.runtime = shutil.which(cls.runtime_name)

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="wrix-execution-live-")
        self.root = Path(self.temp.name)
        self.addCleanup(self.temp.cleanup)
        self.launches = []
        self.addCleanup(self.cleanup_launches)
        self.home = self.root / "home"
        self.home.mkdir()
        self.bin = self.root / "bin"
        self.bin.mkdir()
        (self.bin / self.runtime_name).symlink_to(HERE / "execution-runtime.sh")
        self.auth = self.root / "auth.json"
        self.auth.write_text("{}\n")
        self.auth.chmod(0o600)
        self.profiles = {}
        for agent, source in self.packaged_profiles.items():
            profile = json.loads(source.read_text())
            profile["services"]["nix_cache"]["enable"] = False
            path = self.root / f"profile-{agent}.json"
            path.write_text(json.dumps(profile))
            self.profiles[agent] = path

    def runtime_call(self, *args, check=True):
        return subprocess.run([self.runtime, *args], capture_output=True, text=True,
                              timeout=30, check=check)

    def remove_container(self, launch):
        if self.runtime_name == "podman":
            present = self.runtime_call("container", "exists", launch["name"], check=False)
            if present.returncode == 1:
                return  # --rm already removed this container, or run never created it.
            self.assertEqual(present.returncode, 0, present.stderr)
            self.runtime_call("rm", "--force", "--time", "0", launch["name"])
        else:
            listing = self.runtime_call("list", "--all", "--format", "json")
            names = [entry["configuration"]["id"] for entry in json.loads(listing.stdout)]
            if launch["name"] in names:
                self.runtime_call("delete", "--force", launch["name"])

    def cleanup_launches(self):
        failures = []
        for launch in self.launches:
            try:
                stop_group(launch["process"])
            except (subprocess.SubprocessError, OSError) as error:
                failures.append(str(error))
            try:
                self.remove_container(launch)
            except (AssertionError, subprocess.SubprocessError, KeyError, ValueError, OSError) as error:
                failures.append(str(error))
        self.assertFalse(failures, "independent container cleanup failed: " + "; ".join(failures))

    def workspace(self, name):
        workspace = self.root / name
        workspace.mkdir()
        shutil.copyfile(HERE / "execution-probe.sh", workspace / "probe.sh")
        return workspace

    def start(self, mode, workspace, agent="direct", status=0, profile=None, release=True):
        member = f"launch-{len(self.launches)}"
        control = self.root / member
        control.mkdir()
        name = f"wrix-execution-{os.getpid()}-{member}"
        args = (["bash", "/workspace/probe.sh", "/workspace", member, str(status)]
                if agent == "direct" else ["--version" if agent == "claude" else "--help"])
        env = {key: value for key, value in os.environ.items()
               if not key.startswith(("WRIX_", "BEADS_DOLT_SERVER_"))
               and key not in ("TMUX", "TMUX_PANE")}
        env.update(HOME=str(self.home), XDG_CACHE_HOME=str(self.root / "cache"),
                   WRIX_PI_AUTH_FILE=str(self.auth), WRIX_FOCUS_TARGET=f"focus:{member}",
                   WRIX_TEST_REAL_RUNTIME=self.runtime, WRIX_TEST_CONTROL=str(control),
                   WRIX_TEST_CONTAINER_NAME=name, PATH=f"{self.bin}:{os.environ['PATH']}")
        command = [str(self.launcher), "--profile-config", str(profile or self.profiles[agent])]
        if mode == "spawn":
            config = control / "spawn.json"
            config.write_text(json.dumps(dict(workspace=str(workspace), env=[], mounts=[],
                                              git=dict(deploy=False, sign=False),
                                              bead_id=member, agent_args=args)))
            command += ["spawn", "--spawn-config", str(config)]
        else:
            command += ["run", "--no-git-deploy", "--no-git-sign", str(workspace), "--", *args]
            rendered = shlex.join(command)
            command = (["script", "-qefc", rendered, "/dev/null"] if self.runtime_name == "podman"
                       else ["script", "-q", "/dev/null", "bash", "-c", rendered])
        log = control / "output"
        with log.open("wb") as output:
            process = subprocess.Popen(command, stdin=subprocess.DEVNULL, stdout=output,
                                       stderr=subprocess.STDOUT, env=env, start_new_session=True)
        launch = dict(process=process, control=control, name=name, member=member,
                      workspace=workspace, mode=mode, agent=agent, log=log)
        self.launches.append(launch)
        try:
            wait_for(control / "ready", process)
        except AssertionError:
            self.fail(log.read_text())
        records = list((workspace / ".wrix/log").glob("*.json"))
        record = next(path for path in records if json.loads(path.read_text())["focus_target"] == f"focus:{member}")
        launch.update(record=record, initial=record.read_bytes())
        value = json.loads(launch["initial"])
        self.assertEqual(value["state"], "incomplete")
        self.assertEqual(value["execution_id"] + ".json", record.name)
        for key in ("timestamp_end", "duration_seconds", "exit_code", "signal", "agent_session_id"):
            self.assertIsNone(value[key])
        self.assertEqual(value["mode"], mode)
        self.assertEqual(value["agent_kind"], agent)
        self.assertEqual(value["bead_id"], member if mode == "spawn" else None)
        if release:
            (control / "release").touch()
        return launch

    def finish(self, launch, expected):
        actual = launch["process"].wait(timeout=LIMIT)
        if self.runtime_name == "podman" or launch["mode"] == "spawn":
            self.assertEqual(actual, expected, launch["log"].read_text())
        value = json.loads(launch["record"].read_text())
        initial = json.loads(launch["initial"])
        self.assertEqual(value["state"], "completed", launch["log"].read_text())
        self.assertEqual(value["exit_code"], expected)
        self.assertIsNone(value["signal"])
        self.assertIsInstance(value["timestamp_end"], str)
        self.assertGreaterEqual(value["duration_seconds"], 0)
        for key in initial.keys() - {"state", "timestamp_end", "duration_seconds", "exit_code", "signal"}:
            self.assertEqual(value[key], initial[key])
        return value

    def test_normal_runtime_exits_complete_the_original_record(self):
        for mode in ("run", "spawn"):
            for status in (0, 37):
                with self.subTest(mode=mode, status=status):
                    workspace = self.workspace(f"normal-{mode}-{status}")
                    launch = self.start(mode, workspace, status=status)
                    wait_for(workspace / f"ready-{launch['member']}", launch["process"])
                    self.assertEqual(launch["record"].read_bytes(), launch["initial"])
                    self.runtime_call("inspect", launch["name"])
                    (workspace / f"exit-{launch['member']}").touch()
                    value = self.finish(launch, status)
                    self.assertIsNone(value["agent_session_dir"])
                    self.assertEqual(len(list((workspace / ".wrix/log").iterdir())), 1)

    def test_observed_container_startup_failures_complete_the_original_record(self):
        profile = json.loads(self.profiles["direct"].read_text())
        profile["agent"]["kind"] = "claude"
        mismatch = self.root / "mismatched-profile.json"
        mismatch.write_text(json.dumps(profile))
        for mode in ("run", "spawn"):
            with self.subTest(mode=mode):
                workspace = self.workspace(f"failure-{mode}")
                launch = self.start(mode, workspace, agent="claude", profile=mismatch)
                self.finish(launch, 1)
                self.assertIn("this image was built for agent=direct", launch["log"].read_text())
                self.assertFalse((workspace / f"ready-{launch['member']}").exists())
                self.assertEqual(len(list((workspace / ".wrix/log").iterdir())), 1)

    def test_killing_the_host_launcher_leaves_unknown_completion_even_after_container_cleanup(self):
        for mode in ("run", "spawn"):
            with self.subTest(mode=mode):
                workspace = self.workspace(f"killed-{mode}")
                launch = self.start(mode, workspace)
                wait_for(workspace / f"ready-{launch['member']}", launch["process"])
                self.runtime_call("inspect", launch["name"])
                host_pid = int((launch["control"] / "host-pid").read_text())
                os.kill(host_pid, signal.SIGKILL)
                self.assertEqual(launch["record"].read_bytes(), launch["initial"])
                self.remove_container(launch)
                stop_group(launch["process"])
                self.assertEqual(launch["record"].read_bytes(), launch["initial"])
                self.assertEqual(len(list((workspace / ".wrix/log").iterdir())), 1)

    def test_concurrent_packaged_agents_do_not_adopt_shared_transcript_conversations(self):
        for mode in ("run", "spawn"):
            with self.subTest(mode=mode):
                workspace = self.workspace(f"shared-{mode}")
                claude = workspace / ".claude"
                pi = workspace / ".pi/agent/sessions"
                claude.mkdir()
                pi.mkdir(parents=True)
                history = claude / "history.jsonl"
                history.write_text('{"sessionId":"unrelated-claude-conversation"}\n')
                (pi / "unrelated.jsonl").write_text('{"type":"session","id":"unrelated-pi-conversation"}\n')
                first = self.start(mode, workspace, agent="claude", release=False)
                second = self.start(mode, workspace, agent="pi", release=False)
                with history.open("a") as transcript:
                    transcript.write('{"sessionId":"concurrent-unrelated-conversation"}\n')
                (first["control"] / "release").touch()
                (second["control"] / "release").touch()
                for launch, session_dir in ((first, "/workspace/.claude"),
                                            (second, "/workspace/.pi/agent/sessions")):
                    value = self.finish(launch, 0)
                    self.assertIsNone(value["agent_session_id"])
                    self.assertEqual(value["agent_session_dir"], session_dir)
                    self.assertTrue((workspace / session_dir.removeprefix("/workspace/")).is_dir())
                    self.assertNotIn("unrelated", launch["record"].read_text())
                    for retired in ("claude_session_id", "wrix_session_id", "claude_session_dir"):
                        self.assertNotIn(retired, value)
                self.assertNotEqual(first["record"], second["record"])
                self.assertEqual(len(list((workspace / ".wrix/log").iterdir())), 2)


if __name__ == "__main__":
    suite = unittest.defaultTestLoader.loadTestsFromTestCase(
        FixtureConformance if MODE == "fixtures" else LiveExecution)
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    sys.exit(0 if result.wasSuccessful() else 1)
