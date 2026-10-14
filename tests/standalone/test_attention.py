"""Verify attention delivery through the packaged notification daemon and helper."""

import json
import os
from pathlib import Path
import re
import shutil
import socket
import sqlite3
import subprocess
import time
import sys
import tempfile
import unittest
from unittest.mock import patch


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

    def register(self, target, window=1, **fields):
        record = dict(focus_target=target, window_id=str(window), linux_route={"terminal_pid": os.getpid(), "client": None}, **fields)
        name = re.sub(rb"[^A-Za-z0-9_-]", b"-", target.encode()).decode()
        (self.directory / (name + ".json")).write_text(json.dumps(record))
        return self.directory / (name + ".json")

    def pending(self):
        path = self.directory / "attention.sqlite"
        if not path.exists():
            return []
        with sqlite3.connect(path) as db:
            return [row[0] for row in db.execute("SELECT target FROM pending ORDER BY sequence DESC")]

    def saved_origin(self):
        with sqlite3.connect(self.directory / "attention.sqlite") as db:
            row = db.execute("SELECT route FROM return_position").fetchone()
            return json.loads(row[0]) if row else None

    def desktop(self, *windows):
        self.state.write_text(json.dumps(windows))

    def focused_window(self):
        return next(window["id"] for window in json.loads(self.state.read_text()) if window["is_focused"])

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

    @unittest.skipUnless(sys.platform == "linux" and os.environ.get("WRIX_NATIVE_TEST_BIN")
                         and shutil.which("wrix-notify-host") and shutil.which("wrix-notifyd"),
                         "native host packages unavailable")
    def test_native_launch_host_delivery_and_navigation(self):
        runtime = self.root / "runtime"
        self.directory = runtime / "wrix/sessions"
        self.desktop({"id":1, "pid":os.getpid(), "is_focused":True},
                     {"id":99, "pid":os.getpid() + 1, "is_focused":False})
        capture = self.root / "dispatch"
        output = self.root / "target"
        release = self.root / "release"
        env = dict(os.environ, XDG_RUNTIME_DIR=str(runtime),
                   WRIX_NOTIFY_TEST_DISPATCH_CAPTURE=str(capture),
                   WRIX_NOTIFY_TCP="invalid:1", OUTPUT=str(output), RELEASE=str(release))
        missing = subprocess.run(["wrix-notify-host", "missing daemon", "complete"],
                                 env=env, capture_output=True, text=True, timeout=5)
        self.assertEqual(missing.returncode, 0)
        self.assertIn("socket not found", missing.stderr)
        env.pop("TMUX", None)
        env.pop("TMUX_PANE", None)
        log = (self.root / "native-daemon.log").open("w")
        self.addCleanup(log.close)
        daemon = subprocess.Popen(["wrix-notifyd"], env=env, stdout=log, stderr=log)
        agent = subprocess.Popen([os.environ["WRIX_NATIVE_TEST_BIN"], "notify", "run", "--",
            "sh", "-c", 'printf "%s" "$WRIX_FOCUS_TARGET" > "$OUTPUT"; '
            'while [ ! -f "$RELEASE" ]; do sleep 0.02; done'], env=env)
        def stop():
            release.touch()
            agent.wait(timeout=5)
            daemon.terminate()
            daemon.wait(timeout=5)
        self.addCleanup(stop)
        def wait_for(predicate):
            deadline = time.monotonic() + 10
            while not predicate():
                self.assertLess(time.monotonic(), deadline, log.name)
                time.sleep(0.02)
        wait_for(lambda: output.exists() and output.read_text()
                 and (runtime / "wrix/notify.sock").exists())
        target = output.read_text()
        name = re.sub(rb"[^A-Za-z0-9_-]", b"-", target.encode()).decode()
        record = json.loads((self.directory / (name + ".json")).read_text())
        self.assertEqual(record["window_id"], "1")
        self.assertTrue(record["leased"])
        env["WRIX_FOCUS_TARGET"] = target
        subprocess.run(["wrix-notify-host", "suppressed", "complete"], env=env, check=True)
        time.sleep(0.5)
        self.assertFalse(capture.exists())
        self.assertEqual(self.pending(), [])
        self.desktop({"id":1, "pid":os.getpid(), "is_focused":False},
                     {"id":99, "pid":os.getpid() + 1, "is_focused":True})
        subprocess.run(["wrix-notify-host", "Codex", "complete"], env=env, check=True)
        wait_for(lambda: self.pending() == [target])
        self.assertEqual(json.loads(capture.read_text()), ["Codex", "complete"])
        subprocess.run(["wrix-focus-notification"], env=env, check=True)
        self.assertEqual(self.focused_window(), 1)
        subprocess.run(["wrix-focus-notification", "--return"], env=env, check=True)
        self.assertEqual(self.focused_window(), 99)
        release.touch()
        self.assertEqual(agent.wait(timeout=5), 0)
        subprocess.run(["wrix-focus-notification", "--enqueue", target], env=env, check=True)
        self.assertEqual(self.pending(), [])
        subprocess.run(["wrix-notify-host", "exited", "complete"], env=env, check=True)
        wait_for(lambda: len(capture.read_text().splitlines()) == 2)
        self.assertEqual(self.pending(), [])


if __name__ == "__main__":
    unittest.main()
