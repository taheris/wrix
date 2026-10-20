"""Verify seed contracts against emitted OCI/descriptor/archive image contents."""

import copy
import hashlib
import importlib.util
import json
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile


def verify(case, image_artifact):
    image = Path(case["artifact"])
    manifest = json.loads((image / "manifest.json").read_text())[0]
    config_bytes = (image / manifest["Config"]).read_bytes()
    config = json.loads(config_bytes)["config"]
    members = json.loads((image / "members.json").read_text())
    layers = [(name, members[name]) for name in manifest["Layers"]]
    shared = case["shared"]
    assert set(shared) == {"version", "seed_closure"}
    assert type(shared["version"]) is int and shared["version"] == 1
    assert config["Labels"]["wrix.nix.private"] == "1"
    assert config["Labels"]["wrix.nix.shared"] == str(shared["version"])
    assert config["Labels"]["wrix.nix.seed"] == hashlib.sha256(
        shared["seed_closure"].encode()
    ).hexdigest()
    env = dict(item.split("=", 1) for item in config["Env"])
    assert env["IS_SANDBOX"] == "1"
    assert "NIX_REMOTE" not in env
    assert image_artifact.lookup(layers, "nix/var/nix/daemon-socket/socket") is None
    assert image_artifact.lookup(layers, "homeless-shelter") is None

    seed_lines = Path(shared["seed_closure"]).read_text().splitlines()
    seed = set(seed_lines)
    assert seed and len(seed) == len(seed_lines)
    candidates = {
        "/" + match.group().lstrip("/")
        for _, entries in layers
        for name in entries
        if (match := image_artifact.STORE_PATH.search(name))
    }
    on_disk = {
        path for path in candidates
        if image_artifact.lookup(layers, path.lstrip("/")) is not None
    }
    assert seed == on_disk, (
        "seed differs from materialized store",
        sorted(seed - on_disk),
        sorted(on_disk - seed),
    )
    assert set(case.get("required_paths", [])) <= seed

    with tempfile.TemporaryDirectory() as temporary:
        output = Path(temporary) / "member"
        for name, expected in [
            ("etc/wrix/private-nix-version", b"1\n"),
            ("etc/wrix/shared-nix-version", b"1\n"),
        ]:
            image_artifact.extract(image, image_artifact.lookup(layers, name), output)
            assert output.read_bytes() == expected
        database = Path(temporary) / "db.sqlite"
        image_artifact.extract(
            image, image_artifact.lookup(layers, "nix/var/nix/db/db.sqlite"), database
        )
        with sqlite3.connect(f"file:{database}?mode=ro", uri=True) as connection:
            registered = {row[0] for row in connection.execute("SELECT path FROM ValidPaths")}
        assert seed == registered, "seed and private image registration disagree"

    if case["source_kind"] == "nix-descriptor":
        descriptor = json.loads(Path(case["source"]).read_text())
        assert descriptor["shared_nix"] == shared
        assert descriptor["config"]["labels"] == config["Labels"]
        assert descriptor["digest"] == "sha256:" + hashlib.sha256(config_bytes).hexdigest()
    else:
        assert case["source_kind"] == "docker-archive"
    if "profile_config" in case:
        profile = json.loads(Path(case["profile_config"]).read_text())
        assert profile["image"]["shared_nix"] == shared
        assert profile["image"]["source"] == case["source"]
        assert profile["image"]["source_kind"] == case["source_kind"]
        assert profile["image"]["digest"] == Path(case["digest_file"]).read_text().strip()


def main():
    cases = json.loads(Path(sys.argv[1]).read_text())
    specification = importlib.util.spec_from_file_location("image_artifact", sys.argv[2])
    image_artifact = importlib.util.module_from_spec(specification)
    specification.loader.exec_module(image_artifact)
    for case in cases:
        if case["source_kind"] == "docker-archive":
            arguments = ["--archive", case["source"]]
        else:
            descriptor = json.loads(Path(case["source"]).read_text())
            arguments = [descriptor["oci_layout"]]
        subprocess.run([sys.executable, sys.argv[3], *arguments], check=True)
        verify(case, image_artifact)

    with tempfile.TemporaryDirectory() as temporary:
        partial = Path(temporary) / "store-paths"
        paths = Path(cases[0]["shared"]["seed_closure"]).read_text().splitlines()
        partial.write_text("\n".join(paths[1:]) + "\n")
        incomplete = copy.deepcopy(cases[0])
        incomplete["shared"]["seed_closure"] = str(partial)
        incompatible = copy.deepcopy(cases[0])
        incompatible["shared"]["version"] = 2
        for invalid in [incomplete, incompatible]:
            try:
                verify(invalid, image_artifact)
            except AssertionError:
                pass
            else:
                raise AssertionError("image verifier accepted invalid seed/capability metadata")
    print(f"shared Nix image contracts: PASS ({len(cases)} actual artifacts, negative controls)")


if __name__ == "__main__":
    main()
