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
pkgs.runCommand "layers.json"
  {
    __structuredAttrs = true;
    exportReferencesGraph.graph = closureRoots;
    inherit fromImage maxLayers;
    nativeBuildInputs = [
      pkgs.jq
      pkgs.gnutar
      planner
    ];
    excludePathsFile = pkgs.writeText "excludePaths" (
      pkgs.lib.concatMapStrings (path: path + "\n") excludePaths
    );
  }
  ''
    usedLayers=1
    if [ -n "$fromImage" ]; then
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
