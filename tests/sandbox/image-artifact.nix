{ pkgs }:
let
  tools = import ../../lib/verify/tools.nix { inherit pkgs; };
  layout = (import ./oci-layout.nix { inherit pkgs; }).fixtures.first;
  archive = (import ./image-stream.nix { inherit pkgs; }).fixtures.compressed;
in
pkgs.runCommand "native-image-artifact-consumers"
  {
    nativeBuildInputs = [
      pkgs.coreutils
      pkgs.jq
      pkgs.gnugrep
      tools
    ];
  }
  ''
    set -euo pipefail
    for format in oci docker; do
      if [[ "$format" == oci ]]; then
        source=${layout}
      else
        source=${archive}
      fi
      wrix-verify-tools image-artifact prepare "$source" "$format"
      jq -r '.[0].Layers[]' "$format/manifest.json" > "$format.layers"
      wrix-verify-tools image-artifact paths "$format" "$format.layers" > "$format.paths"
    done
    package=$(grep -- '-oci-layout-package-fixture$' oci.paths)
    wrix-verify-tools image-artifact extract oci oci.layers "$package/bin/hardlink" hardlink
    cmp hardlink "$package/bin/payload"
    test "$(stat -c %a hardlink)" = "$(stat -c %a "$package/bin/payload")"
    test "$(wrix-verify-tools image-artifact link oci oci.layers "$package/bin/link")" = payload
    wrix-verify-tools image-artifact extract oci oci.layers generated-setting setting
    printf 'first\n' > expected-setting
    cmp setting expected-setting
    package=$(grep -- '-image-stream-payload$' docker.paths)
    wrix-verify-tools image-artifact extract docker docker.layers "$package/share/image-stream-payload" payload
    cmp payload "$package/share/image-stream-payload"
    touch "$out"
  ''
