import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


VERIFIER = Path(__file__).with_name("services-cache-transport.py")
LAUNCHER = "crates/wrix-sandbox/src/command/launch.rs"
LIFECYCLE = "crates/wrix-service/src/lifecycle/mod.rs"
IMAGE = "lib/services/image.nix"
FIXTURE = {
    LAUNCHER: '''
    fn load_project_cache() {
        let url = format!("http://{sandbox_host}:{}", endpoint.port);
        let nix_config = format!(
            "extra-substituters = {url}\\nextra-trusted-public-keys = {}\\nbuilders-use-substitutes = true",
            public_key.as_str()
        );
    }
    fn load_dolt() {}
''',
    LIFECYCLE: '''
    fn ensure_running() {
        format!("127.0.0.1:{port}:8080");
        format!("{}:/cache:ro", plan.paths().cache_root().display());
    }
    fn status() {}
fn container_command() {
    "wrix-cache-serve /cache"
}
fn expected_dolt_transport_label() {}
''',
    IMAGE: '''
  contents = [
    cacheServe
  ];
''',
}


class CacheTransportVerifierTests(unittest.TestCase):
    def run_verifier(self, fixture):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for relative_path, content in fixture.items():
                path = root / relative_path
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(content)
            return subprocess.run(
                [sys.executable, str(VERIFIER), str(root)],
                capture_output=True,
                text=True,
                check=False,
            )

    def assert_rejected(self, fixture, diagnostic):
        result = self.run_verifier(fixture)
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertEqual(result.stderr.strip(), f"FAIL: {diagnostic}")
        self.assertEqual(result.stdout, "")

    def test_accepts_http_transport_with_typed_public_key(self):
        result = self.run_verifier(FIXTURE)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "")
        self.assertEqual(result.stderr, "")

    def test_rejects_missing_required_transport_configuration(self):
        required = [
            (
                LAUNCHER,
                "project-cache launcher path",
                'format!("http://{sandbox_host}:{}", endpoint.port)',
            ),
            (LAUNCHER, "project-cache launcher path", "extra-substituters = {url}"),
            (LAUNCHER, "project-cache launcher path", "extra-trusted-public-keys = {}"),
            (LAUNCHER, "project-cache launcher path", "public_key.as_str()"),
            (LAUNCHER, "project-cache launcher path", "builders-use-substitutes = true"),
            (
                LIFECYCLE,
                "service container run path",
                'format!("127.0.0.1:{port}:8080")',
            ),
            (
                LIFECYCLE,
                "service container run path",
                'format!("{}:/cache:ro", plan.paths().cache_root().display())',
            ),
            (LIFECYCLE, "service container command", "wrix-cache-serve /cache"),
            (IMAGE, "service image contents", "cacheServe"),
        ]
        for path, label, needle in required:
            with self.subTest(region=label, missing=needle):
                fixture = {**FIXTURE, path: FIXTURE[path].replace(needle, "")}
                self.assert_rejected(fixture, f"{label}: missing {needle!r}")

    def test_rejects_forbidden_cache_transports_in_every_checked_region(self):
        regions = [
            (LAUNCHER, "project-cache launcher path", "    fn load_project_cache() {"),
            (LIFECYCLE, "service container run path", "    fn ensure_running() {"),
            (LIFECYCLE, "service container command", "fn container_command() {"),
            (IMAGE, "service image contents", "  contents = ["),
        ]
        forbidden = [
            "file://",
            "unix://",
            "nix-daemon",
            "/nix/var/nix/daemon-socket",
            ":/nix/store",
            "harmonia",
            "nix-serve",
        ]
        for path, label, marker in regions:
            for needle in forbidden:
                with self.subTest(region=label, forbidden=needle):
                    fixture = {
                        **FIXTURE,
                        path: FIXTURE[path].replace(marker, f'{marker}\n"{needle}"'),
                    }
                    self.assert_rejected(fixture, f"{label}: unexpected {needle!r}")

    def test_rejects_missing_checked_region_markers(self):
        regions = [
            (
                LAUNCHER,
                "project-cache launcher path",
                "    fn load_project_cache",
                "    fn load_dolt",
            ),
            (
                LIFECYCLE,
                "service container run path",
                "    fn ensure_running",
                "    fn status",
            ),
            (
                LIFECYCLE,
                "service container command",
                "fn container_command",
                "fn expected_dolt_transport_label",
            ),
            (IMAGE, "service image contents", "  contents = [", "  ];"),
        ]
        for path, label, start, end in regions:
            for position, marker in [("start", start), ("end", end)]:
                with self.subTest(region=label, missing=position):
                    fixture = {**FIXTURE, path: FIXTURE[path].replace(marker, "")}
                    self.assert_rejected(
                        fixture, f"{label}: missing {position} marker {marker!r}"
                    )


if __name__ == "__main__":
    unittest.main()
