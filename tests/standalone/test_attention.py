"""Exercise persistent attention routing with real tmux and desktop IPC fixtures."""

import concurrent.futures
import importlib.util
import json
import os
import pty
from pathlib import Path
import shutil
import socket
import subprocess
import time
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("attention", ROOT / "lib/notify/attention.py")
attention = importlib.util.module_from_spec(spec)
spec.loader.exec_module(attention)


class AttentionTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.directory = self.root / "sessions"
        self.directory.mkdir()
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.log = self.root / "log"
        self.state = self.root / "windows"
        self.state.write_text(json.dumps([{"id": i, "pid": os.getpid(), "is_focused": i == 99} for i in range(1, 101)]))
        niri = self.bin / "niri"
        niri.write_text(f'''#!{sys.executable}
import json, os, sys
from pathlib import Path
if sys.argv[1:] == ["msg", "-j", "windows"]:
 print(Path(os.environ["WINDOWS"]).read_text())
elif sys.argv[1:] == ["msg", "-j", "focused-window"]:
 print('{{"id": 99}}')
elif sys.argv[1:5] == ["msg", "action", "focus-window", "--id"] and len(sys.argv) == 6:
 with open(os.environ["LOG"], "a") as log: log.write(sys.argv[5] + "\\n")
else:
 sys.exit(64)
''')
        niri.chmod(0o755)
        self.environment = patch.dict(os.environ, PATH=f"{self.bin}:{os.environ['PATH']}",
                                      WINDOWS=str(self.state), LOG=str(self.log))
        self.environment.start()
        self.addCleanup(self.environment.stop)
        self.platform = patch.object(attention.sys, "platform", "linux")
        self.platform.start()
        self.addCleanup(self.platform.stop)

    def register(self, target, window=1, **fields):
        record = dict(focus_target=target, window_id=str(window), linux_route={"terminal_pid": os.getpid(), "client": None}, **fields)
        name = attention.re.sub(rb"[^A-Za-z0-9_-]", b"-", target.encode()).decode()
        (self.directory / (name + ".json")).write_text(json.dumps(record))
        return self.directory / (name + ".json")

    def pending(self):
        with attention.queue(self.directory) as db:
            return [row[0] for row in db.execute("SELECT target FROM pending ORDER BY sequence DESC")]

    def test_newest_first_deduplicated_and_consumed(self):
        self.register("a", 1)
        self.register("b", 2)
        for target in ("a", "b", "a"):
            attention.enqueue(self.directory, target)
        self.assertEqual(self.pending(), ["a", "b"])
        self.assertFalse(self.log.exists(), "delivery must never navigate")
        attention.focus(self.directory)
        self.assertEqual(self.pending(), ["b"])
        attention.focus(self.directory)
        attention.focus(self.directory)
        self.assertEqual(self.log.read_text().splitlines(), ["1", "2"])

    def test_stale_registration_closed_window_and_changed_route_skipped(self):
        self.register("good", 1)
        missing = self.register("missing", 2)
        self.register("closed", 101)
        self.register("changed", 3)
        for target in ("good", "missing", "closed", "changed"):
            attention.enqueue(self.directory, target)
        missing.unlink()
        self.register("changed", 4)
        attention.focus(self.directory)
        self.assertEqual(self.log.read_text().splitlines(), ["1"])
        self.assertEqual(self.pending(), [])

    def test_unregistered_and_filename_collision_not_queued(self):
        self.register("a:b")
        attention.enqueue(self.directory, "a/b")
        attention.enqueue(self.directory, "missing")
        self.assertEqual(self.pending(), [])

    def test_legacy_linux_route_is_invalidated_without_navigation(self):
        path = self.register("legacy", 99)
        record = json.loads(path.read_text())
        del record["linux_route"]
        path.write_text(json.dumps(record))
        attention.enqueue(self.directory, "legacy")
        self.assertEqual(self.pending(), [])
        self.assertFalse(attention.is_focused(self.directory, "legacy"))
        self.assertFalse(self.log.exists())

    def test_window_pid_mismatch_cannot_focus_unrelated_application(self):
        self.register("agent")
        attention.enqueue(self.directory, "agent")
        self.state.write_text(json.dumps([{"id": 1, "pid": os.getpid() + 1, "is_focused": True}]))
        attention.focus(self.directory)
        self.assertEqual(self.pending(), [])
        self.assertFalse(self.log.exists())

    def test_failed_navigation_retains_entry(self):
        self.register("a")
        attention.enqueue(self.directory, "a")
        self.state.write_text("bad IPC response")
        attention.focus(self.directory)
        self.assertEqual(self.pending(), ["a"])

    @unittest.skipUnless(shutil.which("tmux"), "tmux unavailable")
    def test_tmux_socket_window_and_pane_routing_and_dead_pane(self):
        socket = str(self.root / "tmux.sock")
        tmux = ["tmux", "-S", socket, "-f", "/dev/null"]
        def call(*args):
            return subprocess.check_output(tmux + list(args), text=True).strip()
        call("new-session", "-d", "-s", "host", "sleep 300")
        self.addCleanup(lambda: subprocess.run(tmux + ["kill-server"], check=False, capture_output=True))
        call("new-window", "-t", "host:1", "sleep 300")
        call("split-window", "-d", "-t", "host:1", "sleep 300")
        call("select-window", "-t", "host:0")
        pane = call("display-message", "-p", "-t", "host:1.1", "#{pane_id}")
        session = call("display-message", "-p", "-t", pane, "#{session_id}")
        master, slave = pty.openpty()
        self.addCleanup(lambda: os.close(master))
        self.addCleanup(lambda: os.close(slave))
        client_process = subprocess.Popen(tmux + ["attach-session", "-t", "host"],
                                          stdin=slave, stdout=slave, stderr=slave,
                                          env=dict(os.environ, TERM="xterm"))
        def stop_client():
            client_process.terminate()
            client_process.wait(timeout=5)
        self.addCleanup(stop_client)
        deadline = time.monotonic() + 3
        while not (clients := call("list-clients", "-F", "#{client_name}\t#{client_pid}\t#{client_tty}\t#{session_id}")):
            self.assertLess(time.monotonic(), deadline)
            time.sleep(0.01)
        name, pid, tty, session = clients.split("\t")
        path = self.register("agent", tmux_target=pane, tmux_socket=socket)
        record = json.loads(path.read_text())
        record["linux_route"]["client"] = dict(name=name, pid=int(pid), tty=tty, session=session)
        path.write_text(json.dumps(record))
        # Mutable names and indexes no longer identify the pane.
        call("rename-session", "-t", "host", "renamed")
        call("move-window", "-s", "renamed:1", "-t", "renamed:5")
        attention.enqueue(self.directory, "agent")
        attention.focus(self.directory)
        self.assertEqual(call("display-message", "-p", "-t", "renamed", "#{window_index}.#{pane_index}"), "5.1")
        # select-window/select-pane cannot switch a client between sessions.
        # A moved client invalidates the route instead of switching any client.
        call("new-session", "-d", "-s", "other", "sleep 300")
        call("switch-client", "-c", name, "-t", "other")
        attention.enqueue(self.directory, "agent")
        attention.focus(self.directory)
        self.assertEqual(self.pending(), [])
        self.assertEqual(call("display-message", "-p", "-c", name, "#{session_name}"), "other")
        self.assertEqual(self.log.read_text().splitlines(), ["1"])
        call("switch-client", "-c", name, "-t", "renamed")
        call("set-option", "-w", "-t", pane, "remain-on-exit", "on")
        call("respawn-pane", "-k", "-t", pane, "exit 0")
        deadline = time.monotonic() + 3
        while call("display-message", "-p", "-t", pane, "#{pane_dead}") != "1":
            self.assertLess(time.monotonic(), deadline)
            time.sleep(0.01)
        attention.enqueue(self.directory, "agent")
        attention.focus(self.directory)
        self.assertEqual(self.pending(), [])
        self.assertEqual(self.log.read_text().splitlines(), ["1"])

    def test_concurrent_delivery_and_navigation_no_lost_entries(self):
        for i in range(1, 41):
            self.register(str(i), i)
        with concurrent.futures.ThreadPoolExecutor(max_workers=12) as workers:
            futures = [workers.submit(attention.enqueue, self.directory, str(i)) for i in range(1, 41)]
            futures += [workers.submit(attention.focus, self.directory) for _ in range(20)]
            for future in futures:
                future.result()
        visited = self.log.read_text().splitlines() if self.log.exists() else []
        pending = self.pending()
        self.assertEqual(len(visited) + len(pending), 40)
        self.assertEqual(set(visited + pending), {str(i) for i in range(1, 41)})
        with attention.queue(self.directory) as db:
            self.assertEqual(db.execute("PRAGMA integrity_check").fetchone()[0], "ok")

    @unittest.skipUnless(sys.platform == "linux" and shutil.which("wrix-notifyd"),
                         "Linux packaged fixture daemon unavailable")
    def test_dispatched_notifications_queue_without_focus_and_unrouted_delivery_unchanged(self):
        runtime = self.root / "runtime"
        self.directory = runtime / "wrix/sessions"
        self.directory.mkdir(parents=True)
        self.register("agent", 1)
        self.register("focused", 99)
        self.register("failed-agent", 2)
        capture = self.root / "dispatch"
        env = dict(os.environ, XDG_RUNTIME_DIR=str(runtime),
                   WRIX_NOTIFY_TEST_DISPATCH_CAPTURE=str(capture),
                   WRIX_NOTIFY_TEST_FAIL_TITLE="failed")
        log = (self.root / "daemon.log").open("w")
        self.addCleanup(log.close)
        daemon = subprocess.Popen(["wrix-notifyd"], env=env, stdout=log, stderr=log)
        def stop():
            daemon.terminate()
            daemon.wait(timeout=5)
        self.addCleanup(stop)
        endpoint = runtime / "wrix/notify.sock"
        deadline = time.monotonic() + 5
        while not endpoint.exists():
            self.assertIsNone(daemon.poll())
            self.assertLess(time.monotonic(), deadline)
            time.sleep(0.02)
        with socket.socket(socket.AF_UNIX) as client:
            client.connect(str(endpoint))
            for payload in (
                dict(title="queued", message="input", focus_target="agent", command="never execute"),
                dict(title="failed", message="input", focus_target="failed-agent"),
                dict(title="unrouted", message="input"),
                dict(title="missing", message="input", focus_target="missing"),
                dict(title="suppressed", message="input", focus_target="focused"),
            ):
                client.sendall((json.dumps(payload) + "\n").encode())
        deadline = time.monotonic() + 5
        while not capture.exists() or len(capture.read_text().splitlines()) < 3:
            self.assertLess(time.monotonic(), deadline)
            time.sleep(0.02)
        self.assertEqual([json.loads(line)[0] for line in capture.read_text().splitlines()],
                         ["queued", "unrouted", "missing"])
        self.assertEqual(self.pending(), ["agent"])
        self.assertFalse(self.log.exists(), "notification dispatch stole focus")
        subprocess.run(["wrix-focus-notification"], env=env, check=True)
        self.assertEqual(self.pending(), [])
        self.assertEqual(self.log.read_text().splitlines(), ["1"])

    def test_darwin_unique_window_required_and_title_is_data(self):
        osascript = self.bin / "osascript"
        osascript.write_text(f'''#!{sys.executable}
import json, os, sys
with open(os.environ["LOG"], "a") as log: log.write(json.dumps(sys.argv[1:]) + "\\n")
print(os.environ.get("AX_RESULT", "success"))
''')
        osascript.chmod(0o755)
        title = 'Alacritty "quoted"; do shell script "bad"'
        self.register("mac", terminal_app="Alacritty", terminal_window_title=title)
        with patch.object(attention.sys, "platform", "darwin"):
            attention.enqueue(self.directory, "mac")
            attention.focus(self.directory)
            self.assertEqual(self.pending(), [])
            argv = json.loads(self.log.read_text())
            self.assertEqual(argv[-2:], ["Alacritty", title])
            self.assertNotIn(title, argv[1])
            attention.enqueue(self.directory, "mac")
            with patch.dict(os.environ, AX_RESULT="stale"):
                attention.focus(self.directory)
            self.assertEqual(self.pending(), [])
            self.register("app-only", terminal_app="Alacritty")
            attention.enqueue(self.directory, "app-only")
            self.assertEqual(self.pending(), [])


if __name__ == "__main__":
    unittest.main()
