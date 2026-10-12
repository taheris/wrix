import hashlib
import importlib.util
import io
import json
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch


sys.dont_write_bytecode = True
SCRIPT = Path(__file__).with_name("image-artifact.py")
spec = importlib.util.spec_from_file_location("image_artifact", SCRIPT)
artifact = importlib.util.module_from_spec(spec)
spec.loader.exec_module(artifact)
STORE = "nix/store/" + "a" * 32 + "-fixture"


def layer_bytes(entries):
    output = io.BytesIO()
    with tarfile.open(fileobj=output, mode="w") as archive:
        for name, content, mode in entries:
            info = tarfile.TarInfo(name)
            info.mode = mode
            if isinstance(content, tuple):
                info.type, info.linkname = content
                archive.addfile(info)
            else:
                data = content.encode()
                info.size = len(data)
                archive.addfile(info, io.BytesIO(data))
    return output.getvalue()


class ImageArtifact(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="wrix-image-artifact-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.layout = self.root / "layout"
        (self.layout / "blobs/sha256").mkdir(parents=True)
        self.layer_data = [
            layer_bytes([
                ("/" + STORE + "/bin/runner", "#!/bin/sh\necho original\n", 0o755),
                ("./bin/runner", (tarfile.SYMTYPE, "/" + STORE + "/bin/runner"), 0o777),
                ("etc/marker", "old\n", 0o444),
                ("etc/removed", "removed\n", 0o444),
                ("opt/hidden/file", "hidden\n", 0o444),
            ]),
            layer_bytes([
                ("etc/marker", "new\n", 0o444),
                ("etc/.wh.removed", "", 0o444),
                ("opt/hidden/.wh..wh..opq", "", 0o444),
                ("etc/marker-link", (tarfile.LNKTYPE, "etc/marker"), 0o444),
            ]),
        ]
        config = self.write_blob(json.dumps({"config": {"Entrypoint": ["/entrypoint.sh"]}}).encode())
        self.manifest = {"schemaVersion": 2, "config": config, "layers": [self.write_blob(data) for data in self.layer_data]}
        manifest = self.write_blob(json.dumps(self.manifest).encode())
        (self.layout / "index.json").write_text(json.dumps({"manifests": [manifest]}))
        self.descriptor = self.root / "descriptor.json"
        self.descriptor.write_text(json.dumps({"oci_layout": str(self.layout)}))
        self.image = self.root / "inspection"
        artifact.prepare(self.descriptor, self.image)
        self.layers = self.root / "layers"
        self.layers.write_text("\n".join(json.loads((self.image / "manifest.json").read_text())[0]["Layers"]) + "\n")

    def write_blob(self, data):
        digest = hashlib.sha256(data).hexdigest()
        (self.layout / "blobs/sha256" / digest).write_bytes(data)
        return {"digest": "sha256:" + digest, "size": len(data), "mediaType": "application/vnd.oci.image.layer.v1.tar"}

    def append_layer(self, entries):
        self.manifest["layers"].append(self.write_blob(layer_bytes(entries)))
        manifest = self.write_blob(json.dumps(self.manifest).encode())
        (self.layout / "index.json").write_text(json.dumps({"manifests": [manifest]}))
        self.image = self.root / "changed-inspection"
        artifact.prepare(self.layout, self.image)
        layers = json.loads((self.image / "manifest.json").read_text())[0]["Layers"]
        self.layers.write_text("\n".join(layers) + "\n")

    def query(self, operation, *args):
        return subprocess.run([sys.executable, str(SCRIPT), operation, str(self.image), str(self.layers), *map(str, args)],
                              text=True, capture_output=True, check=False)

    def test_descriptor_preparation_links_emitted_blobs_without_copying_them(self):
        self.assertTrue((self.image / "blobs").is_symlink())
        self.assertEqual(json.loads((self.image / "config.json").read_text())["config"]["Entrypoint"], ["/entrypoint.sh"])
        members = json.loads((self.image / "members.json").read_text())
        self.assertEqual(len(members), 2)
        self.assertIn(STORE + "/bin/runner", next(iter(members.values())))

    def test_paths_are_discovered_from_actual_tar_headers_without_reopening_blobs(self):
        layers = artifact.selected(self.image, self.layers)
        with patch.object(artifact.tarfile, "open", side_effect=AssertionError("metadata query opened a tar")):
            found = artifact.lookup(layers, "/" + STORE + "/bin/runner")
        self.assertIsNotNone(found)
        result = self.query("paths")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "/" + STORE + "\n")

    def test_extraction_preserves_emitted_file_bytes_and_executable_mode(self):
        output = self.root / "runner"
        result = self.query("extract", STORE + "/bin/runner", output)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(output.read_text(), "#!/bin/sh\necho original\n")
        self.assertEqual(output.stat().st_mode & 0o777, 0o755)

    def test_last_layer_overrides_earlier_file_contents(self):
        output = self.root / "marker"
        result = self.query("extract", "etc/marker", output)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(output.read_text(), "new\n")

    def test_whiteout_removes_lower_layer_member(self):
        self.assertEqual(self.query("exists", "etc/removed").returncode, 1)

    def test_find_reports_emitted_headers_even_when_whiteouts_hide_the_file(self):
        result = self.query("find", "etc/removed")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "etc/removed\n")

    def test_directory_whiteout_hides_lower_layer_descendants(self):
        self.append_layer([("nix/.wh.store", "", 0o444)])
        self.assertEqual(self.query("exists", STORE + "/bin/runner").returncode, 1)

    def test_whiteout_does_not_hide_a_recreated_file_in_the_same_layer(self):
        self.append_layer([("etc/.wh.removed", "", 0o444), ("etc/removed", "recreated\n", 0o444)])
        output = self.root / "recreated"
        result = self.query("extract", "etc/removed", output)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(output.read_text(), "recreated\n")

    def test_opaque_directory_hides_lower_layer_children(self):
        self.assertEqual(self.query("exists", "opt/hidden/file").returncode, 1)

    def test_selected_tier_queries_do_not_include_unselected_layers(self):
        self.layers.write_text(self.layers.read_text().splitlines()[0] + "\n")
        self.assertEqual(self.query("exists", "etc/removed").returncode, 0)
        output = self.root / "marker"
        self.assertEqual(self.query("extract", "etc/marker", output).returncode, 0)
        self.assertEqual(output.read_text(), "old\n")

    def test_symlink_target_is_read_from_emitted_header(self):
        result = self.query("link", "bin/runner")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "/" + STORE + "/bin/runner\n")

    def test_hardlink_extraction_resolves_indexed_target(self):
        output = self.root / "marker"
        result = self.query("extract", "etc/marker-link", output)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(output.read_text(), "new\n")

    def test_missing_member_fails_without_creating_output(self):
        output = self.root / "missing"
        self.assertEqual(self.query("extract", "absent", output).returncode, 1)
        self.assertFalse(output.exists())

    def test_same_size_layer_corruption_is_rejected_before_indexing(self):
        digest = self.manifest["layers"][0]["digest"].removeprefix("sha256:")
        filename = self.layout / "blobs/sha256" / digest
        filename.write_bytes(self.layer_data[0][:-1] + b"x")
        with self.assertRaisesRegex(ValueError, "digest mismatch"):
            artifact.prepare(self.layout, self.root / "corrupt")

    def test_layer_size_mismatch_is_rejected(self):
        self.manifest["layers"][0]["size"] += 1
        manifest = self.write_blob(json.dumps(self.manifest).encode())
        (self.layout / "index.json").write_text(json.dumps({"manifests": [manifest]}))
        with self.assertRaisesRegex(ValueError, "size mismatch"):
            artifact.prepare(self.layout, self.root / "bad-size")

    def test_unsafe_archive_member_is_rejected(self):
        self.manifest["layers"] = [self.write_blob(layer_bytes([("../outside", "bad", 0o644)]))]
        manifest = self.write_blob(json.dumps(self.manifest).encode())
        (self.layout / "index.json").write_text(json.dumps({"manifests": [manifest]}))
        with self.assertRaisesRegex(ValueError, "unsafe archive member"):
            artifact.prepare(self.layout, self.root / "unsafe")
        self.assertFalse((self.root / "outside").exists())

    def test_docker_archive_fallback_has_the_same_query_contract(self):
        archive_path = self.root / "image.tar.gz"
        with tarfile.open(archive_path, "w:gz") as archive:
            files = {"manifest.json": json.dumps([{"Config": "original-config.json", "Layers": ["layer.tar"]}]).encode(),
                     "original-config.json": b'{"config":{}}', "layer.tar": self.layer_data[0]}
            for name, data in files.items():
                info = tarfile.TarInfo(name)
                info.size = len(data)
                archive.addfile(info, io.BytesIO(data))
        self.image = self.root / "docker-inspection"
        artifact.prepare(archive_path, self.image)
        self.layers.write_text("layer.tar\n")
        result = self.query("paths")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "/" + STORE + "\n")
        output = self.root / "docker-runner"
        self.assertEqual(self.query("extract", STORE + "/bin/runner", output).returncode, 0)
        self.assertEqual(output.read_text(), "#!/bin/sh\necho original\n")


if __name__ == "__main__":
    unittest.main()
