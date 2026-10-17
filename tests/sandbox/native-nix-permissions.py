"""Check emitted Loom image metadata, not just the profile manifest."""

import json
import pathlib
import sys
import tarfile

layout = pathlib.Path(sys.argv[1])


def blob(digest):
    algorithm, value = digest.split(":")
    return layout / "blobs" / algorithm / value


index = json.loads((layout / "index.json").read_text())
manifest = json.loads(blob(index["manifests"][0]["digest"]).read_text())
expected = {
    "nix": 0o755,
    "nix/var": 0o755,
    "nix/var/nix": 0o755,
    "nix/store": 0o777,
    "nix/var/nix/db": 0o777,
    "nix/var/nix/profiles": 0o755,
    "nix/var/nix/profiles/per-user": 0o755,
    "nix/var/nix/gcroots": 0o755,
    "nix/var/nix/gcroots/per-user": 0o755,
    "nix/var/nix/gcroots/auto": 0o1777,
    "nix/var/log/nix/drvs": 0o777,
}
observed = {}
for layer in manifest["layers"]:
    with tarfile.open(blob(layer["digest"])) as archive:
        for entry in archive:
            name = entry.name.removeprefix("./").strip("/")
            if name in expected:
                observed[name] = (entry.uid, entry.gid, entry.mode)

for name, mode in expected.items():
    assert observed.get(name) == (0, 0, mode), (name, observed.get(name), oct(mode))
print("native Nix ancestors and writable state permissions: PASS")
