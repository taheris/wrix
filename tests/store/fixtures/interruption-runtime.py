#!/usr/bin/env python3
"""External process/Podman/kernel-observation stubs; never native acceptance."""
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time

name = Path(sys.argv[0]).name
root = Path(os.environ["WRIX_FAKE_NATIVE_ROOT"])
scenario = os.environ.get("WRIX_FAKE_SCENARIO", "pass")
state_path = root / "fake-state.json"


def load():
    return json.loads(state_path.read_text())


def save(value):
    state_path.write_text(json.dumps(value))


if name == "native":
    root.mkdir(mode=0o700)
    (root / "home/.local/share/containers/storage").mkdir(parents=True)
    (root / "runtime").mkdir(mode=0o700)
    (root / ".wrix-native-test-fixture").write_text("1\n")
    registry = Path(os.environ["WRIX_TEST_POOL_FIXTURE_REGISTRY"])
    (registry / "root").write_text(str(root))
    save({"running": True, "containers": True, "images": True, "views": ["c", "d"], "migrated": False})
    if scenario in ("early-exit", "early-success"):
        sys.exit(7 if scenario == "early-exit" else 0)
    if scenario == "empty-ready":
        save({"running": False, "containers": False, "images": False, "views": [], "migrated": False})
    identity = root.stat()
    nonce = os.environ["WRIX_TEST_POOL_INTERRUPT_NONCE"]
    progress = {"version": 1, "phase": "worker-image", "nonce": "e" * 64 if scenario == "foreign-progress" else nonce, "device": identity.st_dev, "inode": identity.st_ino}
    pending = root / ".wrix-native-test-progress.pending"
    pending.write_text(json.dumps(progress))
    pending.chmod(0o600)
    pending.rename(root / ".wrix-native-test-progress.json")
    if scenario == "term-resistant-helper":
        ready = root / "helper-ready"
        helper = subprocess.Popen([sys.executable, "-c", "import signal, sys, time; from pathlib import Path; signal.signal(signal.SIGTERM, signal.SIG_IGN); Path(sys.argv[1]).touch(); time.sleep(60)", str(ready)], process_group=0)
        (root / "escaped-pid").write_text(str(helper.pid))
        deadline = time.monotonic() + 5
        while not ready.exists():
            assert helper.poll() is None and time.monotonic() < deadline
            time.sleep(0.01)
    if scenario != "timeout":
        identity = root.stat()
        nonce = os.environ["WRIX_TEST_POOL_INTERRUPT_NONCE"]
        record = {"version": 1, "phase": "workers-ready", "nonce": "e" * 64 if scenario == "foreign" else nonce, "device": identity.st_dev, "inode": identity.st_ino}
        pending = root / "ready.pending"
        pending.write_text(json.dumps(record))
        pending.chmod(0o644 if scenario == "unsafe-ready" else 0o600)
        pending.rename(root / ".wrix-native-test-ready.json")
    if scenario == "escaped-helper":
        helper = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(60)"], process_group=0)
        (root / "escaped-pid").write_text(str(helper.pid))
    if scenario == "hung-supervisor":
        signal.signal(signal.SIGTERM, signal.SIG_IGN)
    while True:
        time.sleep(1)
elif name == "git":
    assert sys.argv[1:] == ["rev-parse", "HEAD"]
    print("a" * 40)
elif name == "readlink":
    assert sys.argv[1:] == ["/proc/self/ns/mnt"]
    print("mnt:[2]" if scenario == "changed-namespace" and load()["migrated"] else "mnt:[1]")
elif name == "cat":
    assert sys.argv[1:] == ["/proc/self/mountinfo"]
    state = load()
    views = ["c", "d"] if scenario == "post-mount" and state["migrated"] else state["views"]
    for key in views:
        target = root / "home/.local/state/wrix/control/nix" / ("b" * 32) / "x86_64-linux/executions" / (key * 64) / "store"
        escaped = str(target).replace("\\", "\\134").replace(" ", "\\040")
        print(f"90 2 0:99 / {escaped} ro,nosuid,nodev - fuse.sshfs wrix-nix-{'a' * 32} ro")
elif name == "fusermount3":
    assert sys.argv[1:3] == ["-u", "--"]
    state = load()
    key = Path(sys.argv[3]).parent.name[0]
    state["views"].remove(key)
    save(state)
elif name == "podman":
    arguments = sys.argv[1:]
    assert arguments[:1] == ["--remote=false"]
    assert arguments[arguments.index("--root") + 1] == str(root / "home/.local/share/containers/storage")
    assert arguments[arguments.index("--runroot") + 1] == str(root / "runtime/containers")
    assert arguments[arguments.index("--tmpdir") + 1] == str(root / "runtime/libpod/tmp")
    arguments = arguments[7:]
    if arguments[0] == "unshare":
        command = arguments[1:]
        if Path(command[0]).name in ("chown", "chmod"):
            sys.exit(0)  # Only the real Podman user namespace may normalize mapped owners.
        os.execv(command[0], command)
    state = load()
    operation = arguments[0]
    if operation == "ps":
        assert arguments in (["ps", "--no-trunc", "--format", "{{.ID}}"], ["ps", "--all", "--no-trunc", "--format", "{{.ID}}"])
        present = state["containers"] if "--all" in arguments else state["running"]
        if scenario == "post-container" and state["migrated"]:
            present = True
        if present:
            print("\n".join(key * 64 for key in ("1", "2", "3")))
    elif operation == "images":
        if scenario == "inspection-failure" and state["migrated"]:
            sys.exit(1)
        assert arguments == ["images", "--no-trunc", "--format", "{{.ID}}"]
        if state["images"]:
            print("sha256:" + "4" * 64)
    elif operation == "inspect":
        assert arguments[1:3] == ["--format", "{{json .State}}"]
        assert len(arguments[3]) == 64
        print('{"Status":"running"}')
    elif operation == "logs":
        assert arguments[1:2] == ["--tail=200"]
        assert len(arguments[2]) == 64
        print("retained fake native diagnostic")
    elif operation == "stop":
        assert arguments == ["stop", "--all", "--time=10"]
        if scenario == "hung-cleanup":
            signal.signal(signal.SIGTERM, signal.SIG_IGN)
            while True:
                time.sleep(1)
        if scenario == "cleanup-failure":
            sys.exit(1)
        state["running"] = False
    elif operation == "rm":
        assert arguments == ["rm", "--all"]
        state["containers"] = False
    elif operation == "rmi":
        assert arguments == ["rmi", "--all"]
        state["images"] = False
    elif operation == "system":
        assert arguments == ["system", "migrate"]
        state["migrated"] = True
    else:
        raise AssertionError(arguments)
    save(state)
else:
    raise AssertionError(name)
