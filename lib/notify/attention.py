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

PROC_ROOT = Path("/proc")
CLIENT_FORMAT = "#{client_name}\t#{client_pid}\t#{client_tty}\t#{session_id}\t#{window_id}\t#{pane_id}"


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
        if sys.platform != "darwin" and not linux_record_valid(record):
            return None
        if sys.platform == "darwin" and not record.get("terminal_window_title"):
            return None
        for key in ("tmux_target", "tmux_socket", "terminal_window_title"):
            if record.get(key) is not None and not isinstance(record[key], str):
                return None
        return record
    except (OSError, ValueError, AttributeError):
        return None


def linux_record_valid(record):
    if not isinstance(record, dict) or not isinstance(record.get("window_id"), str):
        return False
    route = record.get("linux_route")
    if not isinstance(route, dict) or type(route.get("terminal_pid")) is not int or route["terminal_pid"] <= 0:
        return False
    if record.get("tmux_target"):
        client = route.get("client")
        if (
            not isinstance(record["tmux_target"], str)
            or not re.fullmatch(r"%[0-9]+", record["tmux_target"])
            or not isinstance(record.get("tmux_socket"), str) or not record["tmux_socket"]
            or not isinstance(client, dict)
            or not all(isinstance(client.get(key), str) and client[key] for key in ("name", "tty", "session"))
            or type(client.get("pid")) is not int or client["pid"] <= 0
        ):
            return False
        if record.get("tmux_window") is not None and (
            not isinstance(record["tmux_window"], str) or not re.fullmatch(r"@[0-9]+", record["tmux_window"])
        ):
            return False
    return bool(record["window_id"])


@contextlib.contextmanager
def queue(directory):
    directory.mkdir(parents=True, exist_ok=True)
    path = directory / "attention.sqlite"
    fd = os.open(path, os.O_CREAT | os.O_RDWR, 0o600)
    os.close(fd)
    with contextlib.closing(sqlite3.connect(path, timeout=30)) as db:
        db.execute("CREATE TABLE IF NOT EXISTS pending (target TEXT PRIMARY KEY, route TEXT NOT NULL, sequence INTEGER NOT NULL)")
        db.execute("CREATE TABLE IF NOT EXISTS return_position (singleton INTEGER PRIMARY KEY CHECK (singleton = 1), route TEXT NOT NULL)")
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


def tmux_command(record):
    return ["tmux", "-S", record["tmux_socket"]] if record.get("tmux_socket") else ["tmux"]


def clients_on_socket(socket):
    text = run(["tmux", "-S", socket, "list-clients", "-F", CLIENT_FORMAT])
    if text is None:
        return None
    clients = []
    try:
        for line in text.splitlines():
            name, pid, tty, session, window, pane = line.split("\t")
            clients.append(dict(name=name, pid=int(pid), tty=tty, session=session, window=window, pane=pane))
    except ValueError:
        return None
    return clients


def validate(record, returning=False):
    if sys.platform != "darwin":
        if not linux_record_valid(record):
            return "stale"
        windows = run(["niri", "msg", "-j", "windows"])
        if windows is None:
            return "retry"
        try:
            if not any(str(window["id"]) == record["window_id"] and window.get("pid") == record["linux_route"]["terminal_pid"] for window in json.loads(windows)):
                return "stale"
        except (ValueError, TypeError, KeyError, AttributeError):
            return "retry"
    tmux_target = record.get("tmux_target")
    if tmux_target:
        tmux = tmux_command(record)
        if sys.platform != "darwin":
            client = record["linux_route"]["client"]
            clients = clients_on_socket(record["tmux_socket"])
            if clients is None:
                return "retry"
            identified = [other for other in clients if all(other[key] == client[key] for key in ("name", "pid", "tty"))]
            if len(identified) != 1:
                return "stale"
            attached = [other for other in clients if other["session"] == client["session"]]
            if returning:
                # Restore only this client. Selecting shared session state must
                # not move another attached client's window/pane.
                if any(other is not identified[0] for other in attached):
                    return "stale"
            elif attached != identified or identified[0]["session"] != client["session"]:
                return "stale"
            if not client_origin_matches(client, record["linux_route"]["terminal_pid"]):
                return "stale"
            panes = run(tmux + ["list-panes", "-a", "-F", "#{pane_id}\t#{session_id}"])
            if panes is None:
                return "retry"
            sessions = [line.split("\t")[1:] for line in panes.splitlines() if line.split("\t")[0] == tmux_target]
            if sessions != [[client["session"]]]:
                return "stale"
            pane_format = "#{pane_id} #{pane_dead} #{session_id}"
            expected = tmux_target + " 0 " + client["session"]
            if returning:
                if not record.get("tmux_window"):
                    return "stale"
                pane_format += " #{window_id}"
                expected += " " + record["tmux_window"]
            state = run(tmux + ["display-message", "-p", "-t", tmux_target, pane_format])
            if state != expected:
                return "stale"
        else:
            state = run(tmux + ["display-message", "-p", "-t", tmux_target,
                               "#{session_name}:#{window_index}.#{pane_index} #{pane_dead}"])
            if state != tmux_target + " 0":
                return "stale"
    return "success"


