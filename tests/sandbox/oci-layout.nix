{ pkgs }:
let
  inherit (pkgs) lib;
  tools = import ../../lib/sandbox/image-tools.nix { inherit pkgs; };
  dockerTools = import ../../lib/sandbox/docker-tools.nix { inherit pkgs; };
  layout = import ../../lib/sandbox/oci-layout.nix { inherit pkgs; };
  package = pkgs.runCommand "oci-layout-package-fixture" { } ''
    mkdir -p "$out/bin"
    printf 'fixture payload\n' > "$out/bin/payload"
    chmod 0751 "$out/bin/payload"
    ln "$out/bin/payload" "$out/bin/hardlink"
    ln -s payload "$out/bin/link"
  '';
  raw =
    value:
    dockerTools.buildLayeredImage {
      name = "wrix-oci-layout-fixture";
      created = "1970-01-01T00:00:01Z";
      contents = [ package ];
      config.Env = [ "SETTING=${value}" ];
      extraCommands = ''printf '%s\n' ${lib.escapeShellArg value} > generated-setting'';
    };
  storeImage = (raw "first").override {
    config = { };
    extraCommands = "";
  };
  first = layout {
    name = "wrix-oci-layout-fixture";
    image = raw "first";
    inherit storeImage;
  };
  second = layout {
    name = "wrix-oci-layout-fixture";
    image = raw "second";
    inherit storeImage;
  };
in
assert first.storeLayers.drvPath == second.storeLayers.drvPath;
pkgs.runCommandLocal "oci-layout-builder"
  {
    passthru = {
      fixtures = { inherit first second tools; };
    };
    nativeBuildInputs = [
      pkgs.coreutils
      pkgs.gnutar
      pkgs.jq
      tools
    ];
  }
  ''
    set -euo pipefail
    for layout in ${first} ${second}; do
      while IFS= read -r descriptor; do
        digest="$(jq -r .digest <<<"$descriptor")"
        size="$(jq -r .size <<<"$descriptor")"
        blob="$layout/blobs/sha256/''${digest#sha256:}"
        test "$(sha256sum "$blob" | cut -d ' ' -f 1)" = "''${digest#sha256:}"
        test "$(wc -c < "$blob" | tr -d ' ')" = "$size"
      done < <(jq -c '.config, .layers[]' "$layout/wrix/manifest.json")
      config="$(cat "$layout/wrix/config-digest")"
      jq -e --slurpfile manifest "$layout/wrix/manifest.json" \
        '.rootfs.diff_ids == [$manifest[0].layers[].digest] and (.history | length) == ($manifest[0].layers | length)' \
        "$layout/blobs/sha256/''${config#sha256:}"
      digest="$(jq -r '.layers[0].digest' "$layout/wrix/manifest.json")"
      tar --absolute-names -xOf "$layout/blobs/sha256/''${digest#sha256:}" ${package}/bin/hardlink > payload
      cmp payload ${package}/bin/payload
    done
    first="$(jq -r '.layers[0].digest' ${first}/wrix/manifest.json)"
    second="$(jq -r '.layers[0].digest' ${second}/wrix/manifest.json)"
    test "$first" = "$second"
    test "${first}/blobs/sha256/''${first#sha256:}" -ef "${second}/blobs/sha256/''${second#sha256:}"
    test "$(jq -r '.layers[-1].digest' ${first}/wrix/manifest.json)" != "$(jq -r '.layers[-1].digest' ${second}/wrix/manifest.json)"
    config="$(cat ${second}/wrix/config-digest)"
    jq -e '.config.Env == ["SETTING=second"]' "${second}/blobs/sha256/''${config#sha256:}"
    touch "$out"
  ''
