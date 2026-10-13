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
import json, os, sys, time
from pathlib import Path
if sys.argv[1:] == ["msg", "-j", "windows"]:
 print(Path(os.environ["WINDOWS"]).read_text())
elif sys.argv[1:] == ["msg", "-j", "focused-window"]:
 print('{{"id": 99}}')
elif sys.argv[1:5] == ["msg", "action", "focus-window", "--id"] and len(sys.argv) == 6:
 if os.environ.get("FAIL_FOCUS_WINDOW") == sys.argv[5]: sys.exit(72)
 if os.environ.get("HOLD_FOCUS_WINDOW") == sys.argv[5]:
  Path(os.environ["FOCUS_READY"]).touch()
  while not Path(os.environ["FOCUS_RELEASE"]).exists(): time.sleep(0.01)
 windows = json.loads(Path(os.environ["WINDOWS"]).read_text())
 for window in windows: window["is_focused"] = str(window["id"]) == sys.argv[5]
 Path(os.environ["WINDOWS"]).write_text(json.dumps(windows))
 with open(os.environ["LOG"], "a") as log: log.write(sys.argv[5] + "\\n")
else:
 sys.exit(64)
''')
        niri.chmod(0o755)
        self.environment = patch.dict(os.environ, PATH=f"{self.bin}:{os.environ['PATH']}",
                                      WINDOWS=str(self.state), LOG=str(self.log))
        self.environment.start()
        self.addCleanup(self.environment.stop)
        if sys.platform != "linux":
            proc = self.root / "proc"
            (proc / "net").mkdir(parents=True)
            (proc / "net/unix").write_text("Num RefCount Protocol Flags Type St Inode Path\n")
            process_files = patch.object(attention, "PROC_ROOT", proc)
            process_files.start()
            self.addCleanup(process_files.stop)
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

    def saved_origin(self):
        with attention.queue(self.directory) as db:
            row = db.execute("SELECT route FROM return_position").fetchone()
            return json.loads(row[0]) if row else None

    def desktop(self, *windows):
        self.state.write_text(json.dumps(windows))

    def focused_window(self):
        return next(window["id"] for window in json.loads(self.state.read_text()) if window["is_focused"])

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

    def test_linux_unavailable_or_malformed_focus_information_does_not_suppress(self):
        self.register("agent")
        for response in ("invalid", "null", "{}", '["invalid"]',
                         '[{"id":1,"pid":%d,"is_focused":"true"}]' % os.getpid()):
            with self.subTest(response=response):
                self.state.write_text(response)
                self.assertFalse(attention.is_focused(self.directory, "agent"))

    def test_linux_ambiguous_focused_windows_do_not_suppress(self):
        self.register("agent")
        self.desktop({"id":1, "pid":os.getpid(), "is_focused":True},
                     {"id":2, "pid":os.getpid() + 1, "is_focused":True})
        self.assertFalse(attention.is_focused(self.directory, "agent"))

    def test_window_pid_mismatch_cannot_focus_unrelated_application(self):
        self.register("agent")
        attention.enqueue(self.directory, "agent")
        self.state.write_text(json.dumps([{"id": 1, "pid": os.getpid() + 1, "is_focused": True}]))
        attention.focus(self.directory)
        self.assertEqual(self.pending(), [])
        self.assertFalse(self.log.exists())

    def test_return_restores_niri_window_and_is_single_use(self):
        self.desktop({"id":1, "pid":os.getpid(), "is_focused":False},
                     {"id":2, "pid":os.getpid() + 1, "is_focused":True})
        self.register("agent")
        attention.enqueue(self.directory, "agent")
        attention.focus(self.directory)
        self.assertEqual(self.focused_window(), 1)
        self.assertEqual(self.saved_origin()["window_id"], "2")
        self.register("still-pending")
        attention.enqueue(self.directory, "still-pending")
        attention.return_to_origin(self.directory)
        self.assertEqual(self.pending(), ["still-pending"])
        self.assertEqual(self.focused_window(), 2)
        self.assertIsNone(self.saved_origin())
        attention.return_to_origin(self.directory)
        self.assertEqual(self.log.read_text().splitlines(), ["1", "2"])

    def test_plain_desktop_origin_can_share_a_process_with_other_windows(self):
        self.desktop({"id":1, "pid":os.getpid(), "is_focused":False},
                     {"id":2, "pid":os.getpid() + 1, "is_focused":True},
                     {"id":3, "pid":os.getpid() + 1, "is_focused":False})
        self.register("agent")
        attention.enqueue(self.directory, "agent")
        attention.focus(self.directory)
        self.assertEqual(self.saved_origin()["window_id"], "2")
        attention.return_to_origin(self.directory)
        self.assertEqual(self.focused_window(), 2)

    def test_empty_queue_does_not_replace_saved_origin(self):
        self.desktop({"id":1, "pid":os.getpid(), "is_focused":False},
                     {"id":2, "pid":os.getpid() + 1, "is_focused":True})
        self.register("agent")
        attention.enqueue(self.directory, "agent")
        attention.focus(self.directory)
        saved = self.saved_origin()
        attention.focus(self.directory)
        self.assertEqual(self.saved_origin(), saved)

    def test_unsuccessful_navigation_does_not_replace_saved_origin(self):
        self.desktop({"id":1, "pid":os.getpid(), "is_focused":False},
                     {"id":2, "pid":os.getpid() + 1, "is_focused":True})
        self.register("agent")
        attention.enqueue(self.directory, "agent")
        attention.focus(self.directory)
        saved = self.saved_origin()
        # Match the destination PID without changing its registration origin.
        path = self.register("failed", 2)
        record = json.loads(path.read_text())
        record["linux_route"]["terminal_pid"] = os.getpid() + 1
        path.write_text(json.dumps(record))
        attention.enqueue(self.directory, "failed")
        with patch.dict(os.environ, FAIL_FOCUS_WINDOW="2"):
            attention.focus(self.directory)
        self.assertEqual(self.saved_origin(), saved)
        self.assertEqual(self.pending(), ["failed"])

    def test_successful_jump_with_unavailable_origin_clears_older_return_position(self):
        self.desktop({"id":1, "pid":os.getpid(), "is_focused":False},
                     {"id":2, "pid":os.getpid() + 1, "is_focused":True})
        self.register("agent")
        attention.enqueue(self.directory, "agent")
        attention.focus(self.directory)
        self.assertIsNotNone(self.saved_origin())
        self.desktop({"id":1, "pid":os.getpid(), "is_focused":False},
                     {"id":2, "pid":None, "is_focused":True})
        attention.enqueue(self.directory, "agent")
        attention.focus(self.directory)
        self.assertEqual(self.focused_window(), 1)
        self.assertIsNone(self.saved_origin())

    def test_stale_return_window_is_discarded_without_focus(self):
        self.desktop({"id":1, "pid":os.getpid(), "is_focused":False},
                     {"id":2, "pid":os.getpid() + 1, "is_focused":True})
        self.register("agent")
        attention.enqueue(self.directory, "agent")
        attention.focus(self.directory)
        self.desktop({"id":1, "pid":os.getpid(), "is_focused":True})
        attention.return_to_origin(self.directory)
        self.assertIsNone(self.saved_origin())
        self.assertEqual(self.log.read_text().splitlines(), ["1"])

    def test_failed_return_retains_saved_position_for_retry(self):
        self.desktop({"id":1, "pid":os.getpid(), "is_focused":False},
                     {"id":2, "pid":os.getpid() + 1, "is_focused":True})
        self.register("agent")
        attention.enqueue(self.directory, "agent")
        attention.focus(self.directory)
        saved = self.saved_origin()
        with patch.dict(os.environ, FAIL_FOCUS_WINDOW="2"):
            attention.return_to_origin(self.directory)
        self.assertEqual(self.saved_origin(), saved)
        attention.return_to_origin(self.directory)
        self.assertEqual(self.focused_window(), 2)

    def test_navigation_and_return_state_updates_are_serialized(self):
        self.desktop({"id":1, "pid":os.getpid(), "is_focused":False},
                     {"id":2, "pid":os.getpid() + 1, "is_focused":False},
                     {"id":3, "pid":os.getpid() + 2, "is_focused":True})
        self.register("first", 2)
        path = self.directory / "first.json"
        record = json.loads(path.read_text())
        record["linux_route"]["terminal_pid"] = os.getpid() + 1
        path.write_text(json.dumps(record))
        attention.enqueue(self.directory, "first")
        attention.focus(self.directory)
        self.assertEqual(self.saved_origin()["window_id"], "3")
        self.register("second", 1)
        attention.enqueue(self.directory, "second")
        ready, release = self.root / "ready", self.root / "release"
        with patch.dict(os.environ, HOLD_FOCUS_WINDOW="1", FOCUS_READY=str(ready), FOCUS_RELEASE=str(release)):
            with concurrent.futures.ThreadPoolExecutor(max_workers=2) as workers:
                jumping = workers.submit(attention.focus, self.directory)
                try:
                    deadline = time.monotonic() + 5
                    while not ready.exists():
                        self.assertLess(time.monotonic(), deadline)
                        time.sleep(0.01)
                    returning = workers.submit(attention.return_to_origin, self.directory)
                    time.sleep(0.1)
                    self.assertFalse(returning.done(), "return ran during navigation")
                finally:
                    release.touch()
                jumping.result()
                returning.result()
        self.assertEqual(self.focused_window(), 2)
        self.assertIsNone(self.saved_origin())

    def test_darwin_return_is_explicitly_unsupported_and_does_not_change_state(self):
        with attention.queue(self.directory) as db:
            db.execute("INSERT INTO return_position VALUES (1, ?)", ('{"window_id":"2"}',))
        saved = self.saved_origin()
        with patch.object(attention.sys, "platform", "darwin"):
            self.assertEqual(attention.return_to_origin(self.directory), 1)
        self.assertEqual(self.saved_origin(), saved)
        self.assertFalse(self.log.exists())

    def test_failed_navigation_retains_entry(self):
        self.register("a")
        attention.enqueue(self.directory, "a")
        self.state.write_text("bad IPC response")
        attention.focus(self.directory)
        self.assertEqual(self.pending(), ["a"])

    def terminal(self, name):
        socket_path = str(self.root / (name + ".sock"))
        tmux = ["tmux", "-S", socket_path, "-f", "/dev/null"]
        def call(*args):
            return subprocess.check_output(tmux + list(args), text=True).strip()
        call("new-session", "-d", "-s", name, "sleep 300")
        self.addCleanup(lambda: subprocess.run(tmux + ["kill-server"], check=False, capture_output=True))
        proxy = subprocess.Popen([sys.executable, "-c", """
import os, pty, select, signal, subprocess, sys
master, slave = pty.openpty()
env = dict(os.environ, TERM="xterm")
env.pop("TMUX", None)
env.pop("TMUX_PANE", None)
client = subprocess.Popen(["tmux", "-S", sys.argv[1], "attach-session", "-t", sys.argv[2]],
                          stdin=slave, stdout=slave, stderr=slave, env=env)
running = True
def stop(*_):
    global running
    running = False
signal.signal(signal.SIGTERM, stop)
print(os.getpid(), flush=True)
while running and client.poll() is None:
    if select.select([master], [], [], 0.1)[0]:
        os.read(master, 4096)
if client.poll() is None:
    client.terminate()
client.wait(timeout=5)
os.close(master)
os.close(slave)
""", socket_path, name], stdout=subprocess.PIPE, text=True)
        def stop_proxy():
            proxy.terminate()
            proxy.wait(timeout=5)
            proxy.stdout.close()
        self.addCleanup(stop_proxy)
        self.assertEqual(int(proxy.stdout.readline()), proxy.pid)
        deadline = time.monotonic() + 5
        while not (clients := attention.clients_on_socket(socket_path)):
            self.assertLess(time.monotonic(), deadline)
            time.sleep(0.01)
        return socket_path, proxy.pid, clients[0], call

    def terminal_registration(self, target, window, socket_path, terminal_pid, client, pane):
        path = self.register(target, window, tmux_target=pane, tmux_socket=socket_path)
        record = json.loads(path.read_text())
        record["linux_route"] = dict(terminal_pid=terminal_pid,
                                    client={key: client[key] for key in ("name", "pid", "tty", "session")})
        path.write_text(json.dumps(record))

    @unittest.skipUnless(sys.platform == "linux" and shutil.which("tmux"), "Linux tmux unavailable")
    def test_terminal_fixture_matches_real_client_ancestry_and_active_position(self):
        socket_path, pid, client, call = self.terminal("fixture")
        self.assertEqual(client["pane"], call("display-message", "-p", "-t", "fixture", "#{pane_id}"))
        self.assertEqual(client["window"], call("display-message", "-p", "-t", "fixture", "#{window_id}"))
        self.assertTrue(attention.client_origin_matches(client, pid))
        self.assertIn(socket_path, attention.socket_paths(self.directory))

    @unittest.skipUnless(sys.platform == "linux" and shutil.which("tmux"), "Linux tmux unavailable")
    def test_cross_workspace_then_same_terminal_pane_jump_returns_to_latest_origin_pane(self):
        socket_a, pid_a, client_a, call_a = self.terminal("workspace-a")
        socket_b, pid_b, client_b, call_b = self.terminal("workspace-b")
        pane_b = call_b("split-window", "-d", "-P", "-F", "#{pane_id}", "-t", client_b["pane"], "sleep 300")
        self.desktop({"id":1, "pid":pid_a, "is_focused":True},
                     {"id":2, "pid":pid_b, "is_focused":False})
        self.terminal_registration("first", 2, socket_b, pid_b, client_b, client_b["pane"])
        self.terminal_registration("second", 2, socket_b, pid_b, client_b, pane_b)
        attention.enqueue(self.directory, "first")
        attention.focus(self.directory)
        self.assertEqual(self.saved_origin()["window_id"], "1")
        self.assertEqual(self.saved_origin()["tmux_target"], client_a["pane"])
        attention.enqueue(self.directory, "second")
        attention.focus(self.directory)
        saved = self.saved_origin()
        self.assertEqual(saved["window_id"], "2")
        self.assertEqual(saved["tmux_window"], client_b["window"])
        self.assertEqual(saved["tmux_target"], client_b["pane"])
        self.assertEqual(call_b("display-message", "-p", "-t", client_b["session"], "#{pane_id}"), pane_b)
        attention.return_to_origin(self.directory)
        self.assertEqual(self.focused_window(), 2)
        self.assertEqual(call_b("display-message", "-p", "-t", client_b["session"], "#{pane_id}"), client_b["pane"])
        self.assertEqual(call_a("display-message", "-p", "-t", client_a["session"], "#{pane_id}"), client_a["pane"])
        self.assertIsNone(self.saved_origin())

    @unittest.skipUnless(sys.platform == "linux" and shutil.which("tmux"), "Linux tmux unavailable")
    def test_return_switches_only_the_recorded_client_to_saved_session(self):
        socket_path, pid, client, call = self.terminal("origin")
        call("new-session", "-d", "-s", "destination", "sleep 300")
        self.desktop({"id":1, "pid":pid, "is_focused":True},
                     {"id":2, "pid":os.getpid() + 1, "is_focused":False})
        self.register("agent", 2)
        path = self.directory / "agent.json"
        record = json.loads(path.read_text())
        record["linux_route"]["terminal_pid"] = os.getpid() + 1
        path.write_text(json.dumps(record))
        attention.enqueue(self.directory, "agent")
        attention.focus(self.directory)
        saved = self.saved_origin()
        self.assertEqual(saved["linux_route"]["client"]["name"], client["name"])
        call("switch-client", "-c", client["name"], "-t", "destination")
        attention.return_to_origin(self.directory)
        self.assertEqual(call("list-clients", "-F", "#{session_id}"), client["session"])
        self.assertEqual(self.focused_window(), 1)

    @unittest.skipUnless(sys.platform == "linux" and shutil.which("tmux"), "Linux tmux unavailable")
    def test_stale_return_pane_is_discarded_without_selecting_another_pane(self):
        socket_path, pid, client, call = self.terminal("origin")
        call("split-window", "-d", "-t", client["pane"], "sleep 300")
        self.desktop({"id":1, "pid":pid, "is_focused":True},
                     {"id":2, "pid":os.getpid(), "is_focused":False})
        self.register("agent", 2)
        attention.enqueue(self.directory, "agent")
        attention.focus(self.directory)
        self.assertEqual(self.saved_origin()["tmux_target"], client["pane"])
        call("kill-pane", "-t", client["pane"])
        attention.return_to_origin(self.directory)
        self.assertEqual(self.focused_window(), 2)
        self.assertIsNone(self.saved_origin())

    @unittest.skipUnless(sys.platform == "linux" and shutil.which("tmux"), "Linux tmux unavailable")
    def test_stale_return_session_does_not_switch_the_client(self):
        socket_path, pid, client, call = self.terminal("origin")
        call("new-session", "-d", "-s", "remaining", "sleep 300")
        self.desktop({"id":1, "pid":pid, "is_focused":True},
                     {"id":2, "pid":os.getpid(), "is_focused":False})
        self.register("agent", 2)
        attention.enqueue(self.directory, "agent")
        attention.focus(self.directory)
        call("switch-client", "-c", client["name"], "-t", "remaining")
        call("kill-session", "-t", client["session"])
        attention.return_to_origin(self.directory)
        self.assertEqual(call("list-clients", "-F", "#{client_session}"), "remaining")
        self.assertEqual(self.focused_window(), 2)
        self.assertIsNone(self.saved_origin())

    @unittest.skipUnless(sys.platform == "linux" and shutil.which("tmux"), "Linux tmux unavailable")
    def test_stale_return_client_does_not_focus_its_old_terminal(self):
        socket_path, pid, client, call = self.terminal("origin")
        self.desktop({"id":1, "pid":pid, "is_focused":True},
                     {"id":2, "pid":os.getpid(), "is_focused":False})
        self.register("agent", 2)
        attention.enqueue(self.directory, "agent")
        attention.focus(self.directory)
        call("detach-client", "-t", client["name"])
        attention.return_to_origin(self.directory)
        self.assertEqual(self.focused_window(), 2)
        self.assertIsNone(self.saved_origin())

    @unittest.skipUnless(sys.platform == "linux" and shutil.which("tmux"), "Linux tmux unavailable")
    def test_ambiguous_return_session_does_not_select_shared_state(self):
        socket_path, pid, client, call = self.terminal("origin")
        self.desktop({"id":1, "pid":pid, "is_focused":True},
                     {"id":2, "pid":os.getpid(), "is_focused":False})
        self.register("agent", 2)
        attention.enqueue(self.directory, "agent")
        attention.focus(self.directory)
        other = subprocess.Popen(["tmux", "-S", socket_path, "-C", "attach-session", "-t", "origin"],
                                 stdin=subprocess.PIPE, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
        def stop():
            other.stdin.close()
            other.wait(timeout=5)
            other.stderr.close()
        self.addCleanup(stop)
        deadline = time.monotonic() + 5
        while len(attention.clients_on_socket(socket_path)) != 2:
            self.assertLess(time.monotonic(), deadline)
            time.sleep(0.01)
        attention.return_to_origin(self.directory)
        self.assertEqual(self.focused_window(), 2)
        self.assertEqual(len(attention.clients_on_socket(socket_path)), 2)
        self.assertIsNone(self.saved_origin())

    @unittest.skipUnless(sys.platform == "linux" and shutil.which("tmux"), "Linux tmux unavailable")
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
        self.desktop({"id":1, "pid":os.getpid(), "is_focused":False},
                     {"id":99, "pid":os.getpid() + 1, "is_focused":True},
                     {"id":2, "pid":os.getpid(), "is_focused":False})
        self.register("agent", 1)
        focused = self.register("focused", 99)
        record = json.loads(focused.read_text())
        record["linux_route"]["terminal_pid"] = os.getpid() + 1
        focused.write_text(json.dumps(record))
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
            # Keep the stream open until this batch is handled: socat's EOF
            # close timeout is shorter than several focus/dispatch subprocesses.
            deadline = time.monotonic() + 15
            while not capture.exists() or len(capture.read_text().splitlines()) < 3:
                self.assertLess(time.monotonic(), deadline, (self.root / "daemon.log").read_text())
                time.sleep(0.02)
        self.assertEqual([json.loads(line)[0] for line in capture.read_text().splitlines()],
                         ["queued", "unrouted", "missing"])
        self.assertEqual(self.pending(), ["agent"])
        self.assertFalse(self.log.exists(), "notification dispatch stole focus")
        subprocess.run(["wrix-focus-notification"], env=env, check=True)
        self.assertEqual(self.pending(), [])
        self.assertEqual(self.log.read_text().splitlines(), ["1"])
        subprocess.run(["wrix-focus-notification", "--return"], env=env, check=True)
        self.assertEqual(self.focused_window(), 99)
        self.assertIsNone(self.saved_origin())
        self.assertEqual(self.log.read_text().splitlines(), ["1", "99"])

    def darwin_focus_registration(self, target="mac"):
        title = 'Alacritty "quoted"; do shell script "bad"'
        self.register(target, terminal_app="Alacritty", terminal_window_title=title)
        osascript = self.bin / "osascript"
        osascript.write_text(f'''#!{sys.executable}
