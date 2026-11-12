{
  pkgs,
  tools,
  configuration,
}:
let
  builder = ../../lib/sandbox/oci-layout-reserved-output.sh;
  probe = pkgs.writeShellScript "reserved-output-geometry" ''
    set -euo pipefail
    [[ "$1" == oci-layout ]]
    [[ "$3" == "$out/.result" ]]
    [[ "$(${pkgs.coreutils}/bin/stat -c %d "$out")" == "$(${pkgs.coreutils}/bin/stat -c %d "$(dirname "$3")")" ]]
    exec ${tools}/bin/wrix-image-tools "$@"
  '';
in
pkgs.runCommand "oci-reserved-output-boundary" { nativeBuildInputs = [ pkgs.jq ]; } ''
  set -euo pipefail
  bash ${builder} ${probe} ${configuration} --store-layers-only
  test -s "$out/layers.json"
  test ! -e "$out/.result"
  test -z "$(find "$out" -maxdepth 1 -name '.wrix-oci-*')"
  mkdir "$TMPDIR/existing"
  printf 'preserved\n' > "$TMPDIR/existing/marker"
  if out="$TMPDIR/existing" bash ${builder} ${tools}/bin/wrix-image-tools ${configuration}; then
    echo 'reserved builder accepted an existing destination' >&2
    exit 1
  fi
  grep -Fx preserved "$TMPDIR/existing/marker"
  ln -s absent "$TMPDIR/dangling"
  if out="$TMPDIR/dangling" bash ${builder} ${tools}/bin/wrix-image-tools ${configuration}; then
    echo 'reserved builder accepted a dangling destination' >&2
    exit 1
  fi
  test -L "$TMPDIR/dangling"
  printf '{}\n' > "$TMPDIR/invalid.json"
  if out="$TMPDIR/failed" bash ${builder} ${tools}/bin/wrix-image-tools "$TMPDIR/invalid.json"; then
    echo 'reserved builder accepted invalid configuration' >&2
    exit 1
  fi
  test ! -e "$TMPDIR/failed"
  mkdir "$TMPDIR/custom"
  cp "$(jq -r .customisation_layer ${configuration})/layer.tar" "$TMPDIR/custom/layer.tar"
  printf 'invalid checksum\n' > "$TMPDIR/custom/checksum"
  jq --arg custom "$TMPDIR/custom" '.customisation_layer = $custom' ${configuration} > "$TMPDIR/late.json"
  if out="$TMPDIR/late-failure" bash ${builder} ${tools}/bin/wrix-image-tools "$TMPDIR/late.json"; then
    echo 'reserved builder accepted a late producer failure' >&2
    exit 1
  fi
  test ! -e "$TMPDIR/late-failure"
''
