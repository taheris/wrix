#!/usr/bin/env python3
"""Cache verified emitted image headers; queries read only requested payloads."""

import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import subprocess
import tarfile


STORE_PATH = re.compile(r"(?:^|/)nix/store/[a-z0-9]{32}-[^/]+")


def normalized(name):
    path = PurePosixPath(name.lstrip("/"))
    if ".." in path.parts:
        raise ValueError(f"unsafe archive member: {name}")
    return str(path)


def blob(layout, digest):
    if not re.fullmatch(r"sha256:[0-9a-f]{64}", digest):
        raise ValueError(f"unsupported blob digest: {digest}")
    filename = layout / "blobs" / "sha256" / digest.removeprefix("sha256:")
    with filename.open("rb") as stream:
        actual = "sha256:" + hashlib.file_digest(stream, "sha256").hexdigest()
    if actual != digest:
        raise ValueError(f"blob digest mismatch: {filename}")
    return filename


def index_layer(stream, mode="r:"):
    entries = {}
    with tarfile.open(fileobj=stream, mode=mode) as archive:
        for info in archive:
            entries[normalized(info.name)] = {
                "name": info.name,
                "type": info.type.decode("ascii"),
                "mode": info.mode,
                "size": info.size,
                "offset": info.offset,
                "offset_data": info.offset_data,
                "linkname": info.linkname,
                "sparse": info.sparse,
            }
    return entries


class DigestReader:
    def __init__(self, stream):
        self.stream = stream
        self.digest = hashlib.sha256()

    def read(self, size):
        data = self.stream.read(size)
        self.digest.update(data)
        return data


def read_metadata(archive, info):
    if not info.isfile() or info.size > 16 * 1024 * 1024:
        raise ValueError(f"invalid or oversized image metadata: {info.name}")
    with archive.extractfile(info) as stream:
        return stream.read()


def prepare_docker(source, output):
    # Native stream producers need one snapshot. Darwin tarballs are inspected
    # in place: neither their layer payloads nor a decompressed archive is saved.
    if os.access(source, os.X_OK):
        snapshot = output / "archive.tar"
        try:
            with snapshot.open("wb") as stream:
                subprocess.run([str(source)], stdout=stream, check=True)
        except BaseException:
            snapshot.unlink()
            raise
        source = snapshot
    source = source.resolve()
    manifest, headers = None, set()
    with tarfile.open(source, mode="r|*") as archive:
        for info in archive:
            name = normalized(info.name)
            if name in headers:
                raise ValueError(f"duplicate Docker archive member: {name}")
            headers.add(name)
            if name == "manifest.json":
                manifest = json.loads(read_metadata(archive, info))
    if manifest is None or len(manifest) != 1:
        raise ValueError("image inspection requires exactly one Docker manifest")
    manifest = manifest[0]
    config = normalized(manifest["Config"])
    layers = [normalized(layer) for layer in manifest["Layers"]]
    if len(layers) != len(set(layers)):
        raise ValueError("duplicate Docker image layer")
    if config not in headers or config in layers or config == "manifest.json":
        raise ValueError("invalid Docker config member")
    members, sources, digests = {}, {}, {}
    with tarfile.open(source, mode="r|*") as archive:
        for info in archive:
            name = normalized(info.name)
            if name == config:
                (output / "config.json").write_bytes(read_metadata(archive, info))
            if name not in layers:
                continue
            if not info.isfile():
                raise ValueError(f"not a regular Docker layer: {name}")
            with archive.extractfile(info) as stream:
                reader = DigestReader(stream)
                members[name] = index_layer(reader, mode="r|")
                # Include tar padding, not just the blocks consumed by indexing.
                while reader.read(1024 * 1024):
                    pass
                digests[name] = "sha256:" + reader.digest.hexdigest()
            sources[name] = {"offset": info.offset_data, "size": info.size}
    if set(members) != set(layers):
        raise ValueError("missing Docker image layer")
    (output / "archive.json").write_text(json.dumps({"source": str(source), "layers": sources}))
    return layers, members, digests


