"""Native service-VM observations and known-live loopback controls; never a host-store bridge."""
import errno
import fcntl
import json
import os
from pathlib import Path
import selectors
import signal
import socket
import sqlite3
import subprocess
import sys

ROOT = Path('/run/wrix-native-platform-control')
OUTSIDE = Path('/var/empty/native-platform-outside-build')


def snapshot():
    base = Path('/sys/fs/cgroup/wrix-nix-pool')
    limits = {key: (base / key).read_text().strip() for key in (
        'cpu.max', 'memory.max', 'pids.max', 'work/memory.max', 'work/pids.max',
        'control/memory.max', 'control/pids.max', 'seed/memory.max', 'seed/pids.max')}
    expected = {'cpu.max': '200000 100000', 'memory.max': '3221225472',
                'pids.max': '256', 'work/memory.max': '1610612736',
                'work/pids.max': '144', 'control/memory.max': '402653184',
                'control/pids.max': '96', 'seed/memory.max': '805306368',
                'seed/pids.max': '16'}
    assert limits == expected, limits
    events = {}
    for kind in ('memory.events', 'work/memory.events', 'work/pids.events', 'cpu.stat'):
        events[kind] = {key: int(value) for key, value in
                        (line.split() for line in (base / kind).read_text().splitlines())}
    with open('/nix/var/nix/gc.lock', 'r+b') as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except OSError as error:
            assert error.errno in (errno.EAGAIN, errno.EACCES), error
        else:
            raise AssertionError('independent service GC exclusion disappeared')
    database = sqlite3.connect('file:/nix/var/nix/db/db.sqlite?mode=ro', uri=True)
    rows, nar_bytes = database.execute('select count(*), sum(narSize) from ValidPaths').fetchone()
    database.close()
    assert rows > 0 and nar_bytes > 0
    with socket.create_connection(('127.0.0.1', 18443), timeout=3) as peer:
        peer.sendall(b'probe\n')
        assert peer.recv(16) == b'ok\n'
    with socket.create_connection(('::1', 18443), timeout=3) as peer:
        peer.sendall(b'probe\n')
        assert peer.recv(16) == b'ok\n'
    readable = subprocess.run([sys.executable, '-c',
        'from pathlib import Path; print(Path("/var/empty/native-platform-outside-build").read_text(), end="")'],
        user=30001, group=30000, check=True, capture_output=True)
    assert readable.stdout == b'known-live-readable-service-control\n'
    print(json.dumps({'limits': limits, 'events': events,
                      'outside_readable_unprivileged': True,
                      'registered_paths': rows, 'registered_nar_bytes': nar_bytes,
                      'network_interfaces': Path('/proc/net/dev').read_text(),
                      'block_io': Path('/proc/diskstats').read_text(),
                      'known_live_ipv4': True, 'known_live_ipv6': True}), flush=True)


def serve():
    ROOT.mkdir(mode=0o700)
    token = os.urandom(32).hex().encode()
    (ROOT / 'token').write_bytes(token)
    with OUTSIDE.open('xb') as outside:
        outside.write(b'known-live-readable-service-control\n')
    OUTSIDE.chmod(0o644)
    listener = selectors.DefaultSelector()
    for family, host in ((socket.AF_INET, '127.0.0.1'), (socket.AF_INET6, '::1')):
        sock = socket.socket(family, socket.SOCK_STREAM)
        sock.bind((host, 18443))
        sock.listen(8)
        listener.register(sock, selectors.EVENT_READ)
    print(json.dumps({'ready': True, 'pid': os.getpid()}), flush=True)
    stopping = False
    while not stopping:
        for event, _ in listener.select(timeout=30):
            connection, _ = event.fileobj.accept()
            with connection:
                connection.settimeout(3)
                message = connection.recv(128).strip()
                if message == token:
                    stopping = True
                else:
                    assert message == b'probe', message
                connection.sendall(b'ok\n')
    for key in list(listener.get_map().values()):
        listener.unregister(key.fileobj)
        key.fileobj.close()
    listener.close()
    (ROOT / 'token').unlink()
    OUTSIDE.unlink()
    ROOT.rmdir()


assert os.geteuid() == 0
assert Path('/etc/wrix/nix-pool-service').read_text() == '1\n'
assert not Path('/etc/wrix/image-agent').exists()
if sys.argv[1:] == ['serve']:
    serve()
elif sys.argv[1:] == ['snapshot']:
    snapshot()
elif sys.argv[1:] == ['stop']:
    with socket.create_connection(('127.0.0.1', 18443), timeout=3) as peer:
        peer.sendall((ROOT / 'token').read_bytes() + b'\n')
        assert peer.recv(16) == b'ok\n'
elif sys.argv[1:] == ['kill-daemon']:
    configuration = json.loads(Path('/etc/wrix/nix-pool-native.json').read_text())
    pid = int(Path('/run/wrix-nix-pool/daemon.pid').read_text())
    assert pid > 1
    assert os.path.realpath('/proc/' + str(pid) + '/exe') == os.path.realpath(configuration['nix_daemon'])
    assert b'nix-daemon' in Path('/proc/' + str(pid) + '/cmdline').read_bytes()
    os.kill(pid, signal.SIGKILL)
    print(json.dumps({'signaled_native_daemon': pid, 'signal': int(signal.SIGKILL)}), flush=True)
else:
    raise SystemExit('invalid native service observation request')
