{ pkgs }:
let
  stream = import ../../lib/sandbox/image-stream.nix { inherit pkgs; };
  tools = import ../../lib/sandbox/image-tools.nix { inherit pkgs; };
  reader = import ../../lib/verify/tools.nix { inherit pkgs; };
  payload = pkgs.writeTextDir "share/image-stream-payload" "streaming image fixture\n";
  args = {
    name = "wrix-stream-fixture";
    tag = "test";
    contents = [ payload ];
    maxLayers = 2;
  };
  original = pkgs.dockerTools.buildLayeredImage args;
  raw = stream.dockerTools.streamLayeredImage args;
  compressed = stream.dockerTools.buildLayeredImage args;
  overridden = compressed.override { config.Env = [ "WRIX_STREAM_FIXTURE=1" ]; };
  poisonParent =
    (pkgs.runCommand "forbidden-parent-archive" { } ''
      echo 'native child tried to realize a parent archive' >&2
      exit 99
    '')
    // {
      inherit (compressed) ociLayout gzipParts;
    };
  childPayload = pkgs.writeTextDir "share/image-stream-child" "child fixture\n";
  child = stream.dockerTools.buildLayeredImage (
    args
    // {
      name = "wrix-stream-child-fixture";
      fromImage = poisonParent;
      contents = [ childPayload ];
      layeringPipeline = pkgs.writeText "native-child-layering.json" (
        builtins.toJSON [
          [
            "remove_paths"
            [ (toString payload) ]
          ]
          [ "popularity_contest" ]
          [
            "limit_layers"
            2
          ]
        ]
      );
    }
  );
  defaultChild = stream.dockerTools.buildLayeredImage (
    args
    // {
      name = "wrix-default-stream-child-fixture";
      fromImage = poisonParent;
      contents = [ childPayload ];
      maxLayers = 6;
    }
  );
  changedTime = child.override { mtime = "1970-01-01T00:00:02Z"; };
  legacyParts = pkgs.runCommand "legacy-compressor-parts" { } ''
    mkdir -p "$out"
    cp ${compressed.gzipParts}/mtime "$out/mtime"
    printf '%s\n' '${pkgs.pigz}/bin/pigz -n' > "$out/compressor"
    while IFS= read -r digest; do
      printf 'forbidden legacy cached member\n' > "$out/$digest.gz"
    done < ${compressed.gzipParts}/order
  '';
  changedRecipe = child.override {
    fromImage = poisonParent // {
      gzipParts = legacyParts;
    };
  };
  uncompressed = compressed.override { compressor = "none"; };
  corruptCompressed = pkgs.runCommand "truncated-image-gzip" { } ''
    head -c -1 ${compressed} > "$out"
  '';
  budgetPlanner = import ../../lib/sandbox/auto-layer.nix {
    pkgs = pkgs.buildPackages;
    planner = tools;
  };
  budgetProbe = budgetPlanner {
    closureRoots = [ ];
    fromImage = poisonParent;
    maxLayers = 3;
  };
  budgetScript = pkgs.writeShellScript "native-parent-budget-probe" budgetProbe.buildCommand;
  unit = tools;
