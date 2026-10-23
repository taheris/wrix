#!/usr/bin/env python3
"""Owned external supervision boundary; the replacement controller is native Wrix."""

import fcntl
import json
from pathlib import Path
import signal
import socket
import subprocess
import sys
import time


def legacy(root):
    with (root / "authority/ownership").open("r+") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        (root / "legacy-ready").touch()
        while True:
            time.sleep(1)


def request(path, args):
    with socket.socket(socket.AF_UNIX) as client:
        client.connect(str(path))
        client.sendall(json.dumps(args).encode() + b"\n")
        client.shutdown(socket.SHUT_WR)
        response = b""
        while data := client.recv(4096):
            response += data
    reply = json.loads(response)
    sys.stdout.write(reply["stdout"])
    sys.stderr.write(reply["stderr"])
    return reply["code"]


def serve(root, executable):
    child = subprocess.Popen(
        [sys.executable, __file__, "legacy", str(root)],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=(root / "legacy.stderr").open("wb"),
    )
    listener = socket.socket(socket.AF_UNIX)
    listener.bind(str(root / "manager.sock"))
    listener.listen(4)
    listener.settimeout(0.1)
    stopping = False

    def terminate(_signal, _frame):
        nonlocal stopping
        stopping = True

    signal.signal(signal.SIGTERM, terminate)
    try:
        while not stopping:
            try:
                connection, _address = listener.accept()
            except TimeoutError:
                continue
            with connection:
                payload = b""
                while data := connection.recv(4096):
                    payload += data
                    if len(payload) > 8192:
                        raise ValueError("oversized fixture request")
                args = json.loads(payload)
                code, output, error = 0, "", ""
                with (root / "manager.calls").open("a") as log:
                    log.write(json.dumps(args) + "\n")
                if args == ["--user", "show-environment"]:
                    pass
                elif len(args) == 4 and args[:2] == ["--user", "show"]:
                    output = "LoadState=loaded\nActiveState=active\nFragmentPath=" + str(root / "user-units/wrix-host-controller.service") + "\n"
                elif args == ["--user", "stop", "wrix-host-controller.service"]:
                    if child is not None:
                        child.terminate()
                        child.wait(timeout=10)
                        child = None
                elif args == ["--user", "start", "wrix-host-controller.service"]:
                    if child is None:
                        child = subprocess.Popen(
                            [executable, str(root)],
                            stdin=subprocess.DEVNULL,
                            stdout=subprocess.DEVNULL,
                            stderr=(root / "new-controller.stderr").open("wb"),
                        )
                elif args == ["--user", "daemon-reload"]:
                    pass
                elif len(args) == 3 and args[:2] == ["--user", "link"] and Path(args[2]) == root / "wrix-host-controller.service":
                    linked = root / "user-units/wrix-host-controller.service"
                    linked.parent.mkdir(exist_ok=True)
                    linked.unlink(missing_ok=True)
                    linked.symlink_to(Path(args[2]))
                else:
                    code, error = 91, "unexpected fixture supervisor request: " + repr(args)
                connection.sendall(json.dumps({"code": code, "stdout": output, "stderr": error}).encode())
    finally:
        listener.close()
        if child is not None:
            child.terminate()
            child.wait(timeout=10)


if __name__ == "__main__":
    mode = sys.argv[1]
    if mode == "legacy":
        legacy(Path(sys.argv[2]))
    elif mode == "request":
        sys.exit(request(Path(sys.argv[2]), sys.argv[3:]))
    elif mode == "serve":
        serve(Path(sys.argv[2]), sys.argv[3])
    else:
        raise ValueError("unknown fixture mode")
