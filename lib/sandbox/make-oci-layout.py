#!/usr/bin/env python3
"""Compose cached package blobs and parent OCI layouts with dockerTools metadata."""

from __future__ import annotations

import argparse
import hashlib
import itertools
import json
import os
import pathlib
import sys
import tarfile
import tempfile
from collections.abc import Iterable
from dataclasses import asdict, dataclass
from datetime import datetime, timezone
from typing import Any, BinaryIO

LAYER_MEDIA_TYPE = "application/vnd.oci.image.layer.v1.tar"
CONFIG_MEDIA_TYPE = "application/vnd.oci.image.config.v1+json"
MANIFEST_MEDIA_TYPE = "application/vnd.oci.image.manifest.v1+json"
INDEX_MEDIA_TYPE = "application/vnd.oci.image.index.v1+json"


@dataclass(frozen=True)
class Layer:
    digest: str
    size: int
    paths: list[str] | str


def parse_time(value: str) -> datetime:
    if value == "now":
        return datetime.now(tz=timezone.utc)
    return datetime.fromisoformat(value)


def tar_dir(path: str) -> tarfile.TarInfo:
    info = tarfile.TarInfo(path)
    info.type = tarfile.DIRTYPE
    return info


def apply_filters(
    info: tarfile.TarInfo,
    *,
    mtime: int,
    uid: int,
    gid: int,
    uname: str,
    gname: str,
) -> tarfile.TarInfo:
    info.mtime = mtime
    info.uid = uid
    info.gid = gid
    info.uname = uname
    info.gname = gname
    return info


def store_root(info: tarfile.TarInfo) -> tarfile.TarInfo:
    info.mode = 0o755
    return info


def append_root(info: tarfile.TarInfo) -> tarfile.TarInfo:
    info.name = f"/{info.name}"
    return info


def iter_store_path(path: pathlib.Path) -> Iterable[pathlib.Path]:
    if path.is_symlink():
        return [path]
    return itertools.chain([path], path.rglob("*"))


def archive_paths_to(
    stream: BinaryIO,
    paths: list[str],
    *,
    mtime: int,
    uid: int,
    gid: int,
    uname: str,
    gname: str,
) -> None:
    with tarfile.open(fileobj=stream, mode="w|") as tar:
        for path in ["/nix", "/nix/store"]:
            tar.addfile(
                apply_filters(
                    store_root(tar_dir(path)),
                    mtime=mtime,
                    uid=uid,
                    gid=gid,
                    uname=uname,
                    gname=gname,
                )
            )

        for path_text in paths:
            for filename in sorted(iter_store_path(pathlib.Path(path_text))):
                info = append_root(tar.gettarinfo(filename))
                if info.islnk():
                    info.type = tarfile.REGTYPE
                    info.linkname = ""
                    info.size = filename.stat().st_size
                info = apply_filters(
                    info,
                    mtime=mtime,
                    uid=uid,
                    gid=gid,
                    uname=uname,
                    gname=gname,
                )
                if info.isfile():
                    with filename.open("rb") as handle:
                        tar.addfile(info, handle)
                else:
                    tar.addfile(info)


def write_blob_bytes(content: bytes, blobs_dir: pathlib.Path) -> tuple[str, int]:
    digest = hashlib.sha256(content).hexdigest()
    target = blobs_dir / digest
    if not target.exists():
        target.write_bytes(content)
    return digest, len(content)


class HashingWriter:
    def __init__(self, stream: BinaryIO) -> None:
        self.stream = stream
        self.digest = hashlib.sha256()
        self.size = 0

    def write(self, data: bytes) -> int:
        written = self.stream.write(data)
        self.digest.update(data[:written])
        self.size += written
        return written


def write_store_layer(
    paths: list[str],
    blobs_dir: pathlib.Path,
    *,
    mtime: int,
    uid: int,
    gid: int,
    uname: str,
    gname: str,
) -> Layer:
    with tempfile.NamedTemporaryFile(dir=blobs_dir.parent, delete=False) as temp:
        writer = HashingWriter(temp)
        archive_paths_to(
            writer,
            paths,
            mtime=mtime,
            uid=uid,
            gid=gid,
            uname=uname,
            gname=gname,
        )
        temp.flush()
        digest, size = writer.digest.hexdigest(), writer.size
        target = blobs_dir / digest
        os.replace(temp.name, target)
    return Layer(digest=digest, size=size, paths=paths)


def link_blob(source: pathlib.Path, blobs_dir: pathlib.Path, digest: str) -> None:
    target = blobs_dir / digest
    if not target.exists():
        target.symlink_to(source.absolute())


def add_layout_layers(layout: pathlib.Path, blobs_dir: pathlib.Path) -> tuple[list[Layer], dict[str, Any]]:
    manifest = json.loads((layout / "wrix" / "manifest.json").read_text(encoding="utf-8"))
    config_digest = manifest["config"]["digest"].removeprefix("sha256:")
    config = json.loads((layout / "blobs" / "sha256" / config_digest).read_text(encoding="utf-8"))
    layers = []
    for descriptor, history in zip(manifest["layers"], config["history"], strict=True):
        digest = descriptor["digest"].removeprefix("sha256:")
        link_blob(layout / "blobs" / "sha256" / digest, blobs_dir, digest)
        layers.append(Layer(digest, descriptor["size"], history["comment"]))
    return layers, config.get("config", {})


def add_store_layers(source: pathlib.Path, blobs_dir: pathlib.Path) -> list[Layer]:
    layers = [Layer(**entry) for entry in json.loads((source / "layers.json").read_text(encoding="utf-8"))]
    for layer in layers:
        link_blob(source / "blobs" / "sha256" / layer.digest, blobs_dir, layer.digest)
    return layers


