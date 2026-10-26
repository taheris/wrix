#!/usr/bin/env python3
"""Exercise UTS permission without granting authority in the worker namespace."""
import ctypes
import errno
import os
import subprocess
import sys

libc = ctypes.CDLL(None, use_errno=True)


def set_identity(allowed):
    for name, value in (("sethostname", b"localhost"), ("setdomainname", b"(none)")):
        ctypes.set_errno(0)
        result = getattr(libc, name)(value, len(value))
        if allowed:
            assert result == 0, (name, ctypes.get_errno())
        else:
            assert result == -1 and ctypes.get_errno() == errno.EPERM, (name, result, ctypes.get_errno())


if sys.argv[1:] == ["inside"]:
    set_identity(True)
    assert os.uname().nodename == "localhost"
    domain = ctypes.create_string_buffer(256)
    assert libc.getdomainname(domain, len(domain)) == 0
    assert domain.value == b"(none)"
    print("private user+UTS: both libc syscalls succeeded", flush=True)
else:
    status = dict(line.split(":", 1) for line in open("/proc/self/status") if ":" in line)
    for field in ("CapInh", "CapPrm", "CapEff", "CapBnd", "CapAmb"):
        assert not int(status[field], 16) & (1 << 21), (field, status[field])
        assert not int(status[field], 16) & (1 << 12), (field, status[field])
    assert status["Seccomp"].strip() == "2"
    assert os.getuid() == 0
    uid_map = [list(map(int, line.split())) for line in open("/proc/self/uid_map")]
    assert uid_map[0][0] == 0 and all(mapping[1] != 0 for mapping in uid_map), uid_map
    print("rootless UID mapping:", uid_map, flush=True)
    identity = os.uname()
    uts = os.readlink("/proc/self/ns/uts")
    assert uts != os.environ["TEST_HOST_UTS"]
    set_identity(False)
    ctypes.set_errno(0)
    assert libc.init_module(None, 0, b"") == -1 and ctypes.get_errno() == errno.EPERM
    if os.getgroups():
        os.setgroups([])
    subprocess.run(["unshare", "--user", "--map-root-user", "--uts", sys.executable, __file__, "inside"], check=True)
    assert os.uname() == identity
    assert os.readlink("/proc/self/ns/uts") == uts
    set_identity(False)
    print("worker UTS unchanged; seccomp active; SYS_ADMIN/NET_ADMIN absent from all capability sets", flush=True)
