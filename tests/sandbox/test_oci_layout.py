"""Exercise OCI blob reuse without a container runtime."""

import hashlib
import importlib.util
import json
import os
import pathlib
import sys
import tarfile
import tempfile
import unittest
from unittest import mock

MODULE_PATH = pathlib.Path(os.environ.get("WRIX_OCI_BUILDER", pathlib.Path(__file__).parents[2] / "lib/sandbox/make-oci-layout.py"))
SPEC = importlib.util.spec_from_file_location("oci_layout", MODULE_PATH)
oci = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = oci
SPEC.loader.exec_module(oci)


class LayoutTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = pathlib.Path(self.temp.name)

    def conf(self, name, contents, env=()):
        custom = self.root / f"{name}-custom"
        custom.mkdir()
        with tarfile.open(custom / "layer.tar", "w") as archive:
            archive.add(contents, arcname=name)
        digest = hashlib.sha256((custom / "layer.tar").read_bytes()).hexdigest()
        (custom / "checksum").write_text(digest)
        conf = {
            "created": "1970-01-01T00:00:01+00:00",
            "mtime": "1970-01-01T00:00:01+00:00",
            "uid": "0", "gid": "0", "uname": "root", "gname": "root",
            "architecture": "amd64", "os": "linux", "repo_tag": "test:latest",
            "from_image": None, "store_layers": [[str(contents)]],
            "customisation_layer": str(custom), "config": {"Env": list(env)},
        }
        path = self.root / f"{name}.json"
        path.write_text(json.dumps(conf))
        return path

    def run_builder(self, conf, output, *args):
        with mock.patch.object(sys, "argv", [str(MODULE_PATH), str(conf), str(output), *map(str, args)]):
            oci.main()

    def manifest(self, layout):
        return json.loads((layout / "wrix/manifest.json").read_text())

    def contents(self, name):
        path = self.root / name
        path.mkdir()
        (path / "file").write_text(name)
        (path / "link").symlink_to("file")
        return path

    def test_store_layer_is_reproducible_and_has_an_honest_digest(self):
        contents = self.contents("package")
        (contents / "hardlink").hardlink_to(contents / "file")
        conf = self.conf("base", contents)
        first, second = self.root / "first", self.root / "second"
        self.run_builder(conf, first, "--store-layers-only")
        self.run_builder(conf, second, "--store-layers-only")
        self.assertEqual((first / "layers.json").read_bytes(), (second / "layers.json").read_bytes())
        layer = json.loads((first / "layers.json").read_text())[0]
        blob = first / "blobs/sha256" / layer["digest"]
        self.assertEqual(hashlib.sha256(blob.read_bytes()).hexdigest(), layer["digest"])
        self.assertEqual(blob.stat().st_size, layer["size"])
        with tarfile.open(blob) as archive:
            members = archive.getmembers()
            self.assertTrue(all(member.mtime == 1 and member.uid == 0 for member in members))
            files = {pathlib.Path(member.name).name: member for member in members}
            self.assertTrue(files["link"].issym())
            self.assertTrue(files["hardlink"].isfile())
            self.assertEqual(archive.extractfile(files["hardlink"]).read(), b"package")

    def test_child_reuses_parent_and_package_blobs_without_reading_their_bytes(self):
        base_conf = self.conf("base", self.contents("base-package"), ["BASE=1", "SHARED=base"])
        parent = self.root / "parent"
        self.run_builder(base_conf, parent)
        leaf_conf = self.conf("leaf", self.contents("leaf-package"), ["SHARED=leaf"])
        store = self.root / "store"
        self.run_builder(leaf_conf, store, "--store-layers-only")
        child = self.root / "child"
        layer_digests = {entry["digest"].removeprefix("sha256:") for entry in self.manifest(parent)["layers"]}
        layer_digests.update(entry["digest"] for entry in json.loads((store / "layers.json").read_text()))
        original_open = pathlib.Path.open

        def forbid_blob_reads(path, *args, **kwargs):
            if path.name in layer_digests or path.name == "layer.tar":
                raise AssertionError("reading a cached layer")
            return original_open(path, *args, **kwargs)

        with mock.patch.object(oci, "archive_paths_to", side_effect=AssertionError("re-tar")), \
             mock.patch.object(pathlib.Path, "open", forbid_blob_reads), \
             mock.patch.object(oci.tarfile, "open", side_effect=AssertionError("read archive")):
            self.run_builder(leaf_conf, child, "--from-layout", parent, "--store-layers", store)
        manifest = self.manifest(child)
        parent_layers = self.manifest(parent)["layers"]
        self.assertEqual(manifest["layers"][:len(parent_layers)], parent_layers)
        for layer in manifest["layers"]:
            digest = layer["digest"].removeprefix("sha256:")
            blob = child / "blobs/sha256" / digest
            self.assertTrue(blob.is_symlink())
            self.assertEqual(hashlib.sha256(blob.read_bytes()).hexdigest(), digest)
            self.assertEqual(blob.stat().st_size, layer["size"])
        config_digest = manifest["config"]["digest"].removeprefix("sha256:")
        config = json.loads((child / "blobs/sha256" / config_digest).read_text())
        self.assertEqual(config["rootfs"]["diff_ids"], [layer["digest"] for layer in manifest["layers"]])
        self.assertEqual(config["config"]["Env"], ["BASE=1", "SHARED=leaf"])

    def test_archive_parent_is_rejected_before_output_creation(self):
        conf = self.conf("base", self.contents("package"))
        data = json.loads(conf.read_text())
        data["from_image"] = "/unavailable/whole-image.tar.gz"
        conf.write_text(json.dumps(data))
        output = self.root / "output"
        with self.assertRaisesRegex(ValueError, "whole-image archive"):
            self.run_builder(conf, output)
        self.assertFalse(output.exists())

    def test_generated_change_preserves_package_blob_identity(self):
        contents = self.contents("package")
        first_conf = self.conf("first", contents, ["SETTING=first"])
        second_conf = self.conf("second", contents, ["SETTING=second"])
        store = self.root / "store"
        self.run_builder(first_conf, store, "--store-layers-only")
        first, second = self.root / "first", self.root / "second"
        for conf, layout in [(first_conf, first), (second_conf, second)]:
            self.run_builder(conf, layout, "--store-layers", store)
        first_layers, second_layers = self.manifest(first)["layers"], self.manifest(second)["layers"]
        self.assertEqual(first_layers[:-1], second_layers[:-1])
        self.assertNotEqual(first_layers[-1], second_layers[-1])
        digest = first_layers[0]["digest"].removeprefix("sha256:")
        self.assertTrue((first / "blobs/sha256" / digest).samefile(second / "blobs/sha256" / digest))


if __name__ == "__main__":
    unittest.main()
