#!/usr/bin/env python3
"""Exercise screenshot verifier cleanup after an external package build fails."""

import json
import os
from pathlib import Path
import shlex
import signal
import socket
import subprocess
import sys
import tempfile
import unittest
import urllib.request


def fail_package_build():
    args = sys.argv[2:]
    assert args[0] == "build", args
    repo_arg = args.index("--argstr") + 1
    assert args[repo_arg : repo_arg + 2] == ["repoRoot", os.environ["PLAYWRIGHT_REPO_ROOT"]], args
    assert ["packageName", "playwright-mcp"] == args[-2:], args
    temp_root = Path(os.environ["TMPDIR"])
    port_file, = temp_root.glob("*/http_port")
    port = int(port_file.read_text())
    with urllib.request.urlopen(f"http://127.0.0.1:{port}/", timeout=5) as response:
        assert b"Playwright MCP Screenshot Test" in response.read()
    server_path = str(port_file.parent / "server.py")
    processes = subprocess.check_output(["ps", "-eo", "pid=,ppid=,args="], text=True)
    matches = [line.split(None, 2) for line in processes.splitlines() if server_path in line]
    process, = matches
    Path(os.environ["CLEANUP_EVIDENCE"]).write_text(json.dumps({
        "pid": int(process[0]),
        "ppid": int(process[1]),
        "port": port,
        "temp_dir": str(port_file.parent),
    }))
    print("Injected Playwright package build failure", file=sys.stderr)
    return 42


class ScreenshotCleanupTest(unittest.TestCase):
    def test_setup_failure_terminates_and_reaps_http_child(self):
        script = Path(__file__).with_name("screenshot-test.sh")
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            temp_root = root / "tmp"
            temp_root.mkdir()
            bin_dir = root / "bin"
            bin_dir.mkdir()
            evidence = root / "evidence.json"
            nix = bin_dir / "nix"
            nix.write_text(
                "#!/usr/bin/env bash\nset -euo pipefail\nexec "
                + shlex.join([sys.executable, str(Path(__file__).resolve()), "--fail-package-build"])
                + ' "$@"\n'
            )
            nix.chmod(0o755)
            env = dict(os.environ)
            env.update({
                "PATH": str(bin_dir) + os.pathsep + env["PATH"],
                "TMPDIR": str(temp_root),
                "CLEANUP_EVIDENCE": str(evidence),
                "PLAYWRIGHT_SYSTEM": "x86_64-linux",
                "PLAYWRIGHT_REPO_ROOT": str(script.resolve().parents[3]),
                "PLAYWRIGHT_SERVER_CONFIG_FILE": "",
                "WRIX_MCP_MANIFEST": "",
            })
            process = subprocess.Popen(
                ["bash", str(script), "test_screenshot_returns_png"],
                env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True,
            )
            state = None
            try:
                output, _ = process.communicate(timeout=20)
                self.assertTrue(evidence.exists(), output)
                state = json.loads(evidence.read_text())
                self.assertEqual(process.returncode, 1, output)
                self.assertIn("Injected Playwright package build failure", output)
                self.assertEqual(state["ppid"], process.pid, "HTTP PID must belong to the verifier caller")
                with self.assertRaises(ProcessLookupError, msg="HTTP child is still alive or unreaped"):
                    os.kill(state["pid"], 0)
                with socket.socket() as listener:
                    listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
                    listener.bind(("127.0.0.1", state["port"]))
                self.assertFalse(Path(state["temp_dir"]).exists())
                self.assertEqual(list(temp_root.iterdir()), [])
            finally:
                if process.poll() is None:
                    process.kill()
                    process.communicate()
                if state is None and evidence.exists():
                    state = json.loads(evidence.read_text())
                if state is not None:
                    try:
                        os.kill(state["pid"], signal.SIGTERM)
                    except ProcessLookupError:
                        pass  # The verifier already terminated and reaped its child.


if __name__ == "__main__":
    if sys.argv[1:2] == ["--fail-package-build"]:
        sys.exit(fail_package_build())
    unittest.main()