def overlay_base_config(base_config: dict[str, Any], final_config: dict[str, Any]) -> dict[str, Any]:
    result = dict(final_config)
    final_env = base_config.get("Env", []) + final_config.get("Env", [])
    if final_env:
        resolved = {entry.split("=", 1)[0]: entry for entry in final_env}
        result["Env"] = list(resolved.values())
    return result


def descriptor_for_layer(layer: Layer) -> dict[str, Any]:
    return {
        "mediaType": LAYER_MEDIA_TYPE,
        "digest": f"sha256:{layer.digest}",
        "size": layer.size,
    }


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("conf", type=pathlib.Path)
    parser.add_argument("out", type=pathlib.Path)
    parser.add_argument("--from-layout", type=pathlib.Path)
    parser.add_argument("--store-layers", type=pathlib.Path)
    parser.add_argument("--store-layers-only", action="store_true")
    args = parser.parse_args()
    conf_path = args.conf
    out = args.out
    conf = json.loads(conf_path.read_text(encoding="utf-8"))
    if conf.get("from_image") is not None:
        raise ValueError("OCI assembly requires an OCI parent layout, not a whole-image archive")

    blobs_dir = out / "blobs" / "sha256"
    wrix_dir = out / "wrix"
    blobs_dir.mkdir(parents=True)
    wrix_dir.mkdir(parents=True)

    created = parse_time(conf["created"])
    mtime = int(parse_time(conf["mtime"]).timestamp())
    uid = int(conf["uid"])
    gid = int(conf["gid"])
    uname = conf["uname"]
    gname = conf["gname"]

    if args.store_layers:
        store_layers = add_store_layers(args.store_layers, blobs_dir)
    else:
        store_layers = []
        for store_layer in conf["store_layers"]:
            print(f"Creating OCI layer from paths: {store_layer}", file=sys.stderr)
            store_layers.append(
                write_store_layer(
                    store_layer,
                    blobs_dir,
                    mtime=mtime,
                    uid=uid,
                    gid=gid,
                    uname=uname,
                    gname=gname,
                )
            )
    if args.store_layers_only:
        (out / "layers.json").write_text(json.dumps([asdict(layer) for layer in store_layers]) + "\n", encoding="utf-8")
        return

    if args.from_layout:
        base_layers, base_config = add_layout_layers(args.from_layout, blobs_dir)
    else:
        base_layers, base_config = [], {}
    layers = base_layers + store_layers

    customisation_layer = pathlib.Path(conf["customisation_layer"])
    custom_digest = (customisation_layer / "checksum").read_text(encoding="utf-8").strip()
    custom_size = (customisation_layer / "layer.tar").stat().st_size
    link_blob(customisation_layer / "layer.tar", blobs_dir, custom_digest)
    layers.append(Layer(digest=custom_digest, size=custom_size, paths=[str(customisation_layer)]))

    image_config = {
        "created": datetime.isoformat(created),
        "architecture": conf["architecture"],
        "os": conf["os"],
        "config": overlay_base_config(base_config, conf["config"]),
        "rootfs": {
            "diff_ids": [f"sha256:{layer.digest}" for layer in layers],
            "type": "layers",
        },
        "history": [
            {
                "created": datetime.isoformat(created),
                "comment": layer.paths if isinstance(layer.paths, str) else f"store paths: {layer.paths}",
            }
            for layer in layers
        ],
    }
    config_bytes = json.dumps(image_config, indent=4).encode("utf-8")
    config_digest, config_size = write_blob_bytes(config_bytes, blobs_dir)

    layer_descriptors = [descriptor_for_layer(layer) for layer in layers]
    manifest = {
        "schemaVersion": 2,
        "mediaType": MANIFEST_MEDIA_TYPE,
        "config": {
            "mediaType": CONFIG_MEDIA_TYPE,
            "digest": f"sha256:{config_digest}",
            "size": config_size,
        },
        "layers": layer_descriptors,
    }
    manifest_bytes = json.dumps(manifest, indent=4).encode("utf-8")
    manifest_digest, manifest_size = write_blob_bytes(manifest_bytes, blobs_dir)

    repo_tag = conf["repo_tag"]
    oci_ref = repo_tag.rsplit(":", 1)[1] if ":" in repo_tag else "latest"
    index = {
        "schemaVersion": 2,
        "mediaType": INDEX_MEDIA_TYPE,
        "manifests": [
            {
                "mediaType": MANIFEST_MEDIA_TYPE,
                "digest": f"sha256:{manifest_digest}",
                "size": manifest_size,
                "annotations": {"org.opencontainers.image.ref.name": oci_ref},
            }
        ],
    }

    (out / "oci-layout").write_text('{"imageLayoutVersion":"1.0.0"}\n', encoding="utf-8")
    (out / "index.json").write_text(json.dumps(index, indent=4) + "\n", encoding="utf-8")
    (wrix_dir / "manifest.json").write_bytes(manifest_bytes)
    (wrix_dir / "config-digest").write_text(f"sha256:{config_digest}\n", encoding="utf-8")

    descriptor_manifest = {
        "media_type": manifest["mediaType"],
        "config": manifest["config"],
        "layers": [
            descriptor | {"diff_id": f"sha256:{layer.digest}"}
            for descriptor, layer in zip(layer_descriptors, layers, strict=True)
        ],
    }
    (wrix_dir / "descriptor-manifest.json").write_text(
        json.dumps(descriptor_manifest, indent=2) + "\n",
        encoding="utf-8",
    )


if __name__ == "__main__":
    main()
