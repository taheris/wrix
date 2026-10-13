"""Host-owned pending attention routes; notification payloads supply only keys."""

import argparse
import contextlib
import json
import os
from pathlib import Path
import re
import sqlite3
import subprocess
import sys


def session_directory():
    variable = "XDG_DATA_HOME" if sys.platform == "darwin" else "XDG_RUNTIME_DIR"
    return Path(os.environ.get(variable, str(Path.home() / ".local/share"))) / "wrix/sessions"


def registration(directory, target):
    name = re.sub(rb"[^A-Za-z0-9_-]", b"-", target.encode()).decode()
    try:
        record = json.loads((directory / (name + ".json")).read_text())
        if record.get("focus_target") != target:
            return None
        field = "terminal_app" if sys.platform == "darwin" else "window_id"
        if not isinstance(record.get(field), str) or not record[field]:
            return None
        if sys.platform != "darwin":
            route = record.get("linux_route")
            if not isinstance(route, dict) or not isinstance(route.get("terminal_pid"), int):
                return None
            client = route.get("client")
            if record.get("tmux_target") and (
                not isinstance(record["tmux_target"], str)
                or not re.fullmatch(r"%[0-9]+", record["tmux_target"])
                or not record.get("tmux_socket") or not isinstance(client, dict)
                or not all(isinstance(client.get(key), str) and client[key] for key in ("name", "tty", "session"))
                or not isinstance(client.get("pid"), int)
            ):
                return None
        if sys.platform == "darwin" and not record.get("terminal_window_title"):
            return None
        for key in ("tmux_target", "tmux_socket", "terminal_window_title"):
            if record.get(key) is not None and not isinstance(record[key], str):
                return None
        return record
    except (OSError, ValueError, AttributeError):
        return None


@contextlib.contextmanager
def queue(directory):
    directory.mkdir(parents=True, exist_ok=True)
    path = directory / "attention.sqlite"
    fd = os.open(path, os.O_CREAT | os.O_RDWR, 0o600)
    os.close(fd)
    with contextlib.closing(sqlite3.connect(path, timeout=30)) as db:
        db.execute("CREATE TABLE IF NOT EXISTS pending (target TEXT PRIMARY KEY, route TEXT NOT NULL, sequence INTEGER NOT NULL)")
        db.execute("BEGIN IMMEDIATE")
        try:
            yield db
            db.commit()
        except BaseException:
            db.rollback()
            raise


def route_identity(record):
    fields = ["focus_target", "window_id", "terminal_app", "terminal_window_title", "tmux_target", "tmux_socket"]
    if sys.platform != "darwin":
        fields.append("linux_route")
    return json.dumps({key: record.get(key) for key in fields}, sort_keys=True)


def enqueue(directory, target):
    with queue(directory) as db:
        record = registration(directory, target)
        if record is None:
            return
        sequence = db.execute("SELECT COALESCE(MAX(sequence), 0) + 1 FROM pending").fetchone()[0]
        db.execute("INSERT OR REPLACE INTO pending VALUES (?, ?, ?)",
                   (target, route_identity(record), sequence))


def run(args):
    try:
        return subprocess.check_output(args, text=True, stderr=subprocess.PIPE, timeout=3).strip()
    except (OSError, subprocess.SubprocessError) as error:
        print(f"wrix-focus-notification: navigation unavailable: {error}", file=sys.stderr)
        return None


def validate(record):
    if sys.platform != "darwin":
        windows = run(["niri", "msg", "-j", "windows"])
        if windows is None:
            return "retry"
        try:
            if not any(str(window["id"]) == record["window_id"] and window.get("pid") == record["linux_route"]["terminal_pid"] for window in json.loads(windows)):
                return "stale"
        except (ValueError, TypeError, KeyError):
            return "retry"
    tmux_target = record.get("tmux_target")
    if tmux_target:
        tmux = ["tmux"]
        if record.get("tmux_socket"):
            tmux += ["-S", record["tmux_socket"]]
        if sys.platform != "darwin":
            client = record["linux_route"]["client"]
            clients = run(tmux + ["list-clients", "-F", "#{client_name}\t#{client_pid}\t#{client_tty}\t#{session_id}"])
            if clients is None:
                return "retry"
            attached = [line.split("\t") for line in clients.splitlines() if line.split("\t")[-1] == client["session"]]
            if attached != [[client["name"], str(client["pid"]), client["tty"], client["session"]]]:
                return "stale"
            if not client_origin_matches(client, record["linux_route"]["terminal_pid"]):
                return "stale"
            panes = run(tmux + ["list-panes", "-a", "-F", "#{pane_id}\t#{session_id}"])
            if panes is None:
                return "retry"
            sessions = [line.split("\t")[1:] for line in panes.splitlines() if line.split("\t")[0] == tmux_target]
            if sessions != [[client["session"]]]:
                return "stale"
            state = run(tmux + ["display-message", "-p", "-t", tmux_target, "#{pane_id} #{pane_dead} #{session_id}"])
            if state != tmux_target + " 0 " + client["session"]:
                return "stale"
        else:
            state = run(tmux + ["display-message", "-p", "-t", tmux_target,
                               "#{session_name}:#{window_index}.#{pane_index} #{pane_dead}"])
            if state != tmux_target + " 0":
                return "stale"
    return "success"


