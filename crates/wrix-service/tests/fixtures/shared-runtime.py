#!/usr/bin/env python3
"""External Podman boundary fixture; commands run against real native service processes."""
import json
import os
from pathlib import Path
import re
import signal
import subprocess
import sys

root = Path(os.environ['WRIX_SHARED_FIXTURE'])
args = sys.argv[1:]
if args == ['--version']:
    print('podman version 6.1.4')
    sys.exit(0)
if args == ['info', '--format', 'json']:
    print(json.dumps({'version': {'Version': '6.1.4'}, 'host': {'security': {'rootless': True}},
                     'store': {'graphRoot': '/home/test/containers/storage', 'runRoot': '/run/user/1000/containers'}}))
    sys.exit(0)
with (root / 'runtime.log').open('a') as log:
    log.write(json.dumps(args) + '\n')
if Path(sys.argv[0]).name == 'container':
    legacy_file = root / 'retained-legacy.json'
    legacy = json.loads(legacy_file.read_text()) if legacy_file.exists() else []
    if args == ['list', '--all', '--format', 'json']:
        print(json.dumps(legacy))
        sys.exit(0)
    if args[0] == 'inspect' and legacy and args[-1] == legacy[0]['configuration']['id']:
        print(json.dumps(legacy))
        sys.exit(0)
    sys.exit(64)
record = root / 'runtime.json'
gateway_record = root / 'gateway.json'
selected_name = args[-1] if args[-1] != '--type=container' else args[-2]
if gateway_record.exists():
    gateway = json.loads(gateway_record.read_text())
    if selected_name == gateway['name']:
        record = gateway_record
state = json.loads(record.read_text()) if record.exists() else None
digest = 'sha256:' + 'a' * 64

def save():
    record.write_text(json.dumps(state))

def launch(data, transport, environment):
    log = (root / 'service.log').open('ab')
    environment = dict(environment) | {'SCCACHE_LOG': 'error', 'SCCACHE_ERROR_LOG': str(root / 'compiler.log')}
    return subprocess.Popen([
        'unshare', '--mount', '--propagation', 'private',
        'bash', '-c', 'set -euo pipefail; mount -t tmpfs -o mode=755 tmpfs /run; mkdir -m 700 /run/wrix; exec "$@"',
        'fixture-backend', os.environ['WRIX_SHARED_BINARY'], data, transport,
    ], env=environment, stdin=subprocess.DEVNULL, stdout=log, stderr=log, start_new_session=True)

if args[:2] == ['image', 'inspect']:
    print(digest)
elif args[:2] == ['image', 'exists'] or args[:2] == ['image', 'tag'] or args[0] == 'tag':
    pass
elif args == ['ps', '-a', '--format', '{{.Names}}']:
    if state:
        print(state['name'])
elif args[0] == 'inspect':
    if not state or args[3:] != [state['name'], '--type=container']:
        print('no such container', file=sys.stderr)
        sys.exit(1)
    template = args[2]
    if template == '{{.State.Running}}':
        print('true' if state['running'] else 'false')
    elif template == '{{.Image}}':
        print(digest)
    else:
        label = re.search(r'"([^"]+)"', template).group(1)
        print(state['labels'].get(label, ''))
elif args[0] == 'port':
    if state:
        for port in state['ports']:
            print(port.rsplit(':', 1)[1] + '/tcp -> ' + port.rsplit(':', 1)[0])
elif args[0] == 'run':
    labels = dict(args[i + 1].split('=', 1) for i, value in enumerate(args) if value == '--label')
    ports = [args[i + 1] for i, value in enumerate(args) if value == '-p']
    index = args.index('wrix-shared-service')
    if args[index + 1:] == ['gateway']:
        subprocess.run([os.environ['WRIX_TEST_SQL_CLIENT'], os.environ['WRIX_TEST_ROLES_CONTROL'], 'gateway', *args], check=True)
        gateway_record.write_text(json.dumps({'name': args[args.index('--name') + 1], 'labels': labels, 'ports': ports, 'running': True}))
        sys.exit(0)
    data, transport = args[index + 1:]
    (root / 'run.argv').write_text('\n'.join(args))
    for published in ports:
        listen, target = published.rsplit(':', 1)
        if target in ('3306', '3307', '4226'):
            subprocess.run([os.environ['WRIX_TEST_SQL_CLIENT'], os.environ['WRIX_TEST_ROLES_CONTROL'], 'forward', listen, '127.0.0.1:' + target], check=True)
    child = launch(data, transport, os.environ)
    state = {'name': args[args.index('--name') + 1], 'labels': labels, 'ports': ports, 'running': True, 'pid': child.pid,
             'data': data, 'transport': transport, 'boot': os.environ['WRIX_BACKEND_BOOT']}
    save()
elif args[0] == 'exec':
    index = 1
    environment = dict(os.environ)
    while args[index] in ('-i', '-e'):
        if args[index] == '-e':
            key, value = args[index + 1].split('=', 1)
            environment[key] = value
            index += 2
        else:
            index += 1
    if not state or args[index] != state['name'] or not state['running']:
        sys.exit(1)
    command = args[index + 1:]
    if command[0] == 'wrix-shared-service':
        command[0] = os.environ['WRIX_SHARED_BINARY']
    result = subprocess.run(['nsenter', '--mount', '--target', str(state['pid']), '--', *command], env=environment, cwd=state['data'], capture_output=True)
    with (root / 'native-exec.stderr').open('ab') as diagnostics:
        diagnostics.write(result.stderr)
    sys.stdout.buffer.write(result.stdout)
    sys.stderr.buffer.write(result.stderr)
    sys.exit(result.returncode)
elif args[0] == 'start':
    child = launch(state['data'], state['transport'], os.environ | {'WRIX_BACKEND_BOOT': state['boot']})
    state['pid'] = child.pid
    state['running'] = True
    save()
elif args[0] == 'stop':
    import fcntl
    import time
    os.killpg(state['pid'], signal.SIGTERM)
    deadline = time.monotonic() + 30
    with (Path(state['data']) / 'owner.lock').open('r+b') as owner:
        while True:
            try:
                fcntl.flock(owner.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
                fcntl.flock(owner.fileno(), fcntl.LOCK_UN)
                break
            except BlockingIOError:
                if time.monotonic() >= deadline:
                    raise TimeoutError('native service did not finish its graceful stop')
                time.sleep(0.02)
    state['running'] = False
    save()
elif args[0] == 'rm':
    if state and state['running']:
        raise RuntimeError('fixture rejects removal of a running backend')
    record.unlink(missing_ok=True)
elif args[0] == 'logs':
    print((root / 'service.log').read_text())
else:
    raise RuntimeError('unsupported external runtime invocation: ' + repr(args))
