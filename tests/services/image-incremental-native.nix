{ pkgs }:

let
  inherit (pkgs) dockerTools runCommandLocal writeText;
  tools = import ../../lib/verify/tools.nix { inherit pkgs; };
  imageToolsContract = import ../sandbox/image-tools.nix { inherit pkgs; };
  mkLayout = import ../../lib/sandbox/oci-layout.nix { inherit pkgs; };
  mkService =
    supervisor:
    import ../../lib/services/image.nix {
      inherit pkgs supervisor;
      celler = writeText "celler-fixture" "stable";
      asTarball = true;
    };
  first = mkService (writeText "supervisor-fixture" "first");
  second = mkService (writeText "supervisor-fixture" "second");
  payload = pkgs.writeTextDir "payload" "retained content";
  digest = import ../../lib/sandbox/archive-digest.nix {
    inherit pkgs layout;
    name = "wrix-archive-test";
  };
  image = dockerTools.streamLayeredImage {
    name = "wrix-archive-test";
    tag = "latest";
    architecture = "arm64";
    contents = [ payload ];
    config = {
      Labels = {
        "wrix.managed" = "true";
        "wrix.image.kind" = "service";
      };
      Env = [
        "QUOTED=a \"quote\""
        "UNICODE=λ"
        "HTML=<>&"
        "SPACES=two  words"
      ];
      Entrypoint = [ "/bin/sh" ];
    };
  };
  layout = mkLayout {
    inherit image;
    name = "wrix-archive-test-oci";
  };
  archive = import ../../lib/sandbox/docker-archive.nix {
    inherit pkgs layout;
    name = "wrix-archive-test";
  };
in
assert first.baseLayout.drvPath == second.baseLayout.drvPath;
assert first.ociLayout.drvPath != second.ociLayout.drvPath;
assert first.source.drvPath != second.source.drvPath;
assert first.source_kind == "docker-archive";
assert toString first.digestDerivation == toString first.digest;
assert
  !(pkgs.lib.hasInfix (builtins.unsafeDiscardStringContext (toString first.source)) first.digestDerivation.buildCommand);
runCommandLocal "service-image-incremental"
  {
    nativeBuildInputs = [
      pkgs.skopeo
      pkgs.jq
      tools
    ];
  }
  ''
    set -euo pipefail
    export HOME="$TMPDIR"
    test -e ${imageToolsContract}
    skopeo --insecure-policy copy --quiet \
      "docker-archive:${archive}" "oci:$TMPDIR/roundtrip:latest"
    actual=$(skopeo inspect --raw "oci:$TMPDIR/roundtrip:latest" | jq -r '.config.digest')
    [[ "$actual" == "$(cat ${digest})" ]]
    skopeo inspect --config "oci:$TMPDIR/roundtrip:latest" \
      | jq -e '.architecture == "arm64" and .os == "linux" and .config.Labels["wrix.managed"] == "true"'
    wrix-verify-tools image-payload ${archive} ${payload}/payload
    touch "$out"
  ''
