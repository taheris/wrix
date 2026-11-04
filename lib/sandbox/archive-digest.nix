{
  pkgs,
  layout,
  name,
}:

pkgs.runCommandLocal "${name}-digest"
  {
    nativeBuildInputs = [
      pkgs.skopeo
      pkgs.python3
    ];
  }
  ''
    set -euo pipefail
    export HOME="$TMPDIR"
    skopeo inspect --config "oci:${layout}:latest" \
      | python ${./config-digest.py} > "$out"
  ''
