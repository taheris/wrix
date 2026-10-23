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
state = json.loads(record.read_text()) if record.exists() else None
digest = 'sha256:' + 'a' * 64

def save():
    record.write_text(json.dumps(state))

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
    (root / 'run.argv').write_text('\n'.join(args))
    labels = dict(args[i + 1].split('=', 1) for i, value in enumerate(args) if value == '--label')
    ports = [args[i + 1] for i, value in enumerate(args) if value == '-p']
    index = args.index('wrix-shared-service')
    data, transport = args[index + 1:]
    log = (root / 'service.log').open('ab')
    child = subprocess.Popen([os.environ['WRIX_SHARED_BINARY'], data, transport], stdin=subprocess.DEVNULL,
                             stdout=log, stderr=log, start_new_session=True)
    state = {'name': args[args.index('--name') + 1], 'labels': labels, 'ports': ports, 'running': True, 'pid': child.pid,
             'data': data, 'transport': transport}
    save()
elif args[0] == 'exec':
    index = 2 if args[1] == '-i' else 1
    if not state or args[index] != state['name'] or not state['running']:
        sys.exit(1)
    os.execvpe(args[index + 1], args[index + 1:], os.environ)
elif args[0] == 'start':
    log = (root / 'service.log').open('ab')
    child = subprocess.Popen([os.environ['WRIX_SHARED_BINARY'], state['data'], state['transport']], stdin=subprocess.DEVNULL,
                             stdout=log, stderr=log, start_new_session=True)
    state['pid'] = child.pid
    state['running'] = True
    save()
elif args[0] == 'stop':
    os.killpg(state['pid'], signal.SIGTERM)
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
