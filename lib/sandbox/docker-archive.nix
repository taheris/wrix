{
  pkgs,
  layout,
  name,
}:

pkgs.runCommandLocal "${name}.tar" { nativeBuildInputs = [ pkgs.skopeo ]; } ''
  set -euo pipefail
  export HOME="$TMPDIR"
  skopeo --insecure-policy copy --quiet \
    "oci:${layout}:latest" "docker-archive:$out:${name}:latest"
''