import json, os, sys
if len(sys.argv) != 5 or sys.argv[1] != "-e": sys.exit(64)
with open(os.environ["LOG"], "a") as log: log.write(json.dumps(sys.argv[1:]) + "\\n")
if os.environ.get("AX_FAILURE"): sys.exit(72)
print(os.environ.get("AX_RESULT", "true"))
''')
        osascript.chmod(0o755)
        return title

    def test_darwin_focused_origin_window_suppresses_and_title_is_data(self):
        title = self.darwin_focus_registration()
        with patch.object(attention.sys, "platform", "darwin"):
            self.assertTrue(attention.is_focused(self.directory, "mac"))
        argv = json.loads(self.log.read_text())
        self.assertEqual(argv[-2:], ["Alacritty", title])
        self.assertNotIn(title, argv[1])

    def test_darwin_background_window_of_same_app_does_not_suppress(self):
        self.darwin_focus_registration()
        with patch.object(attention.sys, "platform", "darwin"), patch.dict(os.environ, AX_RESULT="false"):
            self.assertFalse(attention.is_focused(self.directory, "mac"))

    def test_darwin_unavailable_accessibility_does_not_suppress(self):
        self.darwin_focus_registration()
        with patch.object(attention.sys, "platform", "darwin"), patch.dict(os.environ, AX_FAILURE="1"):
            self.assertFalse(attention.is_focused(self.directory, "mac"))

    def test_darwin_unexpected_focus_response_does_not_suppress(self):
        self.darwin_focus_registration()
        with patch.object(attention.sys, "platform", "darwin"), patch.dict(os.environ, AX_RESULT="unknown"):
            self.assertFalse(attention.is_focused(self.directory, "mac"))

    def test_darwin_missing_origin_title_does_not_suppress(self):
        self.register("mac", terminal_app="Alacritty")
        with patch.object(attention.sys, "platform", "darwin"):
            self.assertFalse(attention.is_focused(self.directory, "mac"))
        self.assertFalse(self.log.exists())

    @unittest.skipUnless(shutil.which("tmux"), "tmux unavailable")
    def test_darwin_focused_active_origin_pane_suppresses(self):
        socket_path, _, _, call = self.terminal("mac")
        self.darwin_focus_registration()
        self.register("mac", terminal_app="Alacritty", terminal_window_title="Origin",
                      tmux_target="mac:0.0", tmux_socket=socket_path)
        with patch.object(attention.sys, "platform", "darwin"):
            self.assertTrue(attention.is_focused(self.directory, "mac"))
        self.assertEqual(call("display-message", "-p", "-t", "mac:0.0", "#{pane_active}"), "1")

    @unittest.skipUnless(shutil.which("tmux"), "tmux unavailable")
    def test_darwin_inactive_origin_pane_does_not_suppress(self):
        socket_path, _, _, call = self.terminal("mac")
        self.darwin_focus_registration()
        self.register("mac", terminal_app="Alacritty", terminal_window_title="Origin",
                      tmux_target="mac:0.0", tmux_socket=socket_path)
        call("split-window", "-t", "mac:0.0", "sleep 300")
        with patch.object(attention.sys, "platform", "darwin"):
            self.assertFalse(attention.is_focused(self.directory, "mac"))
        self.assertEqual(call("display-message", "-p", "-t", "mac:0.0", "#{pane_active}"), "0")

    @unittest.skipUnless(shutil.which("tmux"), "tmux unavailable")
    def test_darwin_inactive_tmux_window_does_not_suppress(self):
        socket_path, _, _, call = self.terminal("mac")
        self.darwin_focus_registration()
        self.register("mac", terminal_app="Alacritty", terminal_window_title="Origin",
                      tmux_target="mac:0.0", tmux_socket=socket_path)
        call("new-window", "-t", "mac:", "sleep 300")
        with patch.object(attention.sys, "platform", "darwin"):
            self.assertFalse(attention.is_focused(self.directory, "mac"))

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
