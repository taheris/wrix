"""Verify effective native-image metadata from real OCI or archive artifacts."""

import argparse
import io
import json
import pathlib
import tarfile

parser = argparse.ArgumentParser()
parser.add_argument("image", type=pathlib.Path)
parser.add_argument("--archive", action="store_true")
args = parser.parse_args()

if args.archive:
    with tarfile.open(args.image) as archive:
        manifest = json.load(archive.extractfile("manifest.json"))[0]
        config = json.load(archive.extractfile(manifest["Config"]))
        layer = archive.extractfile(manifest["Layers"][-1]).read()
else:
    def blob(digest):
        algorithm, value = digest.split(":")
        return args.image / "blobs" / algorithm / value

    index = json.loads((args.image / "index.json").read_text())
    manifest = json.loads(blob(index["manifests"][0]["digest"]).read_text())
    config = json.loads(blob(manifest["config"]["digest"]).read_text())
    layer = blob(manifest["layers"][-1]["digest"]).read_bytes()

expected = {
    "nix": (0, 0, 0o755),
    "nix/var": (0, 0, 0o755),
    "nix/var/nix": (0, 0, 0o755),
    "nix/store": (1000, 1000, 0o755),
    "nix/var/nix/db": (1000, 1000, 0o700),
    "nix/var/nix/temproots": (1000, 1000, 0o700),
    "nix/var/nix/builds": (1000, 1000, 0o755),
    "nix/var/nix/gc.lock": (1000, 1000, 0o600),
    "nix/var/nix/profiles": (0, 0, 0o755),
    "nix/var/nix/profiles/per-user": (0, 0, 0o755),
    "nix/var/nix/profiles/per-user/wrix": (1000, 1000, 0o755),
    "nix/var/nix/gcroots": (0, 0, 0o755),
    "nix/var/nix/gcroots/per-user": (0, 0, 0o755),
    "nix/var/nix/gcroots/per-user/wrix": (1000, 1000, 0o755),
    "nix/var/nix/gcroots/auto": (0, 0, 0o1777),
    "nix/var/log/nix/drvs": (1000, 1000, 0o755),
}
with tarfile.open(fileobj=io.BytesIO(layer)) as archive:
    entries = {entry.name.removeprefix("./").strip("/"): entry for entry in archive}
    for name, metadata in expected.items():
        entry = entries[name]
        assert (entry.uid, entry.gid, entry.mode) == metadata, (name, entry, metadata)
        assert entry.isfile() if name.endswith("gc.lock") else entry.isdir(), name
    assert "homeless-shelter" not in entries
    assert "nix/var/nix/daemon-socket/socket" not in entries
    assert "nix/var/nix/db/db.sqlite" in entries
    assert "nix/var/nix/db/reserved" not in entries
    assert "NIX_REMOTE" not in dict(item.split("=", 1) for item in config["config"]["Env"])
    marker = entries["etc/wrix/private-nix-version"]
    assert marker.isfile()
    assert archive.extractfile(marker).read() == b"1\n"
    assert config["config"]["Labels"]["wrix.nix.private"] == "1"
    assert "IS_SANDBOX=1" in config["config"]["Env"]
    assert "wrix.image.policy" not in config["config"]["Labels"]
    assert "etc/wrix/image-policy.json" not in entries
    assert "lib/libfakeuid.so" not in entries
print("emitted image ownership, modes and retained private Nix state: PASS")
