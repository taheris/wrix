"""Run staged-image initialization under fakeroot without a live store."""

import os
import pathlib
import stat
import subprocess
import sys
import tempfile
import unittest

INITIALIZER = pathlib.Path(sys.argv.pop(1))
IMAGE_ROOTS = [f"/nix/store/{'a' * 32}-{name}" for name in ("passwd", "group", "nix.conf")]
MANIFEST = ("\n".join(IMAGE_ROOTS) + "\n").encode()


class PrivateNixInitialization(unittest.TestCase):
    def test_preserves_existing_state_and_is_idempotent(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            files = {
                "etc/wrix/private-nix-version": b"1\n",
                "etc/wrix/store-roots": MANIFEST,
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
                roots = root / "nix/var/nix/gcroots/wrix-image"
                self.assertEqual(roots.stat().st_uid, 0)
                self.assertEqual(stat.S_IMODE(roots.stat().st_mode), 0o755)
                self.assertEqual(sorted(p.name for p in roots.iterdir()), sorted(pathlib.Path(p).name for p in IMAGE_ROOTS))
                for target in IMAGE_ROOTS:
                    link = roots / pathlib.Path(target).name
                    self.assertTrue(link.is_symlink())
                    self.assertEqual(os.readlink(link), target)
                self.assertFalse((root / "homeless-shelter").exists())
                self.assertFalse((root / "nix/var/nix/daemon-socket").exists())

    def test_rejects_live_roots(self):
        for root in ("/", "/nix", "/nix/var", "/nix/var/nix", "/nix/store"):
            with self.subTest(root=root):
                path = pathlib.Path(root)
                before = path.lstat() if path.exists() or path.is_symlink() else None
                result = subprocess.run(["bash", str(INITIALIZER), root], capture_output=True)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(b"requires a staged image root", result.stderr)
                if before is None:
                    self.assertFalse(path.exists())
                    self.assertFalse(path.is_symlink())
                else:
                    self.assertEqual(path.lstat().st_ino, before.st_ino)

    def test_rejects_staged_aliases_without_touching_the_target(self):
        for target in ("nix", "nix/store", "nix/var/nix/db", "nix/var/nix/gc.lock", "nix/var/nix/gcroots/wrix-image"):
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
                self.assertEqual(content.stat(), before)
                self.assertEqual(content.read_bytes(), b"untouched external state")

    def test_rejects_invalid_manifest_before_initializing_state(self):
        for manifest in (None, b"", b"/outside\n", MANIFEST + b"\0", MANIFEST + b"\n", MANIFEST.replace(b"passwd", b"passwd/child"), MANIFEST.replace(b"passwd", b"passwd\r")):
            with self.subTest(manifest=manifest), tempfile.TemporaryDirectory() as temporary:
                root = pathlib.Path(temporary)
                etc = root / "etc/wrix"
                etc.mkdir(parents=True)
                (etc / "private-nix-version").write_bytes(b"1\n")
                if manifest is not None:
                    (etc / "store-roots").write_bytes(manifest)
                result = subprocess.run(["bash", str(INITIALIZER), str(root)], capture_output=True)
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse((root / "nix").exists())

    def test_refuses_manifest_alias_and_conflicting_root_without_overwriting(self):
        for alias in (True, False):
            with self.subTest(alias=alias), tempfile.TemporaryDirectory() as temporary:
                root = pathlib.Path(temporary)
                etc = root / "etc/wrix"
                etc.mkdir(parents=True)
                (etc / "private-nix-version").write_bytes(b"1\n")
                outside = root / "outside"
                outside.write_bytes(MANIFEST)
                manifest = etc / "store-roots"
                if alias:
                    manifest.symlink_to(outside)
                else:
                    manifest.write_bytes(MANIFEST)
                    roots = root / "nix/var/nix/gcroots/wrix-image"
                    roots.mkdir(parents=True)
                    (roots / pathlib.Path(IMAGE_ROOTS[0]).name).symlink_to(outside)
                result = subprocess.run(["bash", str(INITIALIZER), str(root)], capture_output=True)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(outside.read_bytes(), MANIFEST)
                if not alias:
                    self.assertEqual(os.readlink(roots / pathlib.Path(IMAGE_ROOTS[0]).name), str(outside))

    def test_rejects_unmarked_tree_without_changing_it(self):
        with tempfile.TemporaryDirectory() as temporary:
            for root in (temporary, str(pathlib.Path(temporary) / "missing" / "stage")):
                with self.subTest(root=root):
                    result = subprocess.run(["bash", str(INITIALIZER), root], capture_output=True)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn(b"requires a staged image root", result.stderr)
                    self.assertEqual(list(pathlib.Path(temporary).iterdir()), [])


if __name__ == "__main__":
    unittest.main()