in
assert compressed.stream.streamScript == raw.streamScript;
assert overridden.stream.streamScript == raw.streamScript;
assert builtins.all (key: !(builtins.hasAttr key (builtins.getContext compressed.buildCommand))) (
  builtins.attrNames (builtins.getContext (toString original.stream))
);
pkgs.runCommandLocal "docker-image-stream"
  {
    passthru = {
      inherit unit;
      fixtures = {
        inherit
          raw
          compressed
          overridden
          child
          defaultChild
          changedTime
          changedRecipe
          uncompressed
          ;
      };
    };
    nativeBuildInputs = [
      pkgs.coreutils
      pkgs.gnugrep
      pkgs.gzip
      pkgs.diffutils
      pkgs.skopeo
      pkgs.jq
      pkgs.gnutar
    ];
  }
  ''
    set -euo pipefail
    test -e ${unit}
    grep -F '${raw.streamScript}' ${raw} > /dev/null
    ${raw} > expected.tar
    gzip -cd ${compressed} | cmp expected.tar -
    cmp ${uncompressed} expected.tar
    ${pkgs.lib.getExe reader} image-artifact prepare ${compressed} "$TMPDIR/immutable-inspection"
    jq -e --arg source ${compressed} '.source_identity.immutable_nix_store == $source' \
      "$TMPDIR/immutable-inspection/archive.json"
    if ${pkgs.lib.getExe reader} image-artifact prepare ${corruptCompressed} "$TMPDIR/corrupt-inspection"; then
      echo 'immutable source bypassed full gzip validation' >&2
      exit 1
    fi
    IFS= read -r compressor < ${compressed.gzipParts}/compressor
    [[ "$compressor" == '${pkgs.pigz}/bin/pigz -1 -n' ]]
    while IFS= read -r digest; do
      test -L ${child.gzipParts}/"$digest.gz"
      test "$(readlink ${child.gzipParts}/"$digest.gz")" = "${compressed.gzipParts}/$digest.gz"
      test ! -L ${changedTime.gzipParts}/"$digest.gz"
      test ! -L ${changedRecipe.gzipParts}/"$digest.gz"
    done < ${compressed.gzipParts}/order
    if maxLayers=3 parentLayout=${compressed.ociLayout} fromImage= bash ${budgetScript} > budget-output 2> budget-error; then
      echo 'default planner accepted an exhausted inherited layer budget' >&2
      exit 1
    fi
    grep -F 'At least 1 layer is required' budget-error
    if maxLayers=3 parentLayout= fromImage=${compressed} bash ${budgetScript} > archive-budget-output 2> archive-budget-error; then
      echo 'legacy archive planner accepted an exhausted inherited layer budget' >&2
      exit 1
    fi
    grep -F 'At least 1 layer is required' archive-budget-error
    cp -R ${compressed.ociLayout} "$TMPDIR/bad-parent"
    chmod u+w "$TMPDIR/bad-parent" "$TMPDIR/bad-parent/index.json"
    jq '.manifests[0].digest = "sha256:../../forbidden"' "$TMPDIR/bad-parent/index.json" > "$TMPDIR/bad-index.json"
    mv "$TMPDIR/bad-index.json" "$TMPDIR/bad-parent/index.json"
    if maxLayers=100 parentLayout="$TMPDIR/bad-parent" fromImage= bash ${budgetScript} > bad-parent-output 2> bad-parent-error; then
      echo 'default planner accepted an invalid OCI manifest digest' >&2
      exit 1
    fi
    cp -RL ${compressed.ociLayout} "$TMPDIR/changed-parent"
    parentDigest=$(jq -r '.manifests[0].digest' "$TMPDIR/changed-parent/index.json")
    parentManifest="$TMPDIR/changed-parent/blobs/sha256/''${parentDigest#sha256:}"
    chmod u+w "$parentManifest"
    printf '\n' >> "$parentManifest"
    if maxLayers=100 parentLayout="$TMPDIR/changed-parent" fromImage= bash ${budgetScript} > changed-parent-output 2> changed-parent-error; then
      echo 'default planner accepted a manifest whose digest no longer matches' >&2
      exit 1
    fi
    grep -F 'computed checksum did NOT match' changed-parent-error
    ${defaultChild.stream} > default-child.tar
    gzip -cd ${defaultChild} | cmp default-child.tar -
    while IFS= read -r digest; do
      test -L ${defaultChild.gzipParts}/"$digest.gz"
      test "$(readlink ${defaultChild.gzipParts}/"$digest.gz")" = "${compressed.gzipParts}/$digest.gz"
    done < ${compressed.gzipParts}/order
    test "$(jq '.layers | length' ${defaultChild.ociLayout}/wrix/manifest.json)" -le 6
    test "$(jq '.layers | length' ${defaultChild.ociLayout}/wrix/manifest.json)" -gt 2
    ${changedTime.stream} > changed-time.tar
    gzip -cd ${changedTime} | cmp changed-time.tar -
    ${changedRecipe.stream} > changed-recipe.tar
    gzip -cd ${changedRecipe} | cmp changed-recipe.tar -
    export HOME="$TMPDIR"
    skopeo --tmpdir "$TMPDIR" --insecure-policy copy --quiet docker-archive:expected.tar oci:"$TMPDIR/converted":test
    config="$(skopeo --tmpdir "$TMPDIR" inspect --raw oci:"$TMPDIR/converted":test | jq -r '.config.digest')"
    test "$(sha256sum "$TMPDIR/converted/blobs/sha256/''${config#sha256:}" | cut -d ' ' -f 1)" = "''${config#sha256:}"
    jq -e '.config.Env == ["WRIX_STREAM_FIXTURE=1"]' <(
      skopeo --tmpdir "$TMPDIR" --insecure-policy inspect --config docker-archive:${overridden}
    )
    cp -R ${raw.ociLayout} "$TMPDIR/bad-layout"
    chmod u+w "$TMPDIR/bad-layout" "$TMPDIR/bad-layout/blobs" "$TMPDIR/bad-layout/blobs/sha256"
    digest="$(jq -r '.layers[-1].digest' "$TMPDIR/bad-layout/wrix/manifest.json")"
    rm "$TMPDIR/bad-layout/blobs/sha256/''${digest#sha256:}"
    jq --arg layout "$TMPDIR/bad-layout" \
      '. + {wrix_oci_layout: $layout, wrix_parent_layout: null}' ${raw.conf} > bad-conf.json
    if ${raw.streamScript} "$PWD/bad-conf.json" 2> producer-error | gzip > partial.tar.gz; then
      echo 'archive pipeline accepted a failed native producer' >&2
      exit 1
    fi
    grep -F 'OCI filesystem operation failed' producer-error
    if ! (set +o pipefail; ${raw.streamScript} "$PWD/bad-conf.json" 2> negative-error | gzip > negative.tar.gz); then
      echo 'pipeline negative control did not expose a masked producer failure' >&2
      exit 1
    fi
    skopeo --tmpdir "$TMPDIR" --insecure-policy copy --quiet docker-archive:${child} oci:"$TMPDIR/child":test
    test "$(skopeo --tmpdir "$TMPDIR" --insecure-policy inspect --raw oci:"$TMPDIR/child":test | jq '.layers | length')" -gt 2
    skopeo --tmpdir "$TMPDIR" --insecure-policy copy --quiet docker-archive:default-child.tar oci:"$TMPDIR/default-child":test
    test "$(skopeo --tmpdir "$TMPDIR" --insecure-policy inspect --raw oci:"$TMPDIR/default-child":test | jq '.layers | length')" -le 6
    touch "$out"
  ''
