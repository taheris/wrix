#!/usr/bin/env python3
"""Public CLI conformance with native SQL; only the container boundary is mocked."""
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time

def terminate(signum, frame):
    raise TimeoutError('native public CLI fixture deadline reached')


signal.signal(signal.SIGTERM, terminate)
root = Path(sys.argv[1])
wrix = sys.argv[2]
root.joinpath('home').mkdir()
root.joinpath('empty-git-template').mkdir()
bin_dir = root / 'bin'
env = os.environ.copy()
env.update(HOME=str(root / 'home'), XDG_STATE_HOME=str(root / 'state'),
           XDG_CACHE_HOME=str(root / 'cache'), WRIX_SHARED_FIXTURE=str(root),
           WRIX_SERVICE_PACKAGE_BINDING=str(root / 'binding.json'),
           WRIX_CONTAINER_RUNTIME=str(bin_dir / 'podman'), WRIX_DOLT_TRANSPORT='unix',
           PATH=str(bin_dir) + ':' + env['PATH'], WRIX_BIN=wrix,
           GIT_TEMPLATE_DIR=str(root / 'empty-git-template'))
for key in ['GIT_DIR', 'GIT_COMMON_DIR', 'GIT_WORK_TREE', 'GIT_INDEX_FILE', 'GIT_OBJECT_DIRECTORY', 'GIT_ALTERNATE_OBJECT_DIRECTORIES', 'GIT_CONFIG_PARAMETERS']:
    env.pop(key, None)
for key in list(env):
    if key.startswith('BEADS_') or key in ['DOLT_ROOT_HOST', 'DOLT_ROOT_PASSWORD', 'DOLT_CLI_PASSWORD', 'LOOM_INSIDE', 'IS_SANDBOX']:
        env.pop(key)


def run(args, cwd, extra=None, success=True):
    result = subprocess.run(args, cwd=cwd, env=env | (extra or {}), capture_output=True, text=True, timeout=120)
    if success:
        assert result.returncode == 0, (args, result.stdout, result.stderr, (root / 'service.log').read_text() if (root / 'service.log').exists() else '')
    return result


def cli(workspace, *args, success=True):
    return run([wrix, *args], workspace, success=success)


def sql(client, query, success=True):
    creds = client['credentials']
    return run(['dolt', '--host', '127.0.0.1', '--port', '3306', '--no-tls', '--password', creds['password'],
                '--user', creds['user'], '--use-db', creds['database'], 'sql', '-r', 'json', '-q', query],
               root, {'DOLT_CLI_PASSWORD': creds['password']}, success)


