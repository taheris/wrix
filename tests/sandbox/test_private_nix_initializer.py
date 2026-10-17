"""Run staged-image initialization under fakeroot without a live store."""

import pathlib
import stat
import subprocess
import sys
import tempfile
import unittest

INITIALIZER = pathlib.Path(sys.argv.pop(1))


class PrivateNixInitialization(unittest.TestCase):
    def test_preserves_existing_state_and_is_idempotent(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            files = {
                "etc/wrix/image-policy.json": b'{"kind":"private-native-nix","version":1}',
                "nix/store/existing/content": b"existing store contents",
                "nix/var/nix/db/db.sqlite": b"existing database bytes",
                "nix/var/nix/db/db.sqlite-wal": b"existing WAL bytes",
                "nix/var/nix/gc.lock": b"existing lock bytes",
                "nix/var/nix/gcroots/docker/existing": b"existing GC root",
                "nix/var/log/nix/drvs/existing": b"existing build log",
            }
            for name, data in files.items():
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(data)
            inodes = {name: (root / name).stat().st_ino for name in files}
            for _ in range(2):
                subprocess.run(["bash", str(INITIALIZER), str(root)], check=True)
                for name, data in files.items():
                    self.assertEqual((root / name).read_bytes(), data, name)
                    self.assertEqual((root / name).stat().st_ino, inodes[name], name)
                for name in ("nix", "nix/var", "nix/var/nix"):
                    info = (root / name).stat()
                    self.assertEqual(info.st_uid, 0, name)
                    self.assertEqual(stat.S_IMODE(info.st_mode), 0o755, name)
                for name in ("nix/store", "nix/var/nix/db", "nix/var/nix/builds", "nix/var/nix/gc.lock"):
                    self.assertEqual((root / name).stat().st_uid, 1000, name)
                self.assertEqual(stat.S_IMODE((root / "nix/var/nix/builds").stat().st_mode), 0o755)
                self.assertFalse((root / "homeless-shelter").exists())
                self.assertFalse((root / "nix/var/nix/daemon-socket").exists())

    def test_rejects_live_root(self):
        result = subprocess.run(["bash", str(INITIALIZER), "/"], capture_output=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(b"requires a staged image root", result.stderr)

    def test_rejects_unmarked_tree_without_changing_it(self):
        with tempfile.TemporaryDirectory() as temporary:
            result = subprocess.run(["bash", str(INITIALIZER), temporary], capture_output=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(list(pathlib.Path(temporary).iterdir()), [])


if __name__ == "__main__":
    unittest.main()