def prepare(source, output):
    output.mkdir(parents=True)
    if source.is_dir() or source.suffix == ".json":
        layout = source if source.is_dir() else Path(json.loads(source.read_text())["oci_layout"])
        index = json.loads((layout / "index.json").read_text())
        if len(index["manifests"]) != 1:
            raise ValueError("image inspection requires exactly one OCI manifest")
        manifest = json.loads(blob(layout, index["manifests"][0]["digest"]).read_text())
        (output / "config.json").symlink_to(blob(layout, manifest["config"]["digest"]))
        (output / "blobs").symlink_to(layout / "blobs", target_is_directory=True)
        layers, members, digests = [], {}, {}
        for layer in manifest["layers"]:
            filename = blob(layout, layer["digest"])
            if layer["mediaType"] != "application/vnd.oci.image.layer.v1.tar":
                raise ValueError("image inspection requires uncompressed OCI layers")
            if filename.stat().st_size != layer["size"]:
                raise ValueError(f"layer size mismatch: {filename}")
            name = str(filename.relative_to(layout))
            layers.append(name)
            digests[name] = layer["digest"]
            with filename.open("rb") as stream:
                members[name] = index_layer(stream)
    else:
        layers, members, digests = prepare_docker(source, output)

    (output / "manifest.json").write_text(json.dumps([{"Config": "config.json", "Layers": layers}]))
    (output / "members.json").write_text(json.dumps(members, separators=(",", ":")))
    (output / "layer-digests.json").write_text(json.dumps(digests))


def selected(image, layers_file):
    members = json.loads((image / "members.json").read_text())
    layers = [line for line in layers_file.read_text().splitlines() if line]
    return [(layer, members[layer]) for layer in layers]


def lookup(layers, name):
    name = normalized(name)
    path = PurePosixPath(name)
    whiteout = str(path.with_name(".wh." + path.name))
    for layer, members in reversed(layers):
        if name in members:
            return layer, members[name], members
        if whiteout in members or any(
            str(parent.with_name(".wh." + parent.name)) in members
            for parent in path.parents if parent.name
        ):
            return None
        if any(str(parent / ".wh..wh..opq") in members for parent in path.parents):
            return None
    return None


def extract(image, found, output):
    layer, entry, members = found
    visited = set()
    while entry["type"] == tarfile.LNKTYPE.decode("ascii"):
        target = normalized(entry["linkname"])
        if target in visited:
            raise ValueError("cyclic archive hardlink")
        visited.add(target)
        entry = members[target]
    info = tarfile.TarInfo(entry["name"])
    for key in ("mode", "size", "offset", "offset_data", "linkname", "sparse"):
        setattr(info, key, entry[key])
    info.type = entry["type"].encode("ascii")
    if not info.isfile():
        raise ValueError(f"not a regular file: {info.name}")
    source = image / layer
    mode = "r:"
    if (image / "archive.json").exists():
        archive_source = json.loads((image / "archive.json").read_text())
        location = archive_source["layers"][layer]
        if info.offset_data + info.size > location["size"]:
            raise ValueError("member exceeds Docker layer bounds")
        info.offset += location["offset"]
        info.offset_data += location["offset"]
        source = Path(archive_source["source"])
        mode = "r:*"
    with tarfile.open(source, mode=mode) as archive:
        with archive.extractfile(info) as stream, output.open("wb") as destination:
            shutil.copyfileobj(stream, destination)
    output.chmod(info.mode & 0o777)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="operation", required=True)
    command = commands.add_parser("prepare")
    command.add_argument("source", type=Path)
    command.add_argument("output", type=Path)
    for operation in ("paths", "digests", "extract", "exists", "link", "find"):
        command = commands.add_parser(operation)
        command.add_argument("image", type=Path)
        command.add_argument("layers", type=Path)
        if operation not in ("paths", "digests"):
            command.add_argument("member")
        if operation == "extract":
            command.add_argument("output", type=Path)
    args = parser.parse_args()
    if args.operation == "prepare":
        prepare(args.source, args.output)
        return 0
    if args.operation == "digests":
        digests = json.loads((args.image / "layer-digests.json").read_text())
        for layer in args.layers.read_text().splitlines():
            if layer:
                print(digests[layer])
        return 0
    layers = selected(args.image, args.layers)
    if args.operation == "paths":
        paths = {"/" + match.group().lstrip("/") for _, entries in layers
                 for name in entries if (match := STORE_PATH.search(name))}
        print("\n".join(sorted(paths)))
        return 0
    if args.operation == "find":
        names = {name for _, entries in layers for name in entries if re.fullmatch(args.member, name)}
        print("\n".join(sorted(names)))
        return 0 if names else 1
    found = lookup(layers, args.member)
    if found is None:
        return 1
    if args.operation == "extract":
        extract(args.image, found, args.output)
    elif args.operation == "link":
        entry = found[1]
        if entry["type"] != tarfile.SYMTYPE.decode("ascii"):
            raise ValueError(f"not a symlink: {args.member}")
        print(entry["linkname"])
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
