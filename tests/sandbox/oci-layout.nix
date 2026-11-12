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
      config = {
        Env = [ "SETTING=${value}" ];
        Labels.parent = "kept";
      };
      extraCommands = ''printf '%s\n' ${lib.escapeShellArg value} > generated-setting'';
    };
  gcProtection = import ./oci-layout-gc.nix {
    inherit pkgs tools;
    configuration = (((raw "first").stream or (raw "first")).override { fromImage = null; }).conf;
  };
  reservedOutput = import ./oci-layout-reserved.nix {
    inherit pkgs tools;
    configuration = (((raw "first").stream or (raw "first")).override { fromImage = null; }).conf;
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
  childImage = (raw "child").override {
    config.Labels = {
      parent = "kept";
      child = "kept";
    };
  };
  childConfiguration = ((childImage.stream or childImage).override { fromImage = null; }).conf;
  child = layout {
    name = "wrix-oci-layout-child-fixture";
    fromLayout = first;
    image = childImage;
    storeImage = (raw "child").override {
      contents = [ ];
      config = { };
      extraCommands = "";
    };
  };
  closure = pkgs.closureInfo { rootPaths = [ child ]; };
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
    test -s ${reservedOutput}/layers.json
    test -f ${gcProtection}/fixed-manifest.json
    grep -Fx ${first} ${closure}/store-paths
    grep -Fx ${first.storeLayers} ${closure}/store-paths
    grep -Fx ${package} ${closure}/store-paths
    for layout in ${first} ${second} ${child}; do
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
    wrix-image-tools oci-layout ${childConfiguration} "$TMPDIR/original-child" \
      --from-layout ${first} --store-layers ${child.storeLayers}
    diff -r --no-dereference "$TMPDIR/original-child" ${child}
    config="$(cat ${child}/wrix/config-digest)"
    jq -e '.config.Labels == {parent:"kept",child:"kept"}' "${child}/blobs/sha256/''${config#sha256:}"
    test "${first}/blobs/sha256/''${first#sha256:}" -ef "${child}/blobs/sha256/''${first#sha256:}"
    printf '%s\n' ${gcProtection} ${first} ${second} ${child} > "$out"
  ''