def process_ancestors(pid):
    try:
        ancestors = set()
        while pid > 1:
            if pid in ancestors or len(ancestors) >= 128:
                return None
            ancestors.add(pid)
            status = (PROC_ROOT / str(pid) / "status").read_text()
            pid = int(next(line.split()[1] for line in status.splitlines() if line.startswith("PPid:")))
        return ancestors
    except (OSError, ValueError, StopIteration):
        return None


def client_origin_matches(client, terminal_pid):
    try:
        if str((PROC_ROOT / str(client["pid"]) / "fd/0").readlink()) != client["tty"]:
            return False
    except OSError:
        return False
    ancestors = process_ancestors(client["pid"])
    return ancestors is not None and terminal_pid in ancestors


def navigate(record, returning=False):
    result = validate(record, returning=returning)
    if result != "success":
        return result
    tmux_target = record.get("tmux_target")
    tmux = tmux_command(record)
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
        if returning:
            client = record["linux_route"]["client"]
            if run(tmux + ["switch-client", "-c", client["name"], "-t", client["session"]]) is None:
                return "retry"
        if run(tmux + ["select-window", "-t", tmux_target]) is None:
            return "retry"
        if run(tmux + ["select-pane", "-t", tmux_target]) is None:
            return "retry"
    if sys.platform != "darwin":
        if run(["niri", "msg", "action", "focus-window", "--id", record["window_id"]]) is None:
            return "retry"
    return "success"


def socket_paths(directory):
    """Find host-owned tmux listeners, including unregistered origin terminals."""
    sockets = set()
    for path in directory.glob("*.json"):
        try:
            socket = json.loads(path.read_text()).get("tmux_socket")
            if isinstance(socket, str) and Path(socket).is_socket():
                sockets.add(socket)
        except (OSError, ValueError, AttributeError):
            continue  # best-effort: obsolete registrations are not discovery evidence.
    try:
        unix = {}
        for line in (PROC_ROOT / "net/unix").read_text().splitlines()[1:]:
            fields = line.split(None, 7)
            if len(fields) == 8 and fields[7].startswith("/"):
                unix[fields[6]] = fields[7]
        processes = list(PROC_ROOT.iterdir())
    except OSError:
        return None
    for process in processes:
        if not process.name.isdecimal():
            continue
        try:
            if process.stat().st_uid != os.getuid() or (process / "comm").read_text().strip() != "tmux: server":
                continue
            for fd in (process / "fd").iterdir():
                link = str(fd.readlink())
                inode = re.fullmatch(r"socket:\[([0-9]+)\]", link)
                if inode and inode[1] in unix:
                    sockets.add(unix[inode[1]])
        except OSError:
            continue  # best-effort: processes and their descriptors can exit during discovery.
    return sorted(sockets)


