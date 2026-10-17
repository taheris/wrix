"""Run staged-image initialization under fakeroot without a live store."""

import os
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
                "etc/wrix/private-nix-version": b"1\n",
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
            for name in ("nix", "nix/var", "nix/var/nix"):
                os.chown(root / name, 1000, 1000)
                (root / name).chmod(0o777)
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

    def test_rejects_live_roots(self):
        for root in ("/", "/nix", "/nix/var/nix"):
            result = subprocess.run(["bash", str(INITIALIZER), root], capture_output=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn(b"requires a staged image root", result.stderr)

    def test_rejects_staged_aliases_without_touching_the_target(self):
        for target in ("nix", "nix/store", "nix/var/nix/db", "nix/var/nix/gc.lock"):
            with self.subTest(target=target), tempfile.TemporaryDirectory() as temporary:
                parent = pathlib.Path(temporary)
                root = parent / "stage"
                marker = root / "etc/wrix/private-nix-version"
                marker.parent.mkdir(parents=True)
                marker.write_bytes(b"1\n")
                outside = parent / "outside"
                outside.mkdir()
                content = outside / "content"
                content.write_bytes(b"untouched external state")
                before = content.stat()
                alias = root / target
                alias.parent.mkdir(parents=True, exist_ok=True)
                alias.symlink_to(content if target.endswith("gc.lock") else outside)
                result = subprocess.run(["bash", str(INITIALIZER), str(root)], capture_output=True)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(b"rejects staged symlinks", result.stderr)
                self.assertEqual(content.read_bytes(), b"untouched external state")
                self.assertEqual(content.stat(), before)

    def test_rejects_unmarked_tree_without_changing_it(self):
        with tempfile.TemporaryDirectory() as temporary:
            result = subprocess.run(["bash", str(INITIALIZER), temporary], capture_output=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(list(pathlib.Path(temporary).iterdir()), [])


if __name__ == "__main__":
    unittest.main()
