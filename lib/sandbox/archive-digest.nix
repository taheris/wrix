{
  pkgs,
  layout,
  name,
}:

let
  imageTools = import ./image-tools.nix { inherit pkgs; };
in
pkgs.runCommandLocal "${name}-digest"
  {
    nativeBuildInputs = [
      pkgs.skopeo
      imageTools
    ];
  }
  ''
    set -euo pipefail
    export HOME="$TMPDIR"
    skopeo inspect --config "oci:${layout}:latest" \
      | wrix-image-tools config-digest > "$out"
  ''
