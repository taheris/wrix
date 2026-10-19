"""Exercise image-local SSH installation without writing through symlinks."""

import pathlib
import subprocess
import sys
import tempfile
import unittest

INSTALLER = pathlib.Path(sys.argv.pop(1)).resolve()


class KnownHostsInstallation(unittest.TestCase):
    def run_installer(self, source, root, mode, success=True):
        result = subprocess.run(
            ["bash", str(INSTALLER), str(source), str(root), mode],
            capture_output=True,
        )
        self.assertEqual(result.returncode == 0, success, result.stderr.decode())

    def test_materializes_store_directory_in_build_and_runtime_modes(self):
        for mode in ("build", "runtime"):
            with self.subTest(mode=mode), tempfile.TemporaryDirectory() as temporary:
                parent = pathlib.Path(temporary)
                root = parent / "image with spaces"
                (root / "etc").mkdir(parents=True)
                store = parent / "store ssh"
                store.mkdir()
                config = store / "ssh_config"
                config.write_text("Host *\n  StrictHostKeyChecking yes\n")
                external = parent / "external known hosts"
                external.write_text("external data\n")
                (store / "ssh_known_hosts").symlink_to(external)
                config.chmod(0o444)
                store.chmod(0o555)
                (root / "etc/ssh").symlink_to(store)
                source = parent / "pinned hosts"
                source.write_text("github.com ssh-ed25519 pinned\n")
                before = external.stat()
                try:
                    self.run_installer(source, root, mode)
                    self.assertEqual(external.stat(), before)
                    self.assertFalse((root / "etc/ssh").is_symlink())
                    self.assertFalse((root / "etc/ssh/ssh_known_hosts").is_symlink())
                    self.assertEqual((root / "etc/ssh/ssh_known_hosts").read_bytes(), source.read_bytes())
                    self.assertEqual((root / "etc/ssh/ssh_config").read_bytes(), config.read_bytes())
                    self.assertEqual(config.stat().st_mode & 0o777, 0o444)
                    self.assertEqual(store.stat().st_mode & 0o777, 0o555)
                    self.assertTrue((store / "ssh_known_hosts").is_symlink())
                    self.assertEqual(external.read_text(), "external data\n")
                    self.run_installer(source, root, mode)
                    self.assertEqual((root / "etc/ssh/ssh_known_hosts").read_bytes(), source.read_bytes())
                finally:
                    store.chmod(0o755)

    def test_rejects_ancestor_symlinks_without_touching_external_targets(self):
        for mode in ("build", "runtime"):
            for target in ("etc", "etc/wrix", "etc/wrix/known_hosts_dir"):
                with self.subTest(mode=mode, target=target), tempfile.TemporaryDirectory() as temporary:
                    parent = pathlib.Path(temporary)
                    root = parent / "image"
                    alias = root / target
                    alias.parent.mkdir(parents=True)
                    outside = parent / "outside"
                    outside.mkdir()
                    alias.symlink_to(outside)
                    source = parent / "hosts"
                    source.write_text("pinned hosts\n")
                    self.run_installer(source, root, mode, success=False)
                    self.assertEqual(list(outside.iterdir()), [])

    def test_rejects_dangling_or_non_directory_ssh_targets(self):
        for mode in ("build", "runtime"):
            for kind in ("dangling", "file"):
                with self.subTest(mode=mode, kind=kind), tempfile.TemporaryDirectory() as temporary:
                    parent = pathlib.Path(temporary)
                    root = parent / "image"
                    (root / "etc").mkdir(parents=True)
                    outside = parent / "target"
                    if kind == "file":
                        outside.write_text("external data\n")
                    (root / "etc/ssh").symlink_to(outside)
                    source = parent / "hosts"
                    source.write_text("pinned hosts\n")
                    self.run_installer(source, root, mode, success=False)
                    self.assertTrue((root / "etc/ssh").is_symlink())
                    if kind == "file":
                        self.assertEqual(outside.read_text(), "external data\n")
                    else:
                        self.assertFalse(outside.exists())


if __name__ == "__main__":
    unittest.main()
