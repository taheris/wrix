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

    def docker_archive(self, mode="w:gz", extra=None):
        source = self.root / "image.tar.gz"
        names = [f"layer-{i}" for i in range(len(self.layer_data))]
        files = dict(zip(names, self.layer_data))
        files["original-config.json"] = b'{"config":{}}'
        # Producers may put the manifest after every layer; do not assume order.
        files["manifest.json"] = json.dumps([{"Config": "original-config.json", "Layers": names}]).encode()
        files.update(extra or {})
        with tarfile.open(source, mode) as archive:
            for name, data in files.items():
                info = tarfile.TarInfo(name)
                if isinstance(data, tuple):
                    info.type, info.linkname = data
                    archive.addfile(info)
                else:
                    info.size = len(data)
                    archive.addfile(info, io.BytesIO(data))
        return source, names

    def test_docker_archives_keep_payloads_in_the_original_source(self):
        for mode in ("w", "w:gz"):
            with self.subTest(mode=mode):
                source, names = self.docker_archive(mode)
                self.image = self.root / ("docker-" + mode.replace(":", "-"))
                artifact.prepare(source, self.image)
                self.layers.write_text("\n".join(names) + "\n")
                self.assertTrue(all(not (self.image / name).exists() for name in names))
                self.assertLess(sum(p.stat().st_size for p in self.image.iterdir()), 8192)
                self.assertEqual(self.query("paths").stdout, "/" + STORE + "\n")
                result = self.query("digests")
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(result.stdout.splitlines(),
                                 ["sha256:" + hashlib.sha256(data).hexdigest() for data in self.layer_data])
                output = self.root / ("docker-runner-" + mode.replace(":", "-"))
                result = self.query("extract", STORE + "/bin/runner", output)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(output.read_text(), "#!/bin/sh\necho original\n")
                self.assertEqual(output.stat().st_mode & 0o777, 0o755)
                self.assertEqual(self.query("extract", "etc/marker-link", output).returncode, 0)
                self.assertEqual(output.read_text(), "new\n")
                self.assertEqual(self.query("exists", "etc/removed").returncode, 1)
                self.assertEqual(self.query("exists", "opt/hidden/file").returncode, 1)
                self.assertEqual(self.query("link", "bin/runner").stdout, "/" + STORE + "/bin/runner\n")

    def test_archive_inspection_disk_does_not_grow_with_layer_payload(self):
        self.layer_data = [layer_bytes([("large", "x" * (4 * 1024 * 1024), 0o444)])]
        source, names = self.docker_archive()
        image = self.root / "bounded"
        artifact.prepare(source, image)
        self.assertLess(sum(p.stat().st_size for p in image.iterdir()), 8192)
        self.assertFalse((image / names[0]).exists())

    def test_docker_outer_path_traversal_is_rejected_without_writing_payloads(self):
        source, _ = self.docker_archive(extra={"../outside": b"bad"})
        with self.assertRaisesRegex(ValueError, "unsafe archive member"):
            artifact.prepare(source, self.root / "unsafe-docker")
        self.assertFalse((self.root / "outside").exists())

    def test_docker_inner_path_traversal_is_rejected(self):
        self.layer_data = [layer_bytes([("../outside", "bad", 0o444)])]
        source, _ = self.docker_archive()
        with self.assertRaisesRegex(ValueError, "unsafe archive member"):
            artifact.prepare(source, self.root / "unsafe-docker")
        self.assertFalse((self.root / "outside").exists())

    def test_nonregular_docker_metadata_and_layers_are_rejected(self):
        for name, message in [("original-config.json", "invalid or oversized"),
                              ("layer-0", "not a regular Docker layer")]:
            with self.subTest(name=name):
                source, _ = self.docker_archive(extra={name: (tarfile.SYMTYPE, "/etc/passwd")})
                with self.assertRaisesRegex(ValueError, message):
                    artifact.prepare(source, self.root / name)

    def test_missing_docker_layer_is_rejected(self):
        source, _ = self.docker_archive(extra={"manifest.json": json.dumps([
            {"Config": "original-config.json", "Layers": ["missing-layer"]}]).encode()})
        with self.assertRaisesRegex(ValueError, "missing Docker image layer"):
            artifact.prepare(source, self.root / "missing-layer")

    def test_multiple_docker_manifests_are_rejected(self):
        source, _ = self.docker_archive(extra={"manifest.json": b"[{},{}]"})
        with self.assertRaisesRegex(ValueError, "exactly one Docker manifest"):
            artifact.prepare(source, self.root / "multiple")

    def test_oversized_docker_metadata_is_rejected(self):
        source, _ = self.docker_archive(extra={"original-config.json": b" " * (16 * 1024 * 1024 + 1)})
        with self.assertRaisesRegex(ValueError, "oversized image metadata"):
            artifact.prepare(source, self.root / "oversized")

    def test_stream_producer_is_snapshotted_once_without_unpacking_layers(self):
        source, names = self.docker_archive()
        counter = self.root / "producer-count"
        producer = self.root / "producer"
        producer.write_text(f'#!{sys.executable}\nfrom pathlib import Path\nimport sys\n'
                            f'with Path({str(counter)!r}).open("a") as count: count.write("run\\n")\n'
                            f'sys.stdout.buffer.write(Path({str(source)!r}).read_bytes())\n')
        producer.chmod(0o755)
        self.image = self.root / "stream-inspection"
        artifact.prepare(producer, self.image)
        self.layers.write_text("\n".join(names) + "\n")
        self.assertEqual(self.query("extract", "etc/marker", self.root / "stream-marker").returncode, 0)
        self.assertEqual((self.root / "stream-marker").read_text(), "new\n")
        self.assertEqual(counter.read_text(), "run\n")
        self.assertTrue((self.image / "archive.tar").is_file())
        self.assertTrue(all(not (self.image / name).exists() for name in names))


if __name__ == "__main__":
    unittest.main()