def capture_origin(directory):
    windows = run(["niri", "msg", "-j", "windows"])
    try:
        windows = json.loads(windows)
        focused = [window for window in windows if window.get("is_focused")]
        if len(focused) != 1:
            return None
        window = focused[0]
        pid = window.get("pid")
        if type(pid) is not int or pid <= 0:
            return None
        record = dict(window_id=str(window["id"]), linux_route=dict(terminal_pid=pid, client=None))
    except (ValueError, TypeError, KeyError, AttributeError):
        return None
    sockets = socket_paths(directory)
    if sockets is None:
        return None
    matches = []
    for socket in sockets:
        clients = clients_on_socket(socket)
        if clients is None:
            return None
        for client in clients:
            ancestors = process_ancestors(client["pid"])
            if ancestors is None:
                return None
            if pid in ancestors:
                if not client_origin_matches(client, pid):
                    return None  # An originating client with unproven TTY is not a window-only origin.
                matches.append((socket, client))
    if len(matches) > 1:
        return None
    if matches:
        # The focused ID suffices for a plain desktop window, but mapping a
        # tmux client to one of several windows sharing its ancestor PID is
        # ambiguous. Do not save a pane belonging to another terminal window.
        if sum(other.get("pid") == pid for other in windows) != 1:
            return None
        socket, client = matches[0]
        record.update(tmux_socket=socket, tmux_target=client["pane"], tmux_window=client["window"])
        record["linux_route"]["client"] = {key: client[key] for key in ("name", "pid", "tty", "session")}
    return record if validate(record, returning=bool(matches)) == "success" else None


def focus(directory):
    with queue(directory) as db:
        for target, identity in db.execute("SELECT target, route FROM pending ORDER BY sequence DESC").fetchall():
            record = registration(directory, target)
            if record is None or route_identity(record) != identity:
                result = "stale"
            else:
                origin = capture_origin(directory) if sys.platform != "darwin" else None
                result = navigate(record)
            if result in ("stale", "success"):
                db.execute("DELETE FROM pending WHERE target = ?", (target,))
            if result == "success":
                if sys.platform != "darwin":
                    # Always refresh, including same-window/same-pane jumps.
                    # An unavailable origin clears a previous (now unrelated)
                    # return position, but failed navigation never changes it.
                    db.execute("DELETE FROM return_position")
                    if origin is not None:
                        db.execute("INSERT INTO return_position VALUES (1, ?)", (json.dumps(origin),))
                return


def return_to_origin(directory):
    if sys.platform == "darwin":
        print("wrix-focus-notification: --return requires Linux/niri", file=sys.stderr)
        return 1
    with queue(directory) as db:
        row = db.execute("SELECT route FROM return_position WHERE singleton = 1").fetchone()
        if row is None:
            return 0
        try:
            record = json.loads(row[0])
        except ValueError:
            record = None
        result = navigate(record, returning=True) if linux_record_valid(record) else "stale"
        if result in ("stale", "success"):
            db.execute("DELETE FROM return_position")
    return 0


def terminal_window_focused(record):
    if sys.platform == "darwin":
        result = run(["osascript", "-e", '''on run argv
 tell application "System Events"
  if not (exists process (item 1 of argv)) then return false
  tell process (item 1 of argv)
   if not frontmost then return false
   set matches to windows whose name is (item 2 of argv)
   if (count of matches) is not 1 then return false
   return (name of (value of attribute "AXFocusedWindow")) is (item 2 of argv)
  end tell
 end tell
end run''', record["terminal_app"], record["terminal_window_title"]])
        return result == "true"
    windows = run(["niri", "msg", "-j", "windows"])
    try:
        focused = [window for window in json.loads(windows) if window.get("is_focused") is True]
        return (len(focused) == 1 and str(focused[0]["id"]) == record["window_id"]
                and focused[0].get("pid") == record["linux_route"]["terminal_pid"])
    except (ValueError, TypeError, KeyError, AttributeError):
        return False


def is_focused(directory, target):
    record = registration(directory, target)
    if record is None or validate(record) != "success" or not terminal_window_focused(record):
        return False
    if record.get("tmux_target"):
        if not record.get("tmux_socket"):
            return False
        pane_format = "#{window_active} #{pane_active} #{?session_attached,1,0}"
        state = run(tmux_command(record) + ["display-message", "-p", "-t", record["tmux_target"], pane_format])
        return state == "1 1 1"
    return True


def main():
    parser = argparse.ArgumentParser(description="Visit pending attention, or return to the position before the last jump.")
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--enqueue", metavar="TARGET")
    mode.add_argument("--is-focused", metavar="TARGET")
    mode.add_argument("--return", dest="return_position", action="store_true", help="Restore the position before the last successful jump (Linux/niri).")
    args = parser.parse_args()
    try:
        if args.return_position:
            return return_to_origin(session_directory())
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
