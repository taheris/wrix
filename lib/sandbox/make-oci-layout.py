#!/usr/bin/env python3
"""Build an OCI layout from a dockerTools streamLayeredImage config."""

from __future__ import annotations

import hashlib
import io
import itertools
import json
import os
import pathlib
import shutil
import sys
import tarfile
import tempfile
from collections.abc import Iterable
from dataclasses import dataclass
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
    paths: list[str]


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


def sha256_file(path: pathlib.Path) -> tuple[str, int]:
    digest = hashlib.sha256()
    size = 0
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
            size += len(chunk)
    return digest.hexdigest(), size


def copy_blob(source: pathlib.Path, blobs_dir: pathlib.Path) -> tuple[str, int]:
    digest, size = sha256_file(source)
    target = blobs_dir / digest
    if not target.exists():
        shutil.copyfile(source, target)
    return digest, size


def write_blob_bytes(content: bytes, blobs_dir: pathlib.Path) -> tuple[str, int]:
    digest = hashlib.sha256(content).hexdigest()
    target = blobs_dir / digest
    if not target.exists():
        target.write_bytes(content)
    return digest, len(content)


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
    with tempfile.NamedTemporaryFile(dir=blobs_dir.parent) as temp:
        archive_paths_to(
            temp,
            paths,
            mtime=mtime,
            uid=uid,
            gid=gid,
            uname=uname,
            gname=gname,
        )
        temp.flush()
        digest, size = sha256_file(pathlib.Path(temp.name))
        target = blobs_dir / digest
        if not target.exists():
            shutil.copyfile(temp.name, target)
    return Layer(digest=digest, size=size, paths=paths)


def read_json_member(archive: tarfile.TarFile, member: str) -> Any:
    extracted = archive.extractfile(member)
    if extracted is None:
        raise ValueError(f"{member} is not a regular file")
    with extracted:
        return json.load(extracted)


def add_base_layers(from_image: str | None, blobs_dir: pathlib.Path) -> tuple[list[Layer], dict[str, Any]]:
    if from_image is None:
        return [], {}

    layers: list[Layer] = []
    with tarfile.open(from_image, mode="r:*") as archive:
        manifest = read_json_member(archive, "manifest.json")
        image_config = read_json_member(archive, manifest[0]["Config"])
        for layer_name in manifest[0]["Layers"]:
            member = archive.getmember(layer_name)
            extracted = archive.extractfile(member)
            if extracted is None:
                raise ValueError(f"{layer_name} is not a regular file")
            with extracted, tempfile.NamedTemporaryFile(dir=blobs_dir.parent) as temp:
                shutil.copyfileobj(extracted, temp)
                temp.flush()
                digest, size = sha256_file(pathlib.Path(temp.name))
                target = blobs_dir / digest
                if not target.exists():
                    shutil.copyfile(temp.name, target)
            layers.append(Layer(digest=digest, size=size, paths=[layer_name]))
    return layers, image_config.get("config", {})


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
    if len(sys.argv) != 3:
        print("usage: make-oci-layout.py <docker-tools-conf.json> <out>", file=sys.stderr)
        sys.exit(2)

    conf_path = pathlib.Path(sys.argv[1])
    out = pathlib.Path(sys.argv[2])
    conf = json.loads(conf_path.read_text(encoding="utf-8"))

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

    from_image = conf.get("from_image")
    base_layers, base_config = add_base_layers(from_image, blobs_dir)
    layers = list(base_layers)

    for store_layer in conf["store_layers"]:
        print(f"Creating OCI layer from paths: {store_layer}", file=sys.stderr)
        layers.append(
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

    customisation_layer = pathlib.Path(conf["customisation_layer"])
    custom_digest, custom_size = copy_blob(customisation_layer / "layer.tar", blobs_dir)
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
                "comment": f"store paths: {layer.paths}",
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
