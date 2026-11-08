# Preserve dockerTools' default size-based algorithm and layer budget, using
# the native implementation rather than its Python auto-layer script.
{ pkgs, planner }:
{
  closureRoots,
  excludePaths ? [ ],
  maxLayers ? 100,
  fromImage ? null,
  debug ? false,
}:
let
  parentLayout =
    if fromImage == null || !(builtins.isAttrs fromImage) then null else fromImage.ociLayout or null;
in
pkgs.runCommand "layers.json"
  {
    __structuredAttrs = true;
    exportReferencesGraph.graph = closureRoots;
    inherit parentLayout maxLayers;
    fromImage = if parentLayout == null then fromImage else null;
    nativeBuildInputs = [
      pkgs.jq
      pkgs.coreutils
      pkgs.gnutar
      planner
    ];
    excludePathsFile = pkgs.writeText "excludePaths" (
      pkgs.lib.concatMapStrings (path: path + "\n") excludePaths
    );
  }
  ''
    set -euo pipefail
    usedLayers=1
    if [ -n "''${parentLayout:-}" ]; then
      digest=$(jq -er '.manifests | if type == "array" and length == 1 then .[0].digest else error("expected one OCI manifest") end | select(test("^sha256:[0-9a-f]{64}$"))' "$parentLayout/index.json")
      manifest="$parentLayout/blobs/sha256/''${digest#sha256:}"
      printf '%s  %s\n' "''${digest#sha256:}" "$manifest" | sha256sum -c - > /dev/null
      baseImageLayersCount=$(jq -er '.layers | if type == "array" then length else error("expected OCI layers") end' "$manifest")
      (( usedLayers += baseImageLayersCount ))
    elif [ -n "''${fromImage:-}" ]; then
      baseImageLayersCount=$(tar -xOf "$fromImage" manifest.json | jq '.[0].Layers | length')
      (( usedLayers += baseImageLayersCount ))
    fi
    if ! (( usedLayers < maxLayers )); then
      echo >&2 "Error: usedLayers $usedLayers layers to store 'fromImage' and" \
                "'extraCommands', but only maxLayers=$maxLayers were" \
                "allowed. At least 1 layer is required to store contents."
      exit 1
    fi
    availableLayers=$(( maxLayers - usedLayers ))
    jq .graph "$NIX_ATTRS_JSON_FILE" > referencesGraph
    ${pkgs.lib.optionalString debug "export DEBUG=1"}
    ${pkgs.lib.getBin planner}/bin/wrix-auto-layer referencesGraph "$excludePathsFile" "$availableLayers" > "$out"
  ''
