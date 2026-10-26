#!/usr/bin/env python3
"""Verify native interruption and teardown without a manual timing window."""

import argparse
from datetime import datetime, timezone
import importlib.util
import json
import math
import os
from pathlib import Path
import re
import selectors
import signal
import stat
import subprocess
import sys
import tempfile
import time
import uuid

sys.dont_write_bytecode = True
READY = ".wrix-native-test-ready.json"
NONCE = "WRIX_TEST_POOL_INTERRUPT_NONCE"


def cleanup_module(path):
    spec = importlib.util.spec_from_file_location("native_cleanup", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def write_json(path, value):
    temporary = path.with_suffix(".pending")
    with temporary.open("w") as stream:
        json.dump(value, stream, indent=2)
        stream.write("\n")
    temporary.replace(path)


def checkpoint(cleanup, registry, nonce):
    entries = cleanup.registry_entries(registry)
    if len(entries) > 1:
        raise RuntimeError("interruption requires exactly one disposable fixture")
    for root in entries:
        cleanup.checked_root(root, registered=True)
        try:
            descriptor = os.open(root / READY, os.O_RDONLY | os.O_NOFOLLOW)
        except FileNotFoundError:
            return None
        with os.fdopen(descriptor) as stream:
            info = os.fstat(stream.fileno())
            if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or stat.S_IMODE(info.st_mode) != 0o600 or info.st_nlink != 1 or info.st_size > 4096:
                raise RuntimeError("unsafe native readiness checkpoint")
            record = json.load(stream)
        identity = root.stat()
        if record != {"version": 1, "phase": "workers-ready", "nonce": nonce, "device": identity.st_dev, "inode": identity.st_ino}:
            raise RuntimeError("foreign or malformed native readiness checkpoint")
        return root
    return None


def session_members(leader):
    members = []
    for proc in Path("/proc").iterdir():
        if not proc.name.isdecimal() or int(proc.name) == leader:
            continue
        try:
            fields = (proc / "stat").read_text(errors="surrogateescape").rsplit(")", 1)[1].split()
        except (FileNotFoundError, ProcessLookupError):
            continue  # A child can exit between enumeration and observation.
        if int(fields[3]) != leader:
            continue
        active = fields[0] != "Z"
        if not active:
            # A thread-group leader can be a zombie while its other threads run.
            try:
                for task in (proc / "task").iterdir():
                    try:
                        active |= (task / "stat").read_text(errors="surrogateescape").rsplit(")", 1)[1].split()[0] != "Z"
                    except (FileNotFoundError, ProcessLookupError):
                        continue  # This thread exited during observation.
            except (FileNotFoundError, ProcessLookupError):
                continue  # The entire thread group was reaped.
        if active:
            members.append(int(proc.name))
    return members


def signal_session_members(leader, kind):
    signalled = []
    for pid in session_members(leader):
        try:
            descriptor = os.pidfd_open(pid)
        except ProcessLookupError:
            continue  # The owned helper exited before its pidfd was acquired.
        try:
            if pid in session_members(leader):
                try:
                    signal.pidfd_send_signal(descriptor, kind)
                    signalled.append(pid)
                except ProcessLookupError:
                    pass  # The pidfd cannot signal a reused PID.
        finally:
            os.close(descriptor)
    return signalled


class Supervisor:
    """Keep the private session leader unreaped until its helpers have exited."""

    def __init__(self, command, environment, log, stop_timeout):
        self.process = subprocess.Popen(command, env=environment, stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
        self.reaped = False
        self.stop_timeout = stop_timeout
        self.stop_deadline = None
        self.exit_code = None

    def exited(self):
        result = os.waitid(os.P_PID, self.process.pid, os.WEXITED | os.WNOHANG | os.WNOWAIT)
        if result is not None:
            self.exit_code = result.si_status if result.si_code == os.CLD_EXITED else -result.si_status
        return result is not None

    def signal(self):
        if self.exited():
            raise RuntimeError("native supervisor exited before the planned signal")
        self.stop_deadline = time.monotonic() + self.stop_timeout
        os.kill(self.process.pid, signal.SIGTERM)

    def stop_helpers(self, deadline, progress):
        if not self.exited():
            raise RuntimeError("cannot reap assertion helpers before supervisor teardown")
        members = signal_session_members(self.process.pid, signal.SIGTERM)
        if members:
            progress(f"Stopping {len(members)} assertion helpers in the owned private session")
        deadline = min(deadline, time.monotonic() + 10)
        while session_members(self.process.pid) and time.monotonic() < deadline:
            time.sleep(0.05)
        if session_members(self.process.pid):
            raise RuntimeError("owned assertion helpers did not exit gracefully")
        return members

    def finish(self):
        if not self.exited():
            raise RuntimeError("owned native supervisor has not exited")
        if session_members(self.process.pid):
            raise RuntimeError("native assertion helpers outlived their supervisor")
        result = self.process.wait()
        self.reaped = True
        return result

    def abort(self, progress):
        if self.reaped:
            return
        if not self.exited() and self.stop_deadline is None:
            self.signal()
        deadline = self.stop_deadline or time.monotonic()
        next_report = 0
        while not self.exited() and time.monotonic() < deadline:
            if time.monotonic() >= next_report:
                progress("Stopping owned invocation after failure (bounded cleanup)")
                next_report = time.monotonic() + 15
            time.sleep(0.1)
        forced = not self.exited()
        if forced:
            os.kill(self.process.pid, signal.SIGKILL)
        # The unreaped session leader prevents reuse of this private session identity.
        for kind in (signal.SIGTERM, signal.SIGKILL):
            signal_session_members(self.process.pid, kind)
            deadline = time.monotonic() + 5
            while session_members(self.process.pid) and time.monotonic() < deadline:
                time.sleep(0.05)
        deadline = time.monotonic() + 5
        while not self.exited() and time.monotonic() < deadline:
            time.sleep(0.05)
        self.finish()
        if forced:
            raise RuntimeError("supervisor cleanup exceeded its deadline; private processes killed, fixture resources unverified")


class Witness:
    """Retain the original Podman mount namespace across pause-process retirement."""

    def __init__(self, subject, script, cleanup_path, log, progress):
        self.progress = progress
        command = subject.podman + ["unshare", sys.executable, str(script), "--witness", "--root", str(subject.root), "--cleanup", str(cleanup_path)]
        if subject.tools:
            command += ["--tools", str(subject.tools)]
        self.process = subprocess.Popen(command, env=subject.environment, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=log, start_new_session=True)
        os.set_blocking(self.process.stdout.fileno(), False)
        self.buffer = b""

    def snapshot(self):
        self.process.stdin.write(b"snapshot\n")
        self.process.stdin.flush()
        deadline = time.monotonic() + 60
        with selectors.DefaultSelector() as selector:
            selector.register(self.process.stdout, selectors.EVENT_READ)
            while b"\n" not in self.buffer:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise TimeoutError("original Podman namespace inspection timed out")
                if not selector.select(min(remaining, 15)):
                    self.progress("Inspecting retained original Podman namespace (bounded)")
                    continue
                chunk = os.read(self.process.stdout.fileno(), 65536)
                if not chunk:
                    raise RuntimeError("original Podman namespace witness exited")
                self.buffer += chunk
                if len(self.buffer) > 8 * 1024 * 1024:
                    raise RuntimeError("oversized namespace snapshot")
        line, self.buffer = self.buffer.split(b"\n", 1)
        record = json.loads(line)
        if record.get("version") != 1 or "error" in record:
            raise RuntimeError(f"namespace inspection failed: {record}")
        return record

    def close(self):
        if self.process.returncode is not None:
            return
        self.progress("Releasing the original namespace observer")
        try:
            self.process.stdin.write(b"exit\n")
            self.process.stdin.flush()
        except BrokenPipeError:
            pass  # A failed namespace witness may already have exited; wait still observes its status.
        try:
            result = self.process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.progress("Namespace observer timed out; terminating only its owned process group")
            os.killpg(self.process.pid, signal.SIGTERM)
            try:
                self.process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.progress("Namespace observer ignored TERM; killing only its owned process group")
                os.killpg(self.process.pid, signal.SIGKILL)
                self.process.wait(timeout=5)
            raise RuntimeError("namespace witness required termination")
        finally:
            self.process.stdin.close()
            self.process.stdout.close()
        if result:
            raise RuntimeError(f"namespace witness exited {result}")


def witness_main(args, cleanup):
    subject = cleanup.Cleanup(args.root, args.tools, registered=True)
    for command in sys.stdin:
        if command == "exit\n":
            return 0
        if command != "snapshot\n":
            raise RuntimeError("unsupported namespace observation request")
        record = {"version": 1}
        try:
            record["namespace"] = subject.run([subject.program("readlink"), "/proc/self/ns/mnt"], timeout=10).strip()
            record["mountinfo"] = subject.run([subject.program("cat"), "/proc/self/mountinfo"], timeout=10)
            for label, arguments in [
                ("running", ["ps", "--no-trunc", "--format", "{{.ID}}"]),
                ("containers", ["ps", "--all", "--no-trunc", "--format", "{{.ID}}"]),
                ("images", ["images", "--no-trunc", "--format", "{{.ID}}"]),
            ]:
                record[label] = subject.run(subject.podman + arguments, timeout=10).splitlines()
        except (OSError, RuntimeError, subprocess.TimeoutExpired) as error:
            record["error"] = str(error)
        print(json.dumps(record), flush=True)
    return 0


def verify_before(subject, snapshot, cleanup):
    namespace = snapshot.get("namespace", "")
    if not re.fullmatch(r"mnt:\[\d+\]", namespace) or namespace == os.readlink("/proc/self/ns/mnt"):
        raise RuntimeError("Podman witness is not in its own mount namespace")
    running = snapshot.get("running", [])
    if len(set(running)) < 3 or any(not re.fullmatch(r"[0-9a-f]{64}", item) for item in running):
        raise RuntimeError("native pool and two workers are not all running")
    if len(set(subject.views(cleanup.mounts(snapshot["mountinfo"])))) < 2:
        raise RuntimeError("two native worker exports are not mounted in Podman's namespace")
    subject.check_host_mounts()


def verify_after(subject, before, after, cleanup):
    if after["namespace"] != before["namespace"]:
        raise RuntimeError("namespace changed: refusing to inspect a replacement empty namespace")
    if any(after[label] for label in ("running", "containers", "images")):
        raise RuntimeError("fixture still owns containers or images after teardown")
    subject.check_namespace_mounts(cleanup.mounts(after["mountinfo"]))
    subject.check_host_mounts()
    if not subject.is_stopped():
        raise RuntimeError("native supervisor did not record successful teardown")


def run(args, cleanup):
    parent = Path.cwd() / ".loom/scratch/linux-acceptance"
    if args.output:
        output = args.output.resolve()
        output.mkdir(mode=0o700)
    else:
        parent.mkdir(parents=True, exist_ok=True)
        output = Path(tempfile.mkdtemp(prefix=datetime.now(timezone.utc).strftime("interruption-%Y%m%d-%H%M%S-"), dir=parent)).resolve()
    registry = output / "registry"
    registry.mkdir(mode=0o700)
    nonce = uuid.uuid4().hex + uuid.uuid4().hex
    environment = dict(os.environ, **{NONCE: nonce})
    command = args.command
    if command[:1] == ["--"]:
        command = command[1:]
    if len(command) < 3:
        raise RuntimeError("specify bash, run-native-test.sh and the native test command")
    command = command[:2] + ["--registry", str(registry)] + command[2:]
    started = time.monotonic()
    result = {"version": 1, "outcome": "failed", "command": command, "nonce": nonce, "registry": str(registry), "output": str(output), "revision": args.source_revision, "host_namespace": os.readlink("/proc/self/ns/mnt"), "started_at": datetime.now(timezone.utc).isoformat()}
    supervisor = None
    witness = None

    def progress(message):
        print(f"[{time.monotonic() - started:6.1f}s] {message}", flush=True)

    def wait_until(predicate, deadline, phase):
        next_report = 0
        while not predicate():
            now = time.monotonic()
            if now >= deadline:
                raise TimeoutError(f"{phase} exceeded its deadline")
            if now >= next_report:
                progress(phase)
                next_report = now + 15
            time.sleep(0.1)

    try:
        revision = subprocess.run(["git", "rev-parse", "HEAD"], capture_output=True, text=True, check=True)
        result["checkout_revision"] = revision.stdout.strip()
        if not re.fullmatch(r"[0-9a-f]{40}", args.source_revision):
            raise RuntimeError("native acceptance requires a clean Git-filtered, revision-bound app build")
        progress(f"Starting native interruption test; full diagnostics: {output}")
        with (output / "native.log").open("wb") as log, (output / "namespace.log").open("wb") as ns_log:
            supervisor = Supervisor(command, environment, log, args.stop_timeout)
            result["supervisor_pid"] = supervisor.process.pid
            root = None

            def ready():
                nonlocal root
                if supervisor.exited():
                    result["native_exit"] = supervisor.finish()
                    raise RuntimeError("native test exited before verified readiness")
                root = checkpoint(cleanup, registry, nonce)
                return root is not None

            wait_until(ready, started + args.ready_timeout, "Waiting for pool and two workers (readiness deadline applies)")
            result["fixture"] = str(root)
            result["ready_elapsed_seconds"] = time.monotonic() - started
            subject = cleanup.Cleanup(root, args.tools, registered=True)
            progress("Workers ready; checking native objects and pinning original Podman namespace")
            witness = Witness(subject, Path(__file__).resolve(), args.cleanup, ns_log, progress)
            before = witness.snapshot()
            write_json(output / "namespace-before.json", before)
            verify_before(subject, before, cleanup)
            progress("Verified native readiness and original Podman namespace; sending SIGTERM to owned supervisor")
            supervisor.signal()
            result["signal"] = "SIGTERM"
            result["signal_elapsed_seconds"] = time.monotonic() - started
            deadline = time.monotonic() + args.stop_timeout
            wait_until(supervisor.exited, deadline, "Waiting for graceful native teardown")
            result["native_exit"] = supervisor.exit_code
            result["native_exit_elapsed_seconds"] = time.monotonic() - started
            result["assertion_helpers_terminated"] = supervisor.stop_helpers(deadline, progress)
            result["native_exit"] = supervisor.finish()
            if result["native_exit"] != 143:
                raise RuntimeError(f"native supervisor exited {result['native_exit']}, expected intentional SIGTERM exit 143")
            progress("Independently checking host and retained original Podman namespace after teardown")
            after = witness.snapshot()
            write_json(output / "namespace-after.json", after)
            verify_after(subject, before, after, cleanup)
            witness.close()
            witness = None
            result["outcome"] = "passed"
            progress("PASS: no fixture containers, images, host mounts or exports in the original Podman namespace")
    except (Exception, KeyboardInterrupt) as error:
        result["error"] = f"{type(error).__name__}: {error}"
        progress(f"FAIL: {result['error']}; retaining registry, files and full logs")
    finally:
        for kind in (signal.SIGINT, signal.SIGTERM):
            signal.signal(kind, signal.SIG_IGN)
        if supervisor:
            try:
                supervisor.abort(progress)
            except Exception as error:
                result["outcome"] = "failed"
                result["supervisor_cleanup_error"] = str(error)
            if supervisor.exit_code is not None:
                result["native_exit"] = supervisor.exit_code
        if witness:
            try:
                witness.close()
            except Exception as error:
                result["outcome"] = "failed"
                result["witness_cleanup_error"] = str(error)
        result["elapsed_seconds"] = time.monotonic() - started
        write_json(output / "result.json", result)
        (output / "exit").write_text("0\n" if result["outcome"] == "passed" else "1\n")
        progress(f"Evidence and retained fixture registry: {output}")
    return int(result["outcome"] != "passed")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cleanup", type=Path, required=True)
    parser.add_argument("--tools", type=Path)
    parser.add_argument("--source-revision", default="dirty")
    parser.add_argument("--output", type=Path)
    parser.add_argument("--ready-timeout", type=float, default=600)
    parser.add_argument("--stop-timeout", type=float, default=180)
    parser.add_argument("--witness", action="store_true", help=argparse.SUPPRESS)
    parser.add_argument("--root", type=Path, help=argparse.SUPPRESS)
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    if any(not math.isfinite(value) or value <= 0 for value in (args.ready_timeout, args.stop_timeout)):
        parser.error("timeouts must be finite and positive")
    cleanup = cleanup_module(args.cleanup)
    if args.witness:
        return witness_main(args, cleanup)
    def interrupted(_kind, _frame):
        raise KeyboardInterrupt("harness interrupted; stopping only its owned invocation")
    signal.signal(signal.SIGTERM, interrupted)
    return run(args, cleanup)


if __name__ == "__main__":
    sys.exit(main())
