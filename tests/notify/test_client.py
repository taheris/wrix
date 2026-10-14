import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import unittest


class ClientTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="wrix-notify-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.runtime = self.root / "runtime"
        self.ip_calls = self.root / "ip-calls"
        self.env = {
            key: value
            for key, value in os.environ.items()
            if not key.startswith("WRIX_NOTIFY_")
            and key not in ("WRIX_FOCUS_TARGET", "WRIX_SESSION_ID", "XDG_RUNTIME_DIR")
        }
        self.env.update(
            HOME=str(self.root),
            XDG_RUNTIME_DIR=str(self.runtime),
            WRIX_NOTIFY_TEST_IP_CALLS=str(self.ip_calls),
        )
        self.host = os.environ["WRIX_TEST_NOTIFY_HOST"]
        self.client = os.environ["WRIX_TEST_NOTIFY_CLIENT"]
        self.ip = os.environ["WRIX_TEST_IP_COMMAND"]

    def invoke(self, binary, *arguments, **environment):
        result = subprocess.run(
            [binary, *arguments],
            env=self.env | environment,
            capture_output=True,
            text=True,
            timeout=3,
            check=False,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        return result

    def tcp_listener(self, port=0):
        listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self.addCleanup(listener.close)
        listener.bind(("127.0.0.1", port))
        listener.listen(1)
        listener.settimeout(2)
        return listener

    def unix_socket(self, directory):
        path = directory / "wrix" / "notify.sock"
        path.parent.mkdir(parents=True)
        listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.addCleanup(listener.close)
        listener.bind(str(path))
        listener.settimeout(2)
        return listener

    def envelope(self, listener):
        connection, _ = listener.accept()
        with connection:
            connection.settimeout(2)
            data = b""
            while chunk := connection.recv(4096):
                data += chunk
        self.assertEqual(data.count(b"\n"), 1, data)
        return json.loads(data)

    def test_gateway_fixture_conforms_to_ip_route_interface(self):
        routes = "default via 127.0.0.1 dev eth0"
        result = self.invoke(self.ip, "route", WRIX_NOTIFY_TEST_ROUTES=routes)
        self.assertEqual(result.stdout, routes + "\n")
        self.assertEqual(self.ip_calls.read_text(), "route\n")
        rejected = subprocess.run([self.ip, "unexpected"], env=self.env, check=False)
        self.assertEqual(rejected.returncode, 64)

    def test_host_delivers_unix_envelope_and_ignores_tcp_endpoints(self):
        listener = self.unix_socket(self.runtime)
        listener.listen(1)
        for endpoint in ("1", "127.0.0.1:5959"):
            with self.subTest(endpoint=endpoint):
                result = self.invoke(
                    self.host,
                    'Title "quoted"',
                    "Message\nsecond line",
                    "Ping",
                    WRIX_NOTIFY_TCP=endpoint,
                    WRIX_FOCUS_TARGET="opaque:target",
                )
                self.assertEqual(result.stderr, "")
                self.assertEqual(
                    self.envelope(listener),
                    {
                        "title": 'Title "quoted"',
                        "message": "Message\nsecond line",
                        "sound": "Ping",
                        "focus_target": "opaque:target",
                    },
                )
                self.assertFalse(self.ip_calls.exists())

    def test_host_uses_home_socket_when_runtime_directory_is_unset(self):
        del self.env["XDG_RUNTIME_DIR"]
        listener = self.unix_socket(self.root / ".local" / "share")
        listener.listen(1)
        self.invoke(self.host, "Home", "Complete")
        self.assertEqual(
            self.envelope(listener),
            {"title": "Home", "message": "Complete", "sound": ""},
        )

    def test_host_missing_socket_is_best_effort(self):
        result = self.invoke(self.host, "Missing", "Complete", WRIX_NOTIFY_TCP="1")
        self.assertIn("wrix-notify: socket not found at", result.stderr)
        self.assertFalse(self.ip_calls.exists())

    def test_host_unix_transport_failure_is_best_effort(self):
        self.unix_socket(self.runtime)
        result = self.invoke(self.host, "Unavailable", "Complete")
        self.assertIn("wrix-notify: Unix socket send failed", result.stderr)

    def test_container_discovers_first_default_gateway_with_injected_ip_command(self):
        listener = self.tcp_listener(5959)
        self.invoke(
            self.client,
            "Gateway",
            "Complete",
            WRIX_NOTIFY_TCP="1",
            WRIX_NOTIFY_TEST_ROUTES=(
                "10.0.0.0/24 dev eth0\n"
                "default via 127.0.0.1 dev eth0\n"
                "default via 192.0.2.1 dev eth1"
            ),
        )
        self.assertEqual(
            self.envelope(listener),
            {"title": "Gateway", "message": "Complete", "sound": ""},
        )
        self.assertEqual(self.ip_calls.read_text(), "route\n")

    def test_container_explicit_tcp_endpoint_does_not_discover_gateway(self):
        listener = self.tcp_listener()
        self.invoke(
            self.client,
            "Explicit",
            "Complete",
            WRIX_NOTIFY_TCP=f"127.0.0.1:{listener.getsockname()[1]}",
        )
        self.assertEqual(
            self.envelope(listener),
            {"title": "Explicit", "message": "Complete", "sound": ""},
        )
        self.assertFalse(self.ip_calls.exists())

    def test_container_missing_gateway_is_best_effort(self):
        result = self.invoke(self.client, "Missing gateway", "Complete", WRIX_NOTIFY_TCP="1")
        self.assertIn("wrix-notify: invalid TCP endpoint: 1", result.stderr)
        self.assertEqual(self.ip_calls.read_text(), "route\n")

    def test_container_invalid_tcp_endpoint_is_best_effort(self):
        result = self.invoke(self.client, "Invalid", "Complete", WRIX_NOTIFY_TCP="invalid")
        self.assertIn("wrix-notify: invalid TCP endpoint: invalid", result.stderr)
        self.assertFalse(self.ip_calls.exists())

    def test_container_tcp_transport_failure_is_best_effort(self):
        unavailable = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self.addCleanup(unavailable.close)
        unavailable.bind(("127.0.0.1", 0))
        result = self.invoke(
            self.client,
            "Unavailable",
            "Complete",
            WRIX_NOTIFY_TCP=f"127.0.0.1:{unavailable.getsockname()[1]}",
        )
        self.assertIn("wrix-notify: TCP send failed", result.stderr)
        self.assertFalse(self.ip_calls.exists())


if __name__ == "__main__":
    unittest.main(verbosity=2)
