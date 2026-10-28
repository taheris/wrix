#!/usr/bin/env python3
"""Exercise the actual known-live server and its retained failure evidence."""
import json
import os
from pathlib import Path
import select
import socket
import subprocess
import sys
import tempfile
import time
import unittest

SCRIPT = Path(os.environ.get(
    'WRIX_TEST_NATIVE_BOUNDARY_SCRIPT', Path(__file__).with_name('native-boundary.py')))


class BoundaryTest(unittest.TestCase):
    def test_unexpected_connection_is_recorded_without_losing_positive_controls(self):
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary)
            with socket.socket() as reservation:
                reservation.bind(('127.0.0.1', 0))
                port = reservation.getsockname()[1]
            code = '''
import importlib.util
from pathlib import Path
import sys
spec = importlib.util.spec_from_file_location('native_boundary', sys.argv[1])
boundary = importlib.util.module_from_spec(spec)
spec.loader.exec_module(boundary)
boundary.ROOT = Path(sys.argv[2]) / 'control'
boundary.OUTSIDE = Path(sys.argv[2]) / 'outside'
boundary.PORT = int(sys.argv[3])
boundary.serve()
'''
            child = subprocess.Popen([sys.executable, '-c', code, str(SCRIPT), str(base), str(port)],
                                     stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            try:
                self.assertTrue(select.select([child.stdout], [], [], 5)[0])
                self.assertTrue(json.loads(child.stdout.readline())['ready'])
                with socket.create_connection(('127.0.0.1', port), timeout=3):
                    pass
                evidence = base / 'control/unexpected'
                deadline = time.monotonic() + 5
                while not evidence.read_text().endswith('\n'):
                    self.assertLess(time.monotonic(), deadline)
                    time.sleep(0.01)
                recorded = json.loads(evidence.read_text())
                self.assertEqual(recorded['bytes'], 0)
                self.assertEqual(recorded['peer'][0], '127.0.0.1')
                for host in ['127.0.0.1', '::1']:
                    with socket.create_connection((host, port), timeout=3) as peer:
                        peer.sendall(b'probe\n')
                        self.assertEqual(peer.recv(16), b'ok\n')
                self.assertIsNone(child.poll())
                with socket.create_connection(('127.0.0.1', port), timeout=3) as peer:
                    peer.sendall((base / 'control/token').read_bytes() + b'\n')
                    self.assertEqual(peer.recv(16), b'ok\n')
                self.assertEqual(child.wait(timeout=5), 0)
                self.assertFalse((base / 'control').exists())
                self.assertFalse((base / 'outside').exists())
            finally:
                if child.poll() is None:
                    child.kill()
                    child.wait(timeout=5)
                child.stdout.close()
                child.stderr.close()


if __name__ == '__main__':
    unittest.main()
