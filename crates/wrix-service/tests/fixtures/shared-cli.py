#!/usr/bin/env python3
"""Subprocess harness adapter: execute the real production CLI dispatcher."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

with tempfile.TemporaryDirectory(prefix='wrix-cli-', dir=os.environ['WRIX_SHARED_FIXTURE']) as directory:
    result = Path(directory) / 'result.json'
    env = os.environ.copy()
    env['WRIX_SHARED_ARGUMENTS'] = json.dumps(sys.argv[1:])
    env['WRIX_SHARED_RESULT'] = str(result)
    process = subprocess.run([env['WRIX_SHARED_TEST_EXE'], '--exact', 'subprocess_entry', '--nocapture'], env=env, capture_output=True)
    if process.returncode != 0:
        sys.stderr.buffer.write(process.stderr)
        sys.exit(process.returncode)
    reply = json.loads(result.read_text())
    sys.stdout.write(reply['stdout'])
    sys.stderr.write(reply['stderr'])
    sys.exit(0 if reply['success'] else 1)
