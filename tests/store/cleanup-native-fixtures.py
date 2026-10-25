#!/usr/bin/env python3
"""Tear down disposable native-test resources, never the user's Wrix deployment."""

import argparse
import json
import tempfile
import os
from pathlib import Path
import re
import shutil
import stat
import subprocess
import sys

MARKER = ".wrix-native-test-fixture"
STOPPED = ".wrix-native-test-resources-stopped"
LEGACY = re.compile(r"^(/tmp/\.tmp[A-Za-z0-9]+)(?:/|$)")
SOURCE = re.compile(r"wrix-nix-[0-9a-f]{32}")


def unescape(value):
    return re.sub(r"\\([0-7]{3})", lambda match: chr(int(match[1], 8)), value)


def mounts(text):
    result = []
    for line in text.splitlines():
        fields = line.split()
        separator = fields.index("-")
        result.append(
            (Path(unescape(fields[4])), fields[separator + 1], fields[separator + 2])
        )
    return result


def host_mounts():
    return mounts(Path("/proc/self/mountinfo").read_text())


def discover():
    roots = set()
    for target, kind, source in host_mounts():
        match = LEGACY.match(str(target))
        if match and kind == "fuse.sshfs" and SOURCE.fullmatch(source):
            roots.add(Path(match[1]))
    for proc in Path("/proc").iterdir():
        if not proc.name.isdecimal():
            continue
        try:
            if proc.stat().st_uid != os.getuid():
                continue
            args = (proc / "cmdline").read_bytes().split(b"\0")
        except (FileNotFoundError, ProcessLookupError):
            continue  # The process exited between enumeration and inspection.
        except PermissionError:
            continue  # Other users' protected processes are outside this cleanup.
        if not args or Path(os.fsdecode(args[0])).name != "conmon":
            continue
        if b"-n" not in args or not SOURCE.fullmatch(os.fsdecode(args[args.index(b"-n") + 1])):
            continue
        for arg in args:
            match = LEGACY.match(os.fsdecode(arg))
            if match and "/home/.local/share/containers/storage/overlay-containers/" in os.fsdecode(arg):
                roots.add(Path(match[1]))
    # A previous partial cleanup may have removed the visible exports and VMs,
    # leaving only the isolated storage and backing disks.
    for root in Path("/tmp").glob(".tmp*"):
        if not LEGACY.fullmatch(str(root)):
            continue
        try:
            if root.lstat().st_uid == os.getuid() and (
                (root / MARKER).is_file()
                or (root / "home/.local/state/wrix/control/nix").is_dir()
            ):
                roots.add(root)
        except (FileNotFoundError, PermissionError):
            continue
    return sorted(roots)


def checked_root(root, registered=False):
    info = root.lstat()
    if (
        not stat.S_ISDIR(info.st_mode)
        or info.st_uid != os.getuid()
        or stat.S_IMODE(info.st_mode) & 0o022
        or root.resolve() != root
    ):
        raise RuntimeError(
            f"unsafe fixture root: {root} "
            f"(owner={info.st_uid}, caller={os.getuid()}, "
            f"mode={stat.S_IMODE(info.st_mode):04o}, canonical={root.resolve()})"
        )
    marked = (root / MARKER).is_file() or registered
    if not marked and not LEGACY.fullmatch(str(root)):
        raise RuntimeError(f"not a marked native fixture or legacy /tmp/.tmp* fixture: {root}")
    state = root / "home/.local/state/wrix/control/nix"
    if not marked and (not state.is_dir() or state.is_symlink()):
        raise RuntimeError(f"legacy fixture has no native-pool control state: {root}")
    for relative in ["home", "runtime", "home/.local/share/containers/storage"]:
        if not (root / relative).resolve().is_relative_to(root):
            raise RuntimeError(f"fixture path escapes its private root: {root / relative}")
    return root


