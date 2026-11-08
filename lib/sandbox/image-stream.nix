# Native archive export from cached OCI layers; parent archives are not re-read.
{ pkgs }:
let
  inherit (pkgs) lib;
  inherit (lib)
    assertMsg
    escapeShellArg
    getBin
    makeOverridable
    optionalString
    ;
  baseDockerTools = import ./docker-tools.nix { inherit pkgs; };
  imageTools = import ./image-tools.nix { pkgs = pkgs.buildPackages; };
  mkOciLayout = import ./oci-layout.nix { inherit pkgs; };
  mkGzipParts = import ./docker-gzip-parts.nix { inherit pkgs imageTools; };
  program = pkgs.writeShellScript "wrix-image-stream" ''
    set -euo pipefail
    exec ${getBin imageTools}/bin/wrix-image-tools docker-stream "$@"
  '';
  replaceStream =
    from: to: command:
    let
      bareFrom = builtins.unsafeDiscardStringContext (toString from);
      bareCommand = builtins.unsafeDiscardStringContext command;
      remaining = builtins.removeAttrs (builtins.getContext command) (
        builtins.attrNames (builtins.getContext (toString from))
      );
      text =
        lib.replaceStrings [ bareFrom ] [ (builtins.unsafeDiscardStringContext (toString to)) ]
          bareCommand;
    in
    assert assertMsg (
      builtins.length (lib.splitString bareFrom bareCommand) == 2
    ) "dockerTools compression command changed; review the native stream adapter";
    builtins.appendContext text (remaining // builtins.getContext (toString to));
  nativeStream =
    args: raw:
    let
      parentImage = args.fromImage or null;
      parent =
        if parentImage == null then
          null
        else
          parentImage.ociLayout or (throw "Wrix native image streams require a parent OCI layout");
      normalized = raw.override { fromImage = null; };
      countsArchiveParent = parent != null && (args.layeringPipeline or null) == null;
      conf =
        if countsArchiveParent then
          pkgs.runCommandLocal "wrix-native-stream-conf.json"
            {
              nativeBuildInputs = [ pkgs.jq ];
              passthru.imageTag = raw.conf.imageTag;
            }
            ''
              set -euo pipefail
              jq '.from_image = null' ${raw.conf} > "$out"
            ''
        else
          normalized.conf;
      ociLayout = mkOciLayout {
        image = normalized;
        name = "${args.name}-stream-oci";
        fromLayout = parent;
        configuration = conf;
        storeConfiguration = conf;
      };
      gzipParts = mkGzipParts {
        layout = ociLayout;
        inherit conf;
        parentParts = if parentImage == null then null else parentImage.gzipParts or null;
      };
      streamConf =
        pkgs.runCommandLocal "wrix-native-stream-source.json" { nativeBuildInputs = [ pkgs.jq ]; }
          ''
            set -euo pipefail
            jq --arg layout ${escapeShellArg (toString ociLayout)} \
              --arg parent ${escapeShellArg (optionalString (parent != null) (toString parent))} \
              '. + {wrix_oci_layout: $layout, wrix_parent_layout: (if $parent == "" then null else $parent end)}' \
              ${conf} > "$out"
          '';
    in
    normalized.overrideAttrs (previous: {
      streamScript = program;
      buildCommand = ''
        makeWrapper ${program} "$out" --add-flags ${streamConf}
      '';
      passthru = (previous.passthru or { }) // {
        inherit conf ociLayout gzipParts;
        inherit (conf) imageTag;
        streamScript = program;
      };
    });
in
{
  dockerTools = baseDockerTools // {
    streamLayeredImage = makeOverridable (
      args: nativeStream args (baseDockerTools.streamLayeredImage args)
    );
    buildLayeredImage = makeOverridable (
      args:
      let
        image = baseDockerTools.buildLayeredImage args;
        stream = nativeStream args image.stream;
      in
      image.overrideAttrs (previous: {
        buildCommand =
          if args ? compressor then
            ''
              set -euo pipefail
              ${replaceStream image.stream stream previous.buildCommand}
            ''
          else
            ''
              set -euo pipefail
              while IFS= read -r digest; do
                cat ${stream.gzipParts}/"$digest.gz"
              done < ${stream.gzipParts}/order > "$out"
              ${imageTools}/bin/wrix-image-tools docker-tail ${stream.ociLayout} ${stream.conf} \
                ${optionalString ((args.fromImage or null) != null) "--from-layout ${args.fromImage.ociLayout}"} \
                | ${pkgs.pigz}/bin/pigz -n -p "$NIX_BUILD_CORES" >> "$out"
            '';
        passthru = (previous.passthru or { }) // {
          inherit stream;
          inherit (stream)
            conf
            ociLayout
            gzipParts
            streamScript
            imageTag
            ;
        };
      })
    );
  };
}