def client_origin_matches(client, terminal_pid):
    try:
        if str(Path(f"/proc/{client['pid']}/fd/0").readlink()) != client["tty"]:
            return False
        pid = client["pid"]
        ancestors = set()
        while pid > 1 and pid not in ancestors and len(ancestors) < 128:
            ancestors.add(pid)
            status = Path(f"/proc/{pid}/status").read_text()
            pid = int(next(line.split()[1] for line in status.splitlines() if line.startswith("PPid:")))
        return terminal_pid in ancestors
    except (OSError, ValueError, StopIteration):
        return False


def navigate(record):
    result = validate(record)
    if result != "success":
        return result
    tmux_target = record.get("tmux_target")
    tmux = ["tmux"]
    if record.get("tmux_socket"):
        tmux += ["-S", record["tmux_socket"]]
    if sys.platform == "darwin":
        result = run(["osascript", "-e", '''on run argv
 tell application "System Events"
  if not (exists process (item 1 of argv)) then return "stale"
  tell process (item 1 of argv)
   set matches to windows whose name is (item 2 of argv)
   if (count of matches) is not 1 then return "stale"
   perform action "AXRaise" of item 1 of matches
   set frontmost to true
  end tell
 end tell
 return "success"
end run''', record["terminal_app"], record["terminal_window_title"]])
        if result == "stale":
            return "stale"
        if result != "success":
            return "retry"
    if tmux_target:
        if run(tmux + ["select-window", "-t", tmux_target]) is None:
            return "retry"
        if run(tmux + ["select-pane", "-t", tmux_target]) is None:
            return "retry"
    if sys.platform != "darwin":
        if run(["niri", "msg", "action", "focus-window", "--id", record["window_id"]]) is None:
            return "retry"
    return "success"


def focus(directory):
    with queue(directory) as db:
        for target, identity in db.execute("SELECT target, route FROM pending ORDER BY sequence DESC").fetchall():
            record = registration(directory, target)
            if record is None or route_identity(record) != identity:
                result = "stale"
            else:
                result = navigate(record)
            if result in ("stale", "success"):
                db.execute("DELETE FROM pending WHERE target = ?", (target,))
            if result == "success":
                return


def is_focused(directory, target):
    record = registration(directory, target)
    if record is None or validate(record) != "success":
        return False
    windows = run(["niri", "msg", "-j", "windows"])
    try:
        focused = any(str(window["id"]) == record["window_id"] and window.get("is_focused") for window in json.loads(windows))
    except (ValueError, TypeError, KeyError):
        return False
    if not focused:
        return False
    if record.get("tmux_target"):
        state = run(["tmux", "-S", record["tmux_socket"], "display-message", "-p", "-t", record["tmux_target"],
                     "#{window_active} #{pane_active}"])
        return state == "1 1"
    return True


def main():
    parser = argparse.ArgumentParser(description="Visit the newest pending agent terminal.")
    parser.add_argument("--enqueue", metavar="TARGET")
    parser.add_argument("--is-focused", metavar="TARGET")
    args = parser.parse_args()
    try:
        if args.is_focused is not None:
            return 0 if is_focused(session_directory(), args.is_focused) else 1
        if args.enqueue is not None:
            enqueue(session_directory(), args.enqueue)
        else:
            focus(session_directory())
    except (OSError, sqlite3.Error) as error:
        print(f"wrix-focus-notification: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