class Cleanup:
    def __init__(self, root, tools=None, registered=False):
        self.root = checked_root(Path(root), registered)
        self.tools = Path(tools) if tools else None
        self.environment = dict(os.environ)
        for name in [
            "CONTAINER_HOST", "CONTAINER_CONNECTION", "CONTAINERS_STORAGE_CONF",
            "CONTAINERS_CONF", "CONTAINERS_CONF_OVERRIDE", "CONTAINERS_CONF_MODULES",
            "XDG_DATA_DIRS", "XDG_CONFIG_DIRS",
        ]:
            self.environment.pop(name, None)
        self.environment.update(
            HOME=str(self.root / "home"),
            XDG_DATA_HOME=str(self.root / "home/.local/share"),
            XDG_CONFIG_HOME=str(self.root / "home/.config"),
            XDG_STATE_HOME=str(self.root / "home/.local/state"),
            XDG_RUNTIME_DIR=str(self.root / "runtime"),
        )
        self.storage = self.root / "home/.local/share/containers/storage"
        self.podman = [
            self.program("podman"), "--remote=false", "--root", str(self.storage),
            "--runroot", str(self.root / "runtime/containers"),
            "--tmpdir", str(self.root / "runtime/libpod/tmp"),
        ]

    def program(self, name):
        return str(self.tools / name) if self.tools else name

    def run(self, args, timeout=45):
        result = subprocess.run(
            args, env=self.environment, stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=timeout, check=False,
        )
        if result.returncode:
            raise RuntimeError(f"{args!r}: {os.fsdecode(result.stderr)}")
        return os.fsdecode(result.stdout)

    def views(self, table):
        selected = []
        for target, kind, source in table:
            if target.is_relative_to(self.root):
                if kind == "fuse.sshfs" and SOURCE.fullmatch(source):
                    relative = target.relative_to(self.root).parts
                    if (
                        relative[:6] != ("home", ".local", "state", "wrix", "control", "nix")
                        or len(relative) != 11
                        or relative[8] != "executions"
                        or relative[10] != "store"
                    ):
                        raise RuntimeError(f"unexpected fixture export location: {target}")
                    selected.append(target)
        return selected

    def capture_diagnostics(self):
        try:
            directory = self.root / "native-cleanup-diagnostics"
            directory.mkdir(mode=0o700, exist_ok=True)
            if directory.is_symlink() or directory.stat().st_uid != os.getuid() or stat.S_IMODE(directory.stat().st_mode) & 0o077:
                raise RuntimeError("unsafe fixture diagnostic directory")
            identifiers = self.run(self.podman + ["ps", "--all", "--no-trunc", "--format", "{{.ID}}"], timeout=3).splitlines()
            for identifier in identifiers:
                if not re.fullmatch(r"[0-9a-f]{64}", identifier):
                    raise RuntimeError("invalid fixture container identity for diagnostics")
                for label, arguments in [
                    ("state", ["inspect", "--format", "{{json .State}}", identifier]),
                    ("log", ["logs", "--tail=200", identifier]),
                ]:
                    output = self.run(self.podman + arguments, timeout=3)
                    with tempfile.NamedTemporaryFile(mode="w", dir=directory, prefix=identifier + "-" + label + "-", delete=False) as stream:
                        stream.write(output)
        except (OSError, RuntimeError, subprocess.TimeoutExpired) as error:
            print(f"could not capture fixture diagnostics; continuing resource teardown: {error}", file=sys.stderr)

    def stopped_identity(self):
        info = self.root.stat()
        return {"version": 1, "device": info.st_dev, "inode": info.st_ino}

    def is_stopped(self):
        try:
            descriptor = os.open(self.root / STOPPED, os.O_RDONLY | os.O_NOFOLLOW)
        except FileNotFoundError:
            return False
        with os.fdopen(descriptor) as stream:
            info = os.fstat(stream.fileno())
            if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or stat.S_IMODE(info.st_mode) != 0o600 or info.st_nlink != 1:
                raise RuntimeError("unsafe native test teardown receipt")
            if json.load(stream) != self.stopped_identity():
                raise RuntimeError("native test teardown receipt belongs to another root")
        return True

    def record_stopped(self):
        with tempfile.NamedTemporaryFile(mode="w", dir=self.root, prefix=".cleanup-", delete=False) as stream:
            json.dump(self.stopped_identity(), stream)
            stream.flush()
            os.fsync(stream.fileno())
            temporary = stream.name
        os.replace(temporary, self.root / STOPPED)
        descriptor = os.open(self.root, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(descriptor)
        finally:
            os.close(descriptor)

    def check_host_mounts(self):
        if any(target.is_relative_to(self.root) for target, _, _ in host_mounts()):
            raise RuntimeError(f"fixture still has host mounts: {self.root}")

    def remove_files(self):
        if not self.is_stopped():
            raise RuntimeError("fixture teardown must complete before deleting files")
        self.check_host_mounts()
        if self.storage.exists():
            shutil.rmtree(self.storage)
        retained = {MARKER, STOPPED, "runtime"}
        for entry in self.root.iterdir():
            if entry.name not in retained:
                if entry.is_dir() and not entry.is_symlink():
                    shutil.rmtree(entry)
                else:
                    entry.unlink()
        runtime = self.root / "runtime"
        if runtime.exists():
            shutil.rmtree(runtime)
        (self.root / MARKER).unlink(missing_ok=True)
        (self.root / STOPPED).unlink()
        self.root.rmdir()

    def finish(self, diagnostics=False):
        if self.is_stopped():
            self.check_host_mounts()
            return
        has_storage = self.storage.exists()
        if has_storage:
            runtime = self.root / "runtime"
            runtime.mkdir(mode=0o700, exist_ok=True)
            info = runtime.lstat()
            if not stat.S_ISDIR(info.st_mode) or info.st_uid != os.getuid() or stat.S_IMODE(info.st_mode) & 0o077:
                raise RuntimeError("fixture runtime must be an owned private directory")
            if diagnostics:
                self.capture_diagnostics()
            self.run(self.podman + ["stop", "--all", "--time=10"])
        for target in self.views(host_mounts()):
            helper = next(
                (path for path in ["/run/wrappers/bin/fusermount3", "/usr/bin/fusermount3", "/bin/fusermount3"] if os.access(path, os.X_OK)),
                "fusermount3",
            )
            self.run([helper, "-u", "--", str(target)])
        if has_storage:
            table = mounts(self.run(self.podman + ["unshare", self.program("cat"), "/proc/self/mountinfo"]))
            for target in self.views(table):
                self.run(self.podman + ["unshare", self.program("fusermount3"), "-u", "--", str(target)])
            self.run(self.podman + ["rm", "--all"])
            self.run(self.podman + ["rmi", "--all"])
            table = mounts(self.run(self.podman + ["unshare", self.program("cat"), "/proc/self/mountinfo"]))
            # Rootless Podman bind-mounts its storage/runtime directories in
            # the pause namespace. Those administrative mounts remain until
            # migrate terminates the pause process; they are not live exports.
            unexpected = [
                str(target) for target, kind, _ in table
                if target.is_relative_to(self.root)
                and (
                    kind.startswith("fuse.")
                    or not (
                        target == self.storage
                        or target.is_relative_to(self.root / "runtime")
                    )
                )
            ]
            if unexpected:
                raise RuntimeError(f"fixture still has non-administrative namespace mounts: {unexpected}")
            self.run(self.podman + ["unshare", self.program("chown"), "--recursive", "--no-dereference", "0:0", str(self.storage)])
            self.run(self.podman + ["unshare", self.program("chmod"), "--recursive", "u+rwX", str(self.storage)])
            self.run(self.podman + ["system", "migrate"])
        self.check_host_mounts()
        self.record_stopped()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("roots", nargs="*", type=Path)
    parser.add_argument("--discover", action="store_true", help="include this user's legacy test mounts and pool VMs")
    parser.add_argument("--registry", type=Path, help="cleanup only the roots recorded by one native test invocation")
    parser.add_argument("--tools", type=Path, help="package-selected runtime bin directory")
    parser.add_argument("--remove-files", action="store_true", help="delete fixture files only after verified resource teardown")
    parser.add_argument("--diagnostics", action="store_true", help="capture bounded fixture container state/logs before teardown")
    args = parser.parse_args()
    roots = set(args.roots)
    entries = {}
    if args.registry:
        info = args.registry.lstat()
        if not stat.S_ISDIR(info.st_mode) or info.st_uid != os.getuid() or stat.S_IMODE(info.st_mode) & 0o077:
            parser.error("fixture registry must be an owned private directory")
        for entry in args.registry.iterdir():
            info = entry.lstat()
            if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_size > 4096:
                parser.error(f"invalid fixture registry entry: {entry}")
            root = Path(os.fsdecode(entry.read_bytes()))
            roots.add(root)
            entries[root] = entry
    if args.discover:
        roots.update(discover())
    if not roots and not args.discover and not args.registry:
        parser.error("specify fixture roots, --registry or --discover")
    failed = False
    for root in sorted(roots):
        try:
            print(f"cleaning native test fixture: {root}", flush=True)
            subject = Cleanup(root, args.tools, registered=root in entries)
            subject.finish(diagnostics=args.diagnostics)
            if args.remove_files:
                subject.remove_files()
                print(f"native test resources and fixture files removed: {root}")
            else:
                print(f"native test resources stopped and unmounted; files retained: {root}")
            if args.remove_files and root in entries:
                entries[root].unlink()
        except (OSError, RuntimeError, ValueError, subprocess.TimeoutExpired) as error:
            failed = True
            print(f"cleanup incomplete; retain {root}: {error}", file=sys.stderr)
    return int(failed)


if __name__ == "__main__":
    sys.exit(main())
