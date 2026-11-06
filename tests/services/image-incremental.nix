{ pkgs }:

let
  inherit (pkgs) dockerTools runCommandLocal writeText;
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
      pkgs.python3
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
    python - <<'PY'
    import io, json, tarfile
    with tarfile.open('${archive}') as archive:
        manifest = json.load(archive.extractfile('manifest.json'))[0]
        found = []
        for layer_name in manifest['Layers']:
            with tarfile.open(fileobj=io.BytesIO(archive.extractfile(layer_name).read())) as layer:
                for member in layer.getmembers():
                    if member.name.lstrip('./') == '${pkgs.lib.removePrefix "/" (toString payload)}/payload':
                        found.append(layer.extractfile(member).read())
        assert found == [b'retained content'], found
    PY
    touch "$out"
  ''
