{
  pkgs,
  image,
  name,
  parent ? null,
}:

let
  imageTools = import ./image-tools.nix { inherit pkgs; };
  stream = image.stream or image;
in
pkgs.runCommandLocal "${name}-digest"
  {
    nativeBuildInputs = [
      imageTools
      pkgs.gnutar
      pkgs.jq
      pkgs.coreutils
      pkgs.skopeo
    ];
  }
  ''
    set -euo pipefail
    wrix-image-tools docker-tail ${stream.ociLayout} ${stream.conf} \
      ${pkgs.lib.optionalString (parent != null) "--from-layout ${parent.ociLayout}"} \
      > "$TMPDIR/metadata.tar"
    config=$(tar -xOf "$TMPDIR/metadata.tar" manifest.json | jq -er '.[0].Config')
    tar -xOf "$TMPDIR/metadata.tar" "$config" > "$TMPDIR/config.json"
    mkdir "$TMPDIR/layout"
    cp -rs ${stream.ociLayout}/blobs "$TMPDIR/layout/blobs"
    test ! -L "$TMPDIR/layout/blobs/sha256"
    chmod u+w "$TMPDIR/layout/blobs/sha256"
    cp ${stream.ociLayout}/oci-layout "$TMPDIR/layout/oci-layout"
    digest=$(sha256sum "$TMPDIR/config.json" | cut -d ' ' -f 1)
    size=$(wc -c < "$TMPDIR/config.json")
    ln -sf "$TMPDIR/config.json" "$TMPDIR/layout/blobs/sha256/$digest"
    jq --arg digest "sha256:$digest" --argjson size "$size" \
      '.config.digest = $digest | .config.size = $size' \
      ${stream.ociLayout}/wrix/manifest.json > "$TMPDIR/manifest.json"
    digest=$(sha256sum "$TMPDIR/manifest.json" | cut -d ' ' -f 1)
    size=$(wc -c < "$TMPDIR/manifest.json")
    ln -sf "$TMPDIR/manifest.json" "$TMPDIR/layout/blobs/sha256/$digest"
    jq --arg digest "sha256:$digest" --argjson size "$size" \
      '.manifests[0].digest = $digest | .manifests[0].size = $size' \
      ${stream.ociLayout}/index.json > "$TMPDIR/layout/index.json"
    export HOME="$TMPDIR"
    skopeo inspect --config "oci:$TMPDIR/layout" | wrix-image-tools config-digest > "$out"
  ''
