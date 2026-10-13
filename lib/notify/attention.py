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
    return json.dumps({key: record.get(key) for key in (
        "focus_target", "window_id", "terminal_app", "terminal_window_title", "tmux_target", "tmux_socket"
    )}, sort_keys=True)


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


def navigate(record):
    if sys.platform != "darwin":
        windows = run(["niri", "msg", "-j", "windows"])
        if windows is None:
            return "retry"
        try:
            if not any(str(window["id"]) == record["window_id"] for window in json.loads(windows)):
                return "stale"
        except (ValueError, TypeError, KeyError):
            return "retry"
    tmux_target = record.get("tmux_target")
    if tmux_target:
        tmux = ["tmux"]
        if record.get("tmux_socket"):
            tmux += ["-S", record["tmux_socket"]]
        state = run(tmux + ["display-message", "-p", "-t", tmux_target,
                           "#{session_name}:#{window_index}.#{pane_index} #{pane_dead}"])
        if state != tmux_target + " 0":
            return "stale"
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


def main():
    parser = argparse.ArgumentParser(description="Visit the newest pending agent terminal.")
    parser.add_argument("--enqueue", metavar="TARGET")
    args = parser.parse_args()
    try:
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