try:
    workspaces = [root / name for name in ['first', 'second']]
    for workspace in workspaces:
        workspace.mkdir()
        run(['git', 'init', '-q'], workspace)
        (workspace / 'tracked').write_text('source content')
        run(['git', 'add', 'tracked'], workspace)
        run(['git', '-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid', 'commit', '-qm', 'initial'], workspace)
        worktree = workspace / '.git/beads-worktrees/beads'
        run(['git', 'worktree', 'add', '-qb', 'beads', str(worktree)], workspace)
        upstream = root / (workspace.name + '-upstream.git')
        run(['git', 'init', '--bare', '-q', str(upstream)], root)
        run(['git', 'remote', 'add', 'origin', str(upstream)], workspace)
        run(['git', 'push', '-u', 'origin', 'beads'], worktree)
        (workspace / '.beads').mkdir()
        (workspace / '.beads/config.yaml').write_text('sync-branch: beads\n')
        (workspace / '.beads/metadata.json').write_text('{"database":"dolt","backend":"dolt","dolt_database":"beads","dolt_mode":"server"}')
        seed = root / ('seed-' + workspace.name)
        seed.mkdir()
        run([env['WRIX_REAL_BD_BIN'], 'init', '--quiet', '--skip-hooks', '--skip-agents', '--non-interactive', '--prefix', 'fixture'], seed, {'BEADS_DOLT_MODE': 'embedded'})
        source = workspace / '.beads/dolt/beads'
        source.parent.mkdir()
        (seed / '.beads/embeddeddolt/fixture').rename(source)
        run(['dolt', 'config', '--global', '--add', 'user.name', 'Fixture'], source)
        run(['dolt', 'config', '--global', '--add', 'user.email', 'fixture@example.invalid'], source)
        run(['dolt', 'sql', '-q', "CREATE TABLE marker(id INT PRIMARY KEY); INSERT INTO marker VALUES(1); CALL DOLT_ADD('.'); CALL DOLT_COMMIT('-m','checkpoint'); CALL DOLT_BRANCH('retained'); CALL DOLT_TAG('retainedtag'); INSERT INTO marker VALUES(2); CALL DOLT_ADD('.'); INSERT INTO marker VALUES(3)"], source)
    cli(workspaces[0], 'workspace', 'acknowledge-scope', '--version', '1')
    clients = []
    for workspace in workspaces:
        cli(workspace, 'service', 'start', '--no-cache')
        clients.append(json.loads(cli(workspace, 'service', 'dolt', 'client').stdout))
    assert clients[0]['credentials']['database'] != clients[1]['credentials']['database']
    # A genuinely adopted managed checkout must not revive JSONL import from
    # ambient native Git hooks; its independently chained hook still executes.
    workspace = workspaces[0]
    legacy_jsonl = workspace / '.beads/issues.jsonl'
    legacy_jsonl.write_text('legacy JSONL must not be imported\n')
    hooks = workspace / '.git/hooks'
    hooks.mkdir(exist_ok=True)
    (hooks / 'post-checkout').write_text('#!/usr/bin/env bash\nset -euo pipefail\nexec bd hooks run post-checkout "$@"\n')
    (hooks / 'post-checkout.old').write_text('#!/usr/bin/env bash\nset -euo pipefail\ntouch chained-hook-ran\n')
    (hooks / 'post-checkout').chmod(0o755)
    (hooks / 'post-checkout.old').chmod(0o755)
    checkout = run(['git', 'checkout', '-b', 'after-managed-start'], workspace)
    assert 'JSONL import' not in checkout.stderr, checkout.stderr
    assert 'no Dolt remote' not in checkout.stderr, checkout.stderr
    assert (workspace / 'chained-hook-ran').is_file()
    assert not (workspace / '.beads/dolt-server.pid').exists()
    assert legacy_jsonl.read_text() == 'legacy JSONL must not be imported\n'
    for client in clients:
        readiness = json.loads(sql(client, 'SELECT @@GLOBAL.secure_file_priv AS secure_file_priv').stdout)
        assert readiness == {'rows': [{'secure_file_priv': '/dev/null'}]}, readiness
        rows = json.loads(sql(client, 'SELECT * FROM marker ORDER BY id').stdout)['rows']
        assert [int(row['id']) for row in rows] == [1, 2, 3], rows
        assert json.loads(sql(client, "SELECT name FROM dolt_branches WHERE name = 'retained'").stdout)['rows'] == [{'name': 'retained'}]
        assert json.loads(sql(client, "SELECT tag_name FROM dolt_tags WHERE tag_name = 'retainedtag'").stdout)['rows'] == [{'tag_name': 'retainedtag'}]
        assert len(json.loads(sql(client, 'SELECT staged FROM dolt_status').stdout)['rows']) == 2
        assert Path(client['git_common']).is_relative_to(root / 'state/wrix/host-control/beads-git')
        sql(client, 'INSERT INTO marker VALUES(4)')
    peer = clients[1]['credentials']['database']
    assert sql(clients[0], f'SELECT * FROM `{peer}`.marker', success=False).returncode != 0
    for user in ['root', 'wrixadmin']:
        for password in ['', 'wrong-private-password']:
            bad = {'credentials': clients[0]['credentials'] | {'user': user, 'password': password}}
            assert sql(bad, 'SELECT CURRENT_USER()', success=False).returncode != 0
    cli(workspaces[0], 'service', 'stop', '--no-cache')
    assert len(json.loads(sql(clients[1], 'SELECT * FROM marker').stdout)['rows']) == 4
    assert sql(clients[0], 'SELECT * FROM marker', success=False).returncode != 0
    cli(workspaces[0], 'service', 'start', '--no-cache')
    renewed = json.loads(cli(workspaces[0], 'service', 'dolt', 'client').stdout)
    assert len(json.loads(sql(renewed, 'SELECT * FROM marker').stdout)['rows']) == 4
    calls = [json.loads(line) for line in (root / 'runtime.log').read_text().splitlines()]
    runs = [call for call in calls if call[0] == 'run']
    assert len(runs) == 1, runs
    assert not any(call[0] == 'rm' for call in calls), calls
    assert 'wrix.dolt.shared=native-v1' in runs[0]
    for workspace, client in zip(workspaces, clients):
        guest = json.loads(cli(workspace, 'service', 'dolt', 'sandbox-client').stdout)
        assert Path(guest['git_common']).is_relative_to(root / 'state/wrix/service-data/dolt/namespaces')
        assert not Path(client['git_common']).is_relative_to(root / 'state/wrix/service-data')
        assert guest['git_common'] != client['git_common']
    assert not any('host-control' in arg or '/first/.git' in arg or '/second/.git' in arg for arg in runs[0])
    print('public CLI: one native shared backend; retained rows/branches, scoped writes, peer/admin denial, retire/reactivate preserved peer')
finally:
    record = root / 'runtime.json'
    if record.exists():
        state = json.loads(record.read_text())
        try:
            os.killpg(state['pid'], signal.SIGTERM)
        except ProcessLookupError:
            # The native entrypoint can already have exited after a startup failure.
            pass
        deadline = time.monotonic() + 2
        while time.monotonic() < deadline:
            try:
                os.killpg(state['pid'], 0)
            except ProcessLookupError:
                break
            time.sleep(0.02)
        else:
            try:
                os.killpg(state['pid'], signal.SIGKILL)
            except ProcessLookupError:
                # The process group exited between the bounded wait and kill.
                pass
