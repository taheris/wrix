"""Execute the shipped guide through independent native shell calls."""

import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import time
import unittest


class NativeGuidance(unittest.TestCase):
    def test_documented_commands_support_native_debugging(self):
        root = Path(os.environ["REPO_ROOT"])
        blocks = re.findall(r"```bash\n(.*?)\n```", (root / "docs/tmux.md").read_text(), re.S)
        self.assertEqual(len(blocks), 6)
        env = dict(os.environ)
        with tempfile.TemporaryDirectory() as workspace:
            def call(command):
                return subprocess.run(
                    ["bash", "-c", "set -euo pipefail\n" + command],
                    env=env, cwd=workspace, text=True, capture_output=True, timeout=10,
                )

            def run(command):
                result = call(command)
                self.assertEqual(result.returncode, 0, result.stderr)
                return result.stdout

            def until(predicate):
                deadline = time.monotonic() + 5
                while time.monotonic() < deadline:
                    if predicate():
                        return
                    time.sleep(0.05)
                self.fail("documented workflow did not become ready within five seconds")

            created = run(blocks[0] + '\nprintf "TARGETS=%s %s %s\\n" "$workflow_dir" "$socket" "$pane"')
            directory, socket, pane = created.split("TARGETS=", 1)[1].strip().split()
            env.update(workflow_dir=directory, socket=socket, pane=pane)
            try:
                run(blocks[1])
                until(lambda: call("curl --fail --silent --max-time 1 http://127.0.0.1:8765/").returncode == 0)
                self.assertIn("dead=0", run(blocks[2]))
                until(lambda: 'GET /' in run('tmux -S "$socket" capture-pane -p -t "$pane" -S -1000'))
                run(blocks[3])
                until(lambda: run('tmux -S "$socket" display-message -p -t "$pane" "#{pane_dead}:#{pane_dead_status}"').strip() == "1:7")
                self.assertIn("fast failure", run('tmux -S "$socket" capture-pane -p -t "$pane" -S -1000'))
                literal = blocks[4].splitlines()
                run(literal[0])
                until(lambda: re.search(r"[$#]\s*$", run('tmux -S "$socket" capture-pane -p -t "$pane"')) is not None)
                run(literal[1])
                output = run('tmux -S "$socket" capture-pane -p -t "$pane"')
                self.assertNotIn("literal Enter C-c", output.splitlines())
                run(literal[2])
                until(lambda: 'literal Enter C-c' in run('tmux -S "$socket" capture-pane -p -t "$pane"').splitlines())
                run('tmux -S "$socket" send-keys -t "$pane" -l -- "sleep 100"; tmux -S "$socket" send-keys -t "$pane" Enter')
                until(lambda: run('tmux -S "$socket" display-message -p -t "$pane" "#{pane_current_command}"').strip() == "sleep")
                run(literal[3])
                until(lambda: run('tmux -S "$socket" display-message -p -t "$pane" "#{pane_current_command}"').strip() == "bash")
                run('tmux -S "$socket" new-session -d -s unrelated "sleep 100"')
                run(blocks[5])
                self.assertNotEqual(call('tmux -S "$socket" has-session -t "=debug"').returncode, 0)
                run('tmux -S "$socket" has-session -t "=unrelated"')
            finally:
                result = call('tmux -S "$socket" kill-server')
                if result.returncode:
                    self.fail(f"private workflow server cleanup failed: {result.stderr}")
                shutil.rmtree(directory)


if __name__ == "__main__":
    unittest.main()
