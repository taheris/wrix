{ pkgs }:
let
  stream = import ../../lib/sandbox/image-stream.nix { inherit pkgs; };
  tools = import ../../lib/sandbox/image-tools.nix { inherit pkgs; };
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
  changedTime = child.override { mtime = "1970-01-01T00:00:02Z"; };
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
          changedTime
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
    ];
  }
  ''
    set -euo pipefail
    test -e ${unit}
    grep -F '${raw.streamScript}' ${raw} > /dev/null
    ${raw} > expected.tar
    gzip -cd ${compressed} | cmp expected.tar -
    while IFS= read -r digest; do
      test -L ${child.gzipParts}/"$digest.gz"
      test "$(readlink ${child.gzipParts}/"$digest.gz")" = "${compressed.gzipParts}/$digest.gz"
      test ! -L ${changedTime.gzipParts}/"$digest.gz"
    done < ${compressed.gzipParts}/order
    ${changedTime.stream} > changed-time.tar
    gzip -cd ${changedTime} | cmp changed-time.tar -
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
    touch "$out"
  ''
