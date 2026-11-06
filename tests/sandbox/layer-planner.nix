{ pkgs }:
let
  tools = import ../../lib/sandbox/docker-tools.nix { inherit pkgs; };
  planner = import ../../lib/sandbox/image-tools.nix { inherit pkgs; };
  payload = pkgs.runCommand "native-layer-planner-payload" { } ''
    mkdir -p "$out/payload"
    printf '%s' 'native planner retained content' > "$out/payload/file"
    ln -s file "$out/payload/link"
    ln "$out/payload/file" "$out/payload/hardlink"
  '';
  dependent = pkgs.writeTextDir "dependency" (toString payload);
  common = {
    name = "native-layer-planner-fixture";
    tag = "latest";
    contents = [
      payload
      dependent
    ];
    maxLayers = 8;
    config.Labels."wrix.test.layer-planner" = "native";
  };
  defaultImage = tools.buildLayeredImage common;
  customArguments = common // {
    layeringPipeline = [
      [ "popularity_contest" ]
      [
        "limit_layers"
        7
      ]
    ];
  };
  customImage = tools.buildLayeredImage customArguments;
  originalDefaultImage = pkgs.dockerTools.buildLayeredImage common;
  originalCustomImage = pkgs.dockerTools.buildLayeredImage customArguments;
  archiveCompatibility = pkgs.runCommand "layer-planner-archive-compatibility" { } ''
    set -euo pipefail
    cmp ${originalDefaultImage} ${defaultImage}
    cmp ${originalCustomImage} ${customImage}
    touch "$out"
  '';
  repeatedImage = defaultImage.overrideAttrs (previous: {
    name = "${previous.name}-repeat";
  });
  inheritedImage = tools.buildLayeredImage (
    common
    // {
      name = "native-layer-planner-inherited-fixture";
      fromImage = defaultImage;
      # Match Wrix's explicit inherited-closure cut: neither upstream planner
      # automatically excludes a parent's store paths.
      layeringPipeline = [
        [
          "remove_paths"
          [
            (toString payload)
            (toString dependent)
          ]
        ]
        [ "popularity_contest" ]
        [
          "limit_layers"
          7
        ]
      ];
    }
  );
  native =
    pkgs.runCommand "native-layer-planner-archives"
      {
        nativeBuildInputs = [
          pkgs.skopeo
          pkgs.jq
          pkgs.gnutar
          pkgs.coreutils
        ];
        passthru = {
          inherit
            defaultImage
            customImage
            repeatedImage
            inheritedImage
            planner
            ;
        };
      }
      ''
        set -euo pipefail
        export HOME="$TMPDIR"
        cmp ${defaultImage} ${repeatedImage}
        test ! -e ${planner}/bin/planner-differential
        mkdir "$out"
        for kind in default custom inherited; do
          case "$kind" in
            default) archive=${defaultImage} ;;
            custom) archive=${customImage} ;;
            inherited) archive=${inheritedImage} ;;
          esac
          skopeo inspect --config "docker-archive:$archive" > "$out/$kind-config.json"
          jq -e '.os == "linux" and .config.Labels["wrix.test.layer-planner"] == "native"' "$out/$kind-config.json"
          tar -xOf "$archive" manifest.json > manifest.json
          jq -e '.[0].Layers | length > 0 and length <= 8' manifest.json
          index=0
          found=0
          while IFS= read -r layer; do
            # Empty customization layers can share a checksum/member name.
            # Read one member, not GNU tar's concatenation of every match.
            tar --occurrence=1 -xOf "$archive" "$layer" > layer.tar
            tar -tf layer.tar >/dev/null
            expected=$(jq -r --argjson index "$index" '.rootfs.diff_ids[$index]' "$out/$kind-config.json")
            actual="sha256:$(sha256sum layer.tar | cut -d' ' -f1)"
            test "$actual" = "$expected"
            if content=$(tar --absolute-names -xOf layer.tar ${payload}/payload/file 2>/dev/null); then
              test "$content" = 'native planner retained content'
              (( found += 1 ))
            fi
            (( index += 1 ))
          done < <(jq -r '.[0].Layers[]' manifest.json)
          test "$found" = 1
        done
      '';
  python = pkgs.python3.withPackages (ps: [
    ps.igraph
    ps.toolz
  ]);
  upstreamSource = pkgs.path + "/pkgs/by-name/fl/flatten-references-graph/src";
  upstream = pkgs.runCommand "upstream-layer-planner-tests" { } ''
    set -euo pipefail
    export PYTHONDONTWRITEBYTECODE=1
    cd ${upstreamSource}
    ${python}/bin/python3 -m unittest discover -p '*_test.py' > "$out" 2>&1
  '';
  differentialRunner = planner.overrideAttrs (_previous: {
    pname = "wrix-layer-planner-differential";
    cargoBuildFlags = [
      "--package"
      "wrix-image-tools"
      "--example"
      "planner-differential"
    ];
    installPhase = ''
      runHook preInstall
      mkdir -p "$out/bin"
      cp target/${pkgs.stdenv.hostPlatform.rust.rustcTarget}/release/examples/planner-differential "$out/bin/"
      runHook postInstall
    '';
    meta = _previous.meta // {
      mainProgram = "planner-differential";
    };
  });
  defaultReference = pkgs.writeShellScript "upstream-auto-layer-reference" ''
    exec ${pkgs.python3}/bin/python3 ${pkgs.path + "/pkgs/build-support/docker/auto-layer.py"} "$@"
  '';
  differential = pkgs.runCommand "layer-planner-differential" { } ''
    set -euo pipefail
    mkdir "$out"
    ${differentialRunner}/bin/planner-differential custom \
      ${pkgs.flatten-references-graph}/bin/flatten_references_graph \
      ${planner}/bin/flatten_references_graph > "$out/custom.json"
    ${differentialRunner}/bin/planner-differential auto \
      ${defaultReference} ${planner}/bin/wrix-auto-layer > "$out/auto.json"
    test -e ${upstream}
    test -e ${archiveCompatibility}
  '';
in
{
  inherit native upstream differential;
}
